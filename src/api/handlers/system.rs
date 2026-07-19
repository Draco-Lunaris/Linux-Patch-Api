//! System Management API Handlers
//!
//! Implements REST endpoints for system management operations:
//! - GET /api/v1/system/info - OS version, kernel, last update time
//! - GET /api/v1/health - Health check endpoint
//! - POST /api/v1/system/reboot - System reboot - async

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::jobs::manager::JobOperation;
use crate::jobs::scheduler::{AdmissionMode, Scheduler};

use super::packages::ApiResponse;
use crate::auth::crl::{CrlStatus, SharedCrlState};
use crate::enroll::SharedRepoSyncState;
use crate::packages::PackageManagerBackend;

/// Normalize and validate file paths to prevent path traversal attacks (VULN-002)
/// Returns None if path contains traversal patterns
#[allow(dead_code)]
fn validate_path_no_traversal(path: &str) -> bool {
    // Validate path - check for traversal patterns
    if path.contains("..") || path.contains("//") {
        return false;
    }
    true
}

/// System info response data
#[derive(Debug, Serialize)]
pub struct SystemInfoData {
    pub hostname: String,
    pub os: String,
    pub os_version: String,
    pub kernel: String,
    pub architecture: String,
    pub last_update_check: Option<String>,
    pub last_update_apply: Option<String>,
    pub pending_reboot: bool,
}

/// Health check response data
#[derive(Debug, Serialize)]
pub struct HealthData {
    pub status: String, // "healthy" or "degraded"
    pub uptime_seconds: u64,
    pub version: String,
    pub last_cache_update: Option<String>,  // RFC3339 timestamp
    pub cache_status: String,               // "fresh", "stale", "unknown", "failed"
    pub crl_status: Option<String>,         // "valid", "expired", "missing", "invalid", "degraded"
    pub crl_age_seconds: Option<u64>,       // age of on-disk CRL file
    pub crl_next_update: Option<String>,    // RFC3339 timestamp of CRL nextUpdate
    pub gpg_key_status: Option<String>,     // "valid", "expired", "missing", "revoked"
    pub gpg_key_expires_at: Option<String>, // RFC3339 timestamp of GPG key expiry
    pub repo_config_synced: Option<bool>,   // true if last sync matched manager
    pub repo_config_last_sync: Option<String>, // RFC3339 timestamp of last sync
}

/// Service status response data
#[derive(Debug, Serialize)]
pub struct ServiceStatusData {
    pub name: String,
    pub display_name: String,
    pub active_state: String,
    pub sub_state: String,
    pub load_state: String,
    pub enabled_state: String,
    pub main_pid: Option<u32>,
    pub healthy: bool,
}

/// Reboot request
#[derive(Debug, Deserialize, Clone)]
pub struct RebootRequest {
    #[serde(default)]
    pub delay_seconds: u64,
    #[serde(default)]
    pub force: bool,
    /// Required when force=true and a package-manager operation or self-update
    /// is in progress. The caller must explicitly acknowledge that a forced
    /// reboot during package-database mutation may corrupt dpkg/rpm/pacman
    /// state, leaving the system unbootable.
    ///
    /// Without this flag, force=true bypasses ordinary active-job protection
    /// (pending/running jobs) but does NOT bypass the self-update or
    /// package-operation guard — those require this explicit acknowledgment.
    #[serde(default)]
    pub acknowledge_package_database_corruption_risk: bool,
}

/// Get system information
pub async fn get_system_info(
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();

    info!(request_id = %request_id, "Getting system information");

    match backend.get_system_info() {
        Ok(sys_info) => {
            let response = ApiResponse::success(SystemInfoData {
                hostname: sys_info.hostname,
                os: sys_info.os,
                os_version: sys_info.os_version,
                kernel: sys_info.kernel,
                architecture: sys_info.architecture,
                last_update_check: sys_info.last_update_check,
                last_update_apply: sys_info.last_update_apply,
                pending_reboot: sys_info.pending_reboot,
            });

            HttpResponse::Ok().json(response)
        }
        Err(e) => {
            error!(request_id = %request_id, error = ?e, "Failed to get system info");
            let response = ApiResponse::<()>::error(
                "SYSTEM_INFO_ERROR",
                &format!("Failed to get system info: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Health check endpoint
pub async fn health_check(
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    cache_state: web::Data<crate::packages::cache::PackageCacheState>,
    crl_state: web::Data<SharedCrlState>,
    repo_sync_state: web::Data<SharedRepoSyncState>,
    scheduler: web::Data<Arc<Scheduler>>,
    _req: HttpRequest,
) -> impl Responder {
    let _request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();

    // Calculate uptime from /proc/uptime
    let uptime_seconds = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|content| {
            content
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .map(|f| f as u64)
        })
        .unwrap_or(0);

    let version = env!("CARGO_PKG_VERSION").to_string();

    // Check cache status — report stale without synchronously refreshing.
    // Health checks must NOT mutate package state. If the cache is stale,
    // we spawn an async refresh task (best-effort) and report the current
    // status immediately. The refresh goes through `dispatch_mutation`
    // — the scheduler's SOLE production mutation entry point.
    let cache_status_val = cache_state.status();
    let (mut status, cache_status_str, last_cache_update) = if cache_state.is_stale() {
        // Spawn a best-effort background refresh using the scheduler's
        // dispatch_mutation. This deduplicates refreshes (only one can
        // hold the mutation slot at a time) and never blocks the health
        // response.
        let backend_clone = backend.clone();
        let cache_state_clone = cache_state.clone();
        let scheduler_clone = scheduler.clone();
        actix_web::rt::spawn(async move {
            // Admit a tracking job and dispatch through the scheduler.
            match scheduler_clone
                .admit_job(
                    JobOperation::Install,
                    vec!["__health_refresh__".to_string()],
                )
                .await
            {
                Ok(tracking_job_id) => {
                    let backend_for_refresh = backend_clone.clone();
                    let cache_state_for_refresh = cache_state_clone.clone();
                    let refresh_result = scheduler_clone
                        .dispatch_mutation(tracking_job_id, move || {
                            backend_for_refresh.refresh_package_cache(&cache_state_for_refresh)
                        })
                        .await;
                    match refresh_result {
                        Ok(_) => {
                            info!("Background cache refresh from health check succeeded");
                            let _ = scheduler_clone.complete_job(&tracking_job_id).await;
                            let _ = scheduler_clone.delete_job(&tracking_job_id).await;
                        }
                        Err(e) => {
                            warn!(error = ?e, "Background cache refresh from health check failed");
                            let _ = scheduler_clone
                                .fail_job(&tracking_job_id, e.to_string())
                                .await;
                            let _ = scheduler_clone.delete_job(&tracking_job_id).await;
                        }
                    }
                }
                Err(e) => {
                    info!(error = ?e, "Background cache refresh from health check skipped — could not admit tracking job");
                }
            }
        });
        (
            "healthy".to_string(),
            "stale".to_string(),
            cache_status_val.last_update.map(|dt| dt.to_rfc3339()),
        )
    } else {
        (
            "healthy".to_string(),
            "fresh".to_string(),
            cache_status_val.last_update.map(|dt| dt.to_rfc3339()),
        )
    };

    // Check if the scheduler is in recovery mode. If so, report degraded.
    let admission_mode = scheduler.admission_mode().await;
    if admission_mode == AdmissionMode::Recovery {
        status = "degraded".to_string();
    }
    let self_update_active = scheduler.is_self_update_in_progress().await;
    if self_update_active {
        status = "degraded".to_string();
    }

    // CRL status from shared state — re-evaluate expiry at query time
    let crl = crl_state.load();
    let effective_crl_status = crl.reevaluate_expiry();
    let crl_status_str = match effective_crl_status {
        CrlStatus::Valid
        | CrlStatus::Expired
        | CrlStatus::Missing
        | CrlStatus::Invalid
        | CrlStatus::Degraded => {
            // Downgrade overall health if CRL is invalid
            if effective_crl_status == CrlStatus::Invalid {
                status = "degraded".to_string();
            }
            // Also downgrade if CRL is expired (stale revocation data)
            if effective_crl_status == CrlStatus::Expired {
                status = "degraded".to_string();
            }
            effective_crl_status.to_string()
        }
    };
    let crl_age = crl.crl_age_seconds();

    // Convert next_update SystemTime to RFC3339 string for health payload
    let crl_next_update = crl.next_update.and_then(|nu| {
        use std::time::UNIX_EPOCH;
        nu.duration_since(UNIX_EPOCH).ok().and_then(|d| {
            let secs = d.as_secs() as i64;
            chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339())
        })
    });

    // GPG key health — check the provisioned repo keyring.
    // The manager uses this to determine whether the agent's repo is signed
    // by a valid, non-expired GPG key (issue #126 HIGH-1).
    let (gpg_key_status_enum, gpg_key_expires_at) = crate::enroll::check_gpg_key_health();
    let gpg_key_status = Some(gpg_key_status_enum.as_str().to_string());

    // Downgrade overall health if GPG key is missing or expired.
    if gpg_key_status_enum == crate::enroll::GpgKeyStatus::Missing
        || gpg_key_status_enum == crate::enroll::GpgKeyStatus::Expired
        || gpg_key_status_enum == crate::enroll::GpgKeyStatus::Revoked
    {
        status = "degraded".to_string();
    }

    // Repo config sync status from the background reconciliation task.
    // `synced=Some(false)` means the last sync attempt failed (network
    // error or provisioning failure) — downgrade to degraded so the
    // manager can see which agents are out of sync.
    let repo_sync = repo_sync_state.load();
    let repo_config_synced = repo_sync.synced;
    let repo_config_last_sync = repo_sync.last_sync_at.and_then(|t| {
        use std::time::UNIX_EPOCH;
        t.duration_since(UNIX_EPOCH).ok().and_then(|d| {
            let secs = d.as_secs() as i64;
            chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339())
        })
    });
    if repo_config_synced == Some(false) {
        status = "degraded".to_string();
    }

    let response = ApiResponse::success(HealthData {
        status,
        uptime_seconds,
        version,
        last_cache_update,
        cache_status: cache_status_str,
        crl_status: Some(crl_status_str),
        crl_age_seconds: crl_age,
        crl_next_update,
        gpg_key_status,
        gpg_key_expires_at,
        repo_config_synced,
        repo_config_last_sync,
    });

    HttpResponse::Ok().json(response)
}

/// Reboot the system (async operation)
pub async fn reboot_system(
    body: web::Json<RebootRequest>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    scheduler: web::Data<Arc<Scheduler>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let delay = body.delay_seconds;
    let force = body.force;
    let ack_corruption_risk = body.acknowledge_package_database_corruption_risk;

    info!(
        request_id = %request_id,
        delay_seconds = delay,
        force = force,
        ack_corruption_risk = ack_corruption_risk,
        "Initiating system reboot"
    );

    // Use the scheduler's atomic admit_reboot — all checks and job
    // creation happen under one lock, preventing races between the
    // reboot check and concurrent job/self-update/mutation creation.
    match scheduler.admit_reboot(force, ack_corruption_risk).await {
        Ok(handle) => {
            let reboot_job_id = handle.job_id;
            // Spawn background task to execute the reboot
            let backend_clone = backend.clone();
            let scheduler_clone = scheduler.clone();
            let delay_clone = delay;

            tokio::spawn(async move {
                // Transition the reboot job to Running immediately
                // before invoking the backend reboot command. This
                // is ownership-safe: only the current reboot owner
                // can perform the transition.
                if !scheduler_clone.begin_reboot_execution(reboot_job_id).await {
                    // The reservation was rolled back by a stale
                    // owner or the job is no longer Pending. Either
                    // way, we cannot proceed.
                    error!(
                        job_id = %reboot_job_id,
                        "Could not transition reboot job to Running — reservation no longer owned"
                    );
                    return;
                }
                // Execute reboot — the reboot job is now Running.
                match backend_clone.reboot_system(delay_clone) {
                    Ok(_) => {
                        let _ = scheduler_clone
                            .add_job_log(&reboot_job_id, "Reboot command executed".to_string())
                            .await;
                        // Note: Job won't complete normally since system reboots
                        info!(job_id = %reboot_job_id, "System reboot initiated");
                    }
                    Err(e) => {
                        // Reboot command failed — roll back the reservation
                        // so the scheduler reopens admission.
                        let _ = scheduler_clone
                            .rollback_reboot(reboot_job_id, Some(format!("{}", e)))
                            .await;
                        error!(job_id = %reboot_job_id, error = %e, "System reboot failed");
                    }
                }
            });

            let scheduled_at = if delay > 0 {
                Utc::now() + chrono::Duration::seconds(delay as i64)
            } else {
                Utc::now()
            };

            let response = ApiResponse::success(serde_json::json!({
                "job_id": reboot_job_id.to_string(),
                "status": "pending",
                "operation": "reboot",
                "scheduled_at": scheduled_at.to_rfc3339(),
                "delay_seconds": delay,
                "force": force,
            }));

            HttpResponse::Accepted().json(response)
        }
        Err(ref admission_err) => {
            warn!(request_id = %request_id, error = %admission_err, "Reboot admission rejected");
            let (code, message, data, retry) = match admission_err {
                crate::jobs::scheduler::RebootAdmissionError::SelfUpdateInProgress => (
                    "SELF_UPDATE_IN_PROGRESS",
                    "Cannot reboot while a self-update is in progress. Use force=true with acknowledge_package_database_corruption_risk=true to override.".to_string(),
                    None,
                    false,
                ),
                crate::jobs::scheduler::RebootAdmissionError::PackageMutationInProgress => (
                    "PACKAGE_DB_MUTATION_IN_PROGRESS",
                    "A package-manager operation is in progress. A forced reboot now may corrupt the package database. Set acknowledge_package_database_corruption_risk=true to override.".to_string(),
                    Some(serde_json::json!({"package_operation_in_progress": true})),
                    false,
                ),
                crate::jobs::scheduler::RebootAdmissionError::JobsInProgress { count } => (
                    "REBOOT_BLOCKED",
                    format!("Cannot reboot while {} jobs are running or pending. Use force=true to override.", count),
                    Some(serde_json::json!({"active_jobs": count})),
                    false,
                ),
                crate::jobs::scheduler::RebootAdmissionError::QueueFull => (
                    "QUEUE_FULL",
                    "Job queue is at capacity. Please retry later.".to_string(),
                    None,
                    true,
                ),
                crate::jobs::scheduler::RebootAdmissionError::AdmissionClosed => (
                    "ADMISSION_CLOSED",
                    "Admission is closed (shutdown or recovery in progress).".to_string(),
                    None,
                    false,
                ),
            };
            let response = ApiResponse::<()>::error(code, &message, data, retry);
            HttpResponse::Conflict().json(response)
        }
    }
}

/// Get service status
pub async fn get_service_status(
    path: web::Path<String>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let service_name = path.into_inner();

    info!(
        request_id = %request_id,
        service = %service_name,
        "Getting service status"
    );

    // Validate service name
    if service_name.is_empty() || service_name.contains('/') || service_name.contains("..") {
        let response = ApiResponse::<()>::error(
            "INVALID_SERVICE_NAME",
            &format!("Invalid service name: {}", service_name),
            None,
            false,
        );
        return HttpResponse::BadRequest().json(response);
    }

    match backend.get_service_status(&service_name) {
        Ok(Some(status)) => {
            let response = ApiResponse::success(ServiceStatusData {
                name: status.name,
                display_name: status.display_name,
                active_state: status.active_state,
                sub_state: status.sub_state,
                load_state: status.load_state,
                enabled_state: status.enabled_state,
                main_pid: status.main_pid,
                healthy: status.healthy,
            });
            HttpResponse::Ok().json(response)
        }
        Ok(None) => {
            let response = ApiResponse::<()>::error(
                "SERVICE_NOT_FOUND",
                &format!("Service '{}' not found", service_name),
                None,
                false,
            );
            HttpResponse::NotFound().json(response)
        }
        Err(e) => {
            error!(
                request_id = %request_id,
                service = %service_name,
                error = ?e,
                "Failed to get service status"
            );
            let response = ApiResponse::<()>::error(
                "SERVICE_STATUS_ERROR",
                &format!("Failed to get service status: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Configure routes for system endpoints
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/system")
            .route("/info", web::get().to(get_system_info))
            .route("/reboot", web::post().to(reboot_system))
            .route("/services/{name}", web::get().to(get_service_status)),
    )
    .route("/health", web::get().to(health_check));
    // Note: health_check receives backend and cache_state via app_data injection
    // They are registered in routes.rs and main.rs as web::Data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reboot_request_default() {
        let json = r#"{}"#;
        let request: RebootRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.delay_seconds, 0);
        assert!(!request.force);
        assert!(!request.acknowledge_package_database_corruption_risk);
    }

    #[test]
    fn test_reboot_request_full() {
        let json = r#"{"delay_seconds": 60, "force": true}"#;
        let request: RebootRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.delay_seconds, 60);
        assert!(request.force);
        assert!(!request.acknowledge_package_database_corruption_risk);
    }

    #[test]
    fn test_reboot_request_with_ack() {
        let json = r#"{"delay_seconds": 0, "force": true, "acknowledge_package_database_corruption_risk": true}"#;
        let request: RebootRequest = serde_json::from_str(json).unwrap();
        assert!(request.force);
        assert!(request.acknowledge_package_database_corruption_risk);
    }

    #[test]
    fn test_health_data_serialization() {
        let health = HealthData {
            status: "healthy".to_string(),
            uptime_seconds: 12345,
            version: "0.1.0".to_string(),
            last_cache_update: Some("2026-05-27T14:00:00+00:00".to_string()),
            cache_status: "fresh".to_string(),
            crl_status: Some("valid".to_string()),
            crl_age_seconds: Some(3600),
            crl_next_update: Some("2026-05-28T14:00:00+00:00".to_string()),
            gpg_key_status: Some("valid".to_string()),
            gpg_key_expires_at: Some("2027-01-01T00:00:00+00:00".to_string()),
            repo_config_synced: Some(true),
            repo_config_last_sync: Some("2026-07-18T00:00:00+00:00".to_string()),
        };
        let json = serde_json::to_string(&health).unwrap();
        assert!(json.contains("healthy"));
        assert!(json.contains("12345"));
        assert!(json.contains("fresh"));
        assert!(json.contains("last_cache_update"));
        assert!(json.contains("gpg_key_status"));
        assert!(json.contains("gpg_key_expires_at"));
        assert!(json.contains("repo_config_synced"));
        assert!(json.contains("repo_config_last_sync"));
    }
}
