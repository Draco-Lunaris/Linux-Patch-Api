//! Self-Upgrade Handler Module
//!
//! Provides utilities for the detached self-upgrade mechanism:
//! - Shell script template generation for cross-distro package installation
//! - Lock file management to prevent concurrent installs
//! - Status file reading for reporting install results on next startup
//!
//! The core problem this solves: when the API installs its own package,
//! the package's pre-remove script stops the API service, killing the
//! in-process installation. By forking a detached shell script that
//! escapes the service's cgroup (via systemd-run --scope or cgroup.procs),
//! the API returns 202 Accepted immediately and the script survives
//! the service stop to complete the installation independently.
//!
//! The install script owns the entire download+install process:
//! it downloads the package from the URL, verifies the checksum,
//! stops the service, installs the package, and starts the service.
//! This avoids the race condition where the API downloads the file
//! but gets killed before the install script can use it.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

/// Lock file staleness threshold in seconds (30 minutes).
const LOCK_STALE_THRESHOLD_SECS: u64 = 1800;

/// Escape a string for safe use inside single quotes in a shell script.
/// Single quotes in shell prevent all variable expansion and command
/// substitution. The only character that needs escaping inside single
/// quotes is the single quote itself, which is replaced by `'''`
/// (end quote, escaped quote, start quote).
fn shell_escape_single(s: &str) -> String {
    s.replace("'", "'\''")
}

/// Generate the shell script content for a detached package installation.
///
/// The script:
/// 1. Escapes the systemd cgroup (via `systemd-run --scope` or cgroup.procs)
///    so it survives when the package's pre-remove script stops the API service
/// 2. Writes "installing" status to the status file
/// 3. Downloads the package from the URL (using curl or wget with retries)
/// 4. Verifies the SHA-256 checksum (if provided)
/// 5. Stops the service (if running) using pkill, waiting for exit
/// 6. Installs the package using the appropriate package manager
/// 7. Writes success/failure status to the status file
/// 8. Removes the lock file and cleans up temp files
/// 9. Attempts to start the service on success (safety net)
///
/// The script is designed to work on all supported distros (Debian/Ubuntu,
/// Fedora/RHEL/AlmaLinux, Alpine, Arch) and does NOT rely on systemctl.
pub fn generate_install_script(
    download_url: &str,
    checksum: Option<&str>,
    filename: &str,
    package_type: &str,
    job_id: &str,
    state_dir: &str,
) -> String {
    let status_file = format!("{}/install.status", state_dir);
    let lock_file = format!("{}/install.lock", state_dir);
    let package_path = format!("/tmp/{}", filename);
    let script_file = format!("{}/install_upgrade.sh", state_dir);
    let log_file = format!("{}/install_upgrade.log", state_dir);

    // Strip sha256: prefix from checksum if present, keep only the hex digest
    let checksum_hex = checksum
        .map(|c| c.strip_prefix("sha256:").unwrap_or(c))
        .unwrap_or("");

    // Shell-escape all values interpolated into the script to prevent injection.
    // Single quotes in shell prevent all variable expansion and command
    // substitution. We only need to escape literal single quotes.
    let e_url = shell_escape_single(download_url);
    let e_sha = shell_escape_single(checksum_hex);
    let e_pkg = shell_escape_single(&package_path);
    let e_type = shell_escape_single(package_type);
    let e_job = shell_escape_single(job_id);
    let e_status = shell_escape_single(&status_file);
    let e_lock = shell_escape_single(&lock_file);
    let e_script = shell_escape_single(&script_file);
    let e_log = shell_escape_single(&log_file);

    // Determine the install command based on package type
    let install_cmd = match package_type {
        "deb" => {
            r#"DEBIAN_FRONTEND=noninteractive apt-get install -y --allow-downgrades -- "$PACKAGE_PATH""#.to_string()
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

    // Build the shell script using format! macro.
    // All user-provided values are single-quoted in the script to prevent
    // shell injection. The shell_escape_single function handles single quotes
    // in values by replacing them with '''.
    format!(
        r#"#!/bin/sh
# Self-upgrade installation script
# Job ID: {job_id}
# Download URL: {download_url}
# Package type: {package_type}
#
# This script is forked as a detached process by the API.
# It handles downloading the package, stopping the service, installing,
# and writing status for the API to read on next startup.

# CRITICAL: Escape the service cgroup before doing anything else.
# When this script runs as a child of the API service, it shares the service's
# cgroup. When the service is stopped (by prerm script or pkill), the init
# system kills ALL processes in the cgroup - including this install script.
# We must move ourselves out of the service cgroup to survive the stop.
if [ -z "$LPA_CGROUP_ESCAPED" ]; then
    export LPA_CGROUP_ESCAPED=1
    if command -v systemd-run >/dev/null 2>&1; then
        # systemd-run --scope creates a new transient scope outside the service cgroup.
        # If exec fails (e.g., systemd not actually running), fall through to cgroup.procs.
        exec systemd-run --scope --unit=lpa-upgrade-$$ /bin/sh "$0" "$@"
    fi
    # Try cgroup.procs fallback regardless of whether systemd-run was available
    # (it might exist as a binary but not be functional, e.g. in containers)
    if [ -w /sys/fs/cgroup/cgroup.procs ]; then
        echo $$ > /sys/fs/cgroup/cgroup.procs 2>/dev/null || true
    fi
    # If neither method is available, proceed anyway (best-effort)
fi

set -e

DOWNLOAD_URL='{e_url}'
EXPECTED_SHA256='{e_sha}'
PACKAGE_PATH='{e_pkg}'
PACKAGE_TYPE='{e_type}'
JOB_ID='{e_job}'
STATUS_FILE='{e_status}'
LOCK_FILE='{e_lock}'
SCRIPT_FILE='{e_script}'
LOG_FILE='{e_log}'

# Write initial status
echo "installing:$JOB_ID" > "$STATUS_FILE"

# Trap handler: clean up on unexpected exits (set -e can cause silent exits)
cleanup() {{
    # If status file still shows "installing", the script exited unexpectedly
    if [ -f "$STATUS_FILE" ] && grep -q "^installing:" "$STATUS_FILE" 2>/dev/null; then
        echo "failed:unexpected script exit" > "$STATUS_FILE"
    fi
    rm -f "$PACKAGE_PATH" 2>/dev/null || true
    rm -f "$LOCK_FILE" 2>/dev/null || true
}}
trap cleanup EXIT

# Download the package BEFORE stopping the service.
# The API must stay running during download so the script can fetch the file.
# The API is only stopped after download succeeds.
DOWNLOAD_EXIT_CODE=0
if command -v curl >/dev/null 2>&1; then
    curl -fSL --retry 3 --retry-delay 5 -o "$PACKAGE_PATH" "$DOWNLOAD_URL" || DOWNLOAD_EXIT_CODE=$?
elif command -v wget >/dev/null 2>&1; then
    wget --tries 3 -O "$PACKAGE_PATH" "$DOWNLOAD_URL" || DOWNLOAD_EXIT_CODE=$?
else
    echo "failed:Neither curl nor wget available for download" > "$STATUS_FILE"
    rm -f "$LOCK_FILE"
    exit 1
fi

if [ "$DOWNLOAD_EXIT_CODE" -ne 0 ]; then
    echo "failed:Download failed with exit code $DOWNLOAD_EXIT_CODE" > "$STATUS_FILE"
    rm -f "$PACKAGE_PATH" 2>/dev/null || true
    rm -f "$LOCK_FILE"
    exit 1
fi

# Verify checksum if provided
if [ -n "$EXPECTED_SHA256" ]; then
    CALCULATED=$(sha256sum "$PACKAGE_PATH" | awk '{{print $1}}')
    if [ "$CALCULATED" != "$EXPECTED_SHA256" ]; then
        echo "failed:SHA-256 checksum verification failed (expected $EXPECTED_SHA256, got $CALCULATED)" > "$STATUS_FILE"
        rm -f "$PACKAGE_PATH"
        rm -f "$LOCK_FILE"
        exit 1
    fi
fi

# Stop the service if running (do NOT use systemctl - not all distros have it)
if command -v pkill >/dev/null 2>&1; then
    # Use -x (exact process name match) instead of -f (full command pattern)
    # to avoid killing this install script whose path contains 'linux-patch-api'
    pkill -x linux-patch-api 2>/dev/null || true
else
    # killall matches by process name, not pattern - safe to use as-is
    killall linux-patch-api 2>/dev/null || true
fi
# Wait for the process to actually exit (up to 30 seconds)
WAIT_COUNT=0
while [ "$WAIT_COUNT" -lt 15 ] && pgrep -x linux-patch-api >/dev/null 2>&1; do
    sleep 2
    WAIT_COUNT=$((WAIT_COUNT + 1))
done
# If still running after 30s, force kill
if pgrep -x linux-patch-api >/dev/null 2>&1; then
    pkill -9 -x linux-patch-api 2>/dev/null || true
    sleep 1
fi

# Install the package
INSTALL_EXIT_CODE=0
{install_cmd} || INSTALL_EXIT_CODE=$?

# Determine new version for status reporting
NEW_VERSION=""
case "$PACKAGE_TYPE" in
    deb)
        NEW_VERSION=$(dpkg-query -W -f='${{Version}}' linux-patch-api 2>/dev/null || echo "unknown")
        ;;
    rpm)
        NEW_VERSION=$(rpm -q --qf='%{{VERSION}}-%{{RELEASE}}' linux-patch-api 2>/dev/null || echo "unknown")
        ;;
    apk)
        NEW_VERSION=$(apk info linux-patch-api 2>/dev/null | head -1 | sed 's/^linux-patch-api-//' || echo "unknown")
        ;;
    pkg)
        NEW_VERSION=$(pacman -Q linux-patch-api 2>/dev/null | awk '{{print $2}}' || echo "unknown")
        ;;
esac

if [ "$INSTALL_EXIT_CODE" -eq 0 ]; then
    echo "success:$NEW_VERSION" > "$STATUS_FILE"
else
    echo "failed:Install exited with code $INSTALL_EXIT_CODE" > "$STATUS_FILE"
fi

# Remove downloaded package
rm -f "$PACKAGE_PATH"

# Remove lock file (allow new installs)
rm -f "$LOCK_FILE"

# Only attempt service restart on successful install
if [ "$INSTALL_EXIT_CODE" -eq 0 ]; then
    # Attempt to start the service (safety net; post-install scripts should do this)
    # Use whichever service manager is available
    if command -v systemctl >/dev/null 2>&1; then
        systemctl start linux-patch-api 2>/dev/null || true
    elif command -v rc-service >/dev/null 2>&1; then
        rc-service linux-patch-api start 2>/dev/null || true
    elif [ -x /etc/init.d/linux-patch-api ]; then
        /etc/init.d/linux-patch-api start 2>/dev/null || true
    fi
fi

# Self-cleanup: remove the install script and log file
rm -f "$SCRIPT_FILE" 2>/dev/null || true
rm -f "$LOG_FILE" 2>/dev/null || true
"#,
        job_id = job_id,
        download_url = download_url,
        package_type = package_type,
        e_url = e_url,
        e_sha = e_sha,
        e_pkg = e_pkg,
        e_type = e_type,
        e_job = e_job,
        e_status = e_status,
        e_lock = e_lock,
        e_script = e_script,
        e_log = e_log,
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

/// Create a lock file with job_id, timestamp, and download URL.
pub fn create_lock_file(state_dir: &str, job_id: &str, download_url: &str) -> Result<(), String> {
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
        "job_id={}\ntimestamp={}\nurl={}",
        job_id, timestamp, download_url
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
/// and launched via setsid to create a new session, fully detaching from the
/// API's process group. The script itself also escapes the service cgroup
/// via systemd-run --scope (systemd) or cgroup.procs migration (cgroups v2)
/// before performing the installation.
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

    // Launch the script as a detached process using setsid
    let script_path_str = script_path.display().to_string();
    let log_path = state_path.join("install_upgrade.log");

    info!(
        script = %script_path_str,
        log = %log_path.display(),
        "Forking detached install script"
    );

    // Use setsid to create a new session, detaching from the parent's
    // process group and cgroup. This is more robust than nohup because
    // setsid creates a new session ID, not just ignoring SIGHUP.
    let log_file = std::fs::File::create(&log_path)
        .map_err(|e| format!("Failed to create log file {}: {}", log_path.display(), e))?;

    let child = std::process::Command::new("setsid")
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
    fn test_shell_escape_single() {
        // No special characters
        assert_eq!(shell_escape_single("hello"), "hello");
        // Single quote needs escaping
        assert_eq!(shell_escape_single("it's"), "it'\''s");
        // Multiple single quotes
        assert_eq!(shell_escape_single("a'b'c"), "a'\''b'\''c");
        // Empty string
        assert_eq!(shell_escape_single(""), "");
        // String with dollar and backtick (safe inside single quotes)
        assert_eq!(shell_escape_single("$HOME"), "$HOME");
        assert_eq!(shell_escape_single("`whoami`"), "`whoami`");
    }

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
            "https://example.com/package.deb",
            Some("abc123def456"),
            "package.deb",
            "deb",
            "test-job-123",
            "/var/lib/linux_patch_api",
        );

        // Script should contain key elements
        assert!(script.contains("#!/bin/sh"));
        assert!(script.contains("https://example.com/package.deb"));
        assert!(script.contains("abc123def456"));
        assert!(script.contains("/tmp/package.deb"));
        assert!(script.contains("test-job-123"));
        assert!(script.contains("/var/lib/linux_patch_api/install.status"));
        assert!(script.contains("/var/lib/linux_patch_api/install.lock"));
        assert!(script.contains("apt-get install -y --allow-downgrades"));
        assert!(script.contains("pkill -x"));

        // Script should contain download step with retries
        assert!(
            script.contains("curl -fSL --retry 3 --retry-delay 5"),
            "curl download with retries missing"
        );
        assert!(
            script.contains("wget --tries 3"),
            "wget download with retries missing"
        );

        // Script should contain checksum verification with detailed error
        assert!(
            script.contains("sha256sum"),
            "sha256sum verification missing"
        );
        assert!(
            script.contains("checksum verification failed (expected"),
            "checksum error should include expected vs actual"
        );

        // Script should contain cgroup escape logic
        assert!(
            script.contains("LPA_CGROUP_ESCAPED"),
            "cgroup escape guard variable missing"
        );
        assert!(
            script.contains("systemd-run --scope"),
            "systemd-run --scope escape missing"
        );
        assert!(
            script.contains("cgroup.procs"),
            "cgroup.procs fallback escape missing"
        );

        // Cgroup fallback should be a separate if, not elif
        let cgroup_section =
            &script[script.find("LPA_CGROUP_ESCAPED").unwrap()..script.find("set -e").unwrap()];
        assert!(
            !cgroup_section.contains("elif"),
            "cgroup escape should use separate if, not elif"
        );

        // Script should contain EXIT trap for cleanup on unexpected exits
        assert!(
            script.contains("trap cleanup EXIT"),
            "EXIT trap handler missing"
        );

        // Download should come BEFORE pkill
        let download_pos = script
            .find("curl -fSL")
            .or_else(|| script.find("wget --tries"))
            .unwrap();
        let pkill_pos = script.find("pkill -x").unwrap();
        assert!(
            download_pos < pkill_pos,
            "Download step should come before pkill step"
        );

        // Script should contain service stop wait loop
        assert!(
            script.contains("pgrep -x linux-patch-api"),
            "Process wait loop missing"
        );

        // Script should contain self-cleanup
        assert!(
            script.contains(r#"rm -f "$SCRIPT_FILE""#),
            "Self-cleanup of script file missing"
        );

        // Script should only start service on success
        assert!(
            script.contains(r#"if [ "$INSTALL_EXIT_CODE" -eq 0 ]; then"#),
            "Conditional service start on success missing"
        );

        // Values should be single-quoted (shell injection prevention)
        assert!(script.contains("DOWNLOAD_URL='https://example.com/package.deb'"));

        // Script should contain SCRIPT_FILE and LOG_FILE variables
        assert!(
            script.contains("SCRIPT_FILE='"),
            "SCRIPT_FILE variable missing"
        );
        assert!(script.contains("LOG_FILE='"), "LOG_FILE variable missing");
    }

    #[test]
    fn test_generate_install_script_no_checksum() {
        let script = generate_install_script(
            "https://example.com/package.rpm",
            None,
            "package.rpm",
            "rpm",
            "test-job-456",
            "/var/lib/linux_patch_api",
        );

        // Script should contain download URL
        assert!(script.contains("https://example.com/package.rpm"));
        assert!(script.contains("/tmp/package.rpm"));

        // Script should still have download step with retries
        assert!(script.contains("curl -fSL --retry 3 --retry-delay 5"));
        assert!(script.contains("wget --tries 3"));

        // Script should have empty checksum that skips verification
        assert!(script.contains("EXPECTED_SHA256=''"));

        // rpm install command
        assert!(script.contains("dnf install -y"));
        assert!(script.contains("yum install -y"));
    }

    #[test]
    fn test_generate_install_script_apk() {
        let script = generate_install_script(
            "https://example.com/package.apk",
            None,
            "package.apk",
            "apk",
            "test-job-789",
            "/var/lib/linux_patch_api",
        );

        assert!(script.contains("apk add --allow-untrusted"));
    }

    #[test]
    fn test_generate_install_script_pacman() {
        let script = generate_install_script(
            "https://example.com/package.tar.zst",
            Some("sha256:deadbeef"),
            "package.tar.zst",
            "pkg",
            "test-job-012",
            "/var/lib/linux_patch_api",
        );

        assert!(script.contains("pacman -U --noconfirm"));
        // sha256: prefix should be stripped
        assert!(script.contains("deadbeef"));
        assert!(!script.contains("sha256:deadbeef"));
    }

    #[test]
    fn test_generate_install_script_shell_injection_prevention() {
        // Test that shell metacharacters in URL are safely escaped
        let script = generate_install_script(
            "https://example.com/pkg$(whoami).deb",
            Some("abc123"),
            "package.deb",
            "deb",
            "test-job-inject",
            "/var/lib/linux_patch_api",
        );

        // The URL should be inside single quotes, preventing command substitution
        assert!(script.contains("DOWNLOAD_URL='https://example.com/pkg$(whoami).deb'"));
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
