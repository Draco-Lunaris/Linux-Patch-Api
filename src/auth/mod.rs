//! Authentication Module - mTLS and IP Whitelist
//!
//! Handles mTLS certificate validation and IP whitelist enforcement:
//! - Certificate validation against internal CA
//! - IP whitelist checking (IPv4 + CIDR + hostname)
//! - Client identity extraction from certificates

pub mod certificate;
pub mod ip_whitelist;
pub mod middleware;
