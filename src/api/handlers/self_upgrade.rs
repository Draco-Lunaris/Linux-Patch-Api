//! Self-Upgrade Handler Module
//!
//! Provides utilities for the detached self-upgrade mechanism:
//! - Shell script template generation for cross-distro package installation
//! - Lock file management to prevent concurrent installs
//! - Status file reading for reporting install results on next startup
//!
//! The core problem this solves: when the API installs its own package,
//! the package's pre-remove script stops the API service, killing the
//! in-process installation. By forking a detached shell script, the
//! API returns 202 Accepted immediately and the script completes
//! the installation independently.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

/// Lock file staleness threshold in seconds (30 minutes).
const LOCK_STALE_THRESHOLD_SECS: u64 = 1800;

/// Generate the shell script content for a detached package installation.
///
/// The script:
/// 1. Writes "installing" status to the status file
/// 2. Stops the service (if running) using pkill
/// 3. Installs the package using the appropriate package manager
/// 4. Writes success/failure status to the status file
/// 5. Removes the lock file
/// 6. Attempts to start the service (safety net; post-install scripts should do this)
///
/// The script is designed to work on all supported distros (Debian/Ubuntu,
/// Fedora/RHEL/AlmaLinux, Alpine, Arch) and does NOT rely on systemctl.
pub fn generate_install_script(
    staged_package_path: &str,
    package_type: &str,
    job_id: &str,
    state_dir: &str,
) -> String {
    let status_file = format!("{}/install.status", state_dir);
    let lock_file = format!("{}/install.lock", state_dir);

    // Determine the install command based on package type
    let install_cmd = match package_type {
        "deb" => {
            r#"DEBIAN_FRONTEND=noninteractive apt-get install -y -- "$PACKAGE_PATH""#.to_string()
        }
        "rpm" => r#"if command -v dnf >/dev/null 2>&1; then
        dnf install -y -- "$PACKAGE_PATH"
    else
        yum install -y -- "$PACKAGE_PATH"
    fi"#
        .to_string(),
        "apk" => r#"apk add --allow-untrusted -- "$PACKAGE_PATH""#.to_string(),
        "pkg" => r#"pacman -U --noconfirm -- "$PACKAGE_PATH""#.to_string(),
        _ => {
            let msg = format!("ERROR: Unknown package type: {}", package_type);
            format!("echo \"{}\"; exit 1", msg)
        }
    };

    format!(
        "#!/bin/sh\n\
# Self-upgrade installation script\n\
# Job ID: {job_id}\n\
# Package: {staged_package_path}\n\
# Package type: {package_type}\n\
#\n\
# This script is forked as a detached process by the API.\n\
# It handles stopping the service, installing the package,\n\
# and writing status for the API to read on next startup.\n\
\n\
set -e\n\
\n\
PACKAGE_PATH=\"{staged_package_path}\"\n\
PACKAGE_TYPE=\"{package_type}\"\n\
JOB_ID=\"{job_id}\"\n\
STATUS_FILE=\"{status_file}\"\n\
LOCK_FILE=\"{lock_file}\"\n\
\n\
# Write initial status\n\
echo \"installing:$JOB_ID\" > \"$STATUS_FILE\"\n\
\n\
# Stop the service if running (do NOT use systemctl - not all distros have it)\n\
if command -v pkill >/dev/null 2>&1; then\n\
    pkill -f \"linux-patch-api\" 2>/dev/null || true\n\
else\n\
    killall linux-patch-api 2>/dev/null || true\n\
fi\n\
# Give the process a moment to shut down gracefully\n\
sleep 2\n\
\n\
# Install the package\n\
INSTALL_EXIT_CODE=0\n\
{install_cmd} || INSTALL_EXIT_CODE=$?\n\
\n\
# Determine new version for status reporting\n\
NEW_VERSION=\"\"\n\
case \"$PACKAGE_TYPE\" in\n\
    deb)\n\
        NEW_VERSION=$(dpkg-query -W -f='${{Version}}' linux-patch-api 2>/dev/null || echo \"unknown\")\n\
        ;;\n\
    rpm)\n\
        NEW_VERSION=$(rpm -q --qf='%{{VERSION}}-%{{RELEASE}}' linux-patch-api 2>/dev/null || echo \"unknown\")\n\
        ;;\n\
    apk)\n\
        NEW_VERSION=$(apk info linux-patch-api 2>/dev/null | head -1 | sed 's/^linux-patch-api-//' || echo \"unknown\")\n\
        ;;\n\
    pkg)\n\
        NEW_VERSION=$(pacman -Q linux-patch-api 2>/dev/null | awk '{{print $2}}' || echo \"unknown\")\n\
        ;;\n\
esac\n\
\n\
if [ \"$INSTALL_EXIT_CODE\" -eq 0 ]; then\n\
    echo \"success:$NEW_VERSION\" > \"$STATUS_FILE\"\n\
else\n\
    echo \"failed:Install exited with code $INSTALL_EXIT_CODE\" > \"$STATUS_FILE\"\n\
fi\n\
\n\
# Remove staged package\n\
rm -f \"$PACKAGE_PATH\"\n\
\n\
# Remove lock file (allow new installs)\n\
rm -f \"$LOCK_FILE\"\n\
\n\
# Attempt to start the service (safety net; post-install scripts should do this)\n\
# Use whichever service manager is available\n\
if command -v systemctl >/dev/null 2>&1; then\n\
    systemctl start linux-patch-api 2>/dev/null || true\n\
elif command -v rc-service >/dev/null 2>&1; then\n\
    rc-service linux-patch-api start 2>/dev/null || true\n\
elif [ -x /etc/init.d/linux-patch-api ]; then\n\
    /etc/init.d/linux-patch-api start 2>/dev/null || true\n\
fi\n",
        job_id = job_id,
        staged_package_path = staged_package_path,
        package_type = package_type,
        status_file = status_file,
        lock_file = lock_file,
        install_cmd = install_cmd,
    )
}

/// Detect the package type from a filename extension.
pub fn detect_package_type(filename: &str) -> Option<&'static str> {
    let lower = filename.to_lowercase();
    if lower.ends_with(".deb") {
        Some("deb")
    } else if lower.ends_with(".rpm") {
        Some("rpm")
    } else if lower.ends_with(".apk") {
        Some("apk")
    } else if lower.ends_with(".tar.zst") || lower.ends_with(".pkg.tar.zst") {
        Some("pkg")
    } else {
        None
    }
}

/// Check for an existing lock file. Returns Ok(()) if no lock or lock is stale.
/// Returns Err with a user-friendly message if a valid lock exists.
pub fn check_lock_file(state_dir: &str) -> Result<(), String> {
    let lock_path = PathBuf::from(state_dir).join("install.lock");

    if !lock_path.exists() {
        return Ok(());
    }

    // Check lock file age - if older than 30 minutes, consider it stale
    let metadata = match fs::metadata(&lock_path) {
        Ok(m) => m,
        Err(e) => {
            // Can not read metadata - treat as stale and remove
            warn!(path = %lock_path.display(), error = %e, "Cannot read lock file metadata, removing stale lock");
            let _ = fs::remove_file(&lock_path);
            return Ok(());
        }
    };

    let modified = match metadata.modified() {
        Ok(t) => t,
        Err(e) => {
            warn!(path = %lock_path.display(), error = %e, "Cannot read lock file mtime, removing stale lock");
            let _ = fs::remove_file(&lock_path);
            return Ok(());
        }
    };

    let age_secs = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now.saturating_sub(d.as_secs())
        }
        Err(_) => {
            // Modified time is in the future? Treat as stale.
            let _ = fs::remove_file(&lock_path);
            return Ok(());
        }
    };

    if age_secs > LOCK_STALE_THRESHOLD_SECS {
        info!(
            path = %lock_path.display(),
            age_secs = age_secs,
            "Stale lock file detected (older than 30 minutes), removing"
        );
        let _ = fs::remove_file(&lock_path);
        return Ok(());
    }

    // Active lock file exists - read its contents for the error message
    let contents = fs::read_to_string(&lock_path).unwrap_or_default();
    Err(format!(
        "An installation is already in progress. Lock file: {}. Details: {}",
        lock_path.display(),
        if contents.is_empty() {
            "no details".to_string()
        } else {
            contents
        }
    ))
}

/// Create a lock file with job_id, timestamp, and package path.
pub fn create_lock_file(state_dir: &str, job_id: &str, package_path: &str) -> Result<(), String> {
    // Ensure state directory exists
    let state_path = PathBuf::from(state_dir);
    if !state_path.exists() {
        fs::create_dir_all(&state_path).map_err(|e| {
            format!(
                "Failed to create state directory {}: {}",
                state_path.display(),
                e
            )
        })?;
    }

    let lock_path = state_path.join("install.lock");
    let timestamp = chrono::Utc::now().to_rfc3339();
    let contents = format!(
        "job_id={}\ntimestamp={}\npackage={}",
        job_id, timestamp, package_path
    );

    fs::write(&lock_path, contents)
        .map_err(|e| format!("Failed to create lock file {}: {}", lock_path.display(), e))
}

/// Read the install status file and return its contents.
/// Returns None if the file does not exist.
pub fn read_install_status(state_dir: &str) -> Option<InstallStatus> {
    let status_path = PathBuf::from(state_dir).join("install.status");

    let contents = fs::read_to_string(&status_path).ok()?;
    let trimmed = contents.trim();

    let status = if let Some(rest) = trimmed.strip_prefix("success:") {
        InstallStatus::Success {
            new_version: rest.to_string(),
        }
    } else if let Some(rest) = trimmed.strip_prefix("failed:") {
        InstallStatus::Failed {
            error: rest.to_string(),
        }
    } else if trimmed.starts_with("installing:") {
        InstallStatus::Installing {
            job_id: trimmed
                .strip_prefix("installing:")
                .unwrap_or("")
                .to_string(),
        }
    } else {
        InstallStatus::Unknown {
            raw: trimmed.to_string(),
        }
    };

    // Delete the status file after reading
    if let Err(e) = fs::remove_file(&status_path) {
        warn!(path = %status_path.display(), error = %e, "Failed to remove install status file after reading");
    }

    Some(status)
}

/// Install status read from the status file.
#[derive(Debug, Clone)]
pub enum InstallStatus {
    Success { new_version: String },
    Failed { error: String },
    Installing { job_id: String },
    Unknown { raw: String },
}

impl std::fmt::Display for InstallStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallStatus::Success { new_version } => {
                write!(f, "success: upgraded to version {}", new_version)
            }
            InstallStatus::Failed { error } => {
                write!(f, "failed: {}", error)
            }
            InstallStatus::Installing { job_id } => {
                write!(f, "installing: job {}", job_id)
            }
            InstallStatus::Unknown { raw } => {
                write!(f, "unknown status: {}", raw)
            }
        }
    }
}

/// Fork a detached shell script to perform the installation.
/// The script is written to a file in the state directory, made executable,
/// and launched via nohup to fully detach from the API process.
pub fn fork_install_script(script_content: &str, state_dir: &str) -> Result<(), String> {
    // Ensure state directory exists
    let state_path = PathBuf::from(state_dir);
    if !state_path.exists() {
        fs::create_dir_all(&state_path).map_err(|e| {
            format!(
                "Failed to create state directory {}: {}",
                state_path.display(),
                e
            )
        })?;
    }

    // Write the script to a file in the state directory
    let script_path = state_path.join("install_upgrade.sh");
    fs::write(&script_path, script_content)
        .map_err(|e| format!("Failed to write install script: {}", e))?;

    // Make the script executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to make install script executable: {}", e))?;
    }

    // Launch the script as a detached process using nohup
    let script_path_str = script_path.display().to_string();
    let log_path = state_path.join("install_upgrade.log");

    info!(
        script = %script_path_str,
        log = %log_path.display(),
        "Forking detached install script"
    );

    // Use nohup to fully detach from the parent process
    let log_file = std::fs::File::create(&log_path)
        .map_err(|e| format!("Failed to create log file {}: {}", log_path.display(), e))?;

    let child = std::process::Command::new("nohup")
        .arg("/bin/sh")
        .arg(&script_path_str)
        .stdout(
            log_file
                .try_clone()
                .map_err(|e| format!("Failed to clone file handle: {}", e))?,
        )
        .stderr(log_file)
        .spawn()
        .map_err(|e| format!("Failed to fork install script: {}", e))?;

    info!(pid = child.id(), "Detached install script launched");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_package_type_deb() {
        assert_eq!(detect_package_type("package.deb"), Some("deb"));
        assert_eq!(detect_package_type("package.DEB"), Some("deb"));
        assert_eq!(
            detect_package_type("linux-patch-api_1.5.0_amd64.deb"),
            Some("deb")
        );
    }

    #[test]
    fn test_detect_package_type_rpm() {
        assert_eq!(detect_package_type("package.rpm"), Some("rpm"));
        assert_eq!(detect_package_type("package.RPM"), Some("rpm"));
    }

    #[test]
    fn test_detect_package_type_apk() {
        assert_eq!(detect_package_type("package.apk"), Some("apk"));
        assert_eq!(detect_package_type("package.APK"), Some("apk"));
    }

    #[test]
    fn test_detect_package_type_pkg() {
        assert_eq!(detect_package_type("package.tar.zst"), Some("pkg"));
        assert_eq!(detect_package_type("package.pkg.tar.zst"), Some("pkg"));
        assert_eq!(detect_package_type("package.TAR.ZST"), Some("pkg"));
    }

    #[test]
    fn test_detect_package_type_unknown() {
        assert_eq!(detect_package_type("package.zip"), None);
        assert_eq!(detect_package_type("package.exe"), None);
        assert_eq!(detect_package_type("package"), None);
    }

    #[test]
    fn test_generate_install_script_contains_key_elements() {
        let script = generate_install_script(
            "/tmp/package.deb",
            "deb",
            "test-job-123",
            "/var/lib/linux_patch_api",
        );

        // Script should contain key elements
        assert!(script.contains("#!/bin/sh"));
        assert!(script.contains("/tmp/package.deb"));
        assert!(script.contains("test-job-123"));
        assert!(script.contains("/var/lib/linux_patch_api/install.status"));
        assert!(script.contains("/var/lib/linux_patch_api/install.lock"));
        assert!(script.contains("apt-get install -y"));
        assert!(script.contains("pkill"));
    }

    #[test]
    fn test_generate_install_script_rpm() {
        let script = generate_install_script(
            "/tmp/package.rpm",
            "rpm",
            "test-job-456",
            "/var/lib/linux_patch_api",
        );

        assert!(script.contains("dnf install -y"));
        assert!(script.contains("yum install -y"));
    }

    #[test]
    fn test_generate_install_script_apk() {
        let script = generate_install_script(
            "/tmp/package.apk",
            "apk",
            "test-job-789",
            "/var/lib/linux_patch_api",
        );

        assert!(script.contains("apk add --allow-untrusted"));
    }

    #[test]
    fn test_generate_install_script_pacman() {
        let script = generate_install_script(
            "/tmp/package.tar.zst",
            "pkg",
            "test-job-012",
            "/var/lib/linux_patch_api",
        );

        assert!(script.contains("pacman -U --noconfirm"));
    }

    #[test]
    fn test_install_status_display() {
        let success = InstallStatus::Success {
            new_version: "1.5.0".to_string(),
        };
        assert_eq!(format!("{}", success), "success: upgraded to version 1.5.0");

        let failed = InstallStatus::Failed {
            error: "exit code 1".to_string(),
        };
        assert_eq!(format!("{}", failed), "failed: exit code 1");

        let installing = InstallStatus::Installing {
            job_id: "abc-123".to_string(),
        };
        assert_eq!(format!("{}", installing), "installing: job abc-123");
    }
}
