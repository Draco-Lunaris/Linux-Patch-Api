//! Cross-distribution identity extraction for Linux systems.
//!
//! Provides machine-id, FQDN, IP address, and OS-detail collection
//! compatible with Debian/Ubuntu, RHEL/CentOS/Fedora, Alpine, and Arch Linux.

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::net::IpAddr;
use std::process::Command;

/// Read the D-Bus machine identifier from `/etc/machine-id`.
/// Falls back to `/var/lib/dbus/machine-id` on older systems.
pub fn get_machine_id() -> Result<String> {
    let primary = "/etc/machine-id";
    let fallback = "/var/lib/dbus/machine-id";

    if let Ok(id) = fs::read_to_string(primary) {
        let trimmed = id.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }

    let id = fs::read_to_string(fallback)
        .with_context(|| format!("Failed to read machine-id from {} or {}", primary, fallback))?;
    let trimmed = id.trim().to_string();
    if trimmed.is_empty() {
        return Err(anyhow!("machine-id file is empty"));
    }
    Ok(trimmed)
}

/// Resolve the fully-qualified domain name.
/// Strategy: `gethostname` via std → fallback to `hostname` CLI → "localhost".
pub fn get_fqdn() -> Result<String> {
    // Try reading from hostname file first (common on systemd systems)
    if let Ok(name) = fs::read_to_string("/etc/hostname") {
        let trimmed = name.trim().to_string();
        if !trimmed.is_empty() && trimmed != "(none)" {
            return Ok(trimmed);
        }
    }

    // Fallback to hostname command
    if let Ok(output) = Command::new("hostname").arg("-f").output() {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return Ok(name);
            }
        }
    }

    // Fallback to plain hostname
    if let Ok(output) = Command::new("hostname").output() {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return Ok(name);
            }
        }
    }

    Ok("localhost".into())
}

/// Collect all non-loopback IPv4 addresses from network interfaces.
pub fn get_ip_addresses() -> Result<Vec<String>> {
    let ifaces = if_addrs::get_if_addrs().context("Failed to enumerate network interfaces")?;

    let mut addrs: Vec<String> = ifaces
        .iter()
        .filter_map(|iface| {
            if iface.is_loopback() {
                return None;
            }
            match &iface.ip() {
                IpAddr::V4(addr) => Some(addr.to_string()),
                IpAddr::V6(_) => None,
            }
        })
        .collect();

    addrs.sort();
    addrs.dedup();
    Ok(addrs)
}

/// Extract OS distribution details from `/etc/os-release` and kernel version.
/// Returns a JSON object with: distro, version, id_like, kernel.
pub fn get_os_details() -> Result<serde_json::Value> {
    let mut details = serde_json::Map::new();

    // Parse /etc/os-release (exists on all target distros)
    if let Ok(content) = fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                // Strip surrounding quotes from value
                let unquoted = value.trim().trim_matches('"').trim_matches('\'');
                match key {
                    "NAME" => {
                        details.insert(
                            "distro".into(),
                            serde_json::Value::String(unquoted.to_string()),
                        );
                    }
                    "VERSION_ID" => {
                        details.insert(
                            "version".into(),
                            serde_json::Value::String(unquoted.to_string()),
                        );
                    }
                    "ID_LIKE" => {
                        details.insert(
                            "id_like".into(),
                            serde_json::Value::String(unquoted.to_string()),
                        );
                    }
                    "VERSION_CODENAME" => {
                        details.insert(
                            "codename".into(),
                            serde_json::Value::String(unquoted.to_string()),
                        );
                    }
                    _ => {}
                }
            }
        }
    } else {
        // Fallback for systems without os-release (very rare)
        details.insert("distro".into(), serde_json::Value::String("unknown".into()));
        details.insert(
            "version".into(),
            serde_json::Value::String("unknown".into()),
        );
    }

    // Kernel version via uname -r
    if let Ok(output) = Command::new("uname").arg("-r").output() {
        if output.status.success() {
            let kernel = String::from_utf8_lossy(&output.stdout).trim().to_string();
            details.insert("kernel".into(), serde_json::Value::String(kernel));
        }
    } else {
        details.insert("kernel".into(), serde_json::Value::String("unknown".into()));
    }

    Ok(serde_json::Value::Object(details))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_id_is_not_empty() {
        let id = get_machine_id().expect("Failed to get machine-id");
        assert!(!id.is_empty(), "machine-id should not be empty");
        assert_eq!(id.len(), 32, "machine-id should be 32 hex chars");
    }

    #[test]
    fn fqdn_is_not_empty() {
        let fqdn = get_fqdn().expect("Failed to get FQDN");
        assert!(!fqdn.is_empty(), "FQDN should not be empty");
    }

    #[test]
    fn os_details_contains_kernel() {
        let details = get_os_details().expect("Failed to get OS details");
        assert!(
            details.get("kernel").is_some(),
            "OS details must contain kernel version"
        );
    }
}
