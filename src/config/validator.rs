//! Configuration Validator
//!
//! Validates configuration values and warns about deprecated fields.

use tracing::warn;

/// Validate configuration for deprecated or unknown fields.
///
/// This is called after config loading to emit warnings for fields
/// that are no longer functional but may still be present in operator
/// config files.
pub fn validate_config_warnings(config_yaml: &str) {
    // Check for deprecated tls.min_tls_version field
    if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(config_yaml) {
        if let Some(tls) = value.get("tls") {
            if tls.get("min_tls_version").is_some() {
                warn!(
                    "Config contains deprecated 'tls.min_tls_version' field. \
                     This field is ignored — TLS 1.3 is the only supported version. \
                     Remove it from your config to silence this warning."
                );
            }
        }
    }
}
