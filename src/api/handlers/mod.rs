//! API Handlers Module
//!
//! Contains all REST API endpoint handlers organized by domain:
//! - packages: Package management endpoints
//! - patches: Patch management endpoints
//! - system: System management endpoints
//! - jobs: Job management endpoints
//! - websocket: Real-time job status streaming
//! - self_upgrade: Detached self-upgrade utilities

pub mod file_install;
pub mod install_url;
pub mod jobs;
pub mod packages;
pub mod patches;
pub mod self_upgrade;
pub mod system;
pub mod websocket;

// Re-export commonly used types
pub use packages::{ApiError, ApiResponse};
// WebSocket message types are now in crate::jobs::websocket
pub use crate::jobs::websocket::{WsClientMessage, WsServerMessage};
