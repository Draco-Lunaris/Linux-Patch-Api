//! Self-enrollment module for linux_patch_api daemon.
//!
//! Handles secure registration with the patch manager, including
//! identity extraction (machine-id, FQDN, IPs, OS details) and
//! mTLS enrollment via the manager API.
//!
//! Supports:
//! - Auto-enrollment on startup when certs are missing/invalid
//! - Manual enrollment via `--enroll <url>` CLI flag
//! - Resume polling from persisted token after restart
//! - HTTP 409 (host already exists) handling

pub mod client;
pub mod identity;
pub mod provision;
pub mod repo_health;

use anyhow::{Context, Result};

/// Re-export key types for ergonomic access from parent modules.
pub use client::{
    EnrollmentClient, EnrollmentRequest, EnrollmentResponse, EnrollmentStatusResponse, PkiBundle,
    RepoConfig,
};
/// Re-export identity extraction functions.
pub use identity::{
    get_fqdn, get_hostname, get_ip_addresses, get_ip_for_interface, get_machine_id, get_os_details,
    get_primary_ip, get_route_source_ip, is_container_bridge, is_link_local,
};
/// Re-export repo self-heal entry point.
pub use repo_health::{check_and_provision_repo_config, RepoHealResult};

/// Error type for enrollment conflict (HTTP 409).
/// Used to signal that the host is already registered and we should
/// skip to the polling phase.
#[derive(Debug)]
pub struct EnrollmentConflictError;

impl std::fmt::Display for EnrollmentConflictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Host already registered with manager")
    }
}

impl std::error::Error for EnrollmentConflictError {}

/// Run the full enrollment flow against the manager at the given URL.
///
/// # Phases
/// 1. **Registration** - POST machine identity to manager, receive polling token
///    - If HTTP 409 (host already exists), skip to Phase 2 with existing token
/// 2. **Polling** - Poll manager for approval with configurable interval/max attempts
///    - If `polling_token` is already in config, skip Phase 1 and resume polling
/// 3. **Provisioning** - Write PKI bundle to disk (certs/keys) and append manager IP to whitelist
///
/// # Arguments
/// * `manager_url` - The manager API base URL
/// * `config` - Mutable reference to AppConfig for polling token persistence
/// * `config_path` - Path to config file for persisting polling token
///
/// # Errors
/// Returns Err on registration failure, polling timeout, denial, user interruption,
/// PKI provisioning failure, or whitelist update failure.
pub async fn run_enrollment(
    manager_url: &str,
    config: &mut super::AppConfig,
    config_path: &str,
) -> Result<()> {
    // Extract IP reporting overrides from enrollment config
    let (report_interface, report_ip) = config
        .enrollment
        .as_ref()
        .map(|e| (e.report_interface.clone(), e.report_ip.clone()))
        .unwrap_or((None, None));

    let client = EnrollmentClient::with_ip_overrides(manager_url, report_interface, report_ip);

    // Check for existing polling token to resume
    let polling_token = if let Some(ref enrollment) = config.enrollment {
        if !enrollment.polling_token.is_empty() {
            tracing::info!(
                "Resuming enrollment polling from saved token (host already registered)"
            );
            enrollment.polling_token.clone()
        } else {
            // No saved token — need to register first
            String::new()
        }
    } else {
        String::new()
    };

    // Phase 1: Registration (skip if we have a saved polling token)
    let polling_token = if polling_token.is_empty() {
        tracing::info!(
            manager_url = manager_url,
            "Starting enrollment - registration phase"
        );
        match client.register().await {
            Ok(response) => {
                tracing::info!("Registration successful - received polling token");
                response.polling_token
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("ENROLLMENT_CONFLICT") {
                    // HTTP 409 - host already exists
                    // We don't have a polling token, so we can't resume polling
                    // Log a warning and return an error — the user needs to
                    // re-enroll or the manager needs to provide a new token
                    tracing::warn!(
                        "Host already registered but no polling token saved. \
                         Cannot resume polling. Re-run enrollment or check manager status."
                    );
                    return Err(anyhow::anyhow!(
                        "Host already registered with manager but no polling token available for resume. \
                         Please check the manager for your host status or re-enroll."
                    ));
                }
                // For other errors, propagate directly
                return Err(e);
            }
        }
    } else {
        tracing::info!("Using saved polling token to resume enrollment");
        polling_token
    };

    // Persist polling token for resume after restart
    if let Err(e) = config.save_polling_token(&polling_token, config_path) {
        tracing::warn!(
            error = %e,
            "Failed to persist polling token — enrollment will not resume after restart"
        );
    } else {
        tracing::debug!("Polling token persisted to config");
    }

    // Get polling config (use defaults if not set)
    let interval = config
        .enrollment
        .as_ref()
        .map(|e| e.polling_interval_seconds)
        .unwrap_or(60);
    let max_attempts = config
        .enrollment
        .as_ref()
        .map(|e| e.max_poll_attempts)
        .unwrap_or(1440);

    // Phase 2: Polling
    tracing::info!(
        interval_seconds = interval,
        max_attempts = max_attempts,
        "Starting enrollment - polling phase"
    );
    let pki_bundle = client
        .poll_for_approval(&polling_token, interval, max_attempts)
        .await?;

    // Phase 3: PKI provisioning & whitelist update
    tracing::info!("Enrollment approved - starting PKI provisioning phase");

    // Write certificates to configured paths (or defaults)
    provision::provision_pki_bundle(
        &pki_bundle.ca_crt,
        &pki_bundle.ca_chain,
        &pki_bundle.server_crt,
        &pki_bundle.server_key,
        &pki_bundle.crl_pem,
        config.tls_config(),
    )
    .await?;
    tracing::info!("PKI bundle written to disk");

    // Provision package repository configuration if present in the bundle.
    // This writes the GPG key and sources config so the agent can self-update
    // from the manager-hosted repo using native package manager commands.
    // If repo_config is absent (older enrollment), the agent will fetch it
    // on demand from GET /api/v1/pki/repo-config on first self-update.
    if let Some(ref repo) = pki_bundle.repo_config {
        provision::provision_repo_config(repo).await?;
        tracing::info!("Package repository configured from enrollment bundle");
    } else {
        tracing::info!("No repo_config in enrollment bundle — fetching from fallback endpoint");
        match client.fetch_repo_config().await {
            Ok(repo) => {
                if let Err(e) = provision::provision_repo_config(&repo).await {
                    tracing::warn!(
                        error = %e,
                        "Failed to provision repo config from fallback — enrollment continues"
                    );
                } else {
                    tracing::info!("Package repository configured from fallback fetch");
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to fetch repo config from fallback endpoint — enrollment continues without repo provisioning"
                );
            }
        }
    }

    // Resolve manager hostname to IP and append to whitelist
    let manager_ip = client
        .manager_ip()
        .await
        .context("Failed to resolve manager IP - cannot update whitelist")?;
    provision::append_manager_to_whitelist(&manager_ip, config.whitelist_path()).await?;
    tracing::info!(manager_ip = %manager_ip, "Manager IP appended to whitelist");

    // Clear polling token after successful provisioning
    if let Err(e) = config.clear_polling_token(config_path) {
        tracing::warn!(
            error = %e,
            "Failed to clear polling token from config — will attempt re-registration on next start"
        );
    } else {
        tracing::debug!("Polling token cleared from config");
    }

    tracing::info!("Enrollment complete - PKI and whitelist configured");
    Ok(())
}