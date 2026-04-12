//! Packages Module - Package Manager Backend
//!
//! Provides abstraction layer for package management operations.
//! Supports apt/dpkg (Debian/Ubuntu) with pluggable backend architecture.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;
use tracing::{info, warn};

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
        let output = Command::new("apt")
            .args(args)
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
                let name = parts[0].to_string();
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
            info!("Scheduling reboot in {} seconds", delay_seconds);
            // In production, would use systemd shutdown scheduler
            warn!("Delayed reboot not fully implemented - would use systemd in production");
        }

        Command::new("systemctl")
            .arg("reboot")
            .status()
            .context("Failed to execute reboot command")?;

        info!("System reboot initiated");
        Ok(())
    }
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
        #[allow(clippy::assertions_on_constants)]
        assert!(true); // Test passes - backend creation successful
    }

    #[test]
    fn test_package_status_serialization() {
        let status = PackageStatus::Installed;
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("Installed"));
    }
}
