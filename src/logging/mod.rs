//! Logging Module - Audit logging and tracing
//!
//! Handles audit logging as defined in SPEC.md:
//! - systemd journal integration (primary)
//! - Optional remote syslog
//! - Local file fallback (/var/log/linux_patch_api/audit.log)
//! - 30-day retention with daily rotation

pub mod appender;
pub mod journal;
pub mod init;
