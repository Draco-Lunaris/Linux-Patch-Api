//! Jobs Module - Async job queue management
//!
//! Handles job lifecycle management:
//! - Job queue and status tracking
//! - WebSocket broadcast for real-time status
//! - 30-minute timeout enforcement
//! - Rollback support (exclusive mode)

pub mod manager;
pub mod queue;
pub mod scheduler;
pub mod websocket;
