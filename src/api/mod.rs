//! API Module - HTTP endpoints and routing
//!
//! Handles all REST API endpoints as defined in API_SPEC.md:
//! - Package management endpoints
//! - Patch management endpoints
//! - System endpoints
//! - Job management endpoints
//! - WebSocket streaming

pub mod handlers;
pub mod middleware;
pub mod response;
pub mod routes;
