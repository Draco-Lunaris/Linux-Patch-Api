//! Jobs Module - Async job queue and management
//!
//! Handles job lifecycle management as defined in ARCHITECTURE.md:
//! - Job queue and status tracking
//! - WebSocket broadcast for real-time status
//! - 30-minute timeout enforcement
//! - Rollback support (exclusive mode)

pub mod manager;
pub mod queue;
pub mod websocket;
