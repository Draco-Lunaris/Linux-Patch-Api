//! API Handlers Module
//!
//! Contains all REST API endpoint handlers organized by domain:
//! - packages: Package management endpoints
//! - patches: Patch management endpoints
//! - system: System management endpoints
//! - jobs: Job management endpoints
//! - websocket: Real-time job status streaming

pub mod packages;
pub mod patches;
pub mod system;
pub mod jobs;
pub mod websocket;

// Re-export commonly used types
pub use packages::{ApiResponse, ApiError};
pub use websocket::{WsClientMessage, WsServerMessage};
