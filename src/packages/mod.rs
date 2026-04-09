//! Packages Module - Pluggable package manager backend
//!
//! Handles package operations as defined in SPEC.md:
//! - apt/dpkg (Debian/Ubuntu) - primary
//! - dnf/yum (RHEL/CentOS/Fedora) - secondary
//! - apk (Alpine) - secondary
//! - pacman (Arch) - secondary

pub mod backend;
pub mod manager;
pub mod models;
