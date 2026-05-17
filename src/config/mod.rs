//! Config Module - YAML config with auto-reload
//!
//! Handles configuration management as defined in SPEC.md:
//! - YAML config file loading and parsing
//! - Config validation before reload (prevent service offline)
//! - Auto-reload on file change via notify watcher

pub mod loader;
pub use loader::EnrollmentConfig;
pub mod validator;
pub mod watcher;
