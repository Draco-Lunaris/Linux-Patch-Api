//! mTLS Configuration Module
//!
//! Provides rustls-based mutual TLS configuration for the API server.
//!
//! # Architecture Decision Record: rustls as Authoritative Client-Auth Gate
//!
//! Client certificate authentication is enforced at the TLS handshake level by
//! rustls via `CrlAwareVerifier` (which wraps `WebPkiClientVerifier`). This means:
//!
//! - **rustls + CrlAwareVerifier IS the authoritative client-auth gate.**
//! - No application-layer certificate validation middleware is needed because
//!   rustls rejects connections that fail client-cert verification before any
//!   HTTP request is processed.
//! - `build_rustls_config()` configures the TLS listener to require client
//!   certificates (`with_client_cert_verifier`), making mTLS enforcement
//!   unavoidable at the transport layer.
//! - CRL revocation checking is integrated into the same handshake path via
//!   `CrlAwareVerifier`, so revoked certificates are also rejected before any
//!   HTTP handler runs.
//!
//! This design was chosen because rustls provides battle-tested X.509
//! verification, and enforcing auth at the TLS layer eliminates an entire
//! class of bypass vulnerabilities that application-layer checks are
//! susceptible to (e.g., middleware ordering bugs, route-specific skips).

use chrono::{DateTime, Utc};
use rustls::{
    client::danger::HandshakeSignatureValid,
    crypto::aws_lc_rs,
    pki_types::CertificateDer,
    server::{
        danger::{ClientCertVerified, ClientCertVerifier},
        ServerConfig, WebPkiClientVerifier,
    },
    version::TLS13,
    DigitallySignedStruct, DistinguishedName, Error as RustlsError, RootCertStore, SignatureScheme,
};
use rustls_pemfile::{certs, private_key};
use std::{fs::File, io::BufReader, sync::Arc};
use tracing::{error, info, warn};

use super::crl::{cert_serial_hex, SharedCrlState};

/// CRL-aware client certificate verifier.
///
/// Wraps WebPkiClientVerifier for chain validation, then checks the
/// end-entity certificate serial against the in-memory CRL index.
/// If CRL is unavailable (Missing/Degraded), falls back to WebPKI-only.
#[derive(Debug)]
struct CrlAwareVerifier {
    inner: Arc<dyn ClientCertVerifier>,
    crl_state: SharedCrlState,
}

impl CrlAwareVerifier {
    fn new(inner: Arc<dyn ClientCertVerifier>, crl_state: SharedCrlState) -> Self {
        Self { inner, crl_state }
    }
}

impl ClientCertVerifier for CrlAwareVerifier {
    fn offer_client_auth(&self) -> bool {
        self.inner.offer_client_auth()
    }

    fn client_auth_mandatory(&self) -> bool {
        self.inner.client_auth_mandatory()
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> Result<ClientCertVerified, RustlsError> {
        // 1. Delegate chain validation to WebPKI
        self.inner
            .verify_client_cert(end_entity, intermediates, now)?;

        // 2. Check CRL revocation status
        let crl = self.crl_state.load();
        match crl.status {
            super::crl::CrlStatus::Valid | super::crl::CrlStatus::Expired => {
                // CRL is available -- check serial
                if let Some(serial_hex) = cert_serial_hex(end_entity.as_ref()) {
                    if crl.is_revoked(&serial_hex) {
                        warn!(
                            serial = %serial_hex,
                            "Client certificate is revoked per CRL -- rejecting connection"
                        );
                        return Err(RustlsError::InvalidCertificate(
                            rustls::CertificateError::Revoked,
                        ));
                    }
                }
                Ok(ClientCertVerified::assertion())
            }
            super::crl::CrlStatus::Missing | super::crl::CrlStatus::Degraded => {
                // No CRL available -- fall back to WebPKI-only (already passed above)
                warn!(
                    status = %crl.status,
                    "CRL not available -- allowing connection with WebPKI-only verification"
                );
                Ok(ClientCertVerified::assertion())
            }
            super::crl::CrlStatus::Invalid => {
                // Invalid CRL signature -- fail-closed
                error!(
                    "CRL signature is invalid -- refusing all client certificates (fail-closed)"
                );
                Err(RustlsError::InvalidCertificate(
                    rustls::CertificateError::Revoked,
                ))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// mTLS Configuration
///
/// TLS 1.3 is the only supported protocol version — this is hardcoded
/// in `build_rustls_config()` and cannot be configured via this struct.
#[derive(Debug, Clone)]
pub struct MtlsConfig {
    pub ca_cert_path: String,
    pub server_cert_path: String,
    pub server_key_path: String,
}

/// Build a rustls ServerConfig with client certificate verification.
///
/// This is the authoritative mTLS gate — rustls enforces client certificate
/// validation at the TLS handshake level, before any HTTP request is processed.
///
/// When `crl_state` is provided and the CRL is available, wraps the
/// WebPkiClientVerifier with CrlAwareVerifier for revocation checking.
/// When CRL is missing/degraded, falls back to WebPKI-only verification.
pub fn build_rustls_config(
    config: &MtlsConfig,
    crl_state: Option<SharedCrlState>,
) -> Result<Arc<ServerConfig>, MtlsError> {
    let cert_store = load_ca_certs(&config.ca_cert_path)?;

    let webpki_verifier = WebPkiClientVerifier::builder(cert_store.clone().into())
        .build()
        .map_err(|e| MtlsError::ClientVerifierError(e.to_string()))?;

    let client_verifier: Arc<dyn ClientCertVerifier> = match crl_state {
        Some(state) => {
            info!("CRL-aware client verification enabled");
            Arc::new(CrlAwareVerifier::new(webpki_verifier, state))
        }
        None => {
            info!("No CRL state provided -- using WebPKI-only client verification");
            webpki_verifier
        }
    };

    let server_cert = load_certs(&config.server_cert_path)?;
    let server_key = load_private_key(&config.server_key_path)?;

    let server_config =
        ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_protocol_versions(&[&TLS13])
            .map_err(|e| {
                MtlsError::ServerConfigError(format!("Failed to set TLS 1.3 only: {}", e))
            })?
            .with_client_cert_verifier(client_verifier)
            .with_single_cert(server_cert, server_key)
            .map_err(|e| MtlsError::ServerConfigError(e.to_string()))?;

    Ok(Arc::new(server_config))
}

/// Load CA certificates from PEM file
fn load_ca_certs(path: &str) -> Result<RootCertStore, MtlsError> {
    let mut cert_store = RootCertStore::empty();

    let cert_file = File::open(path)
        .map_err(|e| MtlsError::IoError(format!("Failed to open CA cert {}: {}", path, e)))?;
    let mut reader = BufReader::new(cert_file);

    let certs = certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| MtlsError::ParseError(format!("Failed to parse CA certs: {}", e)))?;

    for cert in certs {
        cert_store
            .add(cert)
            .map_err(|e| MtlsError::StoreError(format!("Failed to add CA cert to store: {}", e)))?;
    }

    info!("Loaded CA certificates from {}", path);
    Ok(cert_store)
}

/// Load server certificates from PEM file
fn load_certs(path: &str) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, MtlsError> {
    let cert_file = File::open(path)
        .map_err(|e| MtlsError::IoError(format!("Failed to open cert {}: {}", path, e)))?;
    let mut reader = BufReader::new(cert_file);

    let certs = certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| MtlsError::ParseError(format!("Failed to parse server certs: {}", e)))?;

    Ok(certs)
}

/// Load private key from PEM file
fn load_private_key(path: &str) -> Result<rustls::pki_types::PrivateKeyDer<'static>, MtlsError> {
    let key_file = File::open(path)
        .map_err(|e| MtlsError::IoError(format!("Failed to open key {}: {}", path, e)))?;
    let mut reader = BufReader::new(key_file);

    let key = private_key(&mut reader)
        .map_err(|e| MtlsError::ParseError(format!("Failed to parse private key: {}", e)))?
        .ok_or_else(|| MtlsError::ParseError("No private key found in file".to_string()))?;

    Ok(key)
}

/// Certificate information extracted from client certificate.
///
/// NOTE: This struct is preserved for potential future use in extracting
/// client certificate details from the TLS session at the application layer.
/// Client authentication is enforced at the TLS handshake level by
/// CrlAwareVerifier — this struct is NOT used for validation.
#[derive(Debug, Clone)]
pub struct ClientCertInfo {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

/// mTLS Error types
#[derive(Debug, thiserror::Error)]
pub enum MtlsError {
    #[error("IO error: {0}")]
    IoError(String),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Certificate store error: {0}")]
    StoreError(String),
    #[error("Client verifier error: {0}")]
    ClientVerifierError(String),
    #[error("Server config error: {0}")]
    ServerConfigError(String),
    #[error("Certificate validation error: {0}")]
    ValidationError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn init_crypto_provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    fn make_test_ca_and_root_store() -> (rcgen::KeyPair, RootCertStore) {
        let ca_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let mut ca_params = rcgen::CertificateParams::default();
        ca_params.not_before = time::OffsetDateTime::now_utc();
        ca_params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(365);
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "Test CA for Verifier");
        ca_params.distinguished_name = dn;
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let mut root_store = RootCertStore::empty();
        root_store.add(ca_cert.der().to_owned()).unwrap();

        (ca_key, root_store)
    }

    /// Test that CrlAwareVerifier can be constructed with a WebPKI verifier
    /// and a SharedCrlState. This verifies the wiring is correct.
    #[test]
    fn crl_aware_verifier_construction() {
        init_crypto_provider();
        use super::super::crl::{new_shared_state, CrlState, CrlStatus};

        let (_ca_key, root_store) = make_test_ca_and_root_store();

        let webpki_verifier: Arc<dyn ClientCertVerifier> =
            WebPkiClientVerifier::builder(root_store.into())
                .build()
                .unwrap();

        let crl_state = new_shared_state();
        let valid_state = CrlState {
            status: CrlStatus::Valid,
            revoked_serials: HashSet::new(),
            crl_mtime: None,
            loaded_at: std::time::SystemTime::now(),
        };
        crl_state.store(Arc::new(valid_state));

        let _verifier = CrlAwareVerifier::new(webpki_verifier, crl_state);
    }

    /// Test that CrlAwareVerifier with Missing CRL state can be constructed.
    #[test]
    fn crl_aware_verifier_with_missing_crl() {
        init_crypto_provider();
        use super::super::crl::new_shared_state;

        let (_ca_key, root_store) = make_test_ca_and_root_store();

        let webpki_verifier: Arc<dyn ClientCertVerifier> =
            WebPkiClientVerifier::builder(root_store.into())
                .build()
                .unwrap();

        // Default state is Missing.
        let crl_state = new_shared_state();
        let _verifier = CrlAwareVerifier::new(webpki_verifier, crl_state);
    }

    /// Test that CrlAwareVerifier with Invalid CRL state can be constructed.
    #[test]
    fn crl_aware_verifier_with_invalid_crl() {
        init_crypto_provider();
        use super::super::crl::{new_shared_state, CrlState, CrlStatus};

        let (_ca_key, root_store) = make_test_ca_and_root_store();

        let webpki_verifier: Arc<dyn ClientCertVerifier> =
            WebPkiClientVerifier::builder(root_store.into())
                .build()
                .unwrap();

        let crl_state = new_shared_state();
        let invalid_state = CrlState {
            status: CrlStatus::Invalid,
            revoked_serials: HashSet::new(),
            crl_mtime: None,
            loaded_at: std::time::SystemTime::now(),
        };
        crl_state.store(Arc::new(invalid_state));

        let _verifier = CrlAwareVerifier::new(webpki_verifier, crl_state);
    }

    /// Test that CrlAwareVerifier with a revoked serial in Valid CRL state
    /// can be constructed.
    #[test]
    fn crl_aware_verifier_with_revoked_serial() {
        init_crypto_provider();
        use super::super::crl::{new_shared_state, CrlState, CrlStatus};

        let (_ca_key, root_store) = make_test_ca_and_root_store();

        let webpki_verifier: Arc<dyn ClientCertVerifier> =
            WebPkiClientVerifier::builder(root_store.into())
                .build()
                .unwrap();

        let crl_state = new_shared_state();
        let mut revoked = HashSet::new();
        revoked.insert("deadbeef".to_string());
        let valid_with_revoked = CrlState {
            status: CrlStatus::Valid,
            revoked_serials: revoked,
            crl_mtime: None,
            loaded_at: std::time::SystemTime::now(),
        };
        crl_state.store(Arc::new(valid_with_revoked));

        let _verifier = CrlAwareVerifier::new(webpki_verifier, crl_state);
    }
}
