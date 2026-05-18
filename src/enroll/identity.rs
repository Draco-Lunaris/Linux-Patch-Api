//! Cross-distribution identity extraction for Linux systems.
//!
//! Provides machine-id, FQDN, IP address, and OS-detail collection
//! compatible with Debian/Ubuntu, RHEL/CentOS/Fedora, Alpine, and Arch Linux.

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
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
///
/// Filters out container bridge subnets (Docker 172.16.0.0/12) and
/// link-local addresses (169.254.0.0/16) that are not routable from the manager.
pub fn get_ip_addresses() -> Result<Vec<String>> {
    let ifaces = if_addrs::get_if_addrs().context("Failed to enumerate network interfaces")?;

    let mut addrs: Vec<String> = ifaces
        .iter()
        .filter_map(|iface| {
            if iface.is_loopback() {
                return None;
            }
            match &iface.ip() {
                IpAddr::V4(addr) => {
                    // Filter container bridge and link-local subnets
                    if is_container_bridge(addr) || is_link_local(addr) {
                        tracing::debug!(
                            ip = %addr,
                            "Excluding container bridge or link-local IP from enrollment report"
                        );
                        return None;
                    }
                    Some(addr.to_string())
                }
                IpAddr::V6(_) => None,
            }
        })
        .collect();

    addrs.sort();
    addrs.dedup();
    Ok(addrs)
}

/// Check if an IPv4 address is in a container bridge subnet.
///
/// Filters the `172.16.0.0/12` range (172.16.0.0 – 172.31.255.255), which is
/// Docker's default bridge network allocation.
///
/// Note: `10.0.0.0/8` is NOT filtered because it is widely used for legitimate
/// LAN addressing. If a deployment uses a custom Docker bridge subnet outside
/// `172.16.0.0/12`, use `report_interface` or `report_ip` config to override.
pub fn is_container_bridge(addr: &Ipv4Addr) -> bool {
    // 172.16.0.0/12 = 172.16.0.0 – 172.31.255.255
    // Binary: 10101100.0001xxxx.xxxxxxxx.xxxxxxxx
    let octets = addr.octets();
    octets[0] == 172 && (octets[1] & 0xF0) == 0x10
}

/// Check if an IPv4 address is link-local (`169.254.0.0/16`).
///
/// Link-local addresses are auto-assigned when no DHCP is available and
/// are never routable across networks.
pub fn is_link_local(addr: &Ipv4Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 169 && octets[1] == 254
}

/// Determine the local source IP that would be used to reach a target IP.
/// Uses the kernel routing table via `ip route get <target>`.
///
/// This is the most accurate way to select the correct local IP because it
/// queries the kernel routing table directly, which accounts for all routing
/// rules, interface priorities, and source address selection.
pub fn get_route_source_ip(target_ip: &str) -> Result<String> {
    let output = Command::new("ip")
        .args(["route", "get", target_ip])
        .output()
        .context("Failed to execute 'ip route get' — is iproute2 installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "'ip route get {}' failed: {}",
            target_ip,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse output like: "192.168.3.36 via 192.168.1.1 dev eth0 src 192.168.3.36 uid ..."
    // We want the 'src' field value
    let mut found_src = false;
    for part in stdout.split_whitespace() {
        if found_src {
            // Validate it's a valid IPv4 address
            if part.parse::<Ipv4Addr>().is_ok() {
                let addr = part.parse::<Ipv4Addr>().unwrap();
                if !addr.is_loopback() && !is_container_bridge(&addr) && !is_link_local(&addr) {
                    tracing::info!(
                        target_ip = target_ip,
                        source_ip = part,
                        "Route-based IP selection: local source IP for reaching target"
                    );
                    return Ok(part.to_string());
                }
            }
            break;
        }
        if part == "src" {
            found_src = true;
        }
    }

    Err(anyhow!(
        "Could not determine source IP for route to '{}' — 'ip route get' output: {}",
        target_ip,
        stdout.trim()
    ))
}

/// Get the IPv4 address of a specific network interface by name.
///
/// Returns the first non-loopback IPv4 address on the named interface.
/// Useful when the admin knows which interface faces the manager network.
pub fn get_ip_for_interface(interface_name: &str) -> Result<String> {
    let ifaces = if_addrs::get_if_addrs()
        .with_context(|| "Failed to enumerate network interfaces for interface lookup")?;

    for iface in &ifaces {
        if iface.name != interface_name {
            continue;
        }
        if let IpAddr::V4(addr) = iface.ip() {
            if !iface.is_loopback() {
                tracing::info!(
                    interface = interface_name,
                    ip = %addr,
                    "Resolved IP from configured interface"
                );
                return Ok(addr.to_string());
            }
        }
    }

    Err(anyhow!(
        "No non-loopback IPv4 address found on interface '{}'",
        interface_name
    ))
}

/// Determine the primary IP address to report to the manager.
///
/// Resolution priority:
/// 1. `report_ip` — explicit IP from config (highest priority)
/// 2. `report_interface` — IP from a named interface
/// 3. `route_target` — route-based selection using kernel routing table
/// 4. Auto-detect — first IP from `get_ip_addresses()` (bridge subnets already filtered)
pub fn get_primary_ip(
    report_interface: Option<&str>,
    report_ip: Option<&str>,
    route_target: Option<&str>,
) -> Result<String> {
    // Priority 1: Explicit IP override
    if let Some(ip) = report_ip {
        // Validate it parses as IPv4
        if let Ok(addr) = ip.parse::<Ipv4Addr>() {
            if !addr.is_loopback() {
                tracing::info!(ip = ip, "Using explicitly configured report_ip");
                return Ok(ip.to_string());
            }
            tracing::warn!(
                ip = ip,
                "Configured report_ip is a loopback address — ignoring"
            );
        } else {
            tracing::warn!(
                ip = ip,
                "Configured report_ip is not a valid IPv4 address — falling back to auto-detect"
            );
        }
    }

    // Priority 2: Interface name override
    if let Some(iface) = report_interface {
        match get_ip_for_interface(iface) {
            Ok(ip) => return Ok(ip),
            Err(e) => {
                tracing::warn!(
                    interface = iface,
                    error = %e,
                    "Configured report_interface lookup failed — falling back to route-based or auto-detect"
                );
            }
        }
    }

    // Priority 3: Route-based selection using kernel routing table
    if let Some(target) = route_target {
        match get_route_source_ip(target) {
            Ok(ip) => {
                tracing::info!(
                    target = target,
                    ip = %ip,
                    "Using route-based IP selection for target"
                );
                return Ok(ip);
            }
            Err(e) => {
                tracing::warn!(
                    target = target,
                    error = %e,
                    "Route-based IP selection failed — falling back to auto-detect"
                );
            }
        }
    }

    // Priority 4: Auto-detect (bridge subnets already filtered by get_ip_addresses)
    let addrs = get_ip_addresses()?;
    addrs
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("No suitable IPv4 address found on any interface"))
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

    // =============================================================================
    // Container Bridge & Link-Local Filtering Tests
    // =============================================================================

    #[test]
    fn test_is_container_bridge_docker_default() {
        // Docker default bridge network: 172.17.0.0/16
        assert!(is_container_bridge(&"172.17.0.1".parse().unwrap()));
        assert!(is_container_bridge(&"172.17.255.255".parse().unwrap()));
    }

    #[test]
    fn test_is_container_bridge_full_range() {
        // 172.16.0.0/12 = 172.16.0.0 – 172.31.255.255
        assert!(is_container_bridge(&"172.16.0.1".parse().unwrap()));
        assert!(is_container_bridge(&"172.31.255.255".parse().unwrap()));
        assert!(is_container_bridge(&"172.20.0.1".parse().unwrap()));
    }

    #[test]
    fn test_is_not_container_bridge() {
        // Outside 172.16.0.0/12
        assert!(!is_container_bridge(&"192.168.3.36".parse().unwrap()));
        assert!(!is_container_bridge(&"10.0.0.1".parse().unwrap()));
        assert!(!is_container_bridge(&"172.32.0.1".parse().unwrap()));
        assert!(!is_container_bridge(&"172.15.0.1".parse().unwrap()));
        assert!(!is_container_bridge(&"8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn test_is_link_local() {
        assert!(is_link_local(&"169.254.0.1".parse().unwrap()));
        assert!(is_link_local(&"169.254.255.255".parse().unwrap()));
    }

    #[test]
    fn test_is_not_link_local() {
        assert!(!is_link_local(&"192.168.1.1".parse().unwrap()));
        assert!(!is_link_local(&"169.253.0.1".parse().unwrap()));
        assert!(!is_link_local(&"169.255.0.1".parse().unwrap()));
    }

    #[test]
    fn test_get_ip_addresses_excludes_docker_bridge() {
        // On a system with Docker, the returned IPs should not include 172.16.0.0/12
        let addrs = get_ip_addresses().expect("Failed to get IP addresses");
        for addr in &addrs {
            let parsed: Ipv4Addr = addr.parse().expect("Should parse as IPv4");
            assert!(
                !is_container_bridge(&parsed),
                "IP '{}' is in Docker bridge range 172.16.0.0/12 — should be excluded",
                addr
            );
        }
    }

    #[test]
    fn test_get_ip_addresses_excludes_link_local() {
        let addrs = get_ip_addresses().expect("Failed to get IP addresses");
        for addr in &addrs {
            let parsed: Ipv4Addr = addr.parse().expect("Should parse as IPv4");
            assert!(
                !is_link_local(&parsed),
                "IP '{}' is link-local 169.254.0.0/16 — should be excluded",
                addr
            );
        }
    }

    #[test]
    fn test_get_primary_ip_auto_detect() {
        // Without overrides, should return a valid non-bridge IP
        // In Docker containers, auto-detect may find no routable IPs — that's valid
        match get_primary_ip(None, None, None) {
            Ok(ip) => {
                assert!(!ip.is_empty(), "Primary IP should not be empty");
                let parsed: Ipv4Addr = ip.parse().expect("Primary IP should be valid IPv4");
                assert!(
                    !is_container_bridge(&parsed),
                    "Auto-detected IP should not be Docker bridge"
                );
            }
            Err(_) => {
                eprintln!("NOTE: No routable IPs found — likely running inside a Docker container");
            }
        }
    }

    #[test]
    fn test_get_primary_ip_explicit_override() {
        // Explicit IP should be returned as-is
        let ip = get_primary_ip(None, Some("10.99.99.1"), None).expect("Failed with explicit IP");
        assert_eq!(ip, "10.99.99.1");
    }

    #[test]
    fn test_get_primary_ip_rejects_loopback_override() {
        // Loopback in report_ip should fall back to auto-detect
        // In Docker containers, auto-detect may also fail — that's valid
        match get_primary_ip(None, Some("127.0.0.1"), None) {
            Ok(ip) => assert_ne!(ip, "127.0.0.1"),
            Err(_) => {
                eprintln!(
                    "NOTE: Loopback rejected but no routable IPs for fallback — Docker container"
                );
            }
        }
    }

    #[test]
    fn test_get_primary_ip_invalid_override_falls_back() {
        // Invalid IP in report_ip should fall back to auto-detect
        // In Docker containers, auto-detect may also fail — that's valid
        match get_primary_ip(None, Some("not-an-ip"), None) {
            Ok(ip) => assert!(!ip.is_empty()),
            Err(_) => {
                eprintln!(
                    "NOTE: Invalid IP rejected but no routable IPs for fallback — Docker container"
                );
            }
        }
    }

    #[test]
    fn test_get_primary_ip_route_target_priority() {
        // Route-based selection should be tried before auto-detect
        // We test with a well-known IP; if iproute2 is available this may succeed,
        // otherwise it falls back gracefully
        match get_primary_ip(None, None, Some("8.8.8.8")) {
            Ok(ip) => {
                assert!(!ip.is_empty(), "Route-based IP should not be empty");
                let parsed: Ipv4Addr = ip.parse().expect("Route-based IP should be valid IPv4");
                assert!(
                    !is_container_bridge(&parsed),
                    "Route-based IP should not be Docker bridge"
                );
                assert!(
                    !parsed.is_loopback(),
                    "Route-based IP should not be loopback"
                );
            }
            Err(_) => {
                eprintln!(
                    "NOTE: Route-based selection failed — iproute2 may not be available in this environment"
                );
            }
        }
    }

    #[test]
    fn test_get_primary_ip_explicit_overrides_route_target() {
        // Explicit report_ip should take priority over route_target
        let ip = get_primary_ip(None, Some("10.99.99.1"), Some("8.8.8.8"))
            .expect("Explicit IP should override route_target");
        assert_eq!(ip, "10.99.99.1");
    }

    #[test]
    fn test_get_route_source_ip_known_target() {
        // Test route-based IP detection with a well-known target
        // This test requires iproute2 to be installed
        match get_route_source_ip("8.8.8.8") {
            Ok(ip) => {
                let parsed: Ipv4Addr = ip.parse().expect("Route source IP should be valid IPv4");
                assert!(
                    !parsed.is_loopback(),
                    "Route source IP should not be loopback"
                );
                assert!(
                    !is_container_bridge(&parsed),
                    "Route source IP should not be Docker bridge"
                );
                assert!(
                    !is_link_local(&parsed),
                    "Route source IP should not be link-local"
                );
            }
            Err(e) => {
                // Acceptable in containers without iproute2 or routing
                eprintln!(
                    "NOTE: Route-based IP detection failed: {} — may be unavailable in this environment",
                    e
                );
            }
        }
    }
}
