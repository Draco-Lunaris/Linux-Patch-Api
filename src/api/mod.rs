//! API Module - HTTP endpoints and routing
//!
//! This module provides the REST API layer for the Linux Patch API:
//! - Package management endpoints (GET/POST/PUT/DELETE /packages)
//! - Patch management endpoints (GET/POST /patches)
//! - System management endpoints (GET /system/info, GET /health, POST /system/reboot)
//! - Job management endpoints (GET/POST/DELETE /jobs)
//! - WebSocket endpoint for real-time job status streaming

pub mod handlers;
pub mod routes;

// Re-export handlers for convenience
pub use handlers::packages;
pub use handlers::patches;
pub use handlers::system;
pub use handlers::jobs;
pub use handlers::websocket;

// Re-export routes configuration
pub use routes::{configure_api_routes, configure_health_route};

/// API version
pub const API_VERSION: &str = "v1";

/// API base path
pub const API_BASE_PATH: &str = "/api/v1";
