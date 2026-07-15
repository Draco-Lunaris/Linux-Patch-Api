//! Linux Patch API - Secure Remote Package Management
//!
//! A Rust-based API service for secure remote management of patching processes
//! and software add/remove operations on Linux systems.
//!
//! # Architecture
//!
//! - **API Layer**: HTTP/HTTPS endpoints with mTLS authentication
//! - **Auth Layer**: Certificate validation and IP whitelist enforcement
//! - **Job Manager**: Async job queue with WebSocket status streaming
//! - **Package Backend**: Pluggable package manager adapters
//! - **Audit Logger**: systemd journal + file fallback
//! - **Config Manager**: YAML config with auto-reload

pub mod api;
pub mod auth;
pub mod config;
pub mod enroll;
pub mod jobs;
pub mod logging;
pub mod packages;
pub mod systemd;

// Re-export commonly used types from submodules
pub use config::loader::AppConfig;
pub use jobs::manager::JobManager;
pub use jobs::scheduler::Scheduler;
pub use logging::init::init_logging;
