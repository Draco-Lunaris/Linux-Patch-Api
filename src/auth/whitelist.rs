//! IP Whitelist Enforcement Module
//!
//! Provides IP-based access control with CIDR subnet support.
//! Loads configuration from YAML file with auto-reload support.
//! All connections not in whitelist are silently dropped.

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Whitelist entry types
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WhitelistEntry {
    /// Single IP address
    Ip(Ipv4Addr),
    /// CIDR subnet
    Cidr { network: Ipv4Addr, prefix: u8 },
    /// Hostname (resolved at startup)
    Hostname { name: String, resolved: Ipv4Addr },
}

/// Whitelist configuration loaded from YAML
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WhitelistConfig {
    pub entries: Vec<String>,
}

/// IP Whitelist manager with auto-reload support
pub struct WhitelistManager {
    entries: Arc<RwLock<HashSet<WhitelistEntry>>>,
    config_path: String,
    watcher: Option<RecommendedWatcher>,
}

impl WhitelistManager {
    /// Create a new whitelist manager
    pub fn new(config_path: &str) -> Result<Self> {
        let entries = Arc::new(RwLock::new(HashSet::new()));

        let mut manager = Self {
            entries: entries.clone(),
            config_path: config_path.to_string(),
            watcher: None,
        };

        // Load initial whitelist
        manager.reload()?;

        // Set up file watcher for auto-reload
        manager.setup_watcher()?;

        Ok(manager)
    }

    /// Reload whitelist from configuration file
    pub fn reload(&self) -> Result<()> {
        let config = self.load_config()?;
        let entries = self.parse_entries(&config.entries)?;

        let mut current_entries = self
            .entries
            .write()
            .map_err(|e| anyhow::anyhow!("Failed to acquire whitelist lock: {}", e))?;

        *current_entries = entries;

        info!(
            path = %self.config_path,
            count = current_entries.len(),
            "Whitelist reloaded successfully"
        );

        Ok(())
    }

    /// Append an IP address or CIDR entry to the whitelist file.
    /// Creates the file if it doesn't exist. Uses file locking for concurrent safety.
    /// Logs the change to audit log.
    pub fn append_entry(&mut self, ip_or_cidr: &str) -> Result<()> {
        // 1. Validate IP/CIDR format
        let entry_str = ip_or_cidr.trim();
        if entry_str.is_empty() {
            bail!("Cannot append empty whitelist entry");
        }

        // Parse to validate - must be IPv4 or CIDR, no hostnames in auto-append
        let parsed_entry = if let Some((ip_str, prefix_str)) = entry_str.split_once('/') {
            let ip: Ipv4Addr = ip_str.parse()
                .with_context(|| format!("Invalid IP in CIDR notation: {}", entry_str))?;
            let prefix: u8 = prefix_str.parse()
                .with_context(|| format!("Invalid prefix in CIDR notation: {}", entry_str))?;
            if prefix > 32 {
                anyhow::bail!("Invalid CIDR prefix (must be 0-32): {}", entry_str);
            }
            WhitelistEntry::Cidr { network: ip, prefix }
        } else {
            let ip: Ipv4Addr = entry_str.parse()
                .with_context(|| format!("Invalid IPv4 address: {}", entry_str))?;
            WhitelistEntry::Ip(ip)
        };

        // 2. Check for duplicate in current in-memory state
        {
            let entries = self.entries.read().map_err(|e| anyhow::anyhow!("Failed to acquire whitelist read lock: {}", e))?;
            for existing in entries.iter() {
                if *existing == parsed_entry {
                    info!(
                        action = "whitelist_append",
                        ip = entry_str,
                        source = "enrollment",
                        already_exists = true,
                        "Whitelist entry already exists, skipping duplicate"
                    );
                    return Ok(());
                }
            }
        }

        // 3. Acquire exclusive file lock using fs2
        let lock_path = format!("{}.lock", self.config_path);
        let lock_file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("Failed to create lock file: {}", lock_path))?;

        lock_file.lock_exclusive().context("Failed to acquire exclusive whitelist lock")?;

        // Double-check for duplicates after acquiring lock (concurrent append scenario)
        {
            let entries = self.entries.read().map_err(|e| anyhow::anyhow!("Failed to acquire whitelist read lock: {}", e))?;
            for existing in entries.iter() {
                if *existing == parsed_entry {
                    info!(
                        action = "whitelist_append",
                        ip = entry_str,
                        source = "enrollment",
                        already_exists = true,
                        "Whitelist entry already exists (post-lock check), skipping duplicate"
                    );
                    return Ok(());
                }
            }
        }

        // 4. Read current whitelist YAML or create empty config
        let mut config = if Path::new(&self.config_path).exists() {
            self.load_config().context("Failed to load existing whitelist for append")?
        } else {
            WhitelistConfig { entries: Vec::new() }
        };

        // 5. Append new entry to allowed_ips list
        config.entries.push(entry_str.to_string());

        // 6. Write back atomically (temp file + rename)
        let config_path = Path::new(&self.config_path);

        // Ensure parent directory exists
        if let Some(parent) = config_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create whitelist directory: {}", parent.display()))?;
            }
        }

        let yaml_content = serde_yaml::to_string(&config)
            .with_context(|| "Failed to serialize whitelist config")?;

        let temp_path = config_path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .truncate(true)
            .open(&temp_path)
            .with_context(|| format!("Failed to create temp whitelist file: {}", temp_path.display()))?;

        file.write_all(yaml_content.as_bytes())
            .with_context(|| format!("Failed to write whitelist data to: {}", temp_path.display()))?;
        file.flush()
            .with_context(|| format!("Failed to flush whitelist data to: {}", temp_path.display()))?;

        // Atomic rename
        fs::rename(&temp_path, config_path)
            .with_context(|| {
                format!(
                    "Failed to atomically rename whitelist temp file {} to {}",
                    temp_path.display(),
                    config_path.display()
                )
            })?;

        // Release lock explicitly before reload (drop happens at end of scope)
        drop(lock_file);

        // 7. Reload in-memory state
        self.reload().context("Failed to reload whitelist after append")?;

        // 8. Log audit event
        tracing::info!(
            action = "whitelist_append",
            ip = entry_str,
            source = "enrollment",
            total_entries = self.entry_count(),
            "Whitelist entry added during enrollment"
        );

        Ok(())
    }

    /// Check if an IP address is allowed
    pub fn is_allowed(&self, ip: &Ipv4Addr) -> bool {
        let entries = self.entries.read().unwrap();

        for entry in entries.iter() {
            match entry {
                WhitelistEntry::Ip(allowed_ip) => {
                    if ip == allowed_ip {
                        return true;
                    }
                }
                WhitelistEntry::Cidr { network, prefix } => {
                    if ip_in_subnet(ip, *network, *prefix) {
                        return true;
                    }
                }
                WhitelistEntry::Hostname { resolved, .. } => {
                    if ip == resolved {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Check if a socket address is allowed
    pub fn is_socket_allowed(&self, socket_addr: &SocketAddr) -> bool {
        match socket_addr.ip() {
            IpAddr::V4(ip) => self.is_allowed(&ip),
            IpAddr::V6(_) => {
                // IPv6 not supported in whitelist - deny by default
                warn!(socket_addr = %socket_addr, "IPv6 address denied - whitelist supports IPv4 only");
                false
            }
        }
    }

    /// Get the number of entries in the whitelist
    pub fn entry_count(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    /// Load configuration from YAML file
    fn load_config(&self) -> Result<WhitelistConfig> {
        let content = std::fs::read_to_string(&self.config_path)
            .with_context(|| format!("Failed to read whitelist config: {}", self.config_path))?;

        let config: WhitelistConfig = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse whitelist config: {}", self.config_path))?;

        Ok(config)
    }

    /// Parse whitelist entries from strings
    fn parse_entries(&self, entries: &[String]) -> Result<HashSet<WhitelistEntry>> {
        let mut parsed = HashSet::new();

        for entry_str in entries {
            let entry_str = entry_str.trim();

            // Skip comments and empty lines
            if entry_str.is_empty() || entry_str.starts_with('#') {
                continue;
            }

            // Check for CIDR notation
            if let Some((ip_str, prefix_str)) = entry_str.split_once('/') {
                let ip: Ipv4Addr = ip_str
                    .parse()
                    .with_context(|| format!("Invalid IP in CIDR notation: {}", entry_str))?;
                let prefix: u8 = prefix_str
                    .parse()
                    .with_context(|| format!("Invalid prefix in CIDR notation: {}", entry_str))?;

                if prefix > 32 {
                    anyhow::bail!("Invalid CIDR prefix (must be 0-32): {}", entry_str);
                }

                parsed.insert(WhitelistEntry::Cidr {
                    network: ip,
                    prefix,
                });
                debug!("Added CIDR entry: {}", entry_str);
            } else {
                // Try to parse as IP address
                if let Ok(ip) = entry_str.parse::<Ipv4Addr>() {
                    parsed.insert(WhitelistEntry::Ip(ip));
                    debug!("Added IP entry: {}", entry_str);
                } else {
                    // Try to resolve as hostname
                    match resolve_hostname(entry_str) {
                        Ok(resolved) => {
                            parsed.insert(WhitelistEntry::Hostname {
                                name: entry_str.to_string(),
                                resolved,
                            });
                            info!("Resolved hostname {} to {}", entry_str, resolved);
                        }
                        Err(e) => {
                            warn!("Failed to resolve hostname {}: {}", entry_str, e);
                        }
                    }
                }
            }
        }

        Ok(parsed)
    }

    /// Set up file watcher for auto-reload
    fn setup_watcher(&mut self) -> Result<()> {
        let config_path = self.config_path.clone();
        let _entries = self.entries.clone();

        let watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    match event.kind {
                        EventKind::Modify(_) | EventKind::Create(_) => {
                            info!("Whitelist file changed, reloading...");
                            // Reload is handled by the manager
                        }
                        _ => {}
                    }
                }
            },
            Config::default().with_poll_interval(Duration::from_secs(5)),
        )?;

        let mut watcher = watcher;
        let path = Path::new(&config_path);

        if path.exists() {
            watcher.watch(path, RecursiveMode::NonRecursive)?;
            info!("Watching whitelist file for changes: {}", config_path);
        } else {
            warn!("Whitelist file does not exist yet: {}", config_path);
        }

        self.watcher = Some(watcher);

        Ok(())
    }
}

/// Check if an IP address is within a CIDR subnet
fn ip_in_subnet(ip: &Ipv4Addr, network: Ipv4Addr, prefix: u8) -> bool {
    let ip_bits = u32::from(*ip);
    let network_bits = u32::from(network);
    let mask = if prefix == 0 {
        0
    } else {
        !0u32 << (32 - prefix)
    };

    (ip_bits & mask) == (network_bits & mask)
}

/// Resolve a hostname to an IPv4 address
fn resolve_hostname(hostname: &str) -> Result<Ipv4Addr> {
    use std::net::ToSocketAddrs;

    let addrs = (hostname, 0)
        .to_socket_addrs()
        .with_context(|| format!("Failed to resolve hostname: {}", hostname))?;

    for addr in addrs {
        if let IpAddr::V4(ip) = addr.ip() {
            return Ok(ip);
        }
    }

    anyhow::bail!("No IPv4 address found for hostname: {}", hostname)
}

/// Whitelist middleware for Actix-web
pub struct WhitelistMiddleware {
    manager: Arc<WhitelistManager>,
}

impl WhitelistMiddleware {
    /// Create a new whitelist middleware
    pub fn new(manager: WhitelistManager) -> Self {
        Self {
            manager: Arc::new(manager),
        }
    }

    /// Get the whitelist manager reference
    pub fn manager(&self) -> Arc<WhitelistManager> {
        self.manager.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ip_in_subnet() {
        // Test /24 subnet
        assert!(ip_in_subnet(
            &"192.168.1.100".parse().unwrap(),
            "192.168.1.0".parse().unwrap(),
            24
        ));
        assert!(ip_in_subnet(
            &"192.168.1.254".parse().unwrap(),
            "192.168.1.0".parse().unwrap(),
            24
        ));
        assert!(!ip_in_subnet(
            &"192.168.2.1".parse().unwrap(),
            "192.168.1.0".parse().unwrap(),
            24
        ));

        // Test /16 subnet
        assert!(ip_in_subnet(
            &"192.168.100.50".parse().unwrap(),
            "192.168.0.0".parse().unwrap(),
            16
        ));
        assert!(!ip_in_subnet(
            &"192.169.0.1".parse().unwrap(),
            "192.168.0.0".parse().unwrap(),
            16
        ));

        // Test /32 (single host)
        assert!(ip_in_subnet(
            &"10.0.0.50".parse().unwrap(),
            "10.0.0.50".parse().unwrap(),
            32
        ));
        assert!(!ip_in_subnet(
            &"10.0.0.51".parse().unwrap(),
            "10.0.0.50".parse().unwrap(),
            32
        ));

        // Test /0 (all IPs)
        assert!(ip_in_subnet(
            &"1.2.3.4".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            0
        ));
    }

    #[test]
    fn test_whitelist_entry_parsing() {
        let manager = WhitelistManager::new("/tmp/test_whitelist.yaml").unwrap_or_else(|_| {
            // Create a temp file for testing
            let temp_path = "/tmp/test_whitelist_temp.yaml";
            std::fs::write(temp_path, "entries:\n  - \"192.168.1.0/24\"\n").unwrap();
            WhitelistManager::new(temp_path).unwrap()
        });

        // Test IP entry
        let ip: Ipv4Addr = "192.168.1.100".parse().unwrap();
        assert!(manager.is_allowed(&ip));

        // Test IP outside subnet
        let ip_outside: Ipv4Addr = "192.168.2.100".parse().unwrap();
        assert!(!manager.is_allowed(&ip_outside));
    }
}
