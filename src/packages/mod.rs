//! Packages Module - Package Manager Backend
//!
//! Provides abstraction layer for package management operations.
//! Supports apt/dpkg (Debian/Ubuntu), apk (Alpine Linux), dnf (Fedora/RHEL), yum (CentOS 7),
//! and pacman (Arch Linux) with pluggable backend architecture.

pub mod cache;
pub mod coordinator;
pub mod error_utils;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use coordinator::CommandRunner;
use error_utils::{format_error_for_cache, CommandError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

/// Maximum allowed length for package names and version strings
pub const MAX_NAME_LENGTH: usize = 256;

pub const SELF_PACKAGE_NAME: &str = env!("CARGO_PKG_NAME"); // "linux-patch-api"
pub const SELF_SERVICE_NAME: &str = "linux-patch-api";
pub const MAX_RESTART_DELAY_SECONDS: u64 = 300;
/// Validate a package name against a strict allowlist pattern.
/// Prevents argument injection by blocking shell metacharacters,
/// path separators, whitespace, and leading hyphens.
/// Pattern: ^[a-zA-Z0-9][a-zA-Z0-9+._-]*$
pub fn validate_package_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Package name cannot be empty".to_string());
    }
    if name.len() > MAX_NAME_LENGTH {
        return Err(format!(
            "Package name exceeds maximum length of {} characters",
            MAX_NAME_LENGTH
        ));
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() {
        return Err(format!(
            "Package name must start with an alphanumeric character: '{}'",
            name
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '_' || c == '-')
    {
        return Err(format!(
            "Package name contains invalid characters: '{}'. Only alphanumeric, plus, dot, underscore, and hyphen are allowed",
            name
        ));
    }
    Ok(())
}

/// Validate a version string against a strict allowlist pattern.
/// Allows characters commonly found in package versions (colons for RPM epochs,
/// tildes for version ordering) while blocking shell metacharacters and path separators.
/// Pattern: ^[a-zA-Z0-9][a-zA-Z0-9+.:~_-]*$
pub fn validate_version_string(version: &str) -> Result<(), String> {
    if version.is_empty() {
        return Err("Version string cannot be empty".to_string());
    }
    if version.len() > MAX_NAME_LENGTH {
        return Err(format!(
            "Version string exceeds maximum length of {} characters",
            MAX_NAME_LENGTH
        ));
    }
    let bytes = version.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() {
        return Err(format!(
            "Version string must start with an alphanumeric character: '{}'",
            version
        ));
    }
    if !version.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || c == '+'
            || c == '.'
            || c == '_'
            || c == '-'
            || c == ':'
            || c == '~'
    }) {
        return Err(format!(
            "Version string contains invalid characters: '{}'. Only alphanumeric, plus, dot, underscore, hyphen, colon, and tilde are allowed",
            version
        ));
    }
    Ok(())
}

/// Validate a service name against a strict allowlist pattern.
/// Prevents shell injection and argument injection in systemctl/rc-service commands.
/// Allows hyphens (common in systemd unit names), dots for unit suffixes, and @ for template instances.
/// Pattern: ^[a-zA-Z0-9][a-zA-Z0-9_.@+-]*$
pub fn validate_service_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Service name cannot be empty".to_string());
    }
    if name.len() > MAX_NAME_LENGTH {
        return Err(format!(
            "Service name exceeds maximum length of {} characters",
            MAX_NAME_LENGTH
        ));
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() {
        return Err(format!(
            "Service name must start with an alphanumeric character: '{}'",
            name
        ));
    }
    if !name.chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '@' || c == '+' || c == '-'
    }) {
        return Err(format!(
            "Service name contains invalid characters: '{}'. Only alphanumeric, underscore, dot, at-sign, plus, and hyphen are allowed",
            name
        ));
    }
    Ok(())
}

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

    /// Refresh the local package index (apt-get update, dnf check-update, etc.)
    fn refresh_package_cache(&self, cache_state: &cache::PackageCacheState) -> Result<()>;

    /// Get the last cache update timestamp
    fn last_cache_update(&self, cache_state: &cache::PackageCacheState) -> Option<DateTime<Utc>>;

    /// Check if a package-manager operation (install/upgrade/remove/patch) is
    /// currently in progress. Used by the SIGTERM handler to decide whether to
    /// wait for the operation to complete before exiting, or to exit immediately.
    /// Returns false for backends that don't track operation state.
    fn is_operation_in_progress(&self) -> bool {
        false
    }

    /// Get the currently installed version of a package, or None if not installed.
    /// Used by the self-update flow to verify the installed version changed
    /// after an upgrade.
    fn get_installed_version(&self, name: &str) -> Result<Option<String>> {
        let _ = name;
        Ok(None)
    }

    /// Get the candidate (target) version for a package — the version that
    /// `install` or `upgrade` would install. Used by self-update to persist
    /// the expected target version before the operation begins.
    fn get_candidate_version(&self, name: &str) -> Result<Option<String>> {
        let _ = name;
        Ok(None)
    }

    /// Restart the agent's own service (not the whole system).
    ///
    /// On systemd: `systemctl restart linux-patch-api.service`
    /// On OpenRC: `rc-service linux-patch-api restart`
    ///
    /// Used by the self-update flow after draining active operations.
    /// The restart kills this process and starts the new binary.
    /// Default implementation uses systemctl (most common).
    fn restart_own_service(&self) -> Result<()> {
        let program = "systemctl";
        let args = ["restart", "linux-patch-api.service"];
        // Fire-and-forget: the process is about to be killed by the restart,
        // so waiting for the command to complete is pointless and blocks a
        // tokio worker thread. Spawn the command and return immediately.
        std::process::Command::new(program)
            .args(args)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .spawn()
            .context("Failed to spawn service restart command")?;
        info!("Service restart spawned (fire-and-forget)");
        Ok(())
    }
}

/// Shared helper: get system info via a command runner.
/// Used by all backends to avoid duplicated `get_system_info` implementations.
fn get_system_info_via_runner(runner: &dyn CommandRunner) -> Result<SystemInfo> {
    let hostname = runner
        .run("hostname", &[])
        .ok()
        .map(|o| o.stdout.trim().to_string())
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
                } else if line.starts_with("VERSION_ID=") {
                    version = line
                        .trim_start_matches("VERSION_ID=")
                        .trim()
                        .trim_matches('"')
                        .to_string();
                }
            }

            (os, version)
        })
        .unwrap_or_else(|| ("Linux".to_string(), "unknown".to_string()));

    let kernel = runner
        .run("uname", &["-r"])
        .ok()
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let architecture = runner
        .run("uname", &["-m"])
        .ok()
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let pending_reboot = std::path::Path::new("/var/run/reboot-required").exists()
        || std::path::Path::new("/boot/.reboot-required").exists();

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

/// Shared helper: reboot the system via a command runner.
fn reboot_system_via_runner(runner: &dyn CommandRunner, delay_seconds: u64) -> Result<()> {
    if delay_seconds > 0 {
        let delay_minutes = std::cmp::max(1u64, delay_seconds.div_ceil(60));
        info!(
            "Scheduling system reboot in {} minutes (requested {} seconds)",
            delay_minutes, delay_seconds
        );
        let delay_str = format!("+{}", delay_minutes);
        coordinator::run_command(runner, "shutdown", &["-r", &delay_str])?;
        info!("System reboot scheduled in {} minutes", delay_minutes);
    } else {
        info!("Initiating immediate system reboot");
        coordinator::run_command(runner, "systemctl", &["reboot"])?;
        info!("System reboot initiated");
    }
    Ok(())
}

/// Shared helper: query systemd service status via a command runner.
fn get_systemd_service_status_via_runner(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<Option<ServiceStatus>> {
    let output = runner.run(
        "systemctl",
        &[
            "show",
            "--property=Id,Description,ActiveState,SubState,LoadState,UnitFileState,MainPID",
            "--no-pager",
            "--",
            name,
        ],
    )?;

    let success = output.success();
    let stdout = output.stdout;

    if !success || stdout.trim().is_empty() {
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

    if load_state == "not-found" || load_state == "bad-setting" || id.is_empty() {
        return Ok(None);
    }

    let healthy = active_state == "active" && sub_state == "running";

    let healthy = if !healthy && active_state == "inactive" && unit_file_state == "enabled" {
        let socket_name = format!("{}.socket", id.trim_end_matches(".service"));
        if let Ok(socket_output) = runner.run(
            "systemctl",
            &["show", &socket_name, "--property=ActiveState", "--no-pager"],
        ) {
            if socket_output.stdout.contains("ActiveState=active") {
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

/// Shared helper: query OpenRC service status via a command runner.
fn get_openrc_service_status_via_runner(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<Option<ServiceStatus>> {
    let output = runner.run("rc-service", &[name, "status"])?;

    let stdout = output.stdout.clone();
    let stderr = output.stderr.clone();

    if !output.success() {
        if stderr.contains("does not exist") || stdout.contains("does not exist") {
            return Ok(None);
        }
        return Err(anyhow::anyhow!("rc-service failed: {}", stderr));
    }

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

    let enabled_output = runner.run("rc-update", &["show", "default"]).ok();

    let enabled_state = enabled_output
        .map(|o| {
            if o.stdout.lines().any(|l| l.trim().starts_with(name)) {
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

/// Package specification for installation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSpec {
    pub name: String,
    pub version: Option<String>,
}

/// APT package manager backend (Debian/Ubuntu)
///
/// All apt/dpkg operations are serialized via a process-wide mutex. This prevents
/// concurrent apt-get invocations (which would fail on the dpkg frontend lock and
/// leave the package manager in a broken state) and ensures that dpkg cleanup
/// (`dpkg --configure -a`) runs atomically before/after every operation.
pub struct AptBackend {
    runner: Arc<dyn CommandRunner>,
}

/// Process-wide mutex serializing all apt/dpkg operations.
/// Only one apt-get or dpkg command runs at a time, ever.
static APT_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

/// Process-wide flag indicating an apt operation is in progress.
/// Set true while inside `run_apt_safe`, false when done. Used by the
/// SIGTERM handler to decide whether to wait before exiting.
static APT_IN_PROGRESS: std::sync::OnceLock<std::sync::atomic::AtomicBool> =
    std::sync::OnceLock::new();

impl AptBackend {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// Create with the default system command runner.
    pub fn with_system_runner() -> Self {
        Self::new(Arc::new(coordinator::SystemCommandRunner))
    }

    fn get_apt_mutex() -> &'static std::sync::Mutex<()> {
        APT_MUTEX.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn get_apt_in_progress() -> &'static std::sync::atomic::AtomicBool {
        APT_IN_PROGRESS.get_or_init(|| std::sync::atomic::AtomicBool::new(false))
    }

    /// Run `dpkg --configure -a` to clean up any half-configured packages from
    /// a prior interrupted transaction (agent crash, OOM kill, SIGKILL, power loss).
    ///
    /// This is called before every apt operation (pre-flight) and after every
    /// apt failure (cleanup). It is the same recovery step a human would run
    /// after a failed `apt upgrade`.
    ///
    /// Returns Ok(()) if dpkg is clean or was successfully cleaned up.
    /// Returns Err if `dpkg --configure -a` itself fails (dpkg is broken beyond
    /// simple cleanup — requires manual intervention).
    fn ensure_dpkg_clean(&self) -> Result<()> {
        tracing::info!("Running dpkg --configure -a (pre-flight cleanup)");
        match self.run_dpkg(&["--configure", "-a"]) {
            Ok(_) => {
                tracing::info!("dpkg --configure -a completed (clean or fixed)");
                Ok(())
            }
            Err(e) => {
                tracing::error!(error = ?e, "dpkg --configure -a failed — dpkg may require manual intervention");
                Err(e.context("dpkg --configure -a failed: package manager is in a broken state that requires manual intervention"))
            }
        }
    }

    /// Run `dpkg --audit` to verify no packages are left half-configured or
    /// unpacked after an apt operation. This catches the known edge case where
    /// apt-get exits 0 but dpkg triggers haven't fully completed (common with
    /// kernel packages and initramfs-tools).
    ///
    /// If audit finds problems, runs `dpkg --configure -a` to attempt cleanup.
    /// Returns Ok(()) if audit is clean (or was cleaned up), Err if problems
    /// remain after cleanup.
    fn verify_dpkg_clean(&self) -> Result<()> {
        let audit_output = self.run_dpkg(&["--audit"])?;
        if audit_output.trim().is_empty() {
            tracing::info!("dpkg --audit is clean (no half-configured packages)");
            return Ok(());
        }

        tracing::warn!(
            audit_output = %audit_output.trim(),
            "dpkg --audit found half-configured packages after apt operation — running dpkg --configure -a"
        );
        self.ensure_dpkg_clean()?;

        let recheck = self.run_dpkg(&["--audit"])?;
        if recheck.trim().is_empty() {
            tracing::info!("dpkg --audit is clean after cleanup");
            Ok(())
        } else {
            tracing::error!(
                audit_output = %recheck.trim(),
                "dpkg --audit still dirty after dpkg --configure -a — packages may be in a broken state"
            );
            Err(anyhow::anyhow!(
                "dpkg --audit shows half-configured packages after operation and cleanup: {}",
                recheck.trim()
            ))
        }
    }

    /// Run apt command with dpkg cleanup and serialization.
    ///
    /// This is the core wrapper for all apt-get operations. It:
    /// 1. Acquires the process-wide mutex (no concurrent apt calls)
    /// 2. Runs `dpkg --configure -a` pre-flight (clean up prior interrupted state)
    /// 3. Runs the apt-get command
    /// 4. On success: runs `dpkg --audit` post-verification
    /// 5. On failure: runs `dpkg --configure -a` cleanup before returning the error
    ///
    /// The mutex is held for the entire duration including pre-flight and cleanup,
    /// ensuring atomicity of the full operation.
    fn run_apt_safe(&self, args: &[&str]) -> Result<String> {
        let _guard = Self::get_apt_mutex()
            .lock()
            .map_err(|e| anyhow::anyhow!("apt mutex poisoned: {}", e))?;

        // Mark operation in progress for SIGTERM handler. The guard ensures
        // the flag is cleared on ALL return paths (including early returns
        // from ensure_dpkg_clean failures, apt-get spawn failures, etc).
        // Without this, a pre-flight failure would leave the flag true forever,
        // causing the SIGTERM handler to wait 25s on every shutdown.
        struct InProgressGuard;
        impl Drop for InProgressGuard {
            fn drop(&mut self) {
                AptBackend::get_apt_in_progress().store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
        Self::get_apt_in_progress().store(true, std::sync::atomic::Ordering::SeqCst);
        let _in_progress_guard = InProgressGuard;

        // Pre-flight: clean up any half-configured state from prior crashes
        self.ensure_dpkg_clean()?;

        // Set DEBIAN_FRONTEND=noninteractive explicitly so apt-get never
        // prompts for user input (conffile conflicts, service restarts, etc).
        // Without this, apt-get can hang forever waiting for input on a TTY-less
        // service, and a subsequent SIGKILL leaves dpkg mid-transaction.
        let program = "apt-get";
        let output = match self.runner.run(program, args) {
            Ok(o) => o,
            Err(e) => {
                let err = e.context("Failed to execute apt command");
                let _ = self.ensure_dpkg_clean();
                return Err(err);
            }
        };

        if !output.success() {
            let err = anyhow::Error::new(CommandError {
                program: program.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
                exit_code: output.status_code,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                spawn_error: None,
            });

            tracing::warn!(error = ?err, "apt-get failed — running dpkg --configure -a cleanup");
            let _ = self.ensure_dpkg_clean();

            return Err(err);
        }

        let verify_result = self.verify_dpkg_clean();

        verify_result?;

        Ok(output.stdout)
    }

    /// Run apt command and capture output.
    ///
    /// On failure, returns a [`CommandError`] (wrapped in anyhow) that preserves the
    /// exit code, stdout, and stderr so the manager receives the same diagnostics the
    /// local journal would show.
    fn run_apt(&self, args: &[&str]) -> Result<String> {
        self.run_apt_safe(args)
    }

    /// Run dpkg command and capture output.
    fn run_dpkg(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "dpkg", args)
    }

    /// Run `apt` (the user-facing CLI, not apt-get) for list/query operations.
    ///
    /// `apt-get` does not support the `list` operation or the `--upgradable` flag.
    /// Those are `apt`-only features. The `apt` tool prints a stability warning to
    /// stderr and a "Listing..." header to stdout, both of which are handled by the
    /// caller. The exit code is 0 on success.
    fn run_apt_cli(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "apt", args)
    }

    /// Run `apt-cache` for query operations like `policy` and `search`.
    ///
    /// `apt-cache` is the scripting-safe tool for package queries. `apt-get` does not
    /// support `policy` — it's an `apt-cache` operation.
    fn run_apt_cache(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "apt-cache", args)
    }

    /// Parse package list from `apt list` output.
    ///
    /// Format: `name/repos version arch [status]`
    /// e.g.: `curl/noble-updates,now 8.5.0-2ubuntu10.10 amd64 [installed]`
    /// The "Listing..." header line from `apt` is skipped.
    fn parse_package_list(&self, output: &str) -> Vec<Package> {
        let mut packages = Vec::new();

        for line in output.lines() {
            // Skip the "Listing..." header that `apt` prints
            if line.starts_with("Listing...") || line.is_empty() {
                continue;
            }
            // Format: name/repos version arch [status]
            // e.g.: curl/noble-updates,now 8.5.0-2ubuntu10.10 amd64 [installed]
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                // Strip repo suffix from package name (e.g., "curl/noble-updates,now" → "curl")
                let name = parts[0].split('/').next().unwrap_or(parts[0]).to_string();
                let version = parts[1].to_string();

                // Determine status from the bracketed annotation (parts[2] is arch, parts[3] is [status])
                let status_str = parts.get(3).unwrap_or(&"");
                let status = if status_str.contains("installed") {
                    PackageStatus::Installed
                } else if status_str.contains("upgradable") {
                    PackageStatus::Upgradable
                } else {
                    PackageStatus::Available
                };

                let upgradable = status == PackageStatus::Upgradable;
                let description = String::new();

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
        // Use `apt list` (not apt-get — apt-get doesn't support the `list` operation)
        let args = match filter {
            Some(f) => vec!["list", f],
            None => vec!["list", "--installed"],
        };

        let output = self.run_apt_cli(&args)?;
        Ok(self.parse_package_list(&output))
    }

    fn get_package(&self, name: &str) -> Result<Option<Package>> {
        // Check if installed
        let dpkg_output = self.run_dpkg(&["-s", name]);

        if dpkg_output.is_err() {
            // Package not installed, check if available
            let list_output = self.run_apt_cli(&["list", name])?;
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
            .run_apt_cli(&["list", "--upgradable", name])
            .map(|o| o.contains(name))
            .unwrap_or(false);

        let latest_version = if upgradable {
            self.run_apt_cache(&["policy", name]).ok().and_then(|o| {
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

        // SECURITY: --allow-unauthenticated bypasses GPG signature verification.
        // Only allow when explicitly requested; log a warning.
        // Note: --force-yes was removed in apt 1.1 (2015). --allow-unauthenticated
        // is the modern equivalent.
        if options.force {
            tracing::warn!(
                "--allow-unauthenticated requested: package signature verification will be bypassed"
            );
            args.push("--allow-unauthenticated".to_string());
        }

        // SECURITY: Insert -- separator before user-supplied package names to prevent
        // argument injection. Without this, a package name like "--allow-unauthenticated"
        // would be interpreted as an apt option rather than a package name.
        args.push("--".to_string());

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
        // SECURITY: -- separator prevents argument injection via package name
        self.run_apt(&["install", "-y", "--only-upgrade", "--", name])?;
        info!("Updated package: {}", name);
        Ok(())
    }

    fn remove_package(&self, name: &str, purge: bool) -> Result<()> {
        // SECURITY: -- separator prevents argument injection via package name
        let args = if purge {
            vec!["purge", "-y", "--", name]
        } else {
            vec!["remove", "-y", "--", name]
        };

        self.run_apt(&args)?;
        info!("Removed package: {} (purge={})", name, purge);
        Ok(())
    }

    fn list_patches(&self) -> Result<Vec<Patch>> {
        // Use `apt list --upgradable` (not apt-get — apt-get doesn't support `list`).
        // The `apt` CLI prints a "Listing..." header line on stdout which we skip.
        let output = self.run_apt_cli(&["list", "--upgradable"])?;
        let mut patches = Vec::new();

        for line in output.lines() {
            // Skip the "Listing..." header that `apt` prints
            if line.starts_with("Listing...") {
                continue;
            }
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
                // SECURITY: -- separator prevents argument injection via package names
                let mut a: Vec<&str> = vec!["install", "-y", "--"];
                for pkg in pkgs {
                    a.push(pkg);
                }
                a
            }
            None => {
                // Run fix-broken first to resolve any unmet dependencies from
                // interrupted package operations. run_apt_safe already runs
                // dpkg --configure -a as pre-flight, but fix-broken handles
                // dependency resolution that dpkg --configure -a alone cannot.
                // If fix-broken fails, return the error — proceeding with
                // dist-upgrade on a broken-dependency system can make things worse.
                match self.run_apt(&["-f", "install", "-y"]) {
                    Ok(_) => info!("apt-get -f install completed (no broken packages or fixed)"),
                    Err(e) => {
                        tracing::error!(error = ?e, "apt-get -f install failed — aborting dist-upgrade to avoid worsening dependency state");
                        return Err(e.context("Pre-upgrade fix-broken failed — resolve dependency issues before retrying"));
                    }
                }
                vec!["dist-upgrade", "-y"]
            }
        };

        match self.run_apt(&args) {
            Ok(_) => {
                info!("Applied patches for packages: {:?}", packages);
                Ok(())
            }
            Err(e) => {
                // run_apt_safe already ran dpkg --configure -a cleanup on failure.
                // If the error looks like a dependency issue, try fix-broken +
                // one retry. For any other error, return immediately — the
                // cleanup has already been done.
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("unmet dependencies")
                    || err_str.contains("broken")
                    || err_str.contains("dependency")
                {
                    tracing::warn!(error = ?e, "dist-upgrade failed with dependency issues — running fix-broken and retrying once");
                    match self.run_apt(&["-f", "install", "-y"]) {
                        Ok(_) => info!("apt-get -f install completed on retry"),
                        Err(fix_err) => {
                            tracing::error!(error = ?fix_err, "apt-get -f install failed on retry — not retrying dist-upgrade");
                            return Err(fix_err.context(
                                "fix-broken failed on retry — dependency issues remain unresolved",
                            ));
                        }
                    }
                    self.run_apt(&args).map(|_| ())
                } else {
                    Err(e)
                }
            }
        }
    }

    fn get_system_info(&self) -> Result<SystemInfo> {
        get_system_info_via_runner(self.runner.as_ref())
    }

    fn reboot_system(&self, delay_seconds: u64) -> Result<()> {
        reboot_system_via_runner(self.runner.as_ref(), delay_seconds)
    }

    fn get_service_status(&self, name: &str) -> Result<Option<ServiceStatus>> {
        validate_service_name(name).map_err(|e| anyhow::anyhow!("{}", e))?;

        let is_systemd = std::path::Path::new("/run/systemd/system").exists();
        let is_openrc = std::path::Path::new("/sbin/openrc").exists();

        if is_systemd {
            get_systemd_service_status_via_runner(self.runner.as_ref(), name)
        } else if is_openrc {
            get_openrc_service_status_via_runner(self.runner.as_ref(), name)
        } else {
            Err(anyhow::anyhow!(
                "No supported init system detected (systemd or OpenRC required)"
            ))
        }
    }

    fn refresh_package_cache(&self, cache_state: &cache::PackageCacheState) -> Result<()> {
        info!("Refreshing APT package cache");
        // Route through run_apt_safe so cache refresh acquires the APT mutex
        // and sets APT_IN_PROGRESS. This prevents concurrent apt-get update
        // and apt-get install from racing on the dpkg frontend lock.
        match self.run_apt(&["update"]) {
            Ok(_) => {
                cache_state.update_success();
                info!("APT package cache refreshed successfully");
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("APT cache refresh failed: {}", format_error_for_cache(&e));
                cache_state.update_failure(err_msg);
                Err(e)
            }
        }
    }

    fn last_cache_update(&self, cache_state: &cache::PackageCacheState) -> Option<DateTime<Utc>> {
        cache_state.status().last_update
    }

    fn is_operation_in_progress(&self) -> bool {
        Self::get_apt_in_progress().load(std::sync::atomic::Ordering::SeqCst)
    }

    fn get_installed_version(&self, name: &str) -> Result<Option<String>> {
        match self.run_dpkg(&["-s", name]) {
            Ok(output) => {
                for line in output.lines() {
                    if line.starts_with("Version:") {
                        let version = line.trim_start_matches("Version:").trim().to_string();
                        if !version.is_empty() {
                            return Ok(Some(version));
                        }
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    fn get_candidate_version(&self, name: &str) -> Result<Option<String>> {
        match self.run_apt_cache(&["policy", name]) {
            Ok(output) => {
                for line in output.lines() {
                    if line.contains("Candidate:") {
                        let version = line.split_whitespace().nth(1).map(|s| s.to_string());
                        return Ok(version);
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }
}

impl Default for AptBackend {
    fn default() -> Self {
        Self::with_system_runner()
    }
}

/// APK package manager backend (Alpine Linux)
pub struct ApkBackend {
    runner: Arc<dyn CommandRunner>,
}

impl ApkBackend {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// Create with the default system command runner.
    pub fn with_system_runner() -> Self {
        Self::new(Arc::new(coordinator::SystemCommandRunner))
    }

    /// Run apk command and capture output.
    fn run_apk(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "apk", args)
    }

    /// Parse name and version from apk package identifier (name-version format).
    /// Alpine package names can contain hyphens (e.g., "gcc-gnat"), so we find
    /// the first hyphen followed by a digit to separate name from version.
    fn parse_name_version(&self, ident: &str) -> (String, String) {
        let bytes = ident.as_bytes();
        for i in 0..bytes.len() {
            if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                return (ident[..i].to_string(), ident[i + 1..].to_string());
            }
        }
        // Fallback: no version separator found
        (ident.to_string(), String::new())
    }

    /// Parse package list from `apk list --installed` output.
    /// Format: {name}-{version} [{repo}] {description}
    fn parse_apk_package_list(&self, output: &str) -> Vec<Package> {
        let mut packages = Vec::new();

        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }

            // Split on " [" to separate package identifier from repo and description
            let (ident, rest) = if let Some(pos) = line.find(" [") {
                (&line[..pos], &line[pos + 2..])
            } else if let Some(pos) = line.find(' ') {
                (&line[..pos], &line[pos + 1..])
            } else {
                // No separator found, treat entire line as identifier
                let (name, version) = self.parse_name_version(line.trim());
                packages.push(Package {
                    name,
                    version,
                    status: PackageStatus::Installed,
                    upgradable: false,
                    latest_version: None,
                    description: String::new(),
                    dependencies: Vec::new(),
                    reverse_dependencies: Vec::new(),
                    install_date: None,
                    size_installed: None,
                });
                continue;
            };

            let (name, version) = self.parse_name_version(ident);

            // Parse rest: "{repo}] {description}" or just "{description}"
            let description = if let Some(bracket_end) = rest.find("] ") {
                rest[bracket_end + 2..].to_string()
            } else if let Some(bracket_end) = rest.find(']') {
                rest[bracket_end + 1..].trim().to_string()
            } else {
                rest.to_string()
            };

            packages.push(Package {
                name,
                version,
                status: PackageStatus::Installed,
                upgradable: false,
                latest_version: None,
                description,
                dependencies: Vec::new(),
                reverse_dependencies: Vec::new(),
                install_date: None,
                size_installed: None,
            });
        }

        packages
    }

    /// Parse detailed package info from `apk info -a {name}` output.
    /// Output format has section headers like:
    ///   {name}-{version} description:
    ///   the description text
    ///   {name}-{version} installed size:
    ///   32768
    fn parse_apk_info(
        &self,
        output: &str,
        name: &str,
        status: PackageStatus,
    ) -> Result<Option<Package>> {
        let mut version = String::new();
        let mut description = String::new();
        let mut dependencies = Vec::new();
        let mut reverse_dependencies = Vec::new();
        let mut size_installed = None;
        let mut current_field: Option<&str> = None;

        for line in output.lines() {
            if line.contains(" description:") {
                current_field = Some("description");
                // Extract version from the header line
                let header = line.split(" description:").next().unwrap_or("");
                let (parsed_name, v) = self.parse_name_version(header.trim());
                if parsed_name == name || version.is_empty() {
                    version = v;
                }
            } else if line.contains(" webpage:") {
                current_field = Some("webpage");
            } else if line.contains(" installed size:") {
                current_field = Some("installed_size");
                // Size might be on the same line after the header
                if let Some(pos) = line.find(" installed size:") {
                    let size_str = line[pos + " installed size:".len()..].trim();
                    if !size_str.is_empty() {
                        size_installed = Some(format!("{} bytes", size_str));
                    }
                }
            } else if line.contains(" dependencies:") {
                current_field = Some("dependencies");
            } else if line.contains(" provides:") {
                current_field = Some("provides");
            } else if line.contains(" required by:") {
                current_field = Some("required_by");
            } else if !line.trim().is_empty() {
                match current_field {
                    Some("description") if description.is_empty() => {
                        description = line.trim().to_string();
                    }
                    Some("dependencies") => {
                        for dep in line.split_whitespace() {
                            // APK dependencies use prefixes like "so:", "cmd:", "pc:" - strip them
                            let dep_name = dep
                                .trim_start_matches("so:")
                                .trim_start_matches("cmd:")
                                .trim_start_matches("pc:");
                            dependencies.push(dep_name.to_string());
                        }
                    }
                    Some("required_by") => {
                        for req in line.split_whitespace() {
                            let (req_name, _) = self.parse_name_version(req);
                            reverse_dependencies.push(req_name);
                        }
                    }
                    Some("installed_size") => {
                        let size_str = line.trim();
                        if !size_str.is_empty() && size_installed.is_none() {
                            size_installed = Some(format!("{} bytes", size_str));
                        }
                    }
                    _ => {}
                }
            } else {
                current_field = None;
            }
        }

        // Check if upgradable
        let upgradable = self
            .run_apk(&["list", "--upgradable", name])
            .map(|o| !o.trim().is_empty() && o.contains(name))
            .unwrap_or(false);

        let latest_version = if upgradable {
            // Try to get the candidate version from apk info
            self.run_apk(&["info", name]).ok().and_then(|o| {
                o.lines().next().and_then(|l| {
                    let (_, v) = self.parse_name_version(l.trim());
                    if v.is_empty() {
                        None
                    } else {
                        Some(v)
                    }
                })
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
            reverse_dependencies,
            install_date: None,
            size_installed,
        }))
    }
}

impl PackageManagerBackend for ApkBackend {
    fn list_packages(&self, filter: Option<&str>) -> Result<Vec<Package>> {
        let args = match filter {
            Some(f) => vec!["list", "--installed", f],
            None => vec!["list", "--installed"],
        };

        let output = self.run_apk(&args)?;
        Ok(self.parse_apk_package_list(&output))
    }

    fn get_package(&self, name: &str) -> Result<Option<Package>> {
        // Validate package name to prevent shell injection
        if name.is_empty() || name.contains('/') || name.contains("..") || name.contains(' ') {
            return Err(anyhow::anyhow!("Invalid package name: {}", name));
        }

        // Check if package is installed using apk list --installed
        let list_output = self.run_apk(&["list", "--installed", name])?;

        if !list_output.trim().is_empty() && list_output.contains(name) {
            // Package is installed, get detailed info
            let info_output = self.run_apk(&["info", "-a", name])?;
            return self.parse_apk_info(&info_output, name, PackageStatus::Installed);
        }

        // Check if package is available (not installed) using apk search
        let search_output = self.run_apk(&["search", name]);
        if let Ok(output) = search_output {
            if !output.trim().is_empty() && output.contains(name) {
                // Parse first matching line
                if let Some(first_line) = output.lines().next() {
                    let (pkg_name, version) = self.parse_name_version(first_line.trim());
                    return Ok(Some(Package {
                        name: pkg_name,
                        version,
                        status: PackageStatus::Available,
                        upgradable: false,
                        latest_version: None,
                        description: String::new(),
                        dependencies: Vec::new(),
                        reverse_dependencies: Vec::new(),
                        install_date: None,
                        size_installed: None,
                    }));
                }
            }
        }

        Ok(None)
    }

    fn install_packages(&self, packages: &[PackageSpec], options: &InstallOptions) -> Result<()> {
        let mut args: Vec<String> = vec!["add".to_string()];

        if options.force {
            args.push("--force".to_string());
        }

        // SECURITY: -- separator prevents argument injection via package names
        args.push("--".to_string());

        for pkg in packages {
            let pkg_arg = if let Some(version) = &pkg.version {
                format!("{}={}", pkg.name, version)
            } else {
                pkg.name.clone()
            };
            args.push(pkg_arg);
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_apk(&args_ref)?;
        info!(
            "Installed packages: {:?}",
            packages.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        Ok(())
    }

    fn update_package(&self, name: &str) -> Result<()> {
        // SECURITY: -- separator prevents argument injection via package name
        self.run_apk(&["upgrade", "--", name])?;
        info!("Updated package: {}", name);
        Ok(())
    }

    fn remove_package(&self, name: &str, _purge: bool) -> Result<()> {
        // APK doesn't have a purge concept - just remove the package
        // SECURITY: -- separator prevents argument injection via package name
        self.run_apk(&["del", "--", name])?;
        info!("Removed package: {}", name);
        Ok(())
    }

    fn list_patches(&self) -> Result<Vec<Patch>> {
        let output = self.run_apk(&["list", "--upgradable"])?;
        let mut patches = Vec::new();

        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }

            // Parse upgradable package line
            // Format: {name}-{new_version} < {old_version} [{repo}] {description}
            // or fallback: {name}-{new_version} [{repo}] {description}
            let (ident, current_version) = if let Some(pos) = line.find(" < ") {
                let ident = &line[..pos];
                let rest = &line[pos + 3..];
                // Old version ends at the next space or bracket
                let cv = if let Some(space_pos) = rest.find(' ') {
                    rest[..space_pos].to_string()
                } else {
                    rest.to_string()
                };
                (ident, cv)
            } else if let Some(pos) = line.find(' ') {
                (&line[..pos], String::new())
            } else {
                continue;
            };

            let (name, available_version) = self.parse_name_version(ident);

            // Determine severity based on package name heuristics
            let severity =
                if name.contains("kernel") || name.contains("ssl") || name.contains("security") {
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

        Ok(patches)
    }

    fn apply_patches(&self, packages: Option<&[String]>) -> Result<()> {
        match packages {
            Some(pkgs) => {
                // SECURITY: -- separator prevents argument injection via package names
                let mut args: Vec<&str> = vec!["upgrade", "--"];
                for pkg in pkgs {
                    args.push(pkg);
                }
                self.run_apk(&args)?;
                info!("Applied patches for packages: {:?}", packages);
            }
            None => {
                self.run_apk(&["upgrade"])?;
                info!("Applied all available patches");
            }
        }
        Ok(())
    }

    fn get_system_info(&self) -> Result<SystemInfo> {
        get_system_info_via_runner(self.runner.as_ref())
    }

    fn reboot_system(&self, delay_seconds: u64) -> Result<()> {
        if delay_seconds > 0 {
            reboot_system_via_runner(self.runner.as_ref(), delay_seconds)
        } else {
            // Alpine uses `reboot` command, not `systemctl reboot`
            info!("Initiating immediate system reboot");
            coordinator::run_command(self.runner.as_ref(), "reboot", &[])?;
            info!("System reboot initiated");
            Ok(())
        }
    }

    fn get_service_status(&self, name: &str) -> Result<Option<ServiceStatus>> {
        validate_service_name(name).map_err(|e| anyhow::anyhow!("{}", e))?;
        get_openrc_service_status_via_runner(self.runner.as_ref(), name)
    }

    fn refresh_package_cache(&self, cache_state: &cache::PackageCacheState) -> Result<()> {
        info!("Refreshing APK package cache");
        match self.run_apk(&["update"]) {
            Ok(_) => {
                cache_state.update_success();
                info!("APK package cache refreshed successfully");
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("APK cache refresh failed: {}", format_error_for_cache(&e));
                cache_state.update_failure(err_msg);
                Err(e)
            }
        }
    }

    fn last_cache_update(&self, cache_state: &cache::PackageCacheState) -> Option<DateTime<Utc>> {
        cache_state.status().last_update
    }

    fn restart_own_service(&self) -> Result<()> {
        // Fire-and-forget: spawn the command, don't wait for it.
        std::process::Command::new("rc-service")
            .args(["linux-patch-api", "restart"])
            .spawn()
            .context("Failed to spawn rc-service restart")?;
        info!("rc-service restart spawned (fire-and-forget)");
        Ok(())
    }

    fn get_installed_version(&self, name: &str) -> Result<Option<String>> {
        match self.run_apk(&["list", "--installed", name]) {
            Ok(output) => {
                for line in output.lines() {
                    if line.contains(name) {
                        let (_, version) = self.parse_name_version(line.trim());
                        if !version.is_empty() {
                            return Ok(Some(version));
                        }
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    fn get_candidate_version(&self, name: &str) -> Result<Option<String>> {
        match self.run_apk(&["info", name]) {
            Ok(output) => {
                if let Some(first_line) = output.lines().next() {
                    let (_, version) = self.parse_name_version(first_line.trim());
                    if !version.is_empty() {
                        return Ok(Some(version));
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }
}

impl Default for ApkBackend {
    fn default() -> Self {
        Self::with_system_runner()
    }
}

/// DNF package manager backend (Fedora/RHEL/CentOS 8+)
pub struct DnfBackend {
    runner: Arc<dyn CommandRunner>,
}

impl DnfBackend {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// Create with the default system command runner.
    pub fn with_system_runner() -> Self {
        Self::new(Arc::new(coordinator::SystemCommandRunner))
    }

    /// Run dnf command and capture output.
    fn run_dnf(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "dnf", args)
    }

    /// Run rpm command and capture output.
    fn run_rpm(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "rpm", args)
    }

    /// Parse name and version from RPM package identifier (name-version-release.arch).
    /// RPM package names can contain hyphens (e.g., "perl-Net-SSLeay"), so we find
    /// the first hyphen followed by a digit to separate name from version, similar to
    /// the APK parsing logic.
    fn parse_rpm_name_version(&self, ident: &str) -> (String, String) {
        let bytes = ident.as_bytes();
        for i in 0..bytes.len() {
            if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                return (ident[..i].to_string(), ident[i + 1..].to_string());
            }
        }
        // Fallback: no version separator found
        (ident.to_string(), String::new())
    }

    /// Parse package list from `rpm -qa` output.
    /// Format: name-version-release.arch
    fn parse_rpm_package_list(&self, output: &str) -> Vec<Package> {
        let mut packages = Vec::new();

        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Strip .arch suffix (e.g., .x86_64, .noarch, .aarch64)
            let without_arch = if let Some(pos) = trimmed.rfind('.') {
                // Verify the suffix looks like an arch (only alphanumeric, no dots)
                let suffix = &trimmed[pos + 1..];
                if suffix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    &trimmed[..pos]
                } else {
                    trimmed
                }
            } else {
                trimmed
            };

            let (name, version) = self.parse_rpm_name_version(without_arch);

            packages.push(Package {
                name,
                version,
                status: PackageStatus::Installed,
                upgradable: false,
                latest_version: None,
                description: String::new(),
                dependencies: Vec::new(),
                reverse_dependencies: Vec::new(),
                install_date: None,
                size_installed: None,
            });
        }

        packages
    }

    /// Parse detailed package info from `rpm -qi <name>` output.
    fn parse_rpm_info(&self, output: &str, name: &str) -> Result<Option<Package>> {
        let mut version = String::new();
        let mut release = String::new();
        let mut description = String::new();
        let mut dependencies = Vec::new();
        let mut size_installed = None;
        let mut install_date = None;
        let mut in_description = false;

        for line in output.lines() {
            // Once we're in the description block, collect lines until we hit another field
            if in_description {
                if line.starts_with(' ') || line.is_empty() {
                    if !description.is_empty() {
                        description.push(' ');
                    }
                    description.push_str(line.trim());
                    continue;
                } else {
                    in_description = false;
                }
            }

            if line.starts_with("Version") {
                version = line.split(':').nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("Release") {
                release = line.split(':').nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("Install Date") {
                install_date = Some(line.split(':').nth(1).unwrap_or("").trim().to_string());
            } else if line.starts_with("Size") {
                size_installed = Some(line.split(':').nth(1).unwrap_or("").trim().to_string());
            } else if line.starts_with("Summary") {
                // Use Summary as the description if no Description block follows
                let summary = line.split(':').nth(1).unwrap_or("").trim().to_string();
                if description.is_empty() {
                    description = summary;
                }
            } else if line.starts_with("Description") {
                in_description = true;
                // Capture any text after "Description :" on the same line
                let rest = line.split(':').nth(1).unwrap_or("").trim().to_string();
                if !rest.is_empty() {
                    description = rest;
                }
            }
        }

        // Combine version and release for full version string
        let full_version = if release.is_empty() {
            version.clone()
        } else {
            format!("{}-{}", version, release)
        };

        // Check if upgradable via dnf check-update
        // dnf check-update returns exit code 100 when updates are available
        let check_output = self.runner.run("dnf", &["check-update", "-q", name]);

        let (upgradable, latest_version) = match check_output {
            Ok(ref o) if o.status_code == Some(100) => {
                // Updates available - try to parse the available version
                let stdout = &o.stdout;
                let candidate = stdout.lines().find_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 && parts[0].starts_with(name) {
                        // Format: name.arch  version-release  repo
                        if let Some(pos) = parts[0].rfind('.') {
                            if &parts[0][..pos] == name {
                                return Some(parts[1].to_string());
                            }
                        }
                    }
                    None
                });
                (true, candidate)
            }
            _ => (false, Some(full_version.clone())),
        };

        // Get dependencies via rpm -qR
        if let Ok(deps_output) = self.run_rpm(&["-qR", name]) {
            for dep in deps_output.lines() {
                let dep = dep.trim();
                if dep.is_empty() {
                    continue;
                }
                // RPM dependencies can be complex: "rpmlib(...) >= value"
                // or simple: "libc.so.6()(64bit)" or "bash >= 4.0"
                // Extract just the base name
                let dep_name = dep.split_whitespace().next().unwrap_or("").to_string();
                // Skip rpmlib and capability-style deps
                if dep_name.starts_with("rpmlib") || dep_name.starts_with("rtld") {
                    continue;
                }
                dependencies.push(dep_name);
            }
        }

        Ok(Some(Package {
            name: name.to_string(),
            version: full_version,
            status: PackageStatus::Installed,
            upgradable,
            latest_version,
            description,
            dependencies,
            reverse_dependencies: Vec::new(),
            install_date,
            size_installed,
        }))
    }
}

impl PackageManagerBackend for DnfBackend {
    fn list_packages(&self, filter: Option<&str>) -> Result<Vec<Package>> {
        let args = match filter {
            Some(f) => vec!["-qa", f],
            None => vec!["-qa"],
        };

        let output = self.run_rpm(&args)?;
        let mut packages = self.parse_rpm_package_list(&output);

        // If a filter was provided, filter the results by name
        if let Some(f) = filter {
            packages.retain(|p| p.name.contains(f));
        }

        Ok(packages)
    }

    fn get_package(&self, name: &str) -> Result<Option<Package>> {
        // Validate package name to prevent shell injection
        if name.is_empty() || name.contains('/') || name.contains("..") || name.contains(' ') {
            return Err(anyhow::anyhow!("Invalid package name: {}", name));
        }

        // Check if package is installed using rpm -q
        let query_result = self.run_rpm(&["-q", name]);

        if query_result.is_err() {
            // Package not installed, check if available via dnf
            let search_output = self.runner.run("dnf", &["info", "-q", name]);

            if let Ok(output) = search_output {
                if output.success() {
                    let stdout = &output.stdout;
                    // Parse available package info from dnf info output
                    let mut version = String::new();
                    let mut release = String::new();
                    let mut description = String::new();

                    for line in stdout.lines() {
                        if line.starts_with("Version") {
                            version = line.split(':').nth(1).unwrap_or("").trim().to_string();
                        } else if line.starts_with("Release") {
                            release = line.split(':').nth(1).unwrap_or("").trim().to_string();
                        } else if line.starts_with("Description") {
                            // Take first line of description
                            let rest = line.split(':').nth(1).unwrap_or("").trim().to_string();
                            if !rest.is_empty() {
                                description = rest;
                            }
                        }
                    }

                    let full_version = if release.is_empty() {
                        version
                    } else {
                        format!("{}-{}", version, release)
                    };

                    return Ok(Some(Package {
                        name: name.to_string(),
                        version: full_version,
                        status: PackageStatus::Available,
                        upgradable: false,
                        latest_version: None,
                        description,
                        dependencies: Vec::new(),
                        reverse_dependencies: Vec::new(),
                        install_date: None,
                        size_installed: None,
                    }));
                }
            }
            return Ok(None);
        }

        // Package is installed, get detailed info
        let info_output = self.run_rpm(&["-qi", name])?;
        self.parse_rpm_info(&info_output, name)
    }

    fn install_packages(&self, packages: &[PackageSpec], options: &InstallOptions) -> Result<()> {
        let mut args: Vec<String> = vec!["install".to_string(), "-y".to_string()];

        if options.no_recommends {
            args.push("--setopt=install_weak_deps=0".to_string());
        }

        if options.force {
            args.push("--allowerasing".to_string());
        }

        // SECURITY: -- separator prevents argument injection via package names
        args.push("--".to_string());

        for pkg in packages {
            let pkg_arg = if let Some(version) = &pkg.version {
                format!("{}-{}", pkg.name, version)
            } else {
                pkg.name.clone()
            };
            args.push(pkg_arg);
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_dnf(&args_ref)?;
        info!(
            "Installed packages: {:?}",
            packages.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        Ok(())
    }

    fn update_package(&self, name: &str) -> Result<()> {
        // SECURITY: -- separator prevents argument injection via package name
        self.run_dnf(&["upgrade", "-y", "--", name])?;
        info!("Updated package: {}", name);
        Ok(())
    }

    fn remove_package(&self, name: &str, purge: bool) -> Result<()> {
        // SECURITY: -- separator prevents argument injection via package name
        let args = if purge {
            vec!["remove", "-y", "--noautoremove", "--", name]
        } else {
            vec!["remove", "-y", "--", name]
        };
        self.run_dnf(&args)?;
        info!("Removed package: {} (purge={})", name, purge);
        Ok(())
    }

    fn list_patches(&self) -> Result<Vec<Patch>> {
        // dnf check-update returns exit code 100 when updates are available,
        // exit code 0 when no updates, and other codes for errors.
        let output = coordinator::run_command_with_acceptable_exit(
            self.runner.as_ref(),
            "dnf",
            &["check-update", "-q"],
            &[0, 100],
        )?;

        let stdout = output;
        let mut patches = Vec::new();

        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Format: name.arch  version-release  repo
            // e.g.: bash.x86_64  5.2.21-2.fc43  updates
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                // Strip .arch suffix from package name
                let full_name = parts[0];
                let name = if let Some(pos) = full_name.rfind('.') {
                    full_name[..pos].to_string()
                } else {
                    full_name.to_string()
                };
                let available_version = parts[1].to_string();
                let repo = parts[2].to_string();

                // Get current installed version via rpm -q
                let current_version = self
                    .run_rpm(&["-q", "--qf", "%{VERSION}-%{RELEASE}", &name])
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

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
                    description: format!("Package update available from {}", repo),
                    cve_ids: Vec::new(),
                    requires_reboot: false,
                });
            }
        }

        Ok(patches)
    }

    fn apply_patches(&self, packages: Option<&[String]>) -> Result<()> {
        match packages {
            Some(pkgs) => {
                // SECURITY: -- separator prevents argument injection via package names
                let mut args: Vec<&str> = vec!["upgrade", "-y", "--"];
                for pkg in pkgs {
                    args.push(pkg);
                }
                self.run_dnf(&args)?;
                info!("Applied patches for packages: {:?}", packages);
            }
            None => {
                self.run_dnf(&["upgrade", "-y"])?;
                info!("Applied all available patches");
            }
        }
        Ok(())
    }

    fn get_system_info(&self) -> Result<SystemInfo> {
        get_system_info_via_runner(self.runner.as_ref())
    }

    fn reboot_system(&self, delay_seconds: u64) -> Result<()> {
        reboot_system_via_runner(self.runner.as_ref(), delay_seconds)
    }

    fn get_service_status(&self, name: &str) -> Result<Option<ServiceStatus>> {
        validate_service_name(name).map_err(|e| anyhow::anyhow!("{}", e))?;
        get_systemd_service_status_via_runner(self.runner.as_ref(), name)
    }

    fn refresh_package_cache(&self, cache_state: &cache::PackageCacheState) -> Result<()> {
        info!("Refreshing DNF package cache");
        // dnf check-update returns exit code 100 when updates are available (not an error).
        match coordinator::run_command_with_acceptable_exit(
            self.runner.as_ref(),
            "dnf",
            &["check-update", "--refresh"],
            &[0, 100],
        ) {
            Ok(_) => {
                cache_state.update_success();
                info!("DNF package cache refreshed successfully");
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("DNF cache refresh failed: {}", format_error_for_cache(&e));
                cache_state.update_failure(err_msg);
                Err(e)
            }
        }
    }

    fn last_cache_update(&self, cache_state: &cache::PackageCacheState) -> Option<DateTime<Utc>> {
        cache_state.status().last_update
    }

    fn get_installed_version(&self, name: &str) -> Result<Option<String>> {
        match self.run_rpm(&["-q", "--qf", "%{VERSION}-%{RELEASE}", name]) {
            Ok(output) => {
                let version = output.trim().to_string();
                if !version.is_empty() {
                    Ok(Some(version))
                } else {
                    Ok(None)
                }
            }
            Err(_) => Ok(None),
        }
    }

    fn get_candidate_version(&self, name: &str) -> Result<Option<String>> {
        let output = self.runner.run("dnf", &["info", "-q", name]);
        match output {
            Ok(o) if o.success() => {
                let stdout = &o.stdout;
                let mut version = String::new();
                let mut release = String::new();
                for line in stdout.lines() {
                    if line.starts_with("Version") {
                        version = line.split(':').nth(1).unwrap_or("").trim().to_string();
                    } else if line.starts_with("Release") {
                        release = line.split(':').nth(1).unwrap_or("").trim().to_string();
                    }
                }
                if !version.is_empty() {
                    if release.is_empty() {
                        Ok(Some(version))
                    } else {
                        Ok(Some(format!("{}-{}", version, release)))
                    }
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}

impl Default for DnfBackend {
    fn default() -> Self {
        Self::with_system_runner()
    }
}

/// YUM package manager backend (RHEL/CentOS 7)
pub struct YumBackend {
    runner: Arc<dyn CommandRunner>,
}

impl YumBackend {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// Create with the default system command runner.
    pub fn with_system_runner() -> Self {
        Self::new(Arc::new(coordinator::SystemCommandRunner))
    }

    /// Run yum command and capture output.
    fn run_yum(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "yum", args)
    }

    /// Run rpm command and capture output.
    fn run_rpm(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "rpm", args)
    }

    /// Parse name and version from RPM package identifier (name-version-release.arch).
    /// Same logic as DnfBackend::parse_rpm_name_version.
    fn parse_rpm_name_version(&self, ident: &str) -> (String, String) {
        let bytes = ident.as_bytes();
        for i in 0..bytes.len() {
            if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                return (ident[..i].to_string(), ident[i + 1..].to_string());
            }
        }
        (ident.to_string(), String::new())
    }

    /// Parse package list from `rpm -qa` output.
    fn parse_rpm_package_list(&self, output: &str) -> Vec<Package> {
        let mut packages = Vec::new();

        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let without_arch = if let Some(pos) = trimmed.rfind('.') {
                let suffix = &trimmed[pos + 1..];
                if suffix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    &trimmed[..pos]
                } else {
                    trimmed
                }
            } else {
                trimmed
            };

            let (name, version) = self.parse_rpm_name_version(without_arch);

            packages.push(Package {
                name,
                version,
                status: PackageStatus::Installed,
                upgradable: false,
                latest_version: None,
                description: String::new(),
                dependencies: Vec::new(),
                reverse_dependencies: Vec::new(),
                install_date: None,
                size_installed: None,
            });
        }

        packages
    }

    /// Parse detailed package info from `rpm -qi <name>` output.
    fn parse_rpm_info(&self, output: &str, name: &str) -> Result<Option<Package>> {
        let mut version = String::new();
        let mut release = String::new();
        let mut description = String::new();
        let mut dependencies = Vec::new();
        let mut size_installed = None;
        let mut install_date = None;
        let mut in_description = false;

        for line in output.lines() {
            if in_description {
                if line.starts_with(' ') || line.is_empty() {
                    if !description.is_empty() {
                        description.push(' ');
                    }
                    description.push_str(line.trim());
                    continue;
                } else {
                    in_description = false;
                }
            }

            if line.starts_with("Version") {
                version = line.split(':').nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("Release") {
                release = line.split(':').nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("Install Date") {
                install_date = Some(line.split(':').nth(1).unwrap_or("").trim().to_string());
            } else if line.starts_with("Size") {
                size_installed = Some(line.split(':').nth(1).unwrap_or("").trim().to_string());
            } else if line.starts_with("Summary") {
                let summary = line.split(':').nth(1).unwrap_or("").trim().to_string();
                if description.is_empty() {
                    description = summary;
                }
            } else if line.starts_with("Description") {
                in_description = true;
                let rest = line.split(':').nth(1).unwrap_or("").trim().to_string();
                if !rest.is_empty() {
                    description = rest;
                }
            }
        }

        let full_version = if release.is_empty() {
            version.clone()
        } else {
            format!("{}-{}", version, release)
        };

        // Check if upgradable via yum check-update
        // yum check-update returns exit code 100 when updates are available
        let check_output = self.runner.run("yum", &["check-update", "-q", name]);

        let (upgradable, latest_version) = match check_output {
            Ok(ref o) if o.status_code == Some(100) => {
                let stdout = &o.stdout;
                let candidate = stdout.lines().find_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 && parts[0].starts_with(name) {
                        if let Some(pos) = parts[0].rfind('.') {
                            if &parts[0][..pos] == name {
                                return Some(parts[1].to_string());
                            }
                        }
                    }
                    None
                });
                (true, candidate)
            }
            _ => (false, Some(full_version.clone())),
        };

        // Get dependencies via rpm -qR
        if let Ok(deps_output) = self.run_rpm(&["-qR", name]) {
            for dep in deps_output.lines() {
                let dep = dep.trim();
                if dep.is_empty() {
                    continue;
                }
                let dep_name = dep.split_whitespace().next().unwrap_or("").to_string();
                if dep_name.starts_with("rpmlib") || dep_name.starts_with("rtld") {
                    continue;
                }
                dependencies.push(dep_name);
            }
        }

        Ok(Some(Package {
            name: name.to_string(),
            version: full_version,
            status: PackageStatus::Installed,
            upgradable,
            latest_version,
            description,
            dependencies,
            reverse_dependencies: Vec::new(),
            install_date,
            size_installed,
        }))
    }
}

impl PackageManagerBackend for YumBackend {
    fn list_packages(&self, filter: Option<&str>) -> Result<Vec<Package>> {
        let args = match filter {
            Some(f) => vec!["-qa", f],
            None => vec!["-qa"],
        };

        let output = self.run_rpm(&args)?;
        let mut packages = self.parse_rpm_package_list(&output);

        if let Some(f) = filter {
            packages.retain(|p| p.name.contains(f));
        }

        Ok(packages)
    }

    fn get_package(&self, name: &str) -> Result<Option<Package>> {
        // Validate package name to prevent shell injection
        if name.is_empty() || name.contains('/') || name.contains("..") || name.contains(' ') {
            return Err(anyhow::anyhow!("Invalid package name: {}", name));
        }

        // Check if package is installed using rpm -q
        let query_result = self.run_rpm(&["-q", name]);

        if query_result.is_err() {
            // Package not installed, check if available via yum
            let search_output = self.runner.run("yum", &["info", "-q", name]);

            if let Ok(output) = search_output {
                if output.success() {
                    let stdout = &output.stdout;
                    let mut version = String::new();
                    let mut release = String::new();
                    let mut description = String::new();

                    for line in stdout.lines() {
                        if line.starts_with("Version") {
                            version = line.split(':').nth(1).unwrap_or("").trim().to_string();
                        } else if line.starts_with("Release") {
                            release = line.split(':').nth(1).unwrap_or("").trim().to_string();
                        } else if line.starts_with("Description") {
                            let rest = line.split(':').nth(1).unwrap_or("").trim().to_string();
                            if !rest.is_empty() {
                                description = rest;
                            }
                        }
                    }

                    let full_version = if release.is_empty() {
                        version
                    } else {
                        format!("{}-{}", version, release)
                    };

                    return Ok(Some(Package {
                        name: name.to_string(),
                        version: full_version,
                        status: PackageStatus::Available,
                        upgradable: false,
                        latest_version: None,
                        description,
                        dependencies: Vec::new(),
                        reverse_dependencies: Vec::new(),
                        install_date: None,
                        size_installed: None,
                    }));
                }
            }
            return Ok(None);
        }

        // Package is installed, get detailed info
        let info_output = self.run_rpm(&["-qi", name])?;
        self.parse_rpm_info(&info_output, name)
    }

    fn install_packages(&self, packages: &[PackageSpec], options: &InstallOptions) -> Result<()> {
        let mut args: Vec<String> = vec!["install".to_string(), "-y".to_string()];

        if options.no_recommends {
            // YUM (CentOS 7) doesn't support --setopt=install_weak_deps=0 (that's dnf-only).
            // The closest yum equivalent is --setopt=group_package_types=mandatory, but
            // that only affects group installs. For individual package installs, yum
            // doesn't have a weak-deps concept, so we skip this option.
        }

        // yum doesn't have --allowerasing, skip force option

        // SECURITY: -- separator prevents argument injection via package names
        args.push("--".to_string());

        for pkg in packages {
            let pkg_arg = if let Some(version) = &pkg.version {
                format!("{}-{}", pkg.name, version)
            } else {
                pkg.name.clone()
            };
            args.push(pkg_arg);
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_yum(&args_ref)?;
        info!(
            "Installed packages: {:?}",
            packages.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        Ok(())
    }

    fn update_package(&self, name: &str) -> Result<()> {
        // SECURITY: -- separator prevents argument injection via package name
        self.run_yum(&["update", "-y", "--", name])?;
        info!("Updated package: {}", name);
        Ok(())
    }

    fn remove_package(&self, name: &str, purge: bool) -> Result<()> {
        // yum doesn't distinguish between remove and purge
        let _ = purge;
        // SECURITY: -- separator prevents argument injection via package name
        self.run_yum(&["remove", "-y", "--", name])?;
        info!("Removed package: {} (purge={})", name, purge);
        Ok(())
    }

    fn list_patches(&self) -> Result<Vec<Patch>> {
        let stdout = coordinator::run_command_with_acceptable_exit(
            self.runner.as_ref(),
            "yum",
            &["check-update", "-q"],
            &[0, 100],
        )?;
        let mut patches = Vec::new();

        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                let full_name = parts[0];
                let name = if let Some(pos) = full_name.rfind('.') {
                    full_name[..pos].to_string()
                } else {
                    full_name.to_string()
                };
                let available_version = parts[1].to_string();
                let repo = parts[2].to_string();

                let current_version = self
                    .run_rpm(&["-q", "--qf", "%{VERSION}-%{RELEASE}", &name])
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

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
                    description: format!("Package update available from {}", repo),
                    cve_ids: Vec::new(),
                    requires_reboot: false,
                });
            }
        }

        Ok(patches)
    }

    fn apply_patches(&self, packages: Option<&[String]>) -> Result<()> {
        match packages {
            Some(pkgs) => {
                // SECURITY: -- separator prevents argument injection via package names
                let mut args: Vec<&str> = vec!["update", "-y", "--"];
                for pkg in pkgs {
                    args.push(pkg);
                }
                self.run_yum(&args)?;
                info!("Applied patches for packages: {:?}", packages);
            }
            None => {
                self.run_yum(&["update", "-y"])?;
                info!("Applied all available patches");
            }
        }
        Ok(())
    }

    fn get_system_info(&self) -> Result<SystemInfo> {
        get_system_info_via_runner(self.runner.as_ref())
    }

    fn reboot_system(&self, delay_seconds: u64) -> Result<()> {
        reboot_system_via_runner(self.runner.as_ref(), delay_seconds)
    }

    fn get_service_status(&self, name: &str) -> Result<Option<ServiceStatus>> {
        validate_service_name(name).map_err(|e| anyhow::anyhow!("{}", e))?;
        get_systemd_service_status_via_runner(self.runner.as_ref(), name)
    }

    fn refresh_package_cache(&self, cache_state: &cache::PackageCacheState) -> Result<()> {
        info!("Refreshing YUM package cache");
        match self.run_yum(&["makecache"]) {
            Ok(_) => {
                cache_state.update_success();
                info!("YUM package cache refreshed successfully");
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("YUM cache refresh failed: {}", format_error_for_cache(&e));
                cache_state.update_failure(err_msg);
                Err(e)
            }
        }
    }

    fn last_cache_update(&self, cache_state: &cache::PackageCacheState) -> Option<DateTime<Utc>> {
        cache_state.status().last_update
    }

    fn get_installed_version(&self, name: &str) -> Result<Option<String>> {
        match self.run_rpm(&["-q", "--qf", "%{VERSION}-%{RELEASE}", name]) {
            Ok(output) => {
                let version = output.trim().to_string();
                if !version.is_empty() {
                    Ok(Some(version))
                } else {
                    Ok(None)
                }
            }
            Err(_) => Ok(None),
        }
    }

    fn get_candidate_version(&self, name: &str) -> Result<Option<String>> {
        let output = self.runner.run("yum", &["info", "-q", name]);
        match output {
            Ok(o) if o.success() => {
                let stdout = &o.stdout;
                let mut version = String::new();
                let mut release = String::new();
                for line in stdout.lines() {
                    if line.starts_with("Version") {
                        version = line.split(':').nth(1).unwrap_or("").trim().to_string();
                    } else if line.starts_with("Release") {
                        release = line.split(':').nth(1).unwrap_or("").trim().to_string();
                    }
                }
                if !version.is_empty() {
                    if release.is_empty() {
                        Ok(Some(version))
                    } else {
                        Ok(Some(format!("{}-{}", version, release)))
                    }
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}

impl Default for YumBackend {
    fn default() -> Self {
        Self::with_system_runner()
    }
}

/// Pacman package manager backend (Arch Linux)
pub struct PacmanBackend {
    runner: Arc<dyn CommandRunner>,
}

impl PacmanBackend {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// Create with the default system command runner.
    pub fn with_system_runner() -> Self {
        Self::new(Arc::new(coordinator::SystemCommandRunner))
    }

    /// Run pacman command and capture output.
    fn run_pacman(&self, args: &[&str]) -> Result<String> {
        coordinator::run_command(self.runner.as_ref(), "pacman", args)
    }

    /// Parse package list from `pacman -Q` output.
    /// Format: name version (space separated, one per line)
    fn parse_pacman_package_list(&self, output: &str) -> Vec<Package> {
        let mut packages = Vec::new();

        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // pacman -Q format: "name version"
            let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
            if parts.len() >= 2 {
                let name = parts[0].to_string();
                let version = parts[1].to_string();

                packages.push(Package {
                    name,
                    version,
                    status: PackageStatus::Installed,
                    upgradable: false,
                    latest_version: None,
                    description: String::new(),
                    dependencies: Vec::new(),
                    reverse_dependencies: Vec::new(),
                    install_date: None,
                    size_installed: None,
                });
            }
        }

        packages
    }

    /// Parse detailed package info from `pacman -Qi <name>` output.
    /// Format: multiline with field names like Name:, Version:, Description:, etc.
    fn parse_pacman_info(&self, output: &str, name: &str) -> Result<Option<Package>> {
        let mut version = String::new();
        let mut description = String::new();
        let mut dependencies = Vec::new();
        let mut reverse_dependencies = Vec::new();
        let mut install_date = None;
        let mut size_installed = None;

        for line in output.lines() {
            // pacman -Qi output has format: "Field      : value"
            // with potential continuation lines indented
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                let value = value.trim();

                match key {
                    "Version" => version = value.to_string(),
                    "Description" => description = value.to_string(),
                    "Installed Size" => size_installed = Some(value.to_string()),
                    "Install Date" => install_date = Some(value.to_string()),
                    "Depends On" if !value.is_empty() && value != "None" => {
                        for dep in value.split_whitespace() {
                            dependencies.push(dep.to_string());
                        }
                    }
                    "Required By" if !value.is_empty() && value != "None" => {
                        for req in value.split_whitespace() {
                            reverse_dependencies.push(req.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }

        // Check if upgradable via pacman -Qu
        let upgradable = self
            .run_pacman(&["-Qu", name])
            .map(|o| !o.trim().is_empty())
            .unwrap_or(false);

        let latest_version = if upgradable {
            // Try to get the new version from pacman -Qu output
            self.run_pacman(&["-Qu", name]).ok().and_then(|o| {
                o.lines().find_map(|line| {
                    // Format: name old_version -> new_version
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 4 && parts[0] == name {
                        Some(parts[3].to_string())
                    } else {
                        None
                    }
                })
            })
        } else {
            Some(version.clone())
        };

        Ok(Some(Package {
            name: name.to_string(),
            version,
            status: PackageStatus::Installed,
            upgradable,
            latest_version,
            description,
            dependencies,
            reverse_dependencies,
            install_date,
            size_installed,
        }))
    }
}

impl PackageManagerBackend for PacmanBackend {
    fn list_packages(&self, filter: Option<&str>) -> Result<Vec<Package>> {
        let args = match filter {
            Some(f) => vec!["-Q", f],
            None => vec!["-Q"],
        };

        let output = self.run_pacman(&args)?;
        let mut packages = self.parse_pacman_package_list(&output);

        // If a filter was provided, filter the results by name
        if let Some(f) = filter {
            packages.retain(|p| p.name.contains(f));
        }

        Ok(packages)
    }

    fn get_package(&self, name: &str) -> Result<Option<Package>> {
        // Validate package name to prevent shell injection
        if name.is_empty() || name.contains('/') || name.contains("..") || name.contains(' ') {
            return Err(anyhow::anyhow!("Invalid package name: {}", name));
        }

        // Check if package is installed using pacman -Q
        let query_result = self.run_pacman(&["-Q", name]);

        if query_result.is_err() {
            // Package not installed, check if available via pacman -Si
            let search_output = self.runner.run("pacman", &["-Si", name]);

            if let Ok(output) = search_output {
                if output.success() {
                    let stdout = &output.stdout;
                    let mut version = String::new();
                    let mut description = String::new();

                    for line in stdout.lines() {
                        if let Some((key, value)) = line.split_once(':') {
                            let key = key.trim();
                            let value = value.trim();
                            match key {
                                "Version" => version = value.to_string(),
                                "Description" => description = value.to_string(),
                                _ => {}
                            }
                        }
                    }

                    return Ok(Some(Package {
                        name: name.to_string(),
                        version,
                        status: PackageStatus::Available,
                        upgradable: false,
                        latest_version: None,
                        description,
                        dependencies: Vec::new(),
                        reverse_dependencies: Vec::new(),
                        install_date: None,
                        size_installed: None,
                    }));
                }
            }
            return Ok(None);
        }

        // Package is installed, get detailed info
        let info_output = self.run_pacman(&["-Qi", name])?;
        self.parse_pacman_info(&info_output, name)
    }

    fn install_packages(&self, packages: &[PackageSpec], options: &InstallOptions) -> Result<()> {
        let mut args: Vec<String> = vec![
            "-S".to_string(),
            "--noconfirm".to_string(),
            "--needed".to_string(),
        ];

        if options.force {
            args.push("--overwrite".to_string());
            args.push("*".to_string());
        }

        // SECURITY: -- separator prevents argument injection via package names
        args.push("--".to_string());

        for pkg in packages {
            args.push(pkg.name.clone());
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_pacman(&args_ref)?;
        info!(
            "Installed packages: {:?}",
            packages.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        Ok(())
    }

    fn update_package(&self, name: &str) -> Result<()> {
        // SECURITY: -- separator prevents argument injection via package name
        self.run_pacman(&["-S", "--noconfirm", "--", name])?;
        info!("Updated package: {}", name);
        Ok(())
    }

    fn remove_package(&self, name: &str, _purge: bool) -> Result<()> {
        // pacman doesn't have a purge concept - just remove the package
        // SECURITY: -- separator prevents argument injection via package name
        self.run_pacman(&["-R", "--noconfirm", "--", name])?;
        info!("Removed package: {} (purge={})", name, _purge);
        Ok(())
    }

    fn list_patches(&self) -> Result<Vec<Patch>> {
        let output = self.run_pacman(&["-Qu"])?;
        let mut patches = Vec::new();

        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // pacman -Qu format: name old_version -> new_version
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 4 && parts[2] == "->" {
                let name = parts[0].to_string();
                let current_version = parts[1].to_string();
                let available_version = parts[3].to_string();

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
        match packages {
            Some(pkgs) => {
                // SECURITY: -- separator prevents argument injection via package names
                let mut args: Vec<&str> = vec!["-S", "--noconfirm", "--needed", "--"];
                for pkg in pkgs {
                    args.push(pkg);
                }
                self.run_pacman(&args)?;
                info!("Applied patches for packages: {:?}", packages);
            }
            None => {
                self.run_pacman(&["-Syu", "--noconfirm"])?;
                info!("Applied all available patches");
            }
        }
        Ok(())
    }

    fn get_system_info(&self) -> Result<SystemInfo> {
        get_system_info_via_runner(self.runner.as_ref())
    }

    fn reboot_system(&self, delay_seconds: u64) -> Result<()> {
        reboot_system_via_runner(self.runner.as_ref(), delay_seconds)
    }

    fn get_service_status(&self, name: &str) -> Result<Option<ServiceStatus>> {
        validate_service_name(name).map_err(|e| anyhow::anyhow!("{}", e))?;
        get_systemd_service_status_via_runner(self.runner.as_ref(), name)
    }

    fn refresh_package_cache(&self, cache_state: &cache::PackageCacheState) -> Result<()> {
        info!("Refreshing Pacman package cache");
        match self.run_pacman(&["-Sy"]) {
            Ok(_) => {
                cache_state.update_success();
                info!("Pacman package cache refreshed successfully");
                Ok(())
            }
            Err(e) => {
                let err_msg = format!(
                    "Pacman cache refresh failed: {}",
                    format_error_for_cache(&e)
                );
                cache_state.update_failure(err_msg);
                Err(e)
            }
        }
    }

    fn last_cache_update(&self, cache_state: &cache::PackageCacheState) -> Option<DateTime<Utc>> {
        cache_state.status().last_update
    }

    fn get_installed_version(&self, name: &str) -> Result<Option<String>> {
        match self.run_pacman(&["-Q", name]) {
            Ok(output) => {
                let line = output.lines().next().unwrap_or("").trim();
                let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
                if parts.len() >= 2 {
                    Ok(Some(parts[1].to_string()))
                } else {
                    Ok(None)
                }
            }
            Err(_) => Ok(None),
        }
    }

    fn get_candidate_version(&self, name: &str) -> Result<Option<String>> {
        let output = self.runner.run("pacman", &["-Si", name]);
        match output {
            Ok(o) if o.success() => {
                let stdout = &o.stdout;
                for line in stdout.lines() {
                    if let Some((key, value)) = line.split_once(':') {
                        if key.trim() == "Version" {
                            let version = value.trim().to_string();
                            if !version.is_empty() {
                                return Ok(Some(version));
                            }
                        }
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

impl Default for PacmanBackend {
    fn default() -> Self {
        Self::with_system_runner()
    }
}

/// Package manager factory
pub fn create_backend() -> Result<Box<dyn PackageManagerBackend>> {
    let runner: Arc<dyn CommandRunner> = Arc::new(coordinator::SystemCommandRunner);
    create_backend_with_runner(runner)
}

/// Package manager factory with injected command runner (for testing).
pub fn create_backend_with_runner(
    runner: Arc<dyn CommandRunner>,
) -> Result<Box<dyn PackageManagerBackend>> {
    if std::path::Path::new("/usr/bin/apt").exists() {
        Ok(Box::new(AptBackend::new(runner)))
    } else if std::path::Path::new("/usr/bin/dnf").exists() {
        Ok(Box::new(DnfBackend::new(runner)))
    } else if std::path::Path::new("/usr/bin/yum").exists() {
        Ok(Box::new(YumBackend::new(runner)))
    } else if std::path::Path::new("/usr/bin/apk").exists()
        || std::path::Path::new("/sbin/apk").exists()
    {
        Ok(Box::new(ApkBackend::new(runner)))
    } else if std::path::Path::new("/usr/bin/pacman").exists() {
        Ok(Box::new(PacmanBackend::new(runner)))
    } else {
        Err(anyhow::anyhow!("No supported package manager found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apt_backend_creation() {
        let _backend = AptBackend::with_system_runner();
        // Test passes if backend creation doesn't panic
    }

    #[test]
    fn test_package_status_serialization() {
        let status = PackageStatus::Installed;
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("Installed"));
    }

    #[test]
    fn test_apk_backend_creation() {
        let _backend = ApkBackend::with_system_runner();
        // Test passes if backend creation doesn't panic
    }

    #[test]
    fn test_apk_parse_name_version_simple() {
        let backend = ApkBackend::with_system_runner();
        let (name, version) = backend.parse_name_version("bash-5.2.21-r0");
        assert_eq!(name, "bash");
        assert_eq!(version, "5.2.21-r0");
    }

    #[test]
    fn test_apk_parse_name_version_hyphenated() {
        let backend = ApkBackend::with_system_runner();
        // Package names with hyphens like gcc-gnat
        let (name, version) = backend.parse_name_version("gcc-gnat-13.2.1-r0");
        assert_eq!(name, "gcc-gnat");
        assert_eq!(version, "13.2.1-r0");
    }

    #[test]
    fn test_apk_parse_name_version_no_version() {
        let backend = ApkBackend::with_system_runner();
        let (name, version) = backend.parse_name_version("nohyphen");
        assert_eq!(name, "nohyphen");
        assert_eq!(version, "");
    }

    #[test]
    fn test_apk_parse_name_version_multiple_hyphens() {
        let backend = ApkBackend::with_system_runner();
        let (name, version) = backend.parse_name_version("perl-net-ssleay-1.94-r0");
        assert_eq!(name, "perl-net-ssleay");
        assert_eq!(version, "1.94-r0");
    }

    #[test]
    fn test_apk_parse_package_list() {
        let backend = ApkBackend::with_system_runner();
        let output = "bash-5.2.21-r0 [main] The GNU Bourne Again shell\nopenssl-3.1.4-r0 [main] Toolkit for SSL/TLS";
        let packages = backend.parse_apk_package_list(output);
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "bash");
        assert_eq!(packages[0].version, "5.2.21-r0");
        assert_eq!(packages[1].name, "openssl");
        assert_eq!(packages[1].version, "3.1.4-r0");
    }

    // DNF backend tests

    #[test]
    fn test_dnf_backend_creation() {
        let _backend = DnfBackend::with_system_runner();
        // Test passes if backend creation doesn't panic
    }

    #[test]
    fn test_dnf_parse_rpm_name_version_simple() {
        let backend = DnfBackend::with_system_runner();
        let (name, version) = backend.parse_rpm_name_version("bash-5.2.21-1.fc43");
        assert_eq!(name, "bash");
        assert_eq!(version, "5.2.21-1.fc43");
    }

    #[test]
    fn test_dnf_parse_rpm_name_version_hyphenated() {
        let backend = DnfBackend::with_system_runner();
        // Package names with hyphens like perl-Net-SSLeay
        let (name, version) = backend.parse_rpm_name_version("perl-Net-SSLeay-1.94-1.fc43");
        assert_eq!(name, "perl-Net-SSLeay");
        assert_eq!(version, "1.94-1.fc43");
    }

    #[test]
    fn test_dnf_parse_rpm_name_version_no_version() {
        let backend = DnfBackend::with_system_runner();
        let (name, version) = backend.parse_rpm_name_version("nohyphen");
        assert_eq!(name, "nohyphen");
        assert_eq!(version, "");
    }

    #[test]
    fn test_dnf_parse_rpm_package_list() {
        let backend = DnfBackend::with_system_runner();
        let output =
            "bash-5.2.21-1.fc43.x86_64\nopenssl-3.1.4-1.fc43.x86_64\ncurl-8.6.0-1.fc43.noarch";
        let packages = backend.parse_rpm_package_list(output);
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].name, "bash");
        assert_eq!(packages[0].version, "5.2.21-1.fc43");
        assert_eq!(packages[1].name, "openssl");
        assert_eq!(packages[1].version, "3.1.4-1.fc43");
        assert_eq!(packages[2].name, "curl");
        assert_eq!(packages[2].version, "8.6.0-1.fc43");
    }

    // YUM backend tests

    #[test]
    fn test_yum_backend_creation() {
        let _backend = YumBackend::with_system_runner();
        // Test passes if backend creation doesn't panic
    }

    #[test]
    fn test_yum_parse_rpm_name_version_simple() {
        let backend = YumBackend::with_system_runner();
        let (name, version) = backend.parse_rpm_name_version("bash-4.2.46-34.el7");
        assert_eq!(name, "bash");
        assert_eq!(version, "4.2.46-34.el7");
    }

    #[test]
    fn test_yum_parse_rpm_name_version_hyphenated() {
        let backend = YumBackend::with_system_runner();
        let (name, version) = backend.parse_rpm_name_version("perl-Net-SSLeay-1.94-1.el7");
        assert_eq!(name, "perl-Net-SSLeay");
        assert_eq!(version, "1.94-1.el7");
    }

    #[test]
    fn test_yum_parse_rpm_package_list() {
        let backend = YumBackend::with_system_runner();
        let output = "bash-4.2.46-34.el7.x86_64\nopenssl-1.0.2k-25.el7.x86_64";
        let packages = backend.parse_rpm_package_list(output);
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "bash");
        assert_eq!(packages[0].version, "4.2.46-34.el7");
        assert_eq!(packages[1].name, "openssl");
        assert_eq!(packages[1].version, "1.0.2k-25.el7");
    }

    // Pacman backend tests

    #[test]
    fn test_pacman_backend_creation() {
        let _backend = PacmanBackend::with_system_runner();
        // Test passes if backend creation doesn't panic
    }

    #[test]
    fn test_pacman_parse_package_list() {
        let backend = PacmanBackend::with_system_runner();
        let output = "bash 5.2.21-1\nopenssl 3.1.4-1\ncurl 8.6.0-1";
        let packages = backend.parse_pacman_package_list(output);
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].name, "bash");
        assert_eq!(packages[0].version, "5.2.21-1");
        assert_eq!(packages[1].name, "openssl");
        assert_eq!(packages[1].version, "3.1.4-1");
        assert_eq!(packages[2].name, "curl");
        assert_eq!(packages[2].version, "8.6.0-1");
    }

    #[test]
    fn test_pacman_parse_package_list_empty() {
        let backend = PacmanBackend::with_system_runner();
        let output = "";
        let packages = backend.parse_pacman_package_list(output);
        assert!(packages.is_empty());
    }

    #[test]
    fn test_pacman_parse_info() {
        let backend = PacmanBackend::with_system_runner();
        let output = "Name            : bash\nVersion         : 5.2.21-1\nDescription     : The GNU Bourne Again shell\nInstalled Size  : 12.50 MiB\nDepends On      : readline  glibc  ncurses\nRequired By     : none\nInstall Date    : Mon 20 May 2026 10:00:00 AM CDT";
        let result = backend.parse_pacman_info(output, "bash").unwrap();
        assert!(result.is_some());
        let pkg = result.unwrap();
        assert_eq!(pkg.name, "bash");
        assert_eq!(pkg.version, "5.2.21-1");
        assert_eq!(pkg.description, "The GNU Bourne Again shell");
        assert_eq!(pkg.size_installed, Some("12.50 MiB".to_string()));
        assert_eq!(pkg.dependencies.len(), 3);
        assert!(pkg.dependencies.contains(&"readline".to_string()));
    }

    // --- Validation function tests (Issue #10: Argument injection RCE prevention) ---

    #[test]
    fn test_validate_package_name_valid() {
        assert!(validate_package_name("bash").is_ok());
        assert!(validate_package_name("libssl1.1").is_ok());
        assert!(validate_package_name("python3-pip").is_ok());
        assert!(validate_package_name("g++-11").is_ok());
        assert!(validate_package_name("nginx-common").is_ok());
        assert!(validate_package_name("curl").is_ok());
        assert!(validate_package_name("lib32-glibc").is_ok());
        assert!(validate_package_name("a").is_ok());
    }

    #[test]
    fn test_validate_package_name_empty() {
        assert!(validate_package_name("").is_err());
    }

    #[test]
    fn test_validate_package_name_too_long() {
        let long_name = "a".repeat(257);
        assert!(validate_package_name(&long_name).is_err());
        // Exactly 256 chars should be fine
        let max_name = "a".repeat(256);
        assert!(validate_package_name(&max_name).is_ok());
    }

    #[test]
    fn test_validate_package_name_leading_hyphen() {
        // Leading hyphen could be interpreted as a command-line option
        assert!(validate_package_name("-evil").is_err());
        assert!(validate_package_name("--allow-unauthenticated").is_err());
    }

    #[test]
    fn test_validate_package_name_shell_metacharacters() {
        // Shell metacharacters that could enable injection
        assert!(validate_package_name("pkg;rm -rf").is_err());
        assert!(validate_package_name("pkg$(cmd)").is_err());
        assert!(validate_package_name("pkg`cmd`").is_err());
        assert!(validate_package_name("pkg|other").is_err());
        assert!(validate_package_name("pkg&other").is_err());
        assert!(validate_package_name("pkg>file").is_err());
        assert!(validate_package_name("pkg<file").is_err());
        assert!(validate_package_name("pkg'other").is_err());
        assert!(validate_package_name("pkg\"other").is_err());
        assert!(validate_package_name("pkg!other").is_err());
    }

    #[test]
    fn test_validate_package_name_path_separators() {
        assert!(validate_package_name("/usr/bin/evil").is_err());
        assert!(validate_package_name("..\\..\\evil").is_err());
        assert!(validate_package_name("../evil").is_err());
    }

    #[test]
    fn test_validate_package_name_whitespace() {
        assert!(validate_package_name("pkg name").is_err());
        assert!(validate_package_name("pkg\tname").is_err());
        assert!(validate_package_name("pkg\nname").is_err());
    }

    #[test]
    fn test_validate_package_name_leading_digit() {
        // Digits are valid start characters
        assert!(validate_package_name("3ddesktop").is_ok());
        assert!(validate_package_name("0ad").is_ok());
    }

    #[test]
    fn test_validate_version_string_valid() {
        assert!(validate_version_string("1.2.3").is_ok());
        assert!(validate_version_string("1.2.3-r0").is_ok());
        assert!(validate_version_string("5.2.21-1.fc43").is_ok());
        assert!(validate_version_string("2:1.0-1").is_ok()); // RPM epoch
        assert!(validate_version_string("1.0~beta1").is_ok()); // Debian tilde
        assert!(validate_version_string("1.0+dfsg-1").is_ok()); // Debian suffix
    }

    #[test]
    fn test_validate_version_string_empty() {
        assert!(validate_version_string("").is_err());
    }

    #[test]
    fn test_validate_version_string_leading_hyphen() {
        assert!(validate_version_string("-1.0").is_err());
    }

    #[test]
    fn test_validate_version_string_shell_metacharacters() {
        assert!(validate_version_string("1.0;cmd").is_err());
        assert!(validate_version_string("1.0$(cmd)").is_err());
        assert!(validate_version_string("1.0`cmd`").is_err());
        assert!(validate_version_string("1.0|cmd").is_err());
        assert!(validate_version_string("1.0&cmd").is_err());
        assert!(validate_version_string("1.0 cmd").is_err());
    }

    #[test]
    fn test_validate_version_string_path_separators() {
        assert!(validate_version_string("1.0/evil").is_err());
        assert!(validate_version_string("1.0\\evil").is_err());
    }

    #[test]
    fn test_validate_service_name_valid() {
        assert!(validate_service_name("sshd").is_ok());
        assert!(validate_service_name("nginx.service").is_ok());
        assert!(validate_service_name("ssh@host").is_ok());
        assert!(validate_service_name("docker").is_ok());
        assert!(validate_service_name("networkd-dispatcher").is_ok());
    }

    #[test]
    fn test_validate_service_name_empty() {
        assert!(validate_service_name("").is_err());
    }

    #[test]
    fn test_validate_service_name_leading_hyphen() {
        assert!(validate_service_name("-evil").is_err());
        assert!(validate_service_name("--help").is_err());
    }

    #[test]
    fn test_validate_service_name_path_separators() {
        assert!(validate_service_name("/usr/bin/evil").is_err());
        assert!(validate_service_name("../evil").is_err());
    }

    #[test]
    fn test_validate_service_name_shell_metacharacters() {
        assert!(validate_service_name("svc;rm").is_err());
        assert!(validate_service_name("svc$(cmd)").is_err());
        assert!(validate_service_name("svc|other").is_err());
        assert!(validate_service_name("svc&other").is_err());
        assert!(validate_service_name("svc name").is_err());
    }

    #[test]
    fn test_validate_service_name_with_hyphen() {
        // Hyphens ARE allowed in service names (common in systemd unit names like networkd-dispatcher)
        assert!(validate_service_name("networkd-dispatcher").is_ok());
        // But leading hyphens are NOT allowed (would be interpreted as command flags)
        assert!(validate_service_name("-evil").is_err());
    }

    #[test]
    fn test_validate_service_name_with_plus() {
        // Plus is allowed in service names
        assert!(validate_service_name("cups+daemon").is_ok());
    }
}
