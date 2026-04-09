//! Systemd Module - Systemd service integration
//!
//! Handles systemd integration as defined in ARCHITECTURE.md:
//! - Service notification (Type=notify)
//! - Journal logging integration
//! - PID file management
//! - Graceful shutdown handling

pub mod service;
pub mod journal;
pub mod pid;
