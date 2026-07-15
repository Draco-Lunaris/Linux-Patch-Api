//! Jobs Module - Async job queue management
//!
//! Handles job lifecycle management as defined in ARCHITECTURE.md:
//! - Job queue and status tracking
//! - WebSocket broadcast for real-time status
//! - Persistent upgrade state for self-update lifecycle
//! - 30-minute timeout enforcement
//! - Rollback support (exclusive mode)

pub mod manager;
pub mod queue;
pub mod upgrade_state;
pub mod websocket;
