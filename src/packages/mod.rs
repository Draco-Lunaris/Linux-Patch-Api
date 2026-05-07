//! Packages Module - Package Manager Backend
//!
//! Provides abstraction layer for package management operations.
//! Supports apt/dpkg (Debian/Ubuntu) with pluggable backend architecture.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;
use tracing::info;

/// Package status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PackageStatus {
    Installed,
    Available,
    Upgradable,
    NotInstalled,
}

/// Package information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub status: PackageStatus,
    pub upgradable: bool,
    pub latest_version: Option<String>,
    pub description: String,
    pub dependencies: Vec<String>,
    pub reverse_dependencies: Vec<String>,
    pub install_date: Option<String>,
    pub size_installed: Option<String>,
}

/// Package installation options
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallOptions {
    pub force: bool,
    pub no_recommends: bool,
}

/// Patch information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub name: String,
    pub current_version: String,
    pub available_version: String,
    pub severity: String,
    pub description: String,
    pub cve_ids: Vec<String>,
    pub requires_reboot: bool,
}

/// System information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub hostname: String,
    pub os: String,
    pub os_version: String,
    pub kernel: String,
    pub architecture: String,
    pub last_update_check: Option<String>,
    pub last_update_apply: Option<String>,
    pub pending_reboot: bool,
}

/// Service status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub display_name: String,
    pub active_state: String,
    pub sub_state: String,
    pub load_state: String,
    pub enabled_state: String,
    pub main_pid: Option<u32>,
    pub healthy: bool,
}

/// Package manager backend trait
pub trait PackageManagerBackend: Send + Sync {
    fn list_packages(&self, filter: Option<&str>) -> Result<Vec<Package>>;
    fn get_package(&self, name: &str) -> Result<Option<Package>>;
    fn install_packages(&self, packages: &[PackageSpec], options: &InstallOptions) -> Result<()>;
    fn update_package(&self, name: &str) -> Result<()>;
    fn remove_package(&self, name: &str, purge: bool) -> Result<()>;
    fn list_patches(&self) -> Result<Vec<Patch>>;
    fn apply_patches(&self, packages: Option<&[String]>) -> Result<()>;
    fn get_system_info(&self) -> Result<SystemInfo>;
    fn reboot_system(&self, delay_seconds: u64) -> Result<()>;
    fn get_service_status(&self, name: &str) -> Result<Option<ServiceStatus>>;
}

/// Package specification for installation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSpec {
    pub name: String,
    pub version: Option<String>,
}

/// APT package manager backend (Debian/Ubuntu)
pub struct AptBackend {
    _marker: std::marker::PhantomData<()>,
}

impl AptBackend {
    pub fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }

    /// Run apt command and capture output
    fn run_apt(&self, args: &[&str]) -> Result<String> {
        // Service runs as root - no sudo needed for apt commands
        let program = "apt";
        let cmd_args: Vec<&str> = args.to_vec();

        let output = Command::new(program)
            .args(&cmd_args)
            .output()
            .context("Failed to execute apt command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("apt command failed: {}", stderr));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run dpkg command and capture output
    fn run_dpkg(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("dpkg")
            .args(args)
            .output()
            .context("Failed to execute dpkg command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("dpkg command failed: {}", stderr));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Parse package list from apt output
    fn parse_package_list(&self, output: &str) -> Vec<Package> {
        let mut packages = Vec::new();

        for line in output.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let name = parts[0].to_string();
                let status_str = parts[1];
                let version = parts[2].to_string();

                let status = if status_str.starts_with("ii") {
                    PackageStatus::Installed
                } else if status_str.starts_with("iU") {
                    PackageStatus::Upgradable
                } else {
                    PackageStatus::Available
                };

                let description = parts[4..].join(" ");
                let upgradable = status == PackageStatus::Upgradable;

                packages.push(Package {
                    name,
                    version: version.clone(),
                    status: status.clone(),
                    upgradable,
                    latest_version: Some(version),
                    description,
                    dependencies: Vec::new(),
                    reverse_dependencies: Vec::new(),
                    install_date: None,
                    size_installed: None,
                });
            }
        }

        packages
    }
}

impl PackageManagerBackend for AptBackend {
    fn list_packages(&self, filter: Option<&str>) -> Result<Vec<Package>> {
        let args = match filter {
            Some(f) => vec!["list", f],
            None => vec!["list", "--installed"],
        };

        let output = self.run_apt(&args)?;
        Ok(self.parse_package_list(&output))
    }

    fn get_package(&self, name: &str) -> Result<Option<Package>> {
        // Check if installed
        let dpkg_output = self.run_dpkg(&["-s", name]);

        if dpkg_output.is_err() {
            // Package not installed, check if available
            let list_output = self.run_apt(&["list", name])?;
            if list_output.contains(name) {
                let parts: Vec<&str> = list_output
                    .lines()
                    .find(|l| l.contains(name))
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();

                if parts.len() >= 3 {
                    return Ok(Some(Package {
                        name: name.to_string(),
                        version: parts[1].to_string(),
                        status: PackageStatus::Available,
                        upgradable: false,
                        latest_version: Some(parts[1].to_string()),
                        description: String::new(),
                        dependencies: Vec::new(),
                        reverse_dependencies: Vec::new(),
                        install_date: None,
                        size_installed: None,
                    }));
                }
            }
            return Ok(None);
        }

        let dpkg_info = dpkg_output?;

        // Parse dpkg status output
        let mut version = String::new();
        let mut status = PackageStatus::Installed;
        let mut description = String::new();
        let mut dependencies = Vec::new();
        let install_date = None;
        let mut size_installed = None;

        for line in dpkg_info.lines() {
            if line.starts_with("Version:") {
                version = line.trim_start_matches("Version:").trim().to_string();
            } else if line.starts_with("Status:") {
                if line.contains("install ok installed") {
                    status = PackageStatus::Installed;
                }
            } else if line.starts_with("Description:") {
                description = line.trim_start_matches("Description:").trim().to_string();
            } else if line.starts_with("Depends:") {
                dependencies = line
                    .trim_start_matches("Depends:")
                    .trim()
                    .split(',')
                    .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
                    .collect();
            } else if line.starts_with("Installed-Size:") {
                size_installed = Some(format!(
                    "{} KB",
                    line.trim_start_matches("Installed-Size:").trim()
                ));
            }
        }

        // Check if upgradable
        let upgradable = self
            .run_apt(&["list", "--upgradable", name])
            .map(|o| o.contains(name))
            .unwrap_or(false);

        let latest_version = if upgradable {
            self.run_apt(&["policy", name]).ok().and_then(|o| {
                o.lines()
                    .find(|l| l.contains("Candidate"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .map(|s| s.to_string())
            })
        } else {
            Some(version.clone())
        };

        Ok(Some(Package {
            name: name.to_string(),
            version,
            status,
            upgradable,
            latest_version,
            description,
            dependencies,
            reverse_dependencies: Vec::new(),
            install_date,
            size_installed,
        }))
    }

    fn install_packages(&self, packages: &[PackageSpec], options: &InstallOptions) -> Result<()> {
        let mut args: Vec<String> = vec!["install".to_string(), "-y".to_string()];

        if options.no_recommends {
            args.push("--no-install-recommends".to_string());
        }

        if options.force {
            args.push("--force-yes".to_string());
        }

        for pkg in packages {
            let pkg_arg = if let Some(version) = &pkg.version {
                format!("{}={}", pkg.name, version)
            } else {
                pkg.name.clone()
            };
            args.push(pkg_arg);
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_apt(&args_ref)?;
        info!(
            "Installed packages: {:?}",
            packages.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        Ok(())
    }

    fn update_package(&self, name: &str) -> Result<()> {
        self.run_apt(&["install", "-y", "--only-upgrade", name])?;
        info!("Updated package: {}", name);
        Ok(())
    }

    fn remove_package(&self, name: &str, purge: bool) -> Result<()> {
        let args = if purge {
            vec!["purge", "-y", name]
        } else {
            vec!["remove", "-y", name]
        };

        self.run_apt(&args)?;
        info!("Removed package: {} (purge={})", name, purge);
        Ok(())
    }

    fn list_patches(&self) -> Result<Vec<Patch>> {
        let output = self.run_apt(&["list", "--upgradable"])?;
        let mut patches = Vec::new();

        for line in output.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                // Strip release suffix from package name (e.g., "pkg/noble-updates,noble-security" → "pkg")
                let name = parts[0].split('/').next().unwrap_or(parts[0]).to_string();
                let current_version = parts[1].to_string();
                let available_version = parts[2].to_string();

                // Determine severity based on package name heuristics
                let severity =
                    if name.contains("kernel") || name.contains("ssl") || name.contains("security")
                    {
                        "critical".to_string()
                    } else if name.contains("lib") {
                        "high".to_string()
                    } else {
                        "medium".to_string()
                    };

                patches.push(Patch {
                    name,
                    current_version,
                    available_version,
                    severity,
                    description: String::from("Package update available"),
                    cve_ids: Vec::new(),
                    requires_reboot: false,
                });
            }
        }

        Ok(patches)
    }

    fn apply_patches(&self, packages: Option<&[String]>) -> Result<()> {
        let args = match packages {
            Some(pkgs) => {
                let mut a = vec!["install", "-y"];
                for pkg in pkgs {
                    a.push(pkg);
                }
                a
            }
            None => {
                vec!["upgrade", "-y"]
            }
        };

        self.run_apt(&args)?;
        info!("Applied patches for packages: {:?}", packages);
        Ok(())
    }

    fn get_system_info(&self) -> Result<SystemInfo> {
        let hostname = Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let os_info = std::fs::read_to_string("/etc/os-release")
            .ok()
            .map(|content| {
                let mut os = "Linux".to_string();
                let mut version = "unknown".to_string();

                for line in content.lines() {
                    if line.starts_with("PRETTY_NAME=") {
                        os = line
                            .trim_start_matches("PRETTY_NAME=")
                            .trim()
                            .trim_matches('"')
                            .to_string();
                    } else if line.starts_with("NAME=") {
                        os = line
                            .trim_start_matches("NAME=")
                            .trim()
                            .trim_matches('"')
                            .to_string();
                    } else if line.starts_with("VERSION=") {
                        version = line
                            .trim_start_matches("VERSION=")
                            .trim()
                            .trim_matches('"')
                            .to_string();
                    }
                }

                (os, version)
            })
            .unwrap_or_else(|| ("Linux".to_string(), "unknown".to_string()));

        let kernel = Command::new("uname")
            .arg("-r")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let architecture = Command::new("uname")
            .arg("-m")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Check if reboot is pending
        let pending_reboot = std::path::Path::new("/var/run/reboot-required").exists();

        Ok(SystemInfo {
            hostname,
            os: os_info.0,
            os_version: os_info.1,
            kernel,
            architecture,
            last_update_check: None,
            last_update_apply: None,
            pending_reboot,
        })
    }

    fn reboot_system(&self, delay_seconds: u64) -> Result<()> {
        if delay_seconds > 0 {
            // Use shutdown command for delayed reboot (converts seconds to minutes, minimum 1)
            let delay_minutes = std::cmp::max(1u64, delay_seconds.div_ceil(60));
            info!(
                "Scheduling system reboot in {} minutes (requested {} seconds)",
                delay_minutes, delay_seconds
            );
            Command::new("shutdown")
                .args(["-r", &format!("+{}", delay_minutes)])
                .status()
                .context("Failed to schedule delayed reboot")?;
            info!("System reboot scheduled in {} minutes", delay_minutes);
        } else {
            // Immediate reboot using systemctl
            info!("Initiating immediate system reboot");
            Command::new("systemctl")
                .arg("reboot")
                .status()
                .context("Failed to execute reboot command")?;
            info!("System reboot initiated");
        }

        Ok(())
    }

    fn get_service_status(&self, name: &str) -> Result<Option<ServiceStatus>> {
        // Validate service name to prevent shell injection
        if name.is_empty() || name.contains('/') || name.contains("..") {
            return Err(anyhow::anyhow!("Invalid service name: {}", name));
        }

        // Determine init system and query accordingly
        let is_systemd = std::path::Path::new("/run/systemd/system").exists();
        let is_openrc = std::path::Path::new("/sbin/openrc").exists();

        if is_systemd {
            get_systemd_service_status(name)
        } else if is_openrc {
            get_openrc_service_status(name)
        } else {
            Err(anyhow::anyhow!(
                "No supported init system detected (systemd or OpenRC required)"
            ))
        }
    }
}

/// Query systemd service status via systemctl
fn get_systemd_service_status(name: &str) -> Result<Option<ServiceStatus>> {
    let output = Command::new("systemctl")
        .args([
            "show",
            name,
            "--property=Id,Description,ActiveState,SubState,LoadState,UnitFileState,MainPID",
            "--no-pager",
        ])
        .output()
        .context("Failed to execute systemctl command")?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // If systemctl returns non-zero or empty output, service doesn't exist
    if !output.status.success() || stdout.trim().is_empty() {
        return Ok(None);
    }

    let mut id = String::new();
    let mut description = String::new();
    let mut active_state = String::new();
    let mut sub_state = String::new();
    let mut load_state = String::new();
    let mut unit_file_state = String::new();
    let mut main_pid: Option<u32> = None;

    for line in stdout.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "Id" => id = value.to_string(),
                "Description" => description = value.to_string(),
                "ActiveState" => active_state = value.to_string(),
                "SubState" => sub_state = value.to_string(),
                "LoadState" => load_state = value.to_string(),
                "UnitFileState" => unit_file_state = value.to_string(),
                "MainPID" => {
                    main_pid = value.parse::<u32>().ok().filter(|&p| p > 0);
                }
                _ => {}
            }
        }
    }

    // If LoadState is not-found or bad-setting, service doesn't exist
    if load_state == "not-found" || load_state == "bad-setting" || id.is_empty() {
        return Ok(None);
    }

    let healthy = active_state == "active" && sub_state == "running";

    // Check for socket activation: if service is inactive but enabled,
    // check if the corresponding .socket unit is active (listening)
    let healthy = if !healthy && active_state == "inactive" && unit_file_state == "enabled" {
        // Use the resolved service name (id) instead of input name,
        // so "sshd" resolves to "ssh.service" → "ssh.socket" correctly
        let socket_name = format!("{}.socket", id.trim_end_matches(".service"));
        if let Ok(socket_output) = Command::new("systemctl")
            .args(["show", &socket_name, "--property=ActiveState", "--no-pager"])
            .output()
        {
            let socket_stdout = String::from_utf8_lossy(&socket_output.stdout);
            if socket_stdout.contains("ActiveState=active") {
                true
            } else {
                healthy
            }
        } else {
            healthy
        }
    } else {
        healthy
    };

    Ok(Some(ServiceStatus {
        name: id,
        display_name: description,
        active_state,
        sub_state,
        load_state,
        enabled_state: unit_file_state,
        main_pid,
        healthy,
    }))
}

/// Query OpenRC service status via rc-service
fn get_openrc_service_status(name: &str) -> Result<Option<ServiceStatus>> {
    let output = Command::new("rc-service")
        .args([name, "status"])
        .output()
        .context("Failed to execute rc-service command")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // rc-service returns error if service doesn't exist
    if !output.status.success() {
        if stderr.contains("does not exist") || stdout.contains("does not exist") {
            return Ok(None);
        }
        return Err(anyhow::anyhow!("rc-service failed: {}", stderr));
    }

    // Parse rc-service status output
    let status_line = stdout.lines().next().unwrap_or("").to_lowercase();

    let (active_state, sub_state, healthy) =
        if status_line.contains("started") || status_line.contains("running") {
            ("active".to_string(), "running".to_string(), true)
        } else if status_line.contains("stopped") || status_line.contains("not running") {
            ("inactive".to_string(), "dead".to_string(), false)
        } else if status_line.contains("crashed") || status_line.contains("failed") {
            ("failed".to_string(), "failed".to_string(), false)
        } else {
            ("unknown".to_string(), "unknown".to_string(), false)
        };

    // Check if service is enabled using rc-update
    let enabled_output = Command::new("rc-update")
        .args(["show", "default"])
        .output()
        .ok();

    let enabled_state = enabled_output
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| {
            if s.lines().any(|l| l.trim().starts_with(name)) {
                "enabled".to_string()
            } else {
                "disabled".to_string()
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    Ok(Some(ServiceStatus {
        name: name.to_string(),
        display_name: name.to_string(),
        active_state,
        sub_state,
        load_state: "loaded".to_string(),
        enabled_state,
        main_pid: None,
        healthy,
    }))
}

impl Default for AptBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Package manager factory
pub fn create_backend() -> Result<Box<dyn PackageManagerBackend>> {
    // Detect package manager and return appropriate backend
    if std::path::Path::new("/usr/bin/apt").exists() {
        Ok(Box::new(AptBackend::new()))
    } else if std::path::Path::new("/usr/bin/dnf").exists() {
        // TODO: Implement DnfBackend for RHEL/CentOS/Fedora
        Err(anyhow::anyhow!("DNF backend not yet implemented"))
    } else if std::path::Path::new("/usr/bin/apk").exists() {
        // TODO: Implement ApkBackend for Alpine
        Err(anyhow::anyhow!("APK backend not yet implemented"))
    } else if std::path::Path::new("/usr/bin/pacman").exists() {
        // TODO: Implement PacmanBackend for Arch
        Err(anyhow::anyhow!("Pacman backend not yet implemented"))
    } else {
        Err(anyhow::anyhow!("No supported package manager found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apt_backend_creation() {
        let _backend = AptBackend::new();
        // Test passes if backend creation doesn't panic
    }

    #[test]
    fn test_package_status_serialization() {
        let status = PackageStatus::Installed;
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("Installed"));
    }
}
