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
fn detect_distro_id() -> Result<String> {
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
fn expected_repo_paths(distro_id: &str) -> Result<(String, String)> {
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
fn file_present_and_nonempty(path: &str) -> bool {
    Path::new(path)
        .metadata()
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
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
/// * `manager_url` - The manager base URL (e.g., `https://lpm.moon-dragon.us`).
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
        "Checking repo config presence"
    );

    if file_present_and_nonempty(&sources_path) && file_present_and_nonempty(&keyring_path) {
        tracing::info!(
            sources_path = %sources_path,
            keyring_path = %keyring_path,
            "Repo config already present — self-heal not needed"
        );
        return Ok(RepoHealResult::AlreadyConfigured);
    }

    tracing::warn!(
        sources_path = %sources_path,
        sources_exists = file_present_and_nonempty(&sources_path),
        keyring_path = %keyring_path,
        keyring_exists = file_present_and_nonempty(&keyring_path),
        manager_url = %manager_url,
        "Repo config missing — fetching from manager fallback endpoint"
    );

    let client = EnrollmentClient::new(manager_url);
    let repo = client
        .fetch_repo_config()
        .await
        .context("Failed to fetch repo config from manager during self-heal")?;

    provision::provision_repo_config(&repo)
        .await
        .context("Failed to provision repo config during self-heal")?;

    tracing::info!(
        distro = %distro_id,
        sources_path = %sources_path,
        keyring_path = %keyring_path,
        "Repo config provisioned via self-heal"
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
}
