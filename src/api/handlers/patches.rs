//! Patch Management API Handlers
//!
//! Implements REST endpoints for patch management operations:
//! - GET /api/v1/patches - List available patches
//! - POST /api/v1/patches/apply - Apply patches - async
//!
//! The patch-apply flow is a multi-stage transaction (cache refresh →
//! apply → optional retry). The entire transaction is dispatched to
//! the scheduler as a single closure so the mutation slot is held for
//! the full sequence. The scheduler's `dispatch_mutation` is the SOLE
//! production mutation entry point; this handler never calls
//! `run_mutation`/`try_run_mutation`/`wait_and_start_job`/`start_job`.

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::jobs::manager::JobOperation;
use crate::jobs::scheduler::Scheduler;
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
    backend: web::Data<Arc<dyn PackageManagerBackend>>,
    cache_state: web::Data<crate::packages::cache::PackageCacheState>,
    scheduler: web::Data<Arc<Scheduler>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();

    info!(request_id = %request_id, "Listing available patches");

    // Refresh package cache if stale so the manager sees current patch data.
    // We spawn a best-effort background refresh that goes through
    // `dispatch_mutation` — the scheduler's SOLE production mutation
    // entry point. This is a non-blocking admission: if a mutation is
    // already in progress, the refresh waits via Notify. Health/patch-
    // list refreshes obey reboot admission because they are routed
    // through `dispatch_mutation`.
    if cache_state.is_stale() {
        info!(request_id = %request_id, "Package cache stale, scheduling background refresh through scheduler");
        let backend_clone = backend.clone();
        let cache_state_clone = cache_state.clone();
        let scheduler_clone = scheduler.clone();
        actix_web::rt::spawn(async move {
            // Create a no-op tracking job to anchor the dispatch on the
            // scheduler's job map (so the watchdog can finalize it).
            // We use `admit_job` for the tracking job and then
            // `dispatch_mutation` to acquire the mutation slot.
            match scheduler_clone
                .admit_job(
                    JobOperation::Install,
                    vec!["__patch_list_refresh__".to_string()],
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
                        Ok(_) => info!("Background cache refresh from patch-list succeeded"),
                        Err(e) => {
                            warn!(error = ?e, "Background cache refresh from patch-list failed (admission rejected or execution failed)");
                            // Best-effort: clear the tracking job so it
                            // doesn't pollute the job map.
                            let _ = scheduler_clone.delete_job(&tracking_job_id).await;
                        }
                    }
                }
                Err(e) => {
                    // Could not admit a tracking job — likely a reboot
                    // is reserved or admission is frozen. Skip silently.
                    info!(error = ?e, "Background cache refresh from patch-list skipped — could not admit tracking job");
                }
            }
        });
    }

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
            error!(request_id = %request_id, error = ?e, "Failed to list patches");
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

/// Apply patches (async operation).
///
/// The entire patch transaction (initial cache refresh → apply → optional
/// retry refresh → retry apply) is dispatched to the scheduler as ONE
/// closure passed to `dispatch_mutation`. The mutation slot is held
/// for the full sequence, so no other package job can interleave.
pub async fn apply_patches(
    body: web::Json<PatchApplyRequest>,
    backend: web::Data<Arc<dyn PackageManagerBackend>>,
    scheduler: web::Data<Arc<Scheduler>>,
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

    // Atomically admit the job — checks self-update flag, reboot
    // reservation, and queue capacity under one lock to prevent race
    // with self-update reservation or reboot admission.
    let package_list = body.packages.clone().unwrap_or_default();
    match scheduler
        .admit_job(JobOperation::PatchApply, package_list)
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the multi-stage patch
            // transaction. The closure passed to `dispatch_mutation`
            // contains the entire transaction — no inter-stage release
            // of the mutation slot.
            let backend_clone = backend.clone();
            let scheduler_clone = scheduler.clone();
            let cache_state_clone = cache_state.clone();
            let request = body.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                let _ = scheduler_clone
                    .add_job_log(&job_id_clone, "Refreshing package cache...".to_string())
                    .await;

                // Build the multi-stage transaction closure. It owns
                // the mutation slot for the entire duration:
                //
                //   1. Refresh package cache
                //   2. Apply patches
                //   3. If fetch error: refresh again and retry apply
                //   4. (Optional) reboot — handled outside the
                //      mutation slot because reboot is a system
                //      operation, not a package-manager command.
                let backend_for_tx = backend_clone.clone();
                let cache_state_for_tx = cache_state_clone.clone();
                let request_for_tx = request.clone();

                let patch_result = scheduler_clone
                    .dispatch_mutation(job_id_clone, move || {
                        // Stage 1: initial cache refresh
                        if let Err(e) = backend_for_tx.refresh_package_cache(&cache_state_for_tx) {
                            return Err(e).context("initial cache refresh failed");
                        }

                        // Stage 2: apply patches
                        let apply_result = {
                            let packages_ref: Option<&[String]> =
                                request_for_tx.packages.as_deref();
                            backend_for_tx.apply_patches(packages_ref)
                        };

                        match apply_result {
                            Ok(()) => Ok(()),
                            Err(e) if crate::packages::cache::is_fetch_error(&e) => {
                                // Stage 3: retry — refresh cache and re-apply
                                if let Err(refresh_err) =
                                    backend_for_tx.refresh_package_cache(&cache_state_for_tx)
                                {
                                    return Err(refresh_err).context("retry cache refresh failed");
                                }
                                let packages_ref: Option<&[String]> =
                                    request_for_tx.packages.as_deref();
                                backend_for_tx
                                    .apply_patches(packages_ref)
                                    .context("retry patch apply failed")
                            }
                            Err(e) => Err(e),
                        }
                    })
                    .await;

                match patch_result {
                    Ok(()) => {
                        let _ = scheduler_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, "Patch application completed");

                        // Reboot is requested — handle it OUTSIDE the
                        // mutation slot because reboot is a system
                        // operation, not a package-manager command.
                        // We acquire a reboot reservation; the
                        // reservation guard rolls back automatically
                        // if the reboot command fails.
                        if request.reboot {
                            let _ = scheduler_clone
                                .add_job_log(
                                    &job_id_clone,
                                    format!(
                                        "Reboot scheduled in {} seconds",
                                        request.reboot_delay_seconds
                                    ),
                                )
                                .await;
                            match scheduler_clone.reserve_reboot(true, false).await {
                                Ok(guard) => {
                                    let reboot_job_id = guard.job_id;
                                    let _ = scheduler_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            "Reboot reservation acquired by scheduler".to_string(),
                                        )
                                        .await;
                                    // Transition the reboot job to
                                    // Running before invoking the
                                    // backend reboot command.
                                    if !scheduler_clone.begin_reboot_execution(reboot_job_id).await
                                    {
                                        let _ = scheduler_clone
                                            .add_job_log(
                                                &job_id_clone,
                                                "Reboot reservation lost before command"
                                                    .to_string(),
                                            )
                                            .await;
                                        // Guard drop will roll back.
                                    } else {
                                        match backend_clone
                                            .reboot_system(request.reboot_delay_seconds)
                                        {
                                            Ok(_) => {
                                                let _ = scheduler_clone
                                                    .add_job_log(
                                                        &job_id_clone,
                                                        "Reboot command executed".to_string(),
                                                    )
                                                    .await;
                                                // Commit: process is about to terminate.
                                                let _ = guard.commit();
                                            }
                                            Err(e) => {
                                                // Reboot command failed —
                                                // roll back the reservation
                                                // (which also marks the
                                                // reboot job Failed and
                                                // reopens admission).
                                                let _ = scheduler_clone
                                                    .rollback_reboot(
                                                        reboot_job_id,
                                                        Some(format!("Reboot failed: {}", e)),
                                                    )
                                                    .await;
                                                let _ = scheduler_clone
                                                    .add_job_log(
                                                        &job_id_clone,
                                                        format!("Reboot failed: {}", e),
                                                    )
                                                    .await;
                                            }
                                        }
                                    }
                                }
                                Err(reboot_err) => {
                                    let _ = scheduler_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            format!("Reboot reservation rejected: {}", reboot_err),
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = scheduler_clone
                            .fail_job_with_diagnostics(&job_id_clone, &e)
                            .await;
                        error!(job_id = %job_id_clone, error = ?e, "Patch application failed");
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
        Err(ref admission_err) => {
            warn!(request_id = %request_id, error = %admission_err, "Patch apply admission rejected");
            super::packages::admission_error_response(admission_err)
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
