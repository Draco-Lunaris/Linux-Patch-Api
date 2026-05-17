//! Self-enrollment module for linux_patch_api daemon.
//!
//! Handles secure registration with the patch manager, including
//! identity extraction (machine-id, FQDN, IPs, OS details) and
//! mTLS enrollment via the manager API.

pub mod client;
pub mod identity;
pub mod provision;

use anyhow::{Context, Result};

/// Re-export key types for ergonomic access from parent modules.
pub use client::{
    EnrollmentClient, EnrollmentRequest, EnrollmentResponse,
    EnrollmentStatusResponse, PkiBundle,
};
/// Re-export identity extraction functions.
pub use identity::{get_fqdn, get_ip_addresses, get_machine_id, get_os_details};

/// Run the full enrollment flow against the manager at the given URL.
///
/// # Phases
/// 1. **Registration** - POST machine identity to manager, receive polling token
/// 2. **Polling** - Poll manager for approval with configurable interval/max attempts
/// 3. **Provisioning** - Write PKI bundle to disk (certs/keys) and append manager IP to whitelist
///
/// # Errors
/// Returns Err on registration failure, polling timeout, denial, user interruption,
/// PKI provisioning failure, or whitelist update failure.
pub async fn run_enrollment(manager_url: &str, config: &super::AppConfig) -> Result<()> {
    let client = EnrollmentClient::new(manager_url);

    // Phase 1: Registration
    tracing::info!(
        manager_url = manager_url,
        "Starting enrollment - registration phase"
    );
    let response = client.register().await?;
    tracing::info!("Registration successful - received polling token");

    // Get polling config (use defaults if not set)
    let interval = config.enrollment.as_ref()
        .map(|e| e.polling_interval_seconds).unwrap_or(60);
    let max_attempts = config.enrollment.as_ref()
        .map(|e| e.max_poll_attempts).unwrap_or(1440);

    // Phase 2: Polling
    tracing::info!(
        interval_seconds = interval,
        max_attempts = max_attempts,
        "Starting enrollment - polling phase"
    );
    let pki_bundle = client.poll_for_approval(&response.polling_token, interval, max_attempts).await?;

    // Phase 3: PKI provisioning & whitelist update
    tracing::info!("Enrollment approved - starting PKI provisioning phase");

    // Write certificates to configured paths (or defaults)
    provision::provision_pki_bundle(
        &pki_bundle.ca_crt,
        &pki_bundle.server_crt,
        &pki_bundle.server_key,
        config.tls_config(),
    ).await?;
    tracing::info!("PKI bundle written to disk");

    // Resolve manager hostname to IP and append to whitelist
    let manager_ip = client.manager_ip().await.context(
        "Failed to resolve manager IP - cannot update whitelist",
    )?;
    provision::append_manager_to_whitelist(&manager_ip, config.whitelist_path()).await?;
    tracing::info!(manager_ip = %manager_ip, "Manager IP appended to whitelist");

    tracing::info!("Enrollment complete - PKI and whitelist configured");
    Ok(())
}
