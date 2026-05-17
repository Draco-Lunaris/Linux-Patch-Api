//! Configuration Loader - YAML config loading
//!
//! Loads and parses YAML configuration files.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Server configuration
#[derive(Debug, Deserialize, Clone)]
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
#[derive(Debug, Deserialize, Clone)]
pub struct TlsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub port: u16,
    pub ca_cert: String,
    pub server_cert: String,
    pub server_key: String,
    #[serde(default = "default_tls_version")]
    pub min_tls_version: String,
}

fn default_true() -> bool {
    true
}

fn default_tls_version() -> String {
    "1.3".to_string()
}

/// Jobs configuration
#[derive(Debug, Deserialize, Clone)]
pub struct JobsConfig {
    pub max_concurrent: usize,
    pub timeout_minutes: u64,
    #[serde(default = "default_storage_path")]
    pub storage_path: String,
}

fn default_storage_path() -> String {
    "/var/lib/linux_patch_api/jobs".to_string()
}

/// Logging configuration
#[derive(Debug, Deserialize, Clone)]
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
#[derive(Debug, Deserialize, Clone)]
pub struct WhitelistConfig {
    #[serde(default = "default_whitelist_path")]
    pub path: String,
}

fn default_whitelist_path() -> String {
    "/etc/linux_patch_api/whitelist.yaml".to_string()
}

/// Package manager configuration
#[derive(Debug, Deserialize, Clone)]
pub struct PackageManagerConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
}

fn default_backend() -> String {
    "auto".to_string()
}

/// Enrollment polling configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnrollmentConfig {
    #[serde(default)]
    pub manager_url: String,
    #[serde(default)]
    pub polling_token: String,
    #[serde(default = "default_polling_interval")]
    pub polling_interval_seconds: u64,
    #[serde(default = "default_max_poll_attempts")]
    pub max_poll_attempts: u32,
}

fn default_polling_interval() -> u64 {
    60
}

fn default_max_poll_attempts() -> u32 {
    1440
}

/// Application configuration
#[derive(Debug, Deserialize, Clone)]
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
}

impl AppConfig {
    /// Load configuration from a YAML file
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;

        let config: AppConfig = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path))?;

        // Validate TLS configuration if enabled
        if let Some(ref tls) = config.tls {
            if tls.enabled {
                if !std::path::Path::new(&tls.ca_cert).exists() {
                    anyhow::bail!("TLS CA certificate not found: {}", tls.ca_cert);
                }
                if !std::path::Path::new(&tls.server_cert).exists() {
                    anyhow::bail!("TLS server certificate not found: {}", tls.server_cert);
                }
                if !std::path::Path::new(&tls.server_key).exists() {
                    anyhow::bail!("TLS server key not found: {}", tls.server_key);
                }
            }
        }

        Ok(config)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_load_valid_yaml() {
        let result = AppConfig::load("tests/fixtures/valid_config.yaml");
        assert!(
            result.is_ok(),
            "Failed to load valid config: {:?}",
            result.err()
        );

        let config = result.unwrap();
        assert_eq!(config.server.port, 12443);
        assert_eq!(config.server.bind, "127.0.0.1");
        assert_eq!(config.jobs.max_concurrent, 5);
        assert_eq!(config.jobs.timeout_minutes, 30);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn test_config_load_missing_file() {
        let result = AppConfig::load("/nonexistent/path/config.yaml");
        assert!(result.is_err(), "Should fail for missing file");
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Failed to read config file"));
    }

    #[test]
    fn test_config_load_invalid_yaml() {
        let invalid_path = "/tmp/invalid_config_test.yaml";
        std::fs::write(invalid_path, "invalid: yaml: content: [").unwrap();

        let result = AppConfig::load(invalid_path);
        assert!(result.is_err(), "Should fail for invalid yaml");

        std::fs::remove_file(invalid_path).unwrap();
    }

    #[test]
    fn test_config_validation_port_range() {
        let result = AppConfig::load("tests/fixtures/valid_config.yaml");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.server.port >= 1);
    }

    #[test]
    fn test_config_validation_bind_address() {
        let result = AppConfig::load("tests/fixtures/valid_config.yaml");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(!config.server.bind.is_empty());
    }

    #[test]
    fn test_config_validation_max_concurrent() {
        let result = AppConfig::load("tests/fixtures/valid_config.yaml");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.jobs.max_concurrent > 0);
    }

    #[test]
    fn test_config_validation_timeout() {
        let result = AppConfig::load("tests/fixtures/valid_config.yaml");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.jobs.timeout_minutes >= 1 && config.jobs.timeout_minutes <= 1440);
    }

    #[test]
    fn test_tls_config_defaults() {
        let config = AppConfig {
            server: ServerConfig {
                port: 12443,
                bind: "0.0.0.0".to_string(),
                timeout_seconds: 30,
            },
            tls: Some(TlsConfig {
                enabled: true,
                port: 12443,
                ca_cert: "/etc/linux_patch_api/certs/ca.pem".to_string(),
                server_cert: "/etc/linux_patch_api/certs/server.pem".to_string(),
                server_key: "/etc/linux_patch_api/certs/server.key".to_string(),
                min_tls_version: "1.3".to_string(),
            }),
            jobs: JobsConfig {
                max_concurrent: 5,
                timeout_minutes: 30,
                storage_path: "/var/lib/linux_patch_api/jobs".to_string(),
            },
            logging: LoggingConfig {
                level: "info".to_string(),
                journal_enabled: true,
                syslog_enabled: false,
                syslog_server: None,
                file_path: "/var/log/linux_patch_api/audit.log".to_string(),
                retention_days: 30,
            },
            whitelist: Some(WhitelistConfig {
                path: "/etc/linux_patch_api/whitelist.yaml".to_string(),
            }),
            package_manager: None,
            enrollment: None,
        };

        assert!(config.tls_config().is_some());
        assert_eq!(config.tls_config().unwrap().min_tls_version, "1.3");
        assert_eq!(
            config.whitelist_path(),
            "/etc/linux_patch_api/whitelist.yaml"
        );
    }
}
