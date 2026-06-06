//! Patch Management API Handlers
//!
//! Implements REST endpoints for patch management operations:
//! - GET /api/v1/patches - List available patches
//! - POST /api/v1/patches/apply - Apply patches - async

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use crate::jobs::manager::{JobManager, JobOperation, JobStatus};
use crate::packages::{validate_package_name, PackageManagerBackend};

use super::packages::{ApiResponse, JobResponseData};

/// Patch list response data
#[derive(Debug, Serialize)]
pub struct PatchListData {
    pub patches: Vec<crate::packages::Patch>,
    pub total: usize,
    pub security_updates: usize,
    pub requires_reboot: bool,
}

/// Patch apply request
#[derive(Debug, Deserialize, Clone)]
pub struct PatchApplyRequest {
    #[serde(default)]
    pub packages: Option<Vec<String>>,
    #[serde(default)]
    pub reboot: bool,
    #[serde(default)]
    pub reboot_delay_seconds: u64,
}

/// List available patches
pub async fn list_patches(
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();

    info!(request_id = %request_id, "Listing available patches");

    match backend.list_patches() {
        Ok(patches) => {
            let total = patches.len();
            let security_updates = patches
                .iter()
                .filter(|p| p.severity == "critical" || p.severity == "high")
                .count();
            let requires_reboot = patches.iter().any(|p| p.name.contains("kernel"));

            let response = ApiResponse::success(PatchListData {
                patches,
                total,
                security_updates,
                requires_reboot,
            });

            HttpResponse::Ok().json(response)
        }
        Err(e) => {
            error!(request_id = %request_id, error = %e, "Failed to list patches");
            let response = ApiResponse::<()>::error(
                "PKG_MANAGER_ERROR",
                &format!("Failed to list patches: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Apply patches (async operation)
pub async fn apply_patches(
    body: web::Json<PatchApplyRequest>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    job_manager: web::Data<JobManager>,
    cache_state: web::Data<crate::packages::cache::PackageCacheState>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let packages_count = body.packages.as_ref().map(|p| p.len()).unwrap_or(0);

    // SECURITY: Validate all package names in the request to prevent argument injection
    if let Some(ref pkgs) = body.packages {
        for pkg in pkgs {
            if let Err(e) = validate_package_name(pkg) {
                let response = ApiResponse::<()>::error("VALIDATION_ERROR", &e, None, false);
                return HttpResponse::BadRequest().json(response);
            }
        }
    }

    info!(
        request_id = %request_id,
        packages = ?body.packages,
        reboot = body.reboot,
        "Applying patches"
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

    // Create async job
    let package_list = body.packages.clone().unwrap_or_default();
    match job_manager
        .create_job(JobOperation::PatchApply, package_list)
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the patching
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
            let cache_state_clone = cache_state.clone();
            let request = body.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Update job to running
                let _ = job_manager_clone
                    .update_job(
                        &job_id_clone,
                        JobStatus::Running,
                        Some(0),
                        Some("Starting patch application...".to_string()),
                    )
                    .await;
                let _ = job_manager_clone
                    .add_job_log(&job_id_clone, "Job started".to_string())
                    .await;

                // MANDATORY: Refresh package cache before applying patches
                let _ = job_manager_clone
                    .update_job(
                        &job_id_clone,
                        JobStatus::Running,
                        Some(0),
                        Some("Refreshing package index...".to_string()),
                    )
                    .await;
                let _ = job_manager_clone
                    .add_job_log(&job_id_clone, "Refreshing package cache...".to_string())
                    .await;

                match backend_clone.refresh_package_cache(&cache_state_clone) {
                    Ok(_) => {
                        let _ = job_manager_clone
                            .add_job_log(
                                &job_id_clone,
                                "Package cache refreshed successfully".to_string(),
                            )
                            .await;
                        let _ = job_manager_clone
                            .update_job(
                                &job_id_clone,
                                JobStatus::Running,
                                Some(10),
                                Some("Cache refreshed, applying patches...".to_string()),
                            )
                            .await;
                    }
                    Err(e) => {
                        let err_msg = format!("Package cache refresh failed: {}", e);
                        error!(job_id = %job_id_clone, error = %e, "Cache refresh failed");
                        let _ = job_manager_clone
                            .add_job_log(&job_id_clone, err_msg.clone())
                            .await;
                        let _ = job_manager_clone.fail_job(&job_id_clone, err_msg).await;
                        return; // Exit the spawned task
                    }
                }

                // Execute patching with 404 retry
                let packages_ref = request.packages.as_deref();
                let apply_result = backend_clone.apply_patches(packages_ref);

                match apply_result {
                    Ok(_) => {
                        let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, "Patch application completed");

                        // Handle reboot if requested
                        if request.reboot {
                            let _ = job_manager_clone
                                .add_job_log(
                                    &job_id_clone,
                                    format!(
                                        "Reboot scheduled in {} seconds",
                                        request.reboot_delay_seconds
                                    ),
                                )
                                .await;
                            // Trigger actual reboot via system handler
                            match backend_clone.reboot_system(request.reboot_delay_seconds) {
                                Ok(_) => {
                                    let _ = job_manager_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            "Reboot command executed".to_string(),
                                        )
                                        .await;
                                }
                                Err(e) => {
                                    let _ = job_manager_clone
                                        .add_job_log(&job_id_clone, format!("Reboot failed: {}", e))
                                        .await;
                                }
                            }
                        }
                    }
                    Err(e) if crate::packages::cache::is_fetch_error(&e) => {
                        // 404/fetch error: refresh cache and retry once
                        info!(job_id = %job_id_clone, "Patch apply failed with fetch error, refreshing cache and retrying");
                        let _ = job_manager_clone
                            .add_job_log(
                                &job_id_clone,
                                "Fetch error detected, refreshing cache and retrying..."
                                    .to_string(),
                            )
                            .await;

                        match backend_clone.refresh_package_cache(&cache_state_clone) {
                            Ok(_) => {
                                let _ = job_manager_clone
                                    .add_job_log(
                                        &job_id_clone,
                                        "Cache refreshed, retrying patch apply...".to_string(),
                                    )
                                    .await;
                            }
                            Err(refresh_err) => {
                                let err_msg =
                                    format!("Cache refresh on retry failed: {}", refresh_err);
                                let _ = job_manager_clone.fail_job(&job_id_clone, err_msg).await;
                                error!(job_id = %job_id_clone, error = %refresh_err, "Cache refresh on retry failed");
                                return;
                            }
                        }

                        // Retry the apply
                        match backend_clone.apply_patches(packages_ref) {
                            Ok(_) => {
                                let _ = job_manager_clone.complete_job(&job_id_clone).await;
                                info!(job_id = %job_id_clone, "Patch application completed after retry");

                                // Handle reboot if requested
                                if request.reboot {
                                    let _ = job_manager_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            format!(
                                                "Reboot scheduled in {} seconds",
                                                request.reboot_delay_seconds
                                            ),
                                        )
                                        .await;
                                    match backend_clone.reboot_system(request.reboot_delay_seconds)
                                    {
                                        Ok(_) => {
                                            let _ = job_manager_clone
                                                .add_job_log(
                                                    &job_id_clone,
                                                    "Reboot command executed".to_string(),
                                                )
                                                .await;
                                        }
                                        Err(e) => {
                                            let _ = job_manager_clone
                                                .add_job_log(
                                                    &job_id_clone,
                                                    format!("Reboot failed: {}", e),
                                                )
                                                .await;
                                        }
                                    }
                                }
                            }
                            Err(retry_err) => {
                                let _ = job_manager_clone
                                    .fail_job(&job_id_clone, retry_err.to_string())
                                    .await;
                                error!(job_id = %job_id_clone, error = %retry_err, "Patch application failed after retry");
                            }
                        }
                    }
                    Err(e) => {
                        // Non-fetch error: fail immediately
                        let _ = job_manager_clone
                            .fail_job(&job_id_clone, e.to_string())
                            .await;
                        error!(job_id = %job_id_clone, error = %e, "Patch application failed");
                    }
                }
            });

            let response = ApiResponse::success(JobResponseData {
                job_id: job_id.to_string(),
                status: "pending".to_string(),
                operation: "patch_apply".to_string(),
                packages: Some(vec![format!("{} packages", packages_count)]),
                package: None,
            });

            HttpResponse::Accepted().json(response)
        }
        Err(e) => {
            error!(request_id = %request_id, error = %e, "Failed to create job");
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

/// Configure all patch routes
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/patches")
            .route("", web::get().to(list_patches))
            .route("/apply", web::post().to(apply_patches)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_patch_apply_request_default() {
        let json = r#"{}"#;
        let request: PatchApplyRequest = serde_json::from_str(json).unwrap();
        assert!(request.packages.is_none());
        assert!(!request.reboot);
        assert_eq!(request.reboot_delay_seconds, 0);
    }

    #[test]
    fn test_patch_apply_request_full() {
        let json = r#"{"packages": ["pkg1", "pkg2"], "reboot": true, "reboot_delay_seconds": 60}"#;
        let request: PatchApplyRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.packages.unwrap().len(), 2);
        assert!(request.reboot);
        assert_eq!(request.reboot_delay_seconds, 60);
    }
}
