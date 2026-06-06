//! Configuration Loader - YAML config loading
//!
//! Loads and parses YAML configuration files.
//! Provides certificate validation for auto-enrollment workflow.

use anyhow::{Context, Result};
use rustls_pemfile::{certs, private_key};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use time::OffsetDateTime;

/// Server configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub bind: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    30
}

/// TLS/mTLS configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TlsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub port: u16,
    pub ca_cert: String,
    pub server_cert: String,
    pub server_key: String,
    #[serde(default = "default_tls_version")]
    pub min_tls_version: String,
    /// Path to persist the CRL fetched from the manager.
    /// Defaults to /etc/linux_patch_api/certs/crl.pem
    #[serde(default = "default_crl_path")]
    pub crl_path: String,
}

fn default_crl_path() -> String {
    "/etc/linux_patch_api/certs/crl.pem".to_string()
}

fn default_true() -> bool {
    true
}

fn default_tls_version() -> String {
    "1.3".to_string()
}

/// Jobs configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct JobsConfig {
    pub max_concurrent: usize,
    pub timeout_minutes: u64,
    #[serde(default = "default_storage_path")]
    pub storage_path: String,
    #[serde(default = "default_max_queue_depth")]
    pub max_queue_depth: usize,
}

fn default_storage_path() -> String {
    "/var/lib/linux_patch_api/jobs".to_string()
}

fn default_max_queue_depth() -> usize {
    100
}

/// Rate limiting configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RateLimitConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_destructive_per_minute")]
    pub destructive_per_minute: u32,
    #[serde(default = "default_destructive_burst")]
    pub destructive_burst: u32,
    #[serde(default = "default_read_per_minute")]
    pub read_per_minute: u32,
    #[serde(default = "default_read_burst")]
    pub read_burst: u32,
}

fn default_destructive_per_minute() -> u32 {
    20
}
fn default_destructive_burst() -> u32 {
    10
}
fn default_read_per_minute() -> u32 {
    120
}
fn default_read_burst() -> u32 {
    30
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            destructive_per_minute: default_destructive_per_minute(),
            destructive_burst: default_destructive_burst(),
            read_per_minute: default_read_per_minute(),
            read_burst: default_read_burst(),
        }
    }
}

/// Logging configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_true")]
    pub journal_enabled: bool,
    #[serde(default)]
    pub syslog_enabled: bool,
    #[serde(default)]
    pub syslog_server: Option<String>,
    #[serde(default = "default_log_path")]
    pub file_path: String,
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_path() -> String {
    "/var/log/linux_patch_api/audit.log".to_string()
}

fn default_retention_days() -> u64 {
    30
}

/// Whitelist configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WhitelistConfig {
    #[serde(default = "default_whitelist_path")]
    pub path: String,
}

fn default_whitelist_path() -> String {
    "/etc/linux_patch_api/whitelist.yaml".to_string()
}

/// Package manager configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PackageManagerConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
}

fn default_backend() -> String {
    "auto".to_string()
}

/// Enrollment polling configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentConfig {
    /// Manager URL for enrollment. None means not configured.
    /// Changed from String to Option<String> to support "not configured" state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager_url: Option<String>,
    /// Polling token persisted during enrollment for resume after restart.
    #[serde(default)]
    pub polling_token: String,
    #[serde(default = "default_polling_interval")]
    pub polling_interval_seconds: u64,
    #[serde(default = "default_max_poll_attempts")]
    pub max_poll_attempts: u32,
    /// Network interface whose IPv4 address is reported to the manager.
    /// Overrides auto-detection. Example: `"eth0"`, `"ens192"`.
    #[serde(default)]
    pub report_interface: Option<String>,
    /// Explicit IPv4 address reported to the manager.
    /// Highest priority — overrides both `report_interface` and auto-detect.
    #[serde(default)]
    pub report_ip: Option<String>,
    /// Number of days before certificate expiry to trigger re-enrollment warning.
    #[serde(default = "default_cert_renewal_threshold_days")]
    pub cert_renewal_threshold_days: u32,
}

impl Default for EnrollmentConfig {
    fn default() -> Self {
        Self {
            manager_url: None,
            polling_token: String::new(),
            polling_interval_seconds: 60,
            max_poll_attempts: 1440,
            report_interface: None,
            report_ip: None,
            cert_renewal_threshold_days: 7,
        }
    }
}

impl EnrollmentConfig {
    /// Get the effective manager URL, treating empty strings as None.
    pub fn effective_manager_url(&self) -> Option<&str> {
        self.manager_url.as_deref().filter(|s| !s.is_empty())
    }
}

fn default_polling_interval() -> u64 {
    60
}

fn default_max_poll_attempts() -> u32 {
    1440
}

fn default_cert_renewal_threshold_days() -> u32 {
    7
}

/// Certificate validation status returned by validate_certs().
#[derive(Debug, Clone)]
pub enum CertStatus {
    /// All certificates are valid and not expiring soon.
    Valid,
    /// Certificates are valid but expiring within the threshold.
    ExpiringSoon { not_after: OffsetDateTime },
    /// One or more certificate files are missing.
    Missing { paths: Vec<PathBuf> },
    /// A certificate file exists but cannot be parsed as valid PEM.
    Corrupt { path: PathBuf, error: String },
    /// A certificate has expired (not_after is in the past).
    Expired {
        path: PathBuf,
        not_after: OffsetDateTime,
    },
    /// Server certificate public key does not match server private key.
    KeyMismatch,
    /// Server certificate is not signed by the configured CA.
    Untrusted,
}

impl std::fmt::Display for CertStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CertStatus::Valid => write!(f, "Valid"),
            CertStatus::ExpiringSoon { not_after } => {
                write!(f, "ExpiringSoon (not_after={})", not_after)
            }
            CertStatus::Missing { paths } => {
                let path_strs: Vec<String> =
                    paths.iter().map(|p| p.display().to_string()).collect();
                write!(f, "Missing: [{}]", path_strs.join(", "))
            }
            CertStatus::Corrupt { path, error } => {
                write!(f, "Corrupt: {} ({})", path.display(), error)
            }
            CertStatus::Expired { path, not_after } => {
                write!(f, "Expired: {} (not_after={})", path.display(), not_after)
            }
            CertStatus::KeyMismatch => write!(f, "KeyMismatch"),
            CertStatus::Untrusted => write!(f, "Untrusted"),
        }
    }
}

/// Validate TLS certificates for the auto-enrollment workflow.
///
/// Checks (in order):
/// 1. Existence: All three cert files must exist at configured paths
/// 2. PEM parse validity: CA and server cert must parse as X.509, server key must parse
/// 3. Expiry: CA and server cert must not be expired
/// 4. Key match: Server cert public key must match server key private key
/// 5. CA trust: Server cert must be signed by the CA
///
/// Returns the most severe status found.
pub fn validate_certs(config: &AppConfig) -> Result<CertStatus> {
    let tls = match config.tls_config() {
        Some(tls) => tls,
        None => return Ok(CertStatus::Valid), // TLS disabled, nothing to validate
    };

    let threshold_days = config
        .enrollment
        .as_ref()
        .map(|e| e.cert_renewal_threshold_days)
        .unwrap_or(7);

    // 1. Check existence of all three cert files
    let ca_path = PathBuf::from(&tls.ca_cert);
    let cert_path = PathBuf::from(&tls.server_cert);
    let key_path = PathBuf::from(&tls.server_key);

    let mut missing_paths = Vec::new();
    if !ca_path.exists() {
        missing_paths.push(ca_path.clone());
    }
    if !cert_path.exists() {
        missing_paths.push(cert_path.clone());
    }
    if !key_path.exists() {
        missing_paths.push(key_path.clone());
    }
    if !missing_paths.is_empty() {
        return Ok(CertStatus::Missing {
            paths: missing_paths,
        });
    }

    // 2. Parse and validate PEM files using rustls_pemfile
    // Parse CA certificate(s)
    let ca_file = File::open(&ca_path)
        .with_context(|| format!("Failed to open CA certificate: {}", ca_path.display()))?;
    let ca_certs: Vec<_> = certs(&mut BufReader::new(ca_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("Failed to parse CA certificate PEM: {}", e))?;
    if ca_certs.is_empty() {
        return Ok(CertStatus::Corrupt {
            path: ca_path,
            error: "No certificates found in CA PEM file".to_string(),
        });
    }

    // Parse server certificate
    let server_file = File::open(&cert_path)
        .with_context(|| format!("Failed to open server certificate: {}", cert_path.display()))?;
    let server_certs: Vec<_> = certs(&mut BufReader::new(server_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("Failed to parse server certificate PEM: {}", e))?;
    if server_certs.is_empty() {
        return Ok(CertStatus::Corrupt {
            path: cert_path.clone(),
            error: "No certificates found in server PEM file".to_string(),
        });
    }

    // Parse server private key
    let key_file = File::open(&key_path)
        .with_context(|| format!("Failed to open server key: {}", key_path.display()))?;
    let server_key = private_key(&mut BufReader::new(key_file))
        .map_err(|e| anyhow::anyhow!("Failed to parse server key PEM: {}", e))?;
    let server_key = match server_key {
        Some(key) => key,
        None => {
            return Ok(CertStatus::Corrupt {
                path: key_path,
                error: "No private key found in server key PEM file".to_string(),
            })
        }
    };

    // 3. Check expiry using x509_parser
    let now = OffsetDateTime::now_utc();
    let threshold = time::Duration::days(i64::from(threshold_days));

    // Check CA cert expiry
    let ca_der = ca_certs.first().expect("ca_certs verified non-empty above");
    match x509_parser::parse_x509_certificate(ca_der.as_ref()) {
        Ok((_, ca_cert)) => {
            let ca_not_after = ca_cert.validity().not_after.to_datetime();
            if ca_not_after < now {
                return Ok(CertStatus::Expired {
                    path: ca_path,
                    not_after: ca_not_after,
                });
            }
        }
        Err(e) => {
            return Ok(CertStatus::Corrupt {
                path: ca_path,
                error: format!("Failed to parse CA certificate DER: {}", e),
            })
        }
    }

    // Check server cert expiry
    let server_der = server_certs
        .first()
        .expect("server_certs verified non-empty above");
    let server_not_after: OffsetDateTime =
        match x509_parser::parse_x509_certificate(server_der.as_ref()) {
            Ok((_, cert)) => {
                let not_after = cert.validity().not_after.to_datetime();
                if not_after < now {
                    return Ok(CertStatus::Expired {
                        path: cert_path.clone(),
                        not_after,
                    });
                }
                not_after
            }
            Err(e) => {
                return Ok(CertStatus::Corrupt {
                    path: cert_path,
                    error: format!("Failed to parse server certificate DER: {}", e),
                })
            }
        };

    // Check if expiring soon
    let expires_soon = server_not_after < now + threshold;

    // 4. Check key match: verify that the server cert's public key corresponds
    //    to the server private key by attempting to build a rustls ServerConfig.
    //    If the key doesn't match the cert, rustls will reject it.
    let key_matches = verify_key_match(&ca_certs, &server_certs, &server_key);
    if !key_matches {
        return Ok(CertStatus::KeyMismatch);
    }

    // 5. Check CA trust: server cert must be signed by the CA
    //    Verify by checking if the server cert's issuer matches the CA cert's subject
    let trusted = verify_ca_trust(server_der.as_ref(), ca_der.as_ref());
    if !trusted {
        return Ok(CertStatus::Untrusted);
    }

    // All checks passed
    if expires_soon {
        Ok(CertStatus::ExpiringSoon {
            not_after: server_not_after,
        })
    } else {
        Ok(CertStatus::Valid)
    }
}

/// Verify that the server cert's public key matches the server private key.
/// Attempts to build a rustls ServerConfig with the given certs and key.
/// If the key doesn't match the cert, the configuration will fail.
fn verify_key_match(
    _ca_certs: &[rustls::pki_types::CertificateDer<'static>],
    server_certs: &[rustls::pki_types::CertificateDer<'static>],
    server_key: &rustls::pki_types::PrivateKeyDer<'static>,
) -> bool {
    use rustls::crypto::aws_lc_rs;
    use rustls::version::TLS13;
    use rustls::ServerConfig;
    use std::sync::Arc;

    // Build a simple ServerConfig with no client auth to test key/cert compatibility.
    // If the key doesn't match the cert, with_single_cert will return an error.
    let provider = aws_lc_rs::default_provider();

    let config_result = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&TLS13])
        .map(|b| b.with_no_client_auth())
        .map(|b| b.with_single_cert(server_certs.to_vec(), server_key.clone_key()));

    match config_result {
        Ok(Ok(_)) => true,
        Ok(Err(_)) | Err(_) => {
            tracing::debug!("Key/cert mismatch detected during ServerConfig build");
            false
        }
    }
}

/// Verify that the server certificate is signed by the CA certificate.
/// Checks if the server cert's issuer matches the CA cert's subject.
fn verify_ca_trust(server_der: &[u8], ca_der: &[u8]) -> bool {
    let (_, server_cert) = match x509_parser::parse_x509_certificate(server_der) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let (_, ca_cert) = match x509_parser::parse_x509_certificate(ca_der) {
        Ok(r) => r,
        Err(_) => return false,
    };

    // Check if the server cert's issuer matches the CA cert's subject
    server_cert.issuer() == ca_cert.subject()
}

/// Application configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppConfig {
    pub server: ServerConfig,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    pub jobs: JobsConfig,
    pub logging: LoggingConfig,
    #[serde(default)]
    pub whitelist: Option<WhitelistConfig>,
    #[serde(default)]
    pub package_manager: Option<PackageManagerConfig>,
    #[serde(default)]
    pub enrollment: Option<EnrollmentConfig>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

impl AppConfig {
    /// Load configuration from a YAML file
    pub fn load(path: &str, skip_tls_validation: bool) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;

        let config: AppConfig = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path))?;

        // Migrate: if enrollment.manager_url is an empty string, treat as None
        let config = config.migrate_empty_strings();

        // Validate TLS configuration if enabled (skip during enrollment bootstrap)
        if !skip_tls_validation {
            if let Some(ref tls) = config.tls {
                if tls.enabled {
                    // Cert validation is now handled by validate_certs() in main.rs
                    // This no longer bails on missing cert files
                }
            }
        }

        Ok(config)
    }

    /// Migrate empty strings to None for Option fields.
    /// Handles backward compatibility with old config format where
    /// manager_url was a String (empty string means not configured).
    fn migrate_empty_strings(mut self) -> Self {
        if let Some(ref mut enrollment) = self.enrollment {
            if let Some(ref url) = enrollment.manager_url {
                if url.is_empty() {
                    enrollment.manager_url = None;
                }
            }
        }
        self
    }

    /// Get TLS configuration or default
    pub fn tls_config(&self) -> Option<&TlsConfig> {
        self.tls.as_ref().filter(|t| t.enabled)
    }

    /// Get whitelist configuration path
    pub fn whitelist_path(&self) -> &str {
        self.whitelist
            .as_ref()
            .map(|w| w.path.as_str())
            .unwrap_or("/etc/linux_patch_api/whitelist.yaml")
    }

    /// Get enrollment manager URL, if configured.
    pub fn enrollment_manager_url(&self) -> Option<&str> {
        self.enrollment
            .as_ref()
            .and_then(|e| e.effective_manager_url())
    }

    /// Persist the polling token to the config file for resume after restart.
    /// Updates the in-memory config and writes to disk.
    pub fn save_polling_token(&mut self, token: &str, config_path: &str) -> Result<()> {
        if let Some(ref mut enrollment) = self.enrollment {
            enrollment.polling_token = token.to_string();
        } else {
            self.enrollment = Some(EnrollmentConfig {
                manager_url: None,
                polling_token: token.to_string(),
                polling_interval_seconds: 60,
                max_poll_attempts: 1440,
                report_interface: None,
                report_ip: None,
                cert_renewal_threshold_days: 7,
            });
        }

        // Write updated config to file
        let yaml = serde_yaml::to_string(&self)
            .context("Failed to serialize config for polling token persistence")?;
        std::fs::write(config_path, yaml)
            .with_context(|| format!("Failed to write config file: {}", config_path))?;

        Ok(())
    }

    /// Clear the polling token from the config file after successful enrollment.
    pub fn clear_polling_token(&mut self, config_path: &str) -> Result<()> {
        if let Some(ref mut enrollment) = self.enrollment {
            enrollment.polling_token = String::new();
        }

        // Write updated config to file
        let yaml = serde_yaml::to_string(&self)
            .context("Failed to serialize config for polling token clear")?;
        std::fs::write(config_path, yaml)
            .with_context(|| format!("Failed to write config file: {}", config_path))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_load_valid_yaml() {
        let result = AppConfig::load("tests/fixtures/valid_config.yaml", false);
        assert!(
            result.is_ok(),
            "Failed to load valid config: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cert_status_display() {
        assert_eq!(format!("{}", CertStatus::Valid), "Valid");
        assert_eq!(format!("{}", CertStatus::KeyMismatch), "KeyMismatch");
        assert_eq!(format!("{}", CertStatus::Untrusted), "Untrusted");
    }

    #[test]
    fn test_cert_status_missing_display() {
        let status = CertStatus::Missing {
            paths: vec![PathBuf::from("/etc/ssl/ca.pem")],
        };
        let display = format!("{}", status);
        assert!(display.contains("Missing"));
        assert!(display.contains("/etc/ssl/ca.pem"));
    }

    #[test]
    fn test_enrollment_config_defaults() {
        let config = EnrollmentConfig::default();
        assert!(config.manager_url.is_none());
        assert!(config.polling_token.is_empty());
        assert_eq!(config.polling_interval_seconds, 60);
        assert_eq!(config.max_poll_attempts, 1440);
        assert_eq!(config.cert_renewal_threshold_days, 7);
    }

    #[test]
    fn test_enrollment_config_with_url() {
        let yaml = r#"
manager_url: "https://manager.example.com"
polling_interval_seconds: 30
max_poll_attempts: 720
cert_renewal_threshold_days: 14
"#;
        let config: EnrollmentConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.manager_url,
            Some("https://manager.example.com".to_string())
        );
        assert_eq!(config.polling_interval_seconds, 30);
        assert_eq!(config.max_poll_attempts, 720);
        assert_eq!(config.cert_renewal_threshold_days, 14);
    }

    #[test]
    fn test_effective_manager_url() {
        let mut config = EnrollmentConfig::default();
        assert!(config.effective_manager_url().is_none());

        config.manager_url = Some("https://manager.example.com".to_string());
        assert_eq!(
            config.effective_manager_url(),
            Some("https://manager.example.com")
        );

        config.manager_url = Some("".to_string());
        assert!(config.effective_manager_url().is_none());
    }

    #[test]
    fn test_migrate_empty_strings() {
        let yaml = r#"
server:
  port: 12443
  bind: "0.0.0.0"
jobs:
  max_concurrent: 5
  timeout_minutes: 30
logging:
  level: "info"
enrollment:
  manager_url: ""
"#;
        let config: AppConfig = serde_yaml::from_str(yaml).unwrap();
        let migrated = config.migrate_empty_strings();
        assert!(migrated.enrollment.unwrap().manager_url.is_none());
    }
}
