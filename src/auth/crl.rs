//! CRL (Certificate Revocation List) Loading, Parsing, and Refresh
//!
//! Provides CRL consumption for agent-side mTLS revocation enforcement.
//! Parses CRL from disk, verifies signature against pinned CA,
//! builds an in-memory revoked-serial index, and refreshes from the manager.

use arc_swap::ArcSwap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, warn};
use x509_parser::prelude::FromDer;
use x509_parser::revocation_list::CertificateRevocationList;

/// CRL status reported via the health endpoint.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrlStatus {
    /// CRL loaded, signature valid, not expired.
    Valid,
    /// CRL loaded and signature valid, but nextUpdate has passed.
    Expired,
    /// No CRL file found on disk.
    Missing,
    /// CRL exists but failed signature verification -- fail-closed.
    Invalid,
    /// CRL fetch or load failed; operating in degraded (WebPKI-only) mode.
    Degraded,
}

impl std::fmt::Display for CrlStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CrlStatus::Valid => write!(f, "valid"),
            CrlStatus::Expired => write!(f, "expired"),
            CrlStatus::Missing => write!(f, "missing"),
            CrlStatus::Invalid => write!(f, "invalid"),
            CrlStatus::Degraded => write!(f, "degraded"),
        }
    }
}

/// In-memory CRL state, atomically swapped on refresh via ArcSwap.
#[derive(Debug, Clone)]
pub struct CrlState {
    /// Hex-encoded serial numbers of revoked certificates (lowercase, no prefix).
    pub revoked_serials: HashSet<String>,
    /// CRL status for health reporting.
    pub status: CrlStatus,
    /// Time the CRL file was last modified (used to compute age).
    pub crl_mtime: Option<SystemTime>,
    /// When this CrlState was loaded into memory.
    pub loaded_at: SystemTime,
}

impl Default for CrlState {
    fn default() -> Self {
        Self {
            revoked_serials: HashSet::new(),
            status: CrlStatus::Missing,
            crl_mtime: None,
            loaded_at: SystemTime::now(),
        }
    }
}

impl CrlState {
    /// Check whether a certificate serial is revoked.
    pub fn is_revoked(&self, serial_hex: &str) -> bool {
        self.revoked_serials.contains(serial_hex)
    }

    /// Age of the on-disk CRL file in seconds.
    pub fn crl_age_seconds(&self) -> Option<u64> {
        self.crl_mtime.and_then(|mtime| {
            SystemTime::now()
                .duration_since(mtime)
                .ok()
                .map(|d| d.as_secs())
        })
    }
}

/// Shared, atomically-swappable CRL handle.
pub type SharedCrlState = Arc<ArcSwap<CrlState>>;

/// Create a new shared CRL state (initially missing).
pub fn new_shared_state() -> SharedCrlState {
    Arc::new(ArcSwap::from_pointee(CrlState::default()))
}

/// Extract the hex-encoded serial from a DER-encoded X.509 certificate.
/// Returns lowercase hex with no separators or prefix.
pub fn cert_serial_hex(cert_der: &[u8]) -> Option<String> {
    x509_parser::parse_x509_certificate(cert_der)
        .ok()
        .map(|(_, cert)| format_serial_hex(&cert.serial))
}

/// Format a BigUint serial as lowercase hex string (no 0x prefix, no colons).
fn format_serial_hex(serial: &x509_parser::num_bigint::BigUint) -> String {
    let bytes = serial.to_bytes_be();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Load and validate a CRL from disk.
///
/// Steps:
/// 1. Read PEM file
/// 2. Parse CRL with x509-parser
/// 3. Verify CRL signature against the CA certificate
/// 4. Build in-memory revoked-serial index
/// 5. Check nextUpdate for staleness
///
/// Returns the new CrlState. On signature failure, returns CrlStatus::Invalid (fail-closed).
/// On missing file, returns CrlStatus::Missing. On parse error, returns CrlStatus::Degraded.
pub fn load_crl(crl_path: &Path, ca_cert_der: &[u8]) -> CrlState {
    let crl_bytes = match fs::read(crl_path) {
        Ok(b) => b,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                info!(path = %crl_path.display(), "No CRL file found -- operating in WebPKI-only mode");
                return CrlState {
                    status: CrlStatus::Missing,
                    crl_mtime: None,
                    loaded_at: SystemTime::now(),
                    revoked_serials: HashSet::new(),
                };
            }
            warn!(path = %crl_path.display(), error = %e, "Failed to read CRL file");
            return CrlState {
                status: CrlStatus::Degraded,
                crl_mtime: None,
                loaded_at: SystemTime::now(),
                revoked_serials: HashSet::new(),
            };
        }
    };

    let crl_mtime = fs::metadata(crl_path).ok().and_then(|m| m.modified().ok());

    // Parse PEM: extract the DER block between BEGIN/END X509 CRL markers
    let crl_der = match extract_pem_crl_der(&crl_bytes) {
        Some(der) => der,
        None => {
            // Try parsing as raw DER
            crl_bytes.clone()
        }
    };

    // Parse CRL
    let (_, crl) = match CertificateRevocationList::from_der(&crl_der) {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "Failed to parse CRL -- marking as invalid");
            return CrlState {
                status: CrlStatus::Invalid,
                crl_mtime,
                loaded_at: SystemTime::now(),
                revoked_serials: HashSet::new(),
            };
        }
    };

    // Verify CRL signature against CA
    let (_, ca_cert) = match x509_parser::parse_x509_certificate(ca_cert_der) {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "Failed to parse CA cert for CRL signature verification");
            return CrlState {
                status: CrlStatus::Invalid,
                crl_mtime,
                loaded_at: SystemTime::now(),
                revoked_serials: HashSet::new(),
            };
        }
    };

    let verify_result = crl.verify_signature(ca_cert.public_key());

    if let Err(e) = verify_result {
        error!(error = %e, "CRL signature verification FAILED -- refusing to use this CRL (fail-closed)");
        return CrlState {
            status: CrlStatus::Invalid,
            crl_mtime,
            loaded_at: SystemTime::now(),
            revoked_serials: HashSet::new(),
        };
    }

    // Build revoked serial index
    let revoked_serials: HashSet<String> = crl
        .iter_revoked_certificates()
        .map(|revoked| format_serial_hex(revoked.serial()))
        .collect();

    info!(
        revoked_count = revoked_serials.len(),
        "CRL loaded and signature verified"
    );

    // Check nextUpdate for staleness
    let now = x509_parser::time::ASN1Time::now();
    let is_expired = crl.next_update().map(|next| next < now).unwrap_or(false);

    let status = if is_expired {
        warn!("CRL nextUpdate has passed -- CRL is stale, continuing with degraded status");
        CrlStatus::Expired
    } else {
        CrlStatus::Valid
    };

    CrlState {
        revoked_serials,
        status,
        crl_mtime,
        loaded_at: SystemTime::now(),
    }
}

/// Extract DER bytes from a PEM-encoded CRL.
/// Looks for `-----BEGIN X509 CRL-----` / `-----END X509 CRL-----` blocks.
fn extract_pem_crl_der(pem_bytes: &[u8]) -> Option<Vec<u8>> {
    let pem_str = String::from_utf8_lossy(pem_bytes);
    let begin_marker = "-----BEGIN X509 CRL-----";
    let end_marker = "-----END X509 CRL-----";

    let begin_idx = pem_str.find(begin_marker)?;
    let after_begin = begin_idx + begin_marker.len();
    let end_idx = pem_str[after_begin..].find(end_marker)?;
    let b64_block = pem_str[after_begin..after_begin + end_idx].trim();

    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64_block)
        .ok()
}

/// Fetch the CRL from the manager, verify, persist, and update in-memory state.
///
/// The CRL endpoint is public (no auth): GET {manager_url}/api/v1/pki/crl.pem
pub async fn refresh_crl(
    manager_url: &str,
    crl_path: &Path,
    ca_cert_der: &[u8],
    shared_state: &SharedCrlState,
) -> Result<(), String> {
    let crl_url = format!("{}/api/v1/pki/crl.pem", manager_url.trim_end_matches('/'));

    info!(url = %crl_url, "Fetching CRL from manager");

    let response = reqwest::get(&crl_url)
        .await
        .map_err(|e| format!("CRL fetch request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(format!("CRL fetch returned HTTP {}", status));
    }

    let crl_pem = response
        .text()
        .await
        .map_err(|e| format!("Failed to read CRL response body: {}", e))?;

    // Persist to disk (atomic write via temp file)
    let parent = crl_path.parent().unwrap_or(Path::new("/tmp"));
    if !parent.exists() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create CRL directory: {}", e))?;
    }

    let tmp_path = crl_path.with_extension("pem.tmp");
    fs::write(&tmp_path, &crl_pem).map_err(|e| format!("Failed to write temp CRL file: {}", e))?;

    fs::rename(&tmp_path, crl_path)
        .map_err(|e| format!("Failed to rename temp CRL file: {}", e))?;

    debug!(path = %crl_path.display(), "CRL persisted to disk");

    // Load the freshly written CRL to get a validated CrlState
    let new_state = load_crl(crl_path, ca_cert_der);

    if new_state.status == CrlStatus::Invalid {
        return Err("CRL signature verification failed after fetch".to_string());
    }

    info!(
        status = %new_state.status,
        revoked = new_state.revoked_serials.len(),
        "CRL refreshed successfully"
    );

    // Atomically swap the in-memory state
    shared_state.store(Arc::new(new_state));

    Ok(())
}

/// Spawn the CRL refresh background task.
///
/// Runs on a 24-hour interval. On failure, logs a warning and continues
/// serving with the existing (possibly stale) CRL.
pub fn spawn_crl_refresh_task(
    manager_url: String,
    crl_path: PathBuf,
    ca_cert_der: Vec<u8>,
    shared_state: SharedCrlState,
) {
    let interval = Duration::from_secs(24 * 60 * 60); // 24 hours

    tokio::spawn(async move {
        // Initial small delay to let the server finish binding
        tokio::time::sleep(Duration::from_secs(30)).await;

        loop {
            let result = refresh_crl(&manager_url, &crl_path, &ca_cert_der, &shared_state).await;

            match result {
                Ok(()) => {
                    info!("CRL background refresh completed successfully");
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        "CRL background refresh failed -- continuing with current CRL"
                    );
                }
            }

            tokio::time::sleep(interval).await;
        }
    });

    info!(
        interval_secs = interval.as_secs(),
        "CRL refresh background task spawned"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_serial_hex() {
        use x509_parser::num_bigint::BigUint;
        let serial = BigUint::from(0x0123_abcdu64);
        let hex = format_serial_hex(&serial);
        assert_eq!(hex, "0123abcd");
    }

    #[test]
    fn test_format_serial_hex_single_byte() {
        use x509_parser::num_bigint::BigUint;
        let serial = BigUint::from(0x42u64);
        let hex = format_serial_hex(&serial);
        assert_eq!(hex, "42");
    }

    #[test]
    fn test_crl_state_default_is_missing() {
        let state = CrlState::default();
        assert_eq!(state.status, CrlStatus::Missing);
        assert!(state.revoked_serials.is_empty());
        assert!(state.crl_mtime.is_none());
    }

    #[test]
    fn test_crl_state_is_revoked() {
        let mut state = CrlState::default();
        state.revoked_serials.insert("deadbeef".to_string());
        assert!(state.is_revoked("deadbeef"));
        assert!(!state.is_revoked("cafef00d"));
    }

    #[test]
    fn test_crl_status_display() {
        assert_eq!(CrlStatus::Valid.to_string(), "valid");
        assert_eq!(CrlStatus::Expired.to_string(), "expired");
        assert_eq!(CrlStatus::Missing.to_string(), "missing");
        assert_eq!(CrlStatus::Invalid.to_string(), "invalid");
        assert_eq!(CrlStatus::Degraded.to_string(), "degraded");
    }

    #[test]
    fn test_extract_pem_crl_der_invalid() {
        // Not PEM
        assert!(extract_pem_crl_der(b"not pem").is_none());
        // PEM but wrong type
        assert!(extract_pem_crl_der(
            b"-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----"
        )
        .is_none());
    }

    #[test]
    fn test_shared_crl_state_swap() {
        let shared = new_shared_state();
        let initial = shared.load();
        assert_eq!(initial.status, CrlStatus::Missing);

        let new_state = CrlState {
            status: CrlStatus::Valid,
            revoked_serials: {
                let mut set = HashSet::new();
                set.insert("abc".to_string());
                set
            },
            ..Default::default()
        };
        shared.store(Arc::new(new_state));

        let updated = shared.load();
        assert_eq!(updated.status, CrlStatus::Valid);
        assert!(updated.is_revoked("abc"));
    }
}
