//! Unit Tests - Configuration Module
//!
//! Tests for configuration loading and validation.

use linux_patch_api::config::loader::AppConfig;

#[test]
fn test_config_load_valid_yaml() {
    let result = AppConfig::load("tests/fixtures/valid_config.yaml");
    assert!(result.is_ok(), "Failed to load valid config: {:?}", result.err());
    
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
    // Create a temporary invalid yaml file
    let invalid_path = "/tmp/invalid_config.yaml";
    std::fs::write(invalid_path, "invalid: yaml: content: [").unwrap();
    
    let result = AppConfig::load(invalid_path);
    assert!(result.is_err(), "Should fail for invalid yaml");
    
    // Cleanup
    std::fs::remove_file(invalid_path).unwrap();
}

#[test]
fn test_config_validation_port_range() {
    // Test that port is within valid range (1-65535)
    let result = AppConfig::load("tests/fixtures/valid_config.yaml");
    assert!(result.is_ok());
    let config = result.unwrap();
    assert!(config.server.port >= 1 && config.server.port <= 65535);
}

#[test]
fn test_config_validation_bind_address() {
    // Test that bind address is a valid string
    let result = AppConfig::load("tests/fixtures/valid_config.yaml");
    assert!(result.is_ok());
    let config = result.unwrap();
    assert!(!config.server.bind.is_empty());
}

#[test]
fn test_config_validation_max_concurrent() {
    // Test that max_concurrent is positive
    let result = AppConfig::load("tests/fixtures/valid_config.yaml");
    assert!(result.is_ok());
    let config = result.unwrap();
    assert!(config.jobs.max_concurrent > 0);
}

#[test]
fn test_config_validation_timeout() {
    // Test that timeout is reasonable (1-1440 minutes)
    let result = AppConfig::load("tests/fixtures/valid_config.yaml");
    assert!(result.is_ok());
    let config = result.unwrap();
    assert!(config.jobs.timeout_minutes >= 1 && config.jobs.timeout_minutes <= 1440);
}

#[test]
fn test_config_load_dev_config() {
    // Test loading development config if it exists
    let dev_path = "configs/config.yaml.example";
    if std::path::Path::new(dev_path).exists() {
        let result = AppConfig::load(dev_path);
        assert!(result.is_ok(), "Failed to load example config: {:?}", result.err());
    }
}
