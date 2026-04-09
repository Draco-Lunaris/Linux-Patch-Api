//! Configuration Loader - YAML config loading
//!
//! Loads and parses YAML configuration files.

use anyhow::{Context, Result};
use serde::Deserialize;

/// Server configuration
#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub bind: String,
}

/// Jobs configuration
#[derive(Debug, Deserialize, Clone)]
pub struct JobsConfig {
    pub max_concurrent: usize,
    pub timeout_minutes: u64,
}

/// Logging configuration
#[derive(Debug, Deserialize, Clone)]
pub struct LoggingConfig {
    pub level: String,
}

/// Application configuration
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub jobs: JobsConfig,
    pub logging: LoggingConfig,
}

impl AppConfig {
    /// Load configuration from a YAML file
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;
        
        let config: AppConfig = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path))?;
        
        Ok(config)
    }
}
