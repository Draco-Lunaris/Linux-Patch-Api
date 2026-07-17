//! Repository configuration self-heal.
//!
//! Ensures the manager-hosted package repo is configured on the agent so
//! self-update can actually find and install a newer `linux-patch-api` package.
//!
//! This module addresses the gap where hosts enrolled before `repo_config`
//! was added to the enrollment bundle (or where the repo files were lost) have
//! no manager repo configured. Without this, `apt-get install --only-upgrade
//! linux-patch-api` silently finds "already newest version" and the self-update
//! reports success without upgrading anything.
//!
//! The check is idempotent: if the sources file and GPG keyring already exist
//! and are non-empty, it returns immediately. If either is missing, it fetches
//! `RepoConfig` from the manager's fallback endpoint and provisions it.

use std::path::Path;

use anyhow::{Context, Result};

use super::client::EnrollmentClient;
use super::provision;

/// Outcome of a repo-config self-heal check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoHealResult {
    /// Both the sources file and GPG keyring already exist — nothing to do.
    AlreadyConfigured,
    /// Repo config was missing and has been successfully provisioned.
    Provisioned,
}

/// Detect the distro ID from `/etc/os-release` (`ID` field).
///
/// Falls back to `ID_LIKE` if `ID` is absent, taking the first token.
/// Returns `Err` if the distro cannot be determined.
pub fn detect_distro_id() -> Result<String> {
    let content = std::fs::read_to_string("/etc/os-release").context(
        "Failed to read /etc/os-release — cannot determine distro for repo health check",
    )?;

    let mut id = None;
    let mut id_like = None;

    for line in content.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once('=') {
            let unquoted = value.trim().trim_matches('"').trim_matches('\'');
            match key {
                "ID" => id = Some(unquoted.to_string()),
                "ID_LIKE" => id_like = Some(unquoted.to_string()),
                _ => {}
            }
        }
    }

    if let Some(distro) = id {
        if !distro.is_empty() {
            return Ok(distro);
        }
    }

    if let Some(like) = id_like {
        if let Some(first) = like.split_whitespace().next() {
            return Ok(first.to_string());
        }
    }

    Err(anyhow::anyhow!(
        "Could not determine distro ID from /etc/os-release (no ID or ID_LIKE field)"
    ))
}

/// Determine the expected sources-file path and GPG keyring path for the
/// detected distro.
///
/// These paths mirror the ones written by [`provision::provision_repo_config`].
/// If the distro is unrecognized, returns `Err`.
pub fn expected_repo_paths(distro_id: &str) -> Result<(String, String)> {
    match distro_id {
        "ubuntu" | "debian" | "linuxmint" => Ok((
            "/etc/apt/sources.list.d/lpa.list".to_string(),
            "/etc/apt/keyrings/lpa-repo.gpg".to_string(),
        )),
        "fedora" | "rhel" | "almalinux" | "centos" | "rocky" | "amzn" => Ok((
            "/etc/yum.repos.d/lpa.repo".to_string(),
            "/etc/pki/rpm-gpg/lpa-repo.gpg".to_string(),
        )),
        "alpine" => Ok((
            "/etc/apk/repositories".to_string(),
            "/etc/apk/keys/lpa-repo.gpg".to_string(),
        )),
        "arch" | "manjaro" | "endeavouros" => Ok((
            "/etc/pacman.d/lpa-repo".to_string(),
            "/etc/pacman.d/lpa-repo.gpg".to_string(),
        )),
        other => Err(anyhow::anyhow!(
            "Unrecognized distro '{}' — cannot determine expected repo file paths",
            other
        )),
    }
}

/// Check whether a file exists and is non-empty.
#[allow(dead_code)]
fn file_present_and_nonempty(path: &str) -> bool {
    Path::new(path)
        .metadata()
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// GPG key health status for the agent's `/health` endpoint.
///
/// The manager consumes these values to determine whether the agent's
/// repo is signed by a valid, non-expired GPG key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpgKeyStatus {
    Valid,
    Expired,
    Missing,
    Revoked,
}

impl GpgKeyStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            GpgKeyStatus::Valid => "valid",
            GpgKeyStatus::Expired => "expired",
            GpgKeyStatus::Missing => "missing",
            GpgKeyStatus::Revoked => "revoked",
        }
    }
}

/// Check the health of the provisioned GPG keyring.
///
/// Returns `(GpgKeyStatus, Option<String>)` where the second element is the
/// expiry timestamp in RFC3339 format (if available).
///
/// - If the keyring file doesn't exist → `(Missing, None)`
/// - If the keyring exists but `gpg` is not installed → `(Valid, None)` (can't check expiry)
/// - If `gpg --show-keys` succeeds and the key is not expired → `(Valid, Some(expiry))`
/// - If the key is expired → `(Expired, Some(expiry))`
/// - If the key is revoked → `(Revoked, Some(expiry))`
pub fn check_gpg_key_health() -> (GpgKeyStatus, Option<String>) {
    let distro_id = match detect_distro_id() {
        Ok(id) => id,
        Err(_) => return (GpgKeyStatus::Missing, None),
    };

    let (_sources_path, keyring_path) = match expected_repo_paths(&distro_id) {
        Ok(paths) => paths,
        Err(_) => return (GpgKeyStatus::Missing, None),
    };

    // Check if the keyring file exists and is non-empty.
    if !file_present_and_nonempty(&keyring_path) {
        return (GpgKeyStatus::Missing, None);
    }

    // Try to inspect the key with gpg for expiry information.
    // If gpg is not installed, we can only report "valid" based on file existence.
    let gpg_output = std::process::Command::new("gpg")
        .args([
            "--show-keys",
            "--with-colons",
            "--fingerprint",
            &keyring_path,
        ])
        .output();

    let output = match gpg_output {
        Ok(o) if o.status.success() => o,
        _ => {
            // gpg not installed or failed — report valid based on file existence.
            tracing::debug!(
                keyring = %keyring_path,
                "gpg not available or failed — reporting key as valid based on file existence"
            );
            return (GpgKeyStatus::Valid, None);
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_gpg_key_status(&stdout)
}

/// Parse `gpg --show-keys --with-colons` output to determine key status and expiry.
///
/// The colon-separated format has fields like:
/// `pub:-:2048:1A2B3C4D:1700000000:1800000000::-:KeyID::scESC::`
/// Field indices: 0=type, 1=validity, 2=length, 3=keyid, 4=created, 5=expires, ...
///
/// Validity codes: `-`=unknown, `e`=expired, `r`=revoked, `f`=full, `u`=ultimate
fn parse_gpg_key_status(gpg_output: &str) -> (GpgKeyStatus, Option<String>) {
    let mut status = GpgKeyStatus::Valid;
    let mut expiry_timestamp: Option<String> = None;

    for line in gpg_output.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 6 {
            continue;
        }

        // Look at pub or sub key lines for validity and expiry.
        if fields[0] == "pub" || fields[0] == "sub" {
            let validity = fields[1];
            let expires = fields[5];

            // Parse expiry timestamp (Unix epoch seconds).
            if !expires.is_empty() {
                if let Ok(secs) = expires.parse::<i64>() {
                    if secs > 0 {
                        expiry_timestamp =
                            chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339());
                    }
                }
            }

            // Check validity code.
            if validity.contains('e') {
                status = GpgKeyStatus::Expired;
            } else if validity.contains('r') {
                status = GpgKeyStatus::Revoked;
            }
        }
    }

    (status, expiry_timestamp)
}

/// Read file content as string, returning None if file doesn't exist or is empty.
fn read_file_content(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().filter(|s| !s.is_empty())
}

/// Check whether the manager-hosted repo is configured, and if not, fetch
/// `RepoConfig` from the manager and provision it.
///
/// This is the primary self-heal entry point. It is:
/// - **Idempotent**: returns `AlreadyConfigured` if both files exist and are non-empty.
/// - **Safe to call repeatedly**: provisioning overwrites the sources file and
///   atomically replaces the keyring, so repeated calls converge to the
///   manager's current config.
/// - **Best-effort on startup**: callers may log-and-continue on error.
/// - **Hard-fail on self-update**: the pre-self-update caller should fail the
///   job if this returns `Err`, because without repo config the upgrade is a
///   silent no-op.
///
/// # Arguments
/// * `manager_url` - The manager base URL (e.g., `https://patch-manager.example.com`).
///
/// # Returns
/// - `Ok(AlreadyConfigured)` if both the sources file and GPG keyring exist.
/// - `Ok(Provisioned)` if the repo config was fetched and written successfully.
/// - `Err` if the distro cannot be detected, the manager is unreachable, or
///   provisioning fails.
pub async fn check_and_provision_repo_config(manager_url: &str) -> Result<RepoHealResult> {
    let distro_id = detect_distro_id()?;
    let (sources_path, keyring_path) = expected_repo_paths(&distro_id)?;

    tracing::debug!(
        distro = %distro_id,
        sources_path = %sources_path,
        keyring_path = %keyring_path,
        "Checking repo config presence and content"
    );

    // Always fetch current expected config from manager to validate content
    let client = EnrollmentClient::new(manager_url);
    let expected_repo = client
        .fetch_repo_config()
        .await
        .context("Failed to fetch repo config from manager during self-heal")?;

    let sources_match =
        read_file_content(&sources_path).as_deref() == Some(expected_repo.sources_config.as_str());
    let keyring_match =
        read_file_content(&keyring_path).as_deref() == Some(expected_repo.gpg_public_key.as_str());

    if sources_match && keyring_match {
        tracing::info!(
            sources_path = %sources_path,
            keyring_path = %keyring_path,
            "Repo config content matches manager — self-heal not needed"
        );
        return Ok(RepoHealResult::AlreadyConfigured);
    }

    tracing::warn!(
        sources_path = %sources_path,
        sources_match = sources_match,
        keyring_path = %keyring_path,
        keyring_match = keyring_match,
        manager_url = %manager_url,
        "Repo config content mismatch — re-provisioning from manager"
    );

    provision::provision_repo_config(&expected_repo)
        .await
        .context("Failed to provision repo config during self-heal")?;

    tracing::info!(
        distro = %distro_id,
        sources_path = %sources_path,
        keyring_path = %keyring_path,
        "Repo config provisioned via self-heal (content updated)"
    );

    Ok(RepoHealResult::Provisioned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_distro_id_returns_nonempty() {
        // This test runs on the build host, which should have /etc/os-release
        let distro = detect_distro_id().expect("detect_distro_id failed");
        assert!(!distro.is_empty(), "distro ID should not be empty");
    }

    #[test]
    fn test_expected_repo_paths_ubuntu() {
        let (sources, keyring) = expected_repo_paths("ubuntu").unwrap();
        assert!(sources.contains("sources.list.d/lpa.list"));
        assert!(keyring.contains("keyrings/lpa-repo.gpg"));
    }

    #[test]
    fn test_expected_repo_paths_debian() {
        let (sources, _keyring) = expected_repo_paths("debian").unwrap();
        assert!(sources.contains("sources.list.d/lpa.list"));
    }

    #[test]
    fn test_expected_repo_paths_fedora() {
        let (sources, keyring) = expected_repo_paths("fedora").unwrap();
        assert!(sources.contains("yum.repos.d/lpa.repo"));
        assert!(keyring.contains("rpm-gpg"));
    }

    #[test]
    fn test_expected_repo_paths_alpine() {
        let (sources, _keyring) = expected_repo_paths("alpine").unwrap();
        assert!(sources.contains("apk/repositories"));
    }

    #[test]
    fn test_expected_repo_paths_arch() {
        let (sources, _keyring) = expected_repo_paths("arch").unwrap();
        assert!(sources.contains("pacman.d/lpa-repo"));
    }

    #[test]
    fn test_expected_repo_paths_unknown_distro_fails() {
        assert!(expected_repo_paths("solaris").is_err());
    }

    #[test]
    fn test_file_present_and_nonempty_nonexistent() {
        assert!(!file_present_and_nonempty(
            "/nonexistent/path/that/does/not/exist"
        ));
    }

    #[test]
    fn test_file_present_and_nonempty_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();
        let path_str = path.to_str().unwrap();
        assert!(!file_present_and_nonempty(path_str));
    }

    #[test]
    fn test_file_present_and_nonempty_nonempty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.txt");
        std::fs::write(&path, "some content").unwrap();
        let path_str = path.to_str().unwrap();
        assert!(file_present_and_nonempty(path_str));
    }

    #[test]
    fn test_file_present_and_nonempty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path_str = dir.path().to_str().unwrap();
        // A directory is not a file — should return false
        assert!(!file_present_and_nonempty(path_str));
    }

    #[test]
    fn test_repo_heal_result_equality() {
        assert_eq!(
            RepoHealResult::AlreadyConfigured,
            RepoHealResult::AlreadyConfigured
        );
        assert_eq!(RepoHealResult::Provisioned, RepoHealResult::Provisioned);
        assert_ne!(
            RepoHealResult::AlreadyConfigured,
            RepoHealResult::Provisioned
        );
    }

    #[test]
    fn test_read_file_content_nonexistent() {
        assert!(read_file_content("/nonexistent/path/that/does/not/exist").is_none());
    }

    #[test]
    fn test_read_file_content_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();
        let path_str = path.to_str().unwrap();
        assert!(read_file_content(path_str).is_none());
    }

    #[test]
    fn test_read_file_content_nonempty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.txt");
        std::fs::write(&path, "some content").unwrap();
        let path_str = path.to_str().unwrap();
        assert_eq!(
            read_file_content(path_str),
            Some("some content".to_string())
        );
    }

    #[test]
    fn test_read_file_content_whitespace_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("whitespace.txt");
        std::fs::write(&path, "   \n\t  ").unwrap();
        let path_str = path.to_str().unwrap();
        assert_eq!(read_file_content(path_str), Some("   \n\t  ".to_string()));
    }

    // --- GPG key health tests ---

    #[test]
    fn test_parse_gpg_key_status_valid() {
        // Simulate gpg --with-colons output for a valid, non-expired key.
        // Fields: type:validity:length:keyid:created:expires:...
        let output = "pub:f:2048:ABCDEF1234567890:1700000000:1800000000::-:Test Key::scESC::\nsub:f:2048:ABCDEF1234567890:1700000000:1800000000::-:Test Sub::e::\n";
        let (status, expiry) = parse_gpg_key_status(output);
        assert_eq!(status, GpgKeyStatus::Valid);
        assert!(expiry.is_some());
        // 1800000000 = 2027-01-15T08:00:00Z
        assert!(expiry.unwrap().starts_with("2027"));
    }

    #[test]
    fn test_parse_gpg_key_status_expired() {
        // Validity 'e' means expired.
        let output = "pub:e:2048:ABCDEF1234567890:1600000000:1700000000::-:Expired Key::scESC::\n";
        let (status, _expiry) = parse_gpg_key_status(output);
        assert_eq!(status, GpgKeyStatus::Expired);
    }

    #[test]
    fn test_parse_gpg_key_status_revoked() {
        // Validity 'r' means revoked.
        let output = "pub:r:2048:ABCDEF1234567890:1600000000:1800000000::-:Revoked Key::scESC::\n";
        let (status, _expiry) = parse_gpg_key_status(output);
        assert_eq!(status, GpgKeyStatus::Revoked);
    }

    #[test]
    fn test_parse_gpg_key_status_empty_output() {
        let (status, expiry) = parse_gpg_key_status("");
        assert_eq!(status, GpgKeyStatus::Valid);
        assert!(expiry.is_none());
    }

    #[test]
    fn test_parse_gpg_key_status_no_expiry() {
        // Key with no expiry (expires field empty).
        let output = "pub:f:2048:ABCDEF1234567890:1700000000::::-:No Expiry Key::scESC::\n";
        let (status, expiry) = parse_gpg_key_status(output);
        assert_eq!(status, GpgKeyStatus::Valid);
        assert!(expiry.is_none());
    }

    #[test]
    fn test_gpg_key_status_as_str() {
        assert_eq!(GpgKeyStatus::Valid.as_str(), "valid");
        assert_eq!(GpgKeyStatus::Expired.as_str(), "expired");
        assert_eq!(GpgKeyStatus::Missing.as_str(), "missing");
        assert_eq!(GpgKeyStatus::Revoked.as_str(), "revoked");
    }
}
