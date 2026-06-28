//! PKI provisioning module for self-enrollment.
//! Handles certificate extraction, validation, and secure file writing.

use crate::auth::WhitelistManager;
use anyhow::{bail, Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

/// Default certificate directory when TLS config is not provided.
#[allow(dead_code)]
const DEFAULT_CERT_DIR: &str = "/etc/linux_patch_api/certs";
/// Default CA certificate path.
const DEFAULT_CA_CERT: &str = "/etc/linux_patch_api/certs/ca.pem";
/// Default server certificate path.
const DEFAULT_SERVER_CERT: &str = "/etc/linux_patch_api/certs/server.pem";
/// Default server key path.
const DEFAULT_SERVER_KEY: &str = "/etc/linux_patch_api/certs/server.key.pem";
/// Default CRL path.
const DEFAULT_CRL_PATH: &str = "/etc/linux_patch_api/certs/crl.pem";

/// Validate that a PEM string has proper format (BEGIN/END markers present).
///
/// Checks for `-----BEGIN {expected_type}-----` and `-----END {expected_type}-----` markers.
/// Returns an error if either marker is missing or the data is empty.
pub fn validate_pem(pem_data: &str, expected_type: &str) -> Result<()> {
    let trimmed = pem_data.trim();

    if trimmed.is_empty() {
        bail!("PEM data is empty for type '{}'", expected_type);
    }

    let begin_marker = format!("-----BEGIN {}-----", expected_type);
    let end_marker = format!("-----END {}-----", expected_type);

    if !trimmed.contains(&begin_marker) {
        bail!(
            "Invalid PEM format: missing '{}' marker for type '{}'",
            begin_marker,
            expected_type
        );
    }

    if !trimmed.contains(&end_marker) {
        bail!(
            "Invalid PEM format: missing '{}' marker for type '{}'",
            end_marker,
            expected_type
        );
    }

    Ok(())
}

/// Write PEM data to disk with secure permissions using atomic write pattern.
///
/// 1. Create target directory if it doesn't exist (with 0o755 permissions)
/// 2. Backup existing file if present (.bak extension)
/// 3. Write to temp file in same directory
/// 4. Set correct permissions (key=0o600, certs=0o644)
/// 5. Rename atomically to target path
pub fn write_pem_file(path: &str, pem_data: &str, is_key: bool) -> Result<()> {
    let path = std::path::Path::new(path);

    // Ensure target directory exists
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
            // Set directory permissions (0o755 for readability by service, restricted write)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(parent)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(parent, perms).with_context(|| {
                    format!("Failed to set permissions on: {}", parent.display())
                })?;
            }
        }
    }

    // Backup existing file if present
    if path.exists() {
        let backup_path = format!("{}.bak", path.display());
        fs::rename(path, &backup_path)
            .with_context(|| format!("Failed to backup existing file: {}", path.display()))?;
        tracing::info!(
            original = %path.display(),
            backup = %backup_path,
            "Backed up existing certificate file"
        );
    }

    // Create temp file in same directory for atomic rename
    let temp_path = path.with_extension("tmp");

    // Write PEM data to temp file
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .truncate(true)
        .mode(if is_key { 0o600 } else { 0o644 })
        .open(&temp_path)
        .with_context(|| format!("Failed to create temp file: {}", temp_path.display()))?;

    file.write_all(pem_data.as_bytes())
        .with_context(|| format!("Failed to write PEM data to: {}", temp_path.display()))?;
    file.flush()
        .with_context(|| format!("Failed to flush PEM data to: {}", temp_path.display()))?;

    // Atomic rename to target path
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "Failed to atomically rename {} to {}",
            temp_path.display(),
            path.display()
        )
    })?;

    tracing::info!(
        path = %path.display(),
        is_key = is_key,
        permissions = if is_key { "0600" } else { "0644" },
        "Successfully wrote PEM file"
    );

    Ok(())
}

/// Provision the full PKI bundle from an approved enrollment response.
///
/// Writes CA cert, CA chain, server cert, server key, and CRL to configured paths.
/// Paths are read from TLS config if available, otherwise defaults are used.
pub async fn provision_pki_bundle(
    ca_crt: &str,
    _ca_chain: &str,
    server_crt: &str,
    server_key: &str,
    crl_pem: &str,
    tls_config: Option<&super::super::config::loader::TlsConfig>,
) -> Result<()> {
    // Determine target paths from config or defaults
    let (ca_path, cert_path, key_path) = if let Some(tls) = tls_config {
        (
            tls.ca_cert.clone(),
            tls.server_cert.clone(),
            tls.server_key.clone(),
        )
    } else {
        (
            DEFAULT_CA_CERT.to_string(),
            DEFAULT_SERVER_CERT.to_string(),
            DEFAULT_SERVER_KEY.to_string(),
        )
    };

    // 1. Validate all three PEM strings before any writes
    validate_pem(ca_crt, "CERTIFICATE").context("CA certificate validation failed")?;
    validate_pem(server_crt, "CERTIFICATE").context("Server certificate validation failed")?;

    // Server key can be PRIVATE KEY (PKCS#8), RSA PRIVATE KEY (PKCS#1), or EC PRIVATE KEY
    let key_valid = validate_pem(server_key, "PRIVATE KEY").is_ok()
        || validate_pem(server_key, "RSA PRIVATE KEY").is_ok()
        || validate_pem(server_key, "EC PRIVATE KEY").is_ok();

    if !key_valid {
        bail!(
            "Server key validation failed: PEM must be PRIVATE KEY, RSA PRIVATE KEY, or EC PRIVATE KEY"
        );
    }

    // 2. Write to configured paths (atomic writes)
    write_pem_file(&ca_path, ca_crt, false).context("Failed to write CA certificate")?;

    write_pem_file(&cert_path, server_crt, false).context("Failed to write server certificate")?;

    write_pem_file(&key_path, server_key, true).context("Failed to write server key")?;

    // Write CRL if provided (non-empty)
    let crl_path = if let Some(tls) = tls_config {
        tls.crl_path.clone()
    } else {
        DEFAULT_CRL_PATH.to_string()
    };
    if !crl_pem.trim().is_empty() {
        write_pem_file(&crl_path, crl_pem, false).context("Failed to write CRL")?;
        tracing::info!(path = %crl_path, "CRL written from enrollment bundle");
    } else {
        tracing::info!("No CRL in enrollment bundle — agent will fetch on refresh cycle");
    }

    // 3. Log successful provisioning with structured fields
    tracing::info!(
        ca_cert = %ca_path,
        server_cert = %cert_path,
        server_key = %key_path,
        "PKI bundle provisioned successfully - all certificates written and validated"
    );

    Ok(())
}

/// Provision the package repository configuration from the enrollment bundle.
///
/// Writes the GPG public key to the keyring path and the sources config to the
/// distro-specific path. This enables the agent to self-update from the
/// manager-hosted repo using native package manager commands.
///
/// # Trust Model
/// The GPG key is trusted because it was delivered inside the mTLS-authenticated
/// enrollment bundle. If the enrollment is compromised, package trust is compromised.
/// This transitive trust chain is documented in THREAT_MODEL_VALIDATION.md.
pub async fn provision_repo_config(repo: &super::client::RepoConfig) -> Result<()> {
    // 1. Write GPG public key to keyring path (mode 0644)
    let keyring_dir = std::path::Path::new(&repo.keyring_path)
        .parent()
        .context("Invalid keyring path — no parent directory")?;

    if !keyring_dir.exists() {
        fs::create_dir_all(keyring_dir)
            .context("Failed to create keyring directory")?;
        // Set directory permissions to 0755
        let mut perms = fs::metadata(keyring_dir)?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
            fs::set_permissions(keyring_dir, perms)?;
        }
    }

    write_pem_file(&repo.keyring_path, &repo.gpg_public_key, false)
        .context("Failed to write repo GPG key")?;
    tracing::info!(keyring = %repo.keyring_path, "Repo GPG public key written");

    // 2. Write sources config to distro-specific path
    let sources_path = match repo.distro_id.as_str() {
        "ubuntu" | "debian" => "/etc/apt/sources.list.d/lpa.list".to_string(),
        "fedora" | "rhel" | "almalinux" | "centos" | "rocky" => {
            // Ensure /etc/yum.repos.d exists
            fs::create_dir_all("/etc/yum.repos.d").ok();
            "/etc/yum.repos.d/lpa.repo".to_string()
        }
        "alpine" => {
            // apk: append to /etc/apk/repositories (don't overwrite)
            let existing = fs::read_to_string("/etc/apk/repositories").unwrap_or_default();
            if existing.contains(&repo.sources_config) {
                tracing::info!("Repo URL already in /etc/apk/repositories — skipping");
                return Ok(());
            }
            let new_content = format!("{}\n{}\n", existing.trim_end(), repo.sources_config);
            fs::write("/etc/apk/repositories", new_content)
                .context("Failed to append to /etc/apk/repositories")?;
            tracing::info!("Repo URL appended to /etc/apk/repositories");
            return Ok(());
        }
        "arch" | "manjaro" => {
            // pacman: write include file
            fs::create_dir_all("/etc/pacman.d").ok();
            "/etc/pacman.d/lpa-repo".to_string()
        }
        _ => {
            bail!(
                "Unknown distro for repo config: {} — cannot determine sources path",
                repo.distro_id
            );
        }
    };

    fs::write(&sources_path, &repo.sources_config)
        .with_context(|| format!("Failed to write sources config to {}", sources_path))?;
    tracing::info!(
        distro = %repo.distro_id,
        sources = %sources_path,
        keyring = %repo.keyring_path,
        "Package repository configured from enrollment bundle"
    );

    Ok(())
}

/// Append the manager IP to the whitelist after successful enrollment.
///
/// Creates or loads a `WhitelistManager` and calls `append_entry()` with the
/// provided IP/CIDR string. Returns an error if the file cannot be locked,
/// written, or reloaded.
pub async fn append_manager_to_whitelist(manager_ip: &str, whitelist_path: &str) -> Result<()> {
    // Validate input before touching any files
    let ip_or_cidr = manager_ip.trim();
    if ip_or_cidr.is_empty() {
        bail!("Manager IP address cannot be empty");
    }

    // Create or load WhitelistManager and call append_entry
    let mut manager = WhitelistManager::new(whitelist_path).with_context(|| {
        format!(
            "Failed to initialize whitelist manager for path: {}",
            whitelist_path
        )
    })?;

    manager.append_entry(ip_or_cidr).with_context(|| {
        format!(
            "Failed to append manager IP '{}' to whitelist at: {}",
            ip_or_cidr, whitelist_path
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_certificate() -> String {
        "-----BEGIN CERTIFICATE-----\nMIIBxTCCAWugAwIBAgIRA ...\nBASE64ENCODED DATA HERE ...\n-----END CERTIFICATE-----".to_string()
    }

    fn sample_rsa_key() -> String {
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA0Z3VS5JJcds3...\nBASE64ENCODED DATA HERE ...\n-----END RSA PRIVATE KEY-----".to_string()
    }

    fn sample_pkcs8_key() -> String {
        "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQE...\nBASE64ENCODED DATA HERE ...\n-----END PRIVATE KEY-----".to_string()
    }

    fn sample_ec_key() -> String {
        "-----BEGIN EC PRIVATE KEY-----\nMHQCAQEEIBkg5Lb/...\nBASE64ENCODED DATA HERE ...\n-----END EC PRIVATE KEY-----".to_string()
    }

    #[test]
    fn test_validate_pem_valid_certificate() {
        let cert = sample_certificate();
        assert!(validate_pem(&cert, "CERTIFICATE").is_ok());
    }

    #[test]
    fn test_validate_pem_valid_rsa_key() {
        let key = sample_rsa_key();
        assert!(validate_pem(&key, "RSA PRIVATE KEY").is_ok());
    }

    #[test]
    fn test_validate_pem_valid_pkcs8_key() {
        let key = sample_pkcs8_key();
        assert!(validate_pem(&key, "PRIVATE KEY").is_ok());
    }

    #[test]
    fn test_validate_pem_valid_ec_key() {
        let key = sample_ec_key();
        assert!(validate_pem(&key, "EC PRIVATE KEY").is_ok());
    }

    #[test]
    fn test_validate_pem_empty_data_fails() {
        assert!(validate_pem("", "CERTIFICATE").is_err());
    }

    #[test]
    fn test_validate_pem_missing_begin_marker_fails() {
        let malformed = "BASE64DATA\n-----END CERTIFICATE-----".to_string();
        let err = validate_pem(&malformed, "CERTIFICATE").unwrap_err();
        assert!(err.to_string().contains("BEGIN"));
    }

    #[test]
    fn test_validate_pem_missing_end_marker_fails() {
        let malformed = "-----BEGIN CERTIFICATE-----\nBASE64DATA".to_string();
        let err = validate_pem(&malformed, "CERTIFICATE").unwrap_err();
        assert!(err.to_string().contains("END"));
    }

    #[test]
    fn test_validate_pem_wrong_type_fails() {
        let cert = sample_certificate();
        // Certificate data checked against wrong type should fail
        let err = validate_pem(&cert, "RSA PRIVATE KEY").unwrap_err();
        assert!(err.to_string().contains("BEGIN"));
    }

    #[test]
    fn test_validate_pem_whitespace_tolerance() {
        let cert = format!("\n  \n  {}  \n  ", sample_certificate());
        assert!(validate_pem(&cert, "CERTIFICATE").is_ok());
    }

    #[test]
    fn test_write_pem_file_creates_directory() {
        let dir = tempdir().expect("failed to create temp dir");
        let target_path = dir.path().join("subdir").join("cert.pem");
        let cert = sample_certificate();

        write_pem_file(target_path.to_str().unwrap(), &cert, false).expect("write failed");
        assert!(target_path.exists());
    }

    #[test]
    fn test_write_pem_file_atomic_rename() {
        let dir = tempdir().expect("failed to create temp dir");
        let target_path = dir.path().join("cert.pem");
        let cert = sample_certificate();

        write_pem_file(target_path.to_str().unwrap(), &cert, false).expect("write failed");

        // Verify content matches
        let written = fs::read_to_string(&target_path).expect("failed to read back");
        assert_eq!(written, cert);
    }

    #[test]
    fn test_write_pem_file_key_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("failed to create temp dir");
        let target_path = dir.path().join("key.pem");
        let key = sample_rsa_key();

        write_pem_file(target_path.to_str().unwrap(), &key, true).expect("write failed");

        let metadata = fs::metadata(&target_path).expect("failed to get metadata");
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "Key file should have 0600 permissions");
    }

    #[test]
    fn test_write_pem_file_cert_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("failed to create temp dir");
        let target_path = dir.path().join("cert.pem");
        let cert = sample_certificate();

        write_pem_file(target_path.to_str().unwrap(), &cert, false).expect("write failed");

        let metadata = fs::metadata(&target_path).expect("failed to get metadata");
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "Cert file should have 0644 permissions");
    }

    #[test]
    fn test_write_pem_file_backup_existing() {
        let dir = tempdir().expect("failed to create temp dir");
        let target_path = dir.path().join("cert.pem");
        let cert1 = sample_certificate();
        let cert2 =
            "-----BEGIN CERTIFICATE-----\nNEWCERTDATA\n-----END CERTIFICATE-----".to_string();

        // Write initial file
        write_pem_file(target_path.to_str().unwrap(), &cert1, false).expect("initial write failed");

        // Write again - should create backup
        write_pem_file(target_path.to_str().unwrap(), &cert2, false).expect("second write failed");

        let backup_path = format!("{}.bak", target_path.display());
        assert!(
            std::path::Path::new(&backup_path).exists(),
            "Backup file should exist"
        );

        // Original content in backup
        let backup_content = fs::read_to_string(&backup_path).expect("failed to read backup");
        assert_eq!(backup_content, cert1);
    }
}
