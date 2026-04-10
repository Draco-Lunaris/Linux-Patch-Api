//! Integration Tests for Authentication Layer
//!
//! Tests mTLS authentication and IP whitelist enforcement.

use linux_patch_api::auth::{mtls, whitelist, AuthResult};
use std::net::Ipv4Addr;

#[cfg(test)]
mod mtls_tests {
    use super::*;

    #[test]
    fn test_mtls_config_creation() {
        let config = mtls::MtlsConfig {
            ca_cert_path: "/etc/linux_patch_api/certs/ca.pem".to_string(),
            server_cert_path: "/etc/linux_patch_api/certs/server.pem".to_string(),
            server_key_path: "/etc/linux_patch_api/certs/server.key".to_string(),
            min_tls_version: "1.3".to_string(),
        };

        assert_eq!(config.ca_cert_path, "/etc/linux_patch_api/certs/ca.pem");
        assert_eq!(config.server_cert_path, "/etc/linux_patch_api/certs/server.pem");
        assert_eq!(config.server_key_path, "/etc/linux_patch_api/certs/server.key");
        assert_eq!(config.min_tls_version, "1.3");
    }

    #[test]
    fn test_mtls_error_types() {
        // Test that error types can be created
        let io_error = mtls::MtlsError::IoError("test".to_string());
        assert!(io_error.to_string().contains("test"));

        let parse_error = mtls::MtlsError::ParseError("test".to_string());
        assert!(parse_error.to_string().contains("test"));

        let validation_error = mtls::MtlsError::ValidationError("test".to_string());
        assert!(validation_error.to_string().contains("test"));
    }

    #[test]
    fn test_client_cert_info() {
        let info = mtls::ClientCertInfo {
            subject: "CN=client001,O=Internal,C=US".to_string(),
            issuer: "CN=Linux Patch API CA,O=Internal,C=US".to_string(),
            serial: "01".to_string(),
            not_before: chrono::Utc::now(),
            not_after: chrono::Utc::now() + chrono::Duration::days(365),
        };

        assert!(info.subject.contains("CN=client001"));
        assert!(info.issuer.contains("Linux Patch API CA"));
        assert_eq!(info.serial, "01");
    }
}

#[cfg(test)]
mod whitelist_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_whitelist(content: &str) -> (TempDir, String) {
        let temp_dir = TempDir::new().unwrap();
        let whitelist_path = temp_dir.path().join("whitelist.yaml");
        fs::write(&whitelist_path, content).unwrap();
        (temp_dir, whitelist_path.to_string_lossy().to_string())
    }

    #[test]
    fn test_whitelist_single_ip() {
        let (_temp_dir, whitelist_path) = create_test_whitelist(
            r#"entries:
  - "192.168.1.100"
"#,
        );

        let manager = whitelist::WhitelistManager::new(&whitelist_path).unwrap();

        let allowed_ip: Ipv4Addr = "192.168.1.100".parse().unwrap();
        assert!(manager.is_allowed(&allowed_ip));

        let denied_ip: Ipv4Addr = "192.168.1.101".parse().unwrap();
        assert!(!manager.is_allowed(&denied_ip));
    }

    #[test]
    fn test_whitelist_cidr_subnet() {
        let (_temp_dir, whitelist_path) = create_test_whitelist(
            r#"entries:
  - "192.168.1.0/24"
"#,
        );

        let manager = whitelist::WhitelistManager::new(&whitelist_path).unwrap();

        // IPs within subnet should be allowed
        assert!(manager.is_allowed(&"192.168.1.1".parse().unwrap()));
        assert!(manager.is_allowed(&"192.168.1.100".parse().unwrap()));
        assert!(manager.is_allowed(&"192.168.1.254".parse().unwrap()));

        // IPs outside subnet should be denied
        assert!(!manager.is_allowed(&"192.168.2.1".parse().unwrap()));
        assert!(!manager.is_allowed(&"192.167.1.1".parse().unwrap()));
    }

    #[test]
    fn test_whelist_multiple_entries() {
        let (_temp_dir, whitelist_path) = create_test_whitelist(
            r#"entries:
  - "192.168.1.0/24"
  - "10.0.0.50"
  - "172.16.0.0/16"
"#,
        );

        let manager = whitelist::WhitelistManager::new(&whitelist_path).unwrap();

        // All these should be allowed
        assert!(manager.is_allowed(&"192.168.1.100".parse().unwrap()));
        assert!(manager.is_allowed(&"10.0.0.50".parse().unwrap()));
        assert!(manager.is_allowed(&"172.16.50.100".parse().unwrap()));

        // These should be denied
        assert!(!manager.is_allowed(&"192.168.2.100".parse().unwrap()));
        assert!(!manager.is_allowed(&"10.0.0.51".parse().unwrap()));
        assert!(!manager.is_allowed(&"172.17.0.1".parse().unwrap()));
    }

    #[test]
    fn test_whitelist_entry_count() {
        let (_temp_dir, whitelist_path) = create_test_whitelist(
            r#"entries:
  - "192.168.1.0/24"
  - "10.0.0.50"
"#,
        );

        let manager = whitelist::WhitelistManager::new(&whitelist_path).unwrap();
        assert_eq!(manager.entry_count(), 2);
    }

    #[test]
    fn test_whitelist_socket_addr() {
        use std::net::SocketAddr;

        let (_temp_dir, whitelist_path) = create_test_whitelist(
            r#"entries:
  - "192.168.1.0/24"
"#,
        );

        let manager = whitelist::WhitelistManager::new(&whitelist_path).unwrap();

        let allowed_socket: SocketAddr = "192.168.1.100:8080".parse().unwrap();
        assert!(manager.is_socket_allowed(&allowed_socket));

        let denied_socket: SocketAddr = "192.168.2.100:8080".parse().unwrap();
        assert!(!manager.is_socket_allowed(&denied_socket));
    }
}

#[cfg(test)]
mod auth_result_tests {
    use super::*;

    #[test]
    fn test_auth_result_fully_authenticated() {
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
    fn test_auth_result_mtls_failed() {
        let result = AuthResult {
            mtls_valid: false,
            ip_allowed: true,
            cert_info: None,
            client_ip: Some("192.168.1.100".parse().unwrap()),
        };

        assert!(!result.is_authenticated());
    }

    #[test]
    fn test_auth_result_ip_denied() {
        let result = AuthResult {
            mtls_valid: true,
            ip_allowed: false,
            cert_info: None,
            client_ip: Some("192.168.1.100".parse().unwrap()),
        };

        assert!(!result.is_authenticated());
    }

    #[test]
    fn test_auth_result_both_failed() {
        let result = AuthResult {
            mtls_valid: false,
            ip_allowed: false,
            cert_info: None,
            client_ip: Some("192.168.1.100".parse().unwrap()),
        };

        assert!(!result.is_authenticated());
    }

    #[test]
    fn test_auth_result_with_cert_info() {
        let cert_info = mtls::ClientCertInfo {
            subject: "CN=client001".to_string(),
            issuer: "CN=Linux Patch API CA".to_string(),
            serial: "01".to_string(),
            not_before: chrono::Utc::now(),
            not_after: chrono::Utc::now() + chrono::Duration::days(365),
        };

        let result = AuthResult {
            mtls_valid: true,
            ip_allowed: true,
            cert_info: Some(cert_info),
            client_ip: Some("192.168.1.100".parse().unwrap()),
        };

        assert!(result.is_authenticated());
        assert!(result.cert_info.is_some());
        assert_eq!(
            result.cert_info.unwrap().subject,
            "CN=client001"
        );
    }
}
