//! Auth Module - mTLS and IP Whitelist Enforcement
//!
//! This module provides security authentication and authorization:
//! - mTLS (Mutual TLS) certificate-based authentication
//! - IP whitelist enforcement with CIDR subnet support
//! - Silent drop for non-compliant connections
//! - Comprehensive audit logging

pub mod mtls;
pub mod whitelist;

pub use mtls::{MtlsConfig, MtlsMiddleware, MtlsError, ClientCertInfo};
pub use whitelist::{WhitelistManager, WhitelistMiddleware, WhitelistEntry, WhitelistConfig};

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
