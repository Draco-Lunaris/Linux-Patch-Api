//! System Management API Handlers
//!
//! Implements REST endpoints for system management operations:
//! - GET /api/v1/system/info - OS version, kernel, last update time
//! - GET /api/v1/health - Health check endpoint
//! - POST /api/v1/system/reboot - System reboot - async

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::packages::ApiResponse;
use crate::auth::crl::{CrlStatus, SharedCrlState};
use crate::jobs::manager::{JobManager, JobOperation, JobStatus};
use crate::packages::{self, PackageManagerBackend, MAX_RESTART_DELAY_SECONDS};

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
    pub last_cache_update: Option<String>, // RFC3339 timestamp
    pub cache_status: String,              // "fresh", "stale", "unknown", "failed"
    pub crl_status: Option<String>,        // "valid", "expired", "missing", "invalid", "degraded"
    pub crl_age_seconds: Option<u64>,      // age of on-disk CRL file
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
}

fn default_true() -> bool {
    true
}
fn default_restart_delay() -> u64 {
    5
}

/// Self-update request
#[derive(Debug, Deserialize, Clone)]
pub struct SelfUpdateRequest {
    /// Pin to an exact package version. None = upgrade to latest available.
    #[serde(default)]
    pub target_version: Option<String>,
    /// Restart the service after a successful upgrade so the new binary runs.
    #[serde(default = "default_true")]
    pub restart: bool,
    /// Seconds to wait before the decoupled restart fires.
    /// Clamped to max 300 (5 minutes) in the handler.
    #[serde(default = "default_restart_delay")]
    pub restart_delay_seconds: u64,
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
            error!(request_id = %request_id, error = %e, "Failed to get system info");
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

    // Check cache status and refresh if stale
    let cache_status_val = cache_state.status();
    let (mut status, cache_status_str, last_cache_update) = if cache_state.is_stale() {
        match backend.refresh_package_cache(&cache_state) {
            Ok(_) => {
                let updated = cache_state.status();
                (
                    "healthy".to_string(),
                    "fresh".to_string(),
                    updated.last_update.map(|dt| dt.to_rfc3339()),
                )
            }
            Err(e) => {
                error!("Health check cache refresh failed: {}", e);
                (
                    "degraded".to_string(),
                    "failed".to_string(),
                    cache_status_val.last_update.map(|dt| dt.to_rfc3339()),
                )
            }
        }
    } else {
        (
            "healthy".to_string(),
            "fresh".to_string(),
            cache_status_val.last_update.map(|dt| dt.to_rfc3339()),
        )
    };

    // CRL status from shared state
    let crl = crl_state.load();
    let crl_status_str = match crl.status {
        CrlStatus::Valid
        | CrlStatus::Expired
        | CrlStatus::Missing
        | CrlStatus::Invalid
        | CrlStatus::Degraded => {
            // Downgrade overall health if CRL is invalid
            if crl.status == CrlStatus::Invalid {
                status = "degraded".to_string();
            }
            crl.status.to_string()
        }
    };
    let crl_age = crl.crl_age_seconds();

    let response = ApiResponse::success(HealthData {
        status,
        uptime_seconds,
        version,
        last_cache_update,
        cache_status: cache_status_str,
        crl_status: Some(crl_status_str),
        crl_age_seconds: crl_age,
    });

    HttpResponse::Ok().json(response)
}

/// Reboot the system (async operation)
pub async fn reboot_system(
    body: web::Json<RebootRequest>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    job_manager: web::Data<JobManager>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let delay = body.delay_seconds;
    let force = body.force;

    info!(
        request_id = %request_id,
        delay_seconds = delay,
        force = force,
        "Initiating system reboot"
    );

    // Check for running jobs unless force is true
    if !force {
        let running_count = job_manager.running_count().await;
        if running_count > 0 {
            warn!(request_id = %request_id, running_jobs = running_count, "Reboot blocked by running jobs");
            let response = ApiResponse::<()>::error(
                "REBOOT_BLOCKED",
                "Cannot reboot while jobs are running. Use force=true to override.",
                Some(serde_json::json!({"running_jobs": running_count})),
                false,
            );
            return HttpResponse::Conflict().json(response);
        }
    }

    // Check job queue capacity
    if !job_manager.can_accept_job().await {
        let response = ApiResponse::<()>::error(
            "QUEUE_FULL",
            "Job queue is at capacity. Please retry later.",
            None,
            true,
        );
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(response);
    }

    // Create async job for reboot
    match job_manager.create_job(JobOperation::Reboot, vec![]).await {
        Ok(job_id) => {
            // Spawn background task to execute the reboot
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
            let delay_clone = delay;

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Update job to running
                let _ = job_manager_clone
                    .update_job(
                        &job_id_clone,
                        JobStatus::Running,
                        Some(0),
                        Some("Preparing system reboot...".to_string()),
                    )
                    .await;
                let _ = job_manager_clone
                    .add_job_log(&job_id_clone, "Job started".to_string())
                    .await;

                // Execute reboot
                match backend_clone.reboot_system(delay_clone) {
                    Ok(_) => {
                        let _ = job_manager_clone
                            .add_job_log(&job_id_clone, "Reboot command executed".to_string())
                            .await;
                        // Note: Job won't complete normally since system reboots
                        info!(job_id = %job_id_clone, "System reboot initiated");
                    }
                    Err(e) => {
                        let _ = job_manager_clone
                            .fail_job(&job_id_clone, e.to_string())
                            .await;
                        error!(job_id = %job_id_clone, error = %e, "System reboot failed");
                    }
                }
            });

            let scheduled_at = if delay > 0 {
                Utc::now() + chrono::Duration::seconds(delay as i64)
            } else {
                Utc::now()
            };

            let response = ApiResponse::success(serde_json::json!({
                "job_id": job_id.to_string(),
                "status": "pending",
                "operation": "reboot",
                "scheduled_at": scheduled_at.to_rfc3339(),
                "delay_seconds": delay,
                "force": force,
            }));

            HttpResponse::Accepted().json(response)
        }
        Err(e) => {
            error!(request_id = %request_id, error = %e, "Failed to create reboot job");
            let response = ApiResponse::<()>::error(
                "JOB_CREATE_ERROR",
                &format!("Failed to create job: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Self-update the agent (async operation)
pub async fn update_self(
    body: web::Json<SelfUpdateRequest>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    job_manager: web::Data<JobManager>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();

    // Validate target_version if present
    if let Some(ref v) = body.target_version {
        if let Err(e) = packages::validate_version_string(v) {
            let response = ApiResponse::<()>::error(
                "INVALID_VERSION",
                &format!("Invalid target version: {}", e),
                None,
                false,
            );
            return HttpResponse::BadRequest().json(response);
        }
    }

    // Clamp restart_delay_seconds
    let restart_delay = body
        .restart_delay_seconds
        .clamp(1, MAX_RESTART_DELAY_SECONDS);
    let restart = body.restart;
    let target_version = body.target_version.clone();

    info!(
        request_id = %request_id,
        target_version = ?target_version,
        restart = restart,
        restart_delay_seconds = restart_delay,
        "Initiating self-update"
    );

    // Check job queue capacity
    if !job_manager.can_accept_job().await {
        let response = ApiResponse::<()>::error(
            "QUEUE_FULL",
            "Job queue is at capacity. Please retry later.",
            None,
            true,
        );
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(response);
    }

    // Create async job for self-update
    match job_manager
        .create_job(JobOperation::SelfUpdate, vec![])
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the self-update
            let backend_for_update = backend.clone();
            let backend_for_restart = backend.clone();
            let job_manager_clone = job_manager.clone();
            let tv_owned = target_version.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Update job to running
                let _ = job_manager_clone
                    .update_job(
                        &job_id_clone,
                        JobStatus::Running,
                        Some(0),
                        Some("Starting self-update...".to_string()),
                    )
                    .await;
                let _ = job_manager_clone
                    .add_job_log(&job_id_clone, "Job started".to_string())
                    .await;

                // Execute self-update in blocking context
                let result = tokio::task::spawn_blocking(move || {
                    let tv_ref = tv_owned.as_deref();
                    backend_for_update.update_self(tv_ref)
                })
                .await;

                match result {
                    Ok(Ok(outcome)) => {
                        if !outcome.changed {
                            // No change needed — persist marker with success, complete job
                            let _ = packages::persist_self_update_marker(
                                &outcome.previous_version,
                                &outcome.new_version,
                                false,
                                "success",
                                None,
                            );
                            let _ = job_manager_clone
                                .add_job_log(&job_id_clone, "Already at target version".to_string())
                                .await;
                            let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        } else if restart {
                            // Changed and restart requested — schedule restart FIRST
                            let restart_result = tokio::task::spawn_blocking(move || {
                                backend_for_restart.schedule_self_restart(restart_delay)
                            })
                            .await;
                            match restart_result {
                                Ok(Ok(())) => {
                                    let _ = packages::persist_self_update_marker(
                                        &outcome.previous_version,
                                        &outcome.new_version,
                                        true,
                                        "success",
                                        None,
                                    );
                                    let _ = job_manager_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            format!(
                                                "Self-update applied; restart scheduled in {}s",
                                                restart_delay
                                            ),
                                        )
                                        .await;
                                    let _ = job_manager_clone.complete_job(&job_id_clone).await;
                                }
                                _ => {
                                    let err_msg = "upgrade applied but restart could not be scheduled; run 'systemctl restart linux-patch-api' manually";
                                    let _ = packages::persist_self_update_marker(
                                        &outcome.previous_version,
                                        &outcome.new_version,
                                        true,
                                        "restart_failed",
                                        Some(err_msg),
                                    );
                                    let _ = job_manager_clone
                                        .fail_job(&job_id_clone, err_msg.to_string())
                                        .await;
                                }
                            }
                        } else {
                            // Changed but no restart — persist marker with success, complete job
                            let _ = packages::persist_self_update_marker(
                                &outcome.previous_version,
                                &outcome.new_version,
                                true,
                                "success",
                                None,
                            );
                            let _ = job_manager_clone
                                .add_job_log(
                                    &job_id_clone,
                                    "Self-update applied; new binary will activate on next restart"
                                        .to_string(),
                                )
                                .await;
                            let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        }
                        info!(job_id = %job_id_clone, "Self-update completed successfully");
                    }
                    Ok(Err(e)) => {
                        error!(job_id = %job_id_clone, error = %e, "Self-update failed");
                        let _ = job_manager_clone
                            .fail_job(&job_id_clone, e.to_string())
                            .await;
                    }
                    Err(e) => {
                        error!(job_id = %job_id_clone, error = %e, "Self-update task join error");
                        let _ = job_manager_clone
                            .fail_job(&job_id_clone, format!("Task join error: {}", e))
                            .await;
                    }
                }
            });

            let response = ApiResponse::success(serde_json::json!({
                "job_id": job_id.to_string(),
                "status": "pending",
                "operation": "self_update",
                "target_version": target_version,
                "restart": restart,
                "restart_delay_seconds": restart_delay,
            }));

            HttpResponse::Accepted().json(response)
        }
        Err(e) => {
            error!(request_id = %request_id, error = %e, "Failed to create self-update job");
            let response = ApiResponse::<()>::error(
                "JOB_CREATE_ERROR",
                &format!("Failed to create job: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Get self-update status from marker file
pub async fn get_self_update_status(_req: HttpRequest) -> impl Responder {
    match packages::read_self_update_marker() {
        Some(data) => {
            let response = ApiResponse::success(data);
            HttpResponse::Ok().json(response)
        }
        None => {
            let response = ApiResponse::<()>::error(
                "NO_SELF_UPDATE_RECORD",
                "No self-update record found",
                None,
                false,
            );
            HttpResponse::NotFound().json(response)
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
                error = %e,
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
            .route("/update", web::post().to(update_self))
            .route("/update/status", web::get().to(get_self_update_status))
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
    }

    #[test]
    fn test_reboot_request_full() {
        let json = r#"{"delay_seconds": 60, "force": true}"#;
        let request: RebootRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.delay_seconds, 60);
        assert!(request.force);
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
        };
        let json = serde_json::to_string(&health).unwrap();
        assert!(json.contains("healthy"));
        assert!(json.contains("12345"));
        assert!(json.contains("fresh"));
        assert!(json.contains("last_cache_update"));
    }

    #[test]
    fn test_self_update_request_defaults() {
        let json = r#"{}"#;
        let request: SelfUpdateRequest = serde_json::from_str(json).unwrap();
        assert!(request.target_version.is_none());
        assert!(request.restart);
        assert_eq!(request.restart_delay_seconds, 5);
    }

    #[test]
    fn test_self_update_request_all_fields() {
        let json = r#"{"target_version": "1.5.0", "restart": false, "restart_delay_seconds": 30}"#;
        let request: SelfUpdateRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.target_version, Some("1.5.0".to_string()));
        assert!(!request.restart);
        assert_eq!(request.restart_delay_seconds, 30);
    }

    #[test]
    fn test_self_update_request_version_validation_valid() {
        assert!(packages::validate_version_string("1.5.0").is_ok());
        assert!(packages::validate_version_string("2:1.0-1").is_ok());
        assert!(packages::validate_version_string("1.0~beta1").is_ok());
    }

    #[test]
    fn test_self_update_request_version_validation_invalid() {
        assert!(packages::validate_version_string("").is_err());
        assert!(packages::validate_version_string("-1.0").is_err());
        assert!(packages::validate_version_string("1.0;rm -rf").is_err());
        assert!(packages::validate_version_string("1.0$(cmd)").is_err());
        assert!(packages::validate_version_string("1.0/evil").is_err());
    }

    #[test]
    fn test_self_update_request_restart_delay_clamp() {
        // Verify clamping logic (handler does the clamp, but test the bounds)
        let max = MAX_RESTART_DELAY_SECONDS;
        assert_eq!(0u64.clamp(1, max), 1);
        assert_eq!(500u64.clamp(1, max), 300);
        assert_eq!(30u64.clamp(1, max), 30);
    }
}
