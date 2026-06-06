//! Auth Module - mTLS, IP Whitelist, and Security Headers
//!
//! This module provides security authentication and authorization:
//! - mTLS (Mutual TLS) certificate-based authentication (enforced at TLS handshake by rustls)
//! - IP whitelist enforcement with CIDR subnet support
//! - Security header validation (VULN-006: duplicate critical header rejection)
//! - Silent drop for non-compliant connections
//! - Comprehensive audit logging
//!
//! # Architecture Decision Record: rustls as Authoritative Client-Auth Gate
//!
//! Client certificate authentication is enforced at the TLS handshake level by
//! rustls via `CrlAwareVerifier`. No application-layer certificate validation
//! middleware is needed — rustls rejects connections that fail client-cert
//! verification before any HTTP request is processed. See `mtls.rs` for details.

pub mod crl;
pub mod mtls;
pub mod security_headers;
pub mod whitelist;

pub use crl::{new_shared_state, CrlState, CrlStatus, SharedCrlState};
pub use mtls::{ClientCertInfo, MtlsConfig, MtlsError};
pub use security_headers::SecurityHeadersMiddleware;
pub use whitelist::{
    WhitelistConfig, WhitelistEntry, WhitelistManager, WhitelistMiddleware,
    WhitelistMiddlewareService,
};

/// Combined authentication result
#[derive(Debug, Clone)]
pub struct AuthResult {
    /// Whether mTLS authentication passed
    pub mtls_valid: bool,
    /// Whether IP is in whitelist
    pub ip_allowed: bool,
    /// Client certificate information (if available)
    pub cert_info: Option<ClientCertInfo>,
    /// Client IP address
    pub client_ip: Option<std::net::Ipv4Addr>,
}

impl AuthResult {
    /// Check if authentication is fully successful
    pub fn is_authenticated(&self) -> bool {
        self.mtls_valid && self.ip_allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_result_authenticated() {
        let result = AuthResult {
            mtls_valid: true,
            ip_allowed: true,
            cert_info: None,
            client_ip: Some("192.168.1.100".parse().unwrap()),
        };

        assert!(result.is_authenticated());
        assert!(result.mtls_valid);
        assert!(result.ip_allowed);
    }

    #[test]
    fn test_auth_result_not_authenticated_mtls_fail() {
        let result = AuthResult {
            mtls_valid: false,
            ip_allowed: true,
            cert_info: None,
            client_ip: Some("192.168.1.100".parse().unwrap()),
        };

        assert!(!result.is_authenticated());
    }

    #[test]
    fn test_auth_result_not_authenticated_ip_fail() {
        let result = AuthResult {
            mtls_valid: true,
            ip_allowed: false,
            cert_info: None,
            client_ip: Some("192.168.1.100".parse().unwrap()),
        };

        assert!(!result.is_authenticated());
    }
}
