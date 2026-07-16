//! Package Management API Handlers
//!
//! Implements REST endpoints for package management operations:
//! - GET /api/v1/packages - List/filter packages
//! - GET /api/v1/packages/{name} - Get package details
//! - POST /api/v1/packages - Install package(s) - async
//! - PUT /api/v1/packages/{name} - Update package - async
//! - DELETE /api/v1/packages/{name} - Remove package - async

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::enroll::{check_and_provision_repo_config, RepoHealResult};
use crate::jobs::manager::{JobOperation, JobStatus};
use crate::jobs::scheduler::Scheduler;
use crate::packages::{
    validate_package_name, validate_version_string, InstallOptions, Package, PackageManagerBackend,
    PackageSpec, SELF_PACKAGE_NAME,
};

/// Validate all package names and versions in a request
fn validate_package_names(packages: &[PackageSpec]) -> Result<(), String> {
    for pkg in packages {
        validate_package_name(&pkg.name)?;
        if let Some(version) = &pkg.version {
            validate_version_string(version)?;
        }
    }
    Ok(())
}

/// Standard API response envelope
#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub request_id: String,
    pub timestamp: String,
    pub data: Option<T>,
    pub error: Option<ApiError>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            request_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            data: Some(data),
            error: None,
        }
    }

    pub fn error(
        code: &str,
        message: &str,
        details: Option<serde_json::Value>,
        retryable: bool,
    ) -> Self {
        Self {
            success: false,
            request_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: code.to_string(),
                message: message.to_string(),
                details,
                retryable,
            }),
        }
    }
}

/// API error structure
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub details: Option<serde_json::Value>,
    pub retryable: bool,
}

/// Convert a `JobAdmissionError` into an HTTP response.
///
/// Used by all handlers that call `admit_job` — provides consistent error
/// responses across install, update, remove, patch-apply, reboot, and rollback.
pub fn admission_error_response(err: &crate::jobs::scheduler::JobAdmissionError) -> HttpResponse {
    match err {
        crate::jobs::scheduler::JobAdmissionError::SelfUpdateInProgress => HttpResponse::Conflict()
            .insert_header(("Retry-After", "60"))
            .json(ApiResponse::<()>::error(
                "SELF_UPDATE_IN_PROGRESS",
                "Cannot accept new jobs while a self-update is in progress. Retry after it completes.",
                None,
                true,
            )),
        crate::jobs::scheduler::JobAdmissionError::QueueFull => HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(ApiResponse::<()>::error(
                "QUEUE_FULL",
                "Job queue is at capacity. Please retry later.",
                None,
                true,
            )),
        crate::jobs::scheduler::JobAdmissionError::AdmissionFrozen => HttpResponse::Conflict()
            .insert_header(("Retry-After", "60"))
            .json(ApiResponse::<()>::error(
                "ADMISSION_FROZEN",
                "Admission frozen — shutdown in progress.",
                None,
                true,
            )),
    }
}

/// Package list response data
#[derive(Debug, Serialize)]
pub struct PackageListData {
    pub packages: Vec<Package>,
    pub total: usize,
}

/// Package install request
#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub packages: Vec<PackageSpec>,
    #[serde(default)]
    pub options: InstallOptions,
}

/// Job response data for async operations
#[derive(Debug, Serialize)]
pub struct JobResponseData {
    pub job_id: String,
    pub status: String,
    pub operation: String,
    pub packages: Option<Vec<String>>,
    pub package: Option<String>,
}

/// Query parameters for package listing
#[derive(Debug, Deserialize)]
pub struct PackageListQuery {
    pub name: Option<String>,
    pub status: Option<String>,
    pub upgradable: Option<bool>,
    pub sort: Option<String>,
    pub order: Option<String>,
}

/// List packages with filtering
pub async fn list_packages(
    query: web::Query<PackageListQuery>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let timestamp = Utc::now().to_rfc3339();

    info!(request_id = %request_id, "Listing packages");

    match backend.list_packages(query.name.as_deref()) {
        Ok(mut packages) => {
            // Apply filters
            if let Some(status) = &query.status {
                packages.retain(|p| match status.as_str() {
                    "installed" => p.status == crate::packages::PackageStatus::Installed,
                    "upgradable" => p.upgradable,
                    "available" => p.status == crate::packages::PackageStatus::Available,
                    _ => true,
                });
            }

            if let Some(upgradable) = query.upgradable {
                if upgradable {
                    packages.retain(|p| p.upgradable);
                }
            }

            // Apply sorting
            let sort_field = query.sort.as_deref().unwrap_or("name");
            let ascending = query.order.as_deref().unwrap_or("asc") == "asc";

            packages.sort_by(|a, b| {
                let cmp = match sort_field {
                    "name" => a.name.cmp(&b.name),
                    "version" => a.version.cmp(&b.version),
                    "status" => format!("{:?}", a.status).cmp(&format!("{:?}", b.status)),
                    _ => a.name.cmp(&b.name),
                };
                if ascending {
                    cmp
                } else {
                    cmp.reverse()
                }
            });

            let total = packages.len();
            let response = ApiResponse {
                success: true,
                request_id,
                timestamp,
                data: Some(PackageListData { packages, total }),
                error: None,
            };

            HttpResponse::Ok().json(response)
        }
        Err(e) => {
            error!(request_id = %request_id, error = ?e, "Failed to list packages");
            let response = ApiResponse::<()>::error(
                "PKG_MANAGER_ERROR",
                &format!("Failed to list packages: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Get package details by name
pub async fn get_package(
    path: web::Path<String>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let package_name = path.into_inner();

    // VULN-001, VULN-003: Validate package name (length and empty string)
    if let Err(e) = validate_package_name(&package_name) {
        let response = ApiResponse::<()>::error("VALIDATION_ERROR", &e, None, false);
        return HttpResponse::BadRequest().json(response);
    }

    info!(request_id = %request_id, package = %package_name, "Getting package details");

    match backend.get_package(&package_name) {
        Ok(Some(package)) => {
            let response = ApiResponse::success(package);
            HttpResponse::Ok().json(response)
        }
        Ok(None) => {
            warn!(request_id = %request_id, package = %package_name, "Package not found");
            let response = ApiResponse::<()>::error(
                "PKG_NOT_FOUND",
                &format!("Package '{}' not found", package_name),
                None,
                false,
            );
            HttpResponse::NotFound().json(response)
        }
        Err(e) => {
            error!(request_id = %request_id, package = %package_name, error = ?e, "Failed to get package");
            let response = ApiResponse::<()>::error(
                "PKG_MANAGER_ERROR",
                &format!("Failed to get package: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Install packages (async operation)
pub async fn install_packages(
    body: web::Json<InstallRequest>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    scheduler: web::Data<Arc<Scheduler>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let package_names: Vec<String> = body.packages.iter().map(|p| p.name.clone()).collect();

    // VULN-001, VULN-003: Validate all package names (length and empty string)
    if let Err(e) = validate_package_names(&body.packages) {
        let response = ApiResponse::<()>::error("VALIDATION_ERROR", &e, None, false);
        return HttpResponse::BadRequest().json(response);
    }

    info!(request_id = %request_id, packages = ?package_names, "Installing packages");

    // Atomically admit the job — checks self-update flag and queue capacity
    // under a single lock to prevent race with self-update reservation.
    match scheduler
        .admit_job(JobOperation::Install, package_names.clone())
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the installation
            let backend_clone = backend.clone();
            let scheduler_clone = scheduler.clone();
            let options = body.options.clone();
            let packages = body.packages.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Execute installation through dispatch_mutation — atomically
                // starts the job and acquires the mutation slot.
                let backend_for_mutation = backend_clone.clone();
                let install_result = scheduler_clone
                    .dispatch_mutation(job_id_clone, move || {
                        backend_for_mutation.install_packages(&packages, &options)
                    })
                    .await;

                match install_result {
                    Ok(_) => {
                        let _ = scheduler_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, "Package installation completed");
                    }
                    Err(e) => {
                        let _ = scheduler_clone
                            .fail_job_with_diagnostics(&job_id_clone, &e)
                            .await;
                        error!(job_id = %job_id_clone, error = ?e, "Package installation failed");
                    }
                }
            });

            let response = ApiResponse::success(JobResponseData {
                job_id: job_id.to_string(),
                status: "pending".to_string(),
                operation: "install".to_string(),
                packages: Some(package_names),
                package: None,
            });

            HttpResponse::Accepted().json(response)
        }
        Err(ref admission_err) => {
            warn!(request_id = %request_id, error = %admission_err, "Install job admission rejected");
            admission_error_response(admission_err)
        }
    }
}

/// Update a package (async operation)
pub async fn update_package(
    path: web::Path<String>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    scheduler: web::Data<Arc<Scheduler>>,
    manager_url: web::Data<Option<String>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let package_name = path.into_inner();

    // VULN-001, VULN-003: Validate package name (length and empty string)
    if let Err(e) = validate_package_name(&package_name) {
        let response = ApiResponse::<()>::error("VALIDATION_ERROR", &e, None, false);
        return HttpResponse::BadRequest().json(response);
    }

    info!(request_id = %request_id, package = %package_name, "Updating package");

    // Self-update guard: if updating linux-patch-api itself, block while other
    // jobs are running AND block new jobs from starting until the self-update
    // completes. The delayed restart after self-update would kill any concurrent
    // package operation mid-transaction, leaving the package manager in a
    // broken state.
    let is_self_update = package_name == SELF_PACKAGE_NAME;

    if is_self_update {
        // Pre-self-update repo-config self-heal: ensure the manager-hosted
        // package repo is configured before attempting the upgrade. Without
        // this, `apt-get install --only-upgrade linux-patch-api` silently finds
        // "already newest version" and reports success without upgrading.
        // This catches hosts that were enrolled before repo_config was added
        // to the enrollment bundle, or where the repo files were lost.
        //
        // This runs BEFORE the atomic reservation (try_reserve_self_update)
        // because it's a network call that should not hold the job-manager
        // lock. If the repo config is missing, we reject before reserving.
        match manager_url.as_ref() {
            Some(url) => {
                info!(request_id = %request_id, "Pre-self-update repo config check");
                match check_and_provision_repo_config(url).await {
                    Ok(RepoHealResult::AlreadyConfigured) => {
                        info!(request_id = %request_id, "Repo config already present");
                    }
                    Ok(RepoHealResult::Provisioned) => {
                        info!(request_id = %request_id, "Repo config provisioned via self-heal");
                    }
                    Err(e) => {
                        error!(request_id = %request_id, error = %e, "Repo config self-heal failed — aborting self-update to prevent silent no-op");
                        let response = ApiResponse::<()>::error(
                            "REPO_CONFIG_MISSING",
                            "Cannot self-update: manager-hosted repo is not configured and self-heal failed. The upgrade would be a silent no-op.",
                            Some(serde_json::json!({"error": e.to_string()})),
                            true,
                        );
                        return HttpResponse::Conflict()
                            .insert_header(("Retry-After", "60"))
                            .json(response);
                    }
                }
            }
            None => {
                warn!(request_id = %request_id, "No manager URL configured — cannot run repo config self-heal. Self-update may be a no-op if repo is not configured.");
            }
        }

        // Resolve from/target versions BEFORE the atomic reservation. These
        // are needed by try_reserve_self_update and the persistent state write.
        // FAIL-CLOSED: If we cannot determine the installed version or the
        // candidate version, we must NOT proceed with the self-update.
        let from_version = match backend.get_installed_version(&package_name) {
            Ok(Some(v)) => v,
            Ok(None) => {
                error!(request_id = %request_id, "Cannot determine installed version — aborting self-update");
                let response = ApiResponse::<()>::error(
                    "VERSION_LOOKUP_FAILED",
                    "Cannot determine the currently installed version. Self-update aborted for safety.",
                    None,
                    true,
                );
                return HttpResponse::Conflict().json(response);
            }
            Err(e) => {
                error!(request_id = %request_id, error = %e, "Failed to query installed version — aborting self-update");
                let response = ApiResponse::<()>::error(
                    "VERSION_LOOKUP_FAILED",
                    "Failed to query the installed version from the package manager. Self-update aborted for safety.",
                    Some(serde_json::json!({"error": e.to_string()})),
                    true,
                );
                return HttpResponse::Conflict().json(response);
            }
        };
        let target_version = match backend.get_candidate_version(&package_name) {
            Ok(Some(v)) => v,
            Ok(None) => {
                error!(request_id = %request_id, "Cannot determine candidate version — aborting self-update");
                let response = ApiResponse::<()>::error(
                    "VERSION_LOOKUP_FAILED",
                    "Cannot determine the candidate (target) version. Self-update aborted for safety.",
                    None,
                    true,
                );
                return HttpResponse::Conflict().json(response);
            }
            Err(e) => {
                error!(request_id = %request_id, error = %e, "Failed to query candidate version — aborting self-update");
                let response = ApiResponse::<()>::error(
                    "VERSION_LOOKUP_FAILED",
                    "Failed to query the candidate version from the package manager. Self-update aborted for safety.",
                    Some(serde_json::json!({"error": e.to_string()})),
                    true,
                );
                return HttpResponse::Conflict().json(response);
            }
        };
        if target_version == from_version {
            info!(
                from_version = %from_version,
                "Target version equals installed version — no update available"
            );
            let response = ApiResponse::<()>::error(
                "NO_UPDATE_AVAILABLE",
                "The candidate version matches the installed version — no update is available.",
                None,
                false,
            );
            return HttpResponse::Ok().json(response);
        }
        info!(
            from_version = %from_version,
            target_version = %target_version,
            "Resolved target version for self-update"
        );

        // Atomically reserve the self-update slot. This performs all checks
        // (no running jobs, no existing self-update, queue capacity) and
        // state changes (set flag, create job) under a single lock
        // acquisition, preventing the check-then-set race where a competing
        // patch/install/remove request interleaves between the running-count
        // check and the flag set.
        match scheduler
            .try_reserve_self_update(vec![package_name.clone()], &from_version, &target_version)
            .await
        {
            Ok(reservation) => {
                let job_id = reservation.job_id;
                info!(
                    request_id = %request_id,
                    job_id = %job_id,
                    "Self-update reserved atomically — flag set, job created, other endpoints rejecting new jobs"
                );

                // Write persistent upgrade state — start in Reserving phase.
                // The state file survives process restarts, unlike the in-memory flag.
                // FAIL-CLOSED: If we cannot persist the Reserving state, we MUST
                // abort the self-update before invoking the package manager.
                let upgrade_state = crate::jobs::upgrade_state::UpgradeState::reserving(
                    &job_id.to_string(),
                    &from_version,
                    &target_version,
                );
                if let Err(e) = crate::jobs::upgrade_state::write_state(&upgrade_state) {
                    // FAIL-CLOSED: abort the self-update
                    // The reservation will be dropped here, rolling back
                    // the owner and job automatically.
                    error!(error = %e, "Failed to write persistent Reserving state — aborting self-update before invoking package manager");
                    scheduler.release_self_update(&job_id).await;
                    let response = ApiResponse::<()>::error(
                        "PERSISTENCE_FAILED",
                        "Failed to persist upgrade state — self-update aborted for safety. Retry after resolving the disk issue.",
                        Some(serde_json::json!({"error": e.to_string()})),
                        true,
                    );
                    return HttpResponse::InternalServerError().json(response);
                }

                // Commit the reservation — transfer ownership to the spawned task.
                // If we don't reach this point (cancellation/panic), the
                // reservation guard will roll back the owner and job on drop.
                let job_id = reservation.commit();

                // Spawn background task to execute the update
                let backend_clone = backend.clone();
                let scheduler_clone = scheduler.clone();
                let pkg_name = package_name.clone();

                tokio::spawn(async move {
                    let job_id_clone = job_id;

                    // Transition from Reserving to Installing before invoking
                    // the package manager. FAIL-CLOSED: if this persistence
                    // fails, abort the self-update.
                    let installing_state = crate::jobs::upgrade_state::UpgradeState::installing(
                        &job_id_clone.to_string(),
                        &from_version,
                        &target_version,
                    );
                    if let Err(e) = crate::jobs::upgrade_state::write_state(&installing_state) {
                        error!(error = %e, "Failed to write Installing state — aborting self-update before invoking package manager");
                        crate::jobs::upgrade_state::write_recovering_state();
                        let _ = scheduler_clone
                            .fail_job(
                                &job_id_clone,
                                format!("Failed to persist Installing state: {}", e),
                            )
                            .await;
                        return;
                    }

                    // Execute update through dispatch_mutation — atomically
                    // starts the job and acquires the mutation slot.
                    let backend_for_mutation = backend_clone.clone();
                    let pkg_name_for_mutation = pkg_name.clone();
                    let update_result = scheduler_clone
                        .dispatch_mutation(job_id_clone, move || {
                            backend_for_mutation.update_package(&pkg_name_for_mutation)
                        })
                        .await;

                    match update_result {
                        Ok(_) => {
                            info!(job_id = %job_id_clone, package = %pkg_name, "Self-update install completed");

                            // Transition to Verifying phase — check that the
                            // installed version actually changed.
                            // FAIL-CLOSED: If we cannot persist the Verifying state,
                            // do NOT restart and do NOT clear the admission block.
                            // Enter Recovering state instead.
                            let mut state = crate::jobs::upgrade_state::UpgradeState::installing(
                                &job_id_clone.to_string(),
                                &from_version,
                                &target_version,
                            );
                            state.to_verifying();
                            if let Err(e) = crate::jobs::upgrade_state::write_state(&state) {
                                error!(
                                    error = %e,
                                    "Failed to write Verifying upgrade state — entering recovery mode, NOT restarting"
                                );
                                crate::jobs::upgrade_state::write_recovering_state();
                                let _ = scheduler_clone
                                    .fail_job(&job_id_clone, format!(
                                        "Failed to persist Verifying state: {}. Entered recovery mode — manual intervention required.", e
                                    ))
                                    .await;
                                return;
                            }

                            // Verify the installed version matches the target.
                            // FAIL-CLOSED: If we cannot read the installed version,
                            // or it doesn't match the target, enter recovery.
                            let installed_version = backend_clone.get_installed_version(&pkg_name);

                            match &installed_version {
                                Ok(Some(v)) if v == &target_version => {
                                    info!(
                                        job_id = %job_id_clone,
                                        from_version = %from_version,
                                        installed_version = %v,
                                        target_version = %target_version,
                                        "Self-update verified — installed version matches target"
                                    );
                                    let _ = scheduler_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            format!("Updated from {} to {}", from_version, v),
                                        )
                                        .await;
                                }
                                Ok(Some(v)) if v == &from_version => {
                                    warn!(
                                        job_id = %job_id_clone,
                                        installed_version = %v,
                                        "Self-update was a no-op — installed version unchanged. Not restarting."
                                    );
                                    let _ = scheduler_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            "No update available — installed version unchanged"
                                                .to_string(),
                                        )
                                        .await;
                                    let _ = scheduler_clone.complete_job(&job_id_clone).await;
                                    scheduler_clone.release_self_update(&job_id_clone).await;
                                    crate::jobs::upgrade_state::clear_state();
                                    crate::jobs::upgrade_state::clear_marker();
                                    return;
                                }
                                Ok(Some(v)) => {
                                    error!(
                                        job_id = %job_id_clone,
                                        from_version = %from_version,
                                        installed_version = %v,
                                        target_version = %target_version,
                                        "Self-update installed unexpected version — entering recovery, NOT restarting"
                                    );
                                    crate::jobs::upgrade_state::write_recovering_state();
                                    let _ = scheduler_clone
                                        .fail_job(&job_id_clone, format!(
                                            "Installed version {} does not match target {}. Entered recovery mode.",
                                            v, target_version
                                        ))
                                        .await;
                                    return;
                                }
                                Ok(None) => {
                                    error!(
                                        job_id = %job_id_clone,
                                        "Cannot determine installed version after update — entering recovery, NOT restarting"
                                    );
                                    crate::jobs::upgrade_state::write_recovering_state();
                                    let _ = scheduler_clone
                                        .fail_job(&job_id_clone,
                                            "Cannot determine installed version after update. Entered recovery mode.".to_string()
                                        )
                                        .await;
                                    return;
                                }
                                Err(e) => {
                                    error!(
                                        job_id = %job_id_clone,
                                        error = %e,
                                        "Failed to query installed version after update — entering recovery, NOT restarting"
                                    );
                                    crate::jobs::upgrade_state::write_recovering_state();
                                    let _ = scheduler_clone
                                        .fail_job(&job_id_clone, format!(
                                            "Failed to query installed version: {}. Entered recovery mode.", e
                                        ))
                                        .await;
                                    return;
                                }
                            }

                            let _ = scheduler_clone.complete_job(&job_id_clone).await;

                            // Transition to RestartPending
                            // FAIL-CLOSED: If we cannot persist the RestartPending
                            // state, do NOT restart. Enter Recovering state.
                            let mut state = crate::jobs::upgrade_state::UpgradeState::installing(
                                &job_id_clone.to_string(),
                                &from_version,
                                &target_version,
                            );
                            state.to_restart_pending();
                            if let Err(e) = crate::jobs::upgrade_state::write_state(&state) {
                                error!(
                                    error = %e,
                                    "Failed to write RestartPending upgrade state — entering recovery mode, NOT restarting"
                                );
                                crate::jobs::upgrade_state::write_recovering_state();
                                // Keep the admission block set — do not clear
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = scheduler_clone
                                .fail_job_with_diagnostics(&job_id_clone, &e)
                                .await;
                            error!(job_id = %job_id_clone, package = %pkg_name, error = ?e, "Self-update failed");
                        }
                    }

                    // Self-update flag lifecycle:
                    //
                    // On SUCCESS: Do NOT clear the flag. The self-update has
                    // installed a new binary and the postinst has scheduled a
                    // 30s fallback timer. Instead of relying on the timer,
                    // we actively drain the system and trigger the restart
                    // immediately once all operations have completed.
                    //
                    // State-based drain (replaces the fixed 30s timer as the
                    // primary synchronization mechanism):
                    // 1. The self_update_in_progress flag is already set,
                    //    so no new mutable operations can start.
                    // 2. Wait for running_count() == 0 (no active jobs).
                    // 3. Wait for is_mutation_in_progress() == false (no
                    //    apt/dpkg child process running).
                    // 4. Call restart_own_service() to restart immediately.
                    //
                    // The 30s timer in the postinst remains as a fallback
                    // safety net — if this process crashes before completing
                    // the drain, the timer ensures the restart still happens.
                    //
                    // On FAILURE: Clear the flag and persistent state so the
                    // system can recover.
                    let job = scheduler_clone.get_job(&job_id_clone).await;
                    let is_failed = job
                        .as_ref()
                        .map(|j| j.status == JobStatus::Failed)
                        .unwrap_or(true);
                    if is_failed {
                        // Release the self-update lock using the job_id as
                        // the ownership permit. This only clears the lock if
                        // this job still owns it — if a second self-update
                        // somehow took over, this is a no-op.
                        scheduler_clone.release_self_update(&job_id_clone).await;
                        crate::jobs::upgrade_state::clear_state();
                        // Also clear the marker in case the postinst already
                        // created it (e.g. package was already at target version).
                        // Without this, the next startup would see marker-without-
                        // state and enter RecoveryMode unnecessarily.
                        crate::jobs::upgrade_state::clear_marker();
                        info!(package = %pkg_name, "Self-update failed — lock, state, and marker cleared, job endpoints accepting new jobs");
                    } else {
                        info!(package = %pkg_name, "Self-update succeeded — beginning state-based drain before restart");

                        // State-based drain: wait for all active operations to complete.
                        // The self_update_in_progress flag prevents new operations from
                        // starting, so we only need to wait for existing ones to finish.
                        // We check active_count() (running + pending) because a pending
                        // job could transition to running after the check — both must
                        // be zero before restarting.
                        let drain_deadline =
                            tokio::time::Instant::now() + tokio::time::Duration::from_secs(120);
                        let mut drain_log_interval =
                            tokio::time::interval(tokio::time::Duration::from_secs(10));
                        drain_log_interval.tick().await; // skip first immediate tick

                        loop {
                            let active = scheduler_clone.active_count().await;
                            let mutation_busy = scheduler_clone.is_mutation_in_progress().await;

                            if active == 0 && !mutation_busy {
                                info!(
                                    job_id = %job_id_clone,
                                    active_jobs = active,
                                    mutation_in_progress = mutation_busy,
                                    "Drain complete — all operations finished, triggering restart"
                                );
                                break;
                            }

                            if tokio::time::Instant::now() >= drain_deadline {
                                warn!(
                                    job_id = %job_id_clone,
                                    active_jobs = active,
                                    mutation_in_progress = mutation_busy,
                                    "Drain timeout (120s) reached — restarting with {} active operations (postinst timer is fallback)",
                                    active
                                );
                                break;
                            }

                            drain_log_interval.tick().await;
                            info!(
                                job_id = %job_id_clone,
                                active_jobs = active,
                                mutation_in_progress = mutation_busy,
                                "Waiting for operations to drain before restart..."
                            );
                        }

                        // Trigger the restart immediately. restart_own_service
                        // is fire-and-forget (spawn, not output) so it doesn't
                        // block a tokio worker thread. The process will be
                        // killed by the restart.
                        //
                        // Do NOT transition to StartingNewProcess or clear the
                        // marker here. The restart command is merely spawned —
                        // it may fail, and the new process hasn't reached
                        // readiness yet. The fallback timer must remain
                        // eligible (state stays RestartPending, marker stays).
                        // The new process clears the marker after successful
                        // readiness (READY=1 + version verification).
                        info!(job_id = %job_id_clone, "Initiating service restart after self-update drain");
                        match backend_clone.restart_own_service() {
                            Ok(_) => {
                                info!(job_id = %job_id_clone, "Service restart command spawned — process will be replaced");
                                // Do NOT clear the marker. The fallback timer
                                // remains armed. The new process will clear it
                                // after successful readiness.
                            }
                            Err(e) => {
                                error!(
                                    job_id = %job_id_clone,
                                    error = ?e,
                                    "Failed to trigger service restart — keeping fallback timer armed (marker preserved)"
                                );
                                // Do NOT clear the marker — the fallback
                                // timer is still armed and will retry.
                            }
                        }
                    }
                });

                let response = ApiResponse::success(JobResponseData {
                    job_id: job_id.to_string(),
                    status: "pending".to_string(),
                    operation: "update".to_string(),
                    packages: None,
                    package: Some(package_name),
                });

                return HttpResponse::Accepted().json(response);
            }
            Err(admission_err) => {
                warn!(request_id = %request_id, error = %admission_err, "Self-update reservation rejected");
                let (code, message, data, retry) = match admission_err {
                    crate::jobs::scheduler::SelfUpdateAdmissionError::AlreadyInProgress => (
                        "SELF_UPDATE_IN_PROGRESS",
                        "A self-update is already in progress. Retry after it completes.".to_string(),
                        None,
                        true,
                    ),
                    crate::jobs::scheduler::SelfUpdateAdmissionError::JobsInProgress { count } => (
                        "SELF_UPDATE_BLOCKED",
                        format!("Cannot self-update while {} jobs are in progress. Retry after jobs complete.", count),
                        Some(serde_json::json!({"running_jobs": count})),
                        true,
                    ),
                    crate::jobs::scheduler::SelfUpdateAdmissionError::QueueFull => (
                        "QUEUE_FULL",
                        "Job queue is at capacity. Please retry later.".to_string(),
                        None,
                        true,
                    ),
                };
                let response = ApiResponse::<()>::error(code, &message, data, retry);
                return HttpResponse::Conflict()
                    .insert_header(("Retry-After", "60"))
                    .json(response);
            }
        }
    }

    // Non-self-update path: atomically admit the job
    match scheduler
        .admit_job(JobOperation::Update, vec![package_name.clone()])
        .await
    {
        Ok(job_id) => {
            let backend_clone = backend.clone();
            let scheduler_clone = scheduler.clone();
            let pkg_name = package_name.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Execute update through dispatch_mutation — atomically
                // starts the job and acquires the mutation slot.
                let backend_for_mutation = backend_clone.clone();
                let pkg_name_for_mutation = pkg_name.clone();
                let update_result = scheduler_clone
                    .dispatch_mutation(job_id_clone, move || {
                        backend_for_mutation.update_package(&pkg_name_for_mutation)
                    })
                    .await;

                match update_result {
                    Ok(_) => {
                        let _ = scheduler_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, package = %pkg_name, "Package update completed");
                    }
                    Err(e) => {
                        let _ = scheduler_clone
                            .fail_job_with_diagnostics(&job_id_clone, &e)
                            .await;
                        error!(job_id = %job_id_clone, package = %pkg_name, error = ?e, "Package update failed");
                    }
                }
            });

            let response = ApiResponse::success(JobResponseData {
                job_id: job_id.to_string(),
                status: "pending".to_string(),
                operation: "update".to_string(),
                packages: None,
                package: Some(package_name),
            });

            HttpResponse::Accepted().json(response)
        }
        Err(ref admission_err) => {
            warn!(request_id = %request_id, error = %admission_err, "Update job admission rejected");
            admission_error_response(admission_err)
        }
    }
}

/// Remove a package (async operation)
pub async fn remove_package(
    path: web::Path<String>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    scheduler: web::Data<Arc<Scheduler>>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let package_name = path.into_inner();

    // VULN-001, VULN-003: Validate package name (length and empty string)
    if let Err(e) = validate_package_name(&package_name) {
        let response = ApiResponse::<()>::error("VALIDATION_ERROR", &e, None, false);
        return HttpResponse::BadRequest().json(response);
    }

    info!(request_id = %request_id, package = %package_name, "Removing package");

    // Atomically admit the job — checks self-update flag and queue capacity
    // under a single lock to prevent race with self-update reservation.
    match scheduler
        .admit_job(JobOperation::Remove, vec![package_name.clone()])
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the removal
            let backend_clone = backend.clone();
            let scheduler_clone = scheduler.clone();
            let pkg_name = package_name.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Execute removal through dispatch_mutation — atomically
                // starts the job and acquires the mutation slot.
                let backend_for_mutation = backend_clone.clone();
                let pkg_name_for_mutation = pkg_name.clone();
                let remove_result = scheduler_clone
                    .dispatch_mutation(job_id_clone, move || {
                        backend_for_mutation.remove_package(&pkg_name_for_mutation, false)
                    })
                    .await;

                match remove_result {
                    Ok(_) => {
                        let _ = scheduler_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, package = %pkg_name, "Package removal completed");
                    }
                    Err(e) => {
                        let _ = scheduler_clone
                            .fail_job_with_diagnostics(&job_id_clone, &e)
                            .await;
                        error!(job_id = %job_id_clone, package = %pkg_name, error = ?e, "Package removal failed");
                    }
                }
            });

            let response = ApiResponse::success(JobResponseData {
                job_id: job_id.to_string(),
                status: "pending".to_string(),
                operation: "remove".to_string(),
                packages: None,
                package: Some(package_name),
            });

            HttpResponse::Accepted().json(response)
        }
        Err(ref admission_err) => {
            warn!(request_id = %request_id, error = %admission_err, "Remove job admission rejected");
            admission_error_response(admission_err)
        }
    }
}

/// Configure all package routes
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/packages")
            .route("", web::get().to(list_packages))
            .route("", web::post().to(install_packages))
            .route("/{name}", web::get().to(get_package))
            .route("/{name}", web::put().to(update_package))
            .route("/{name}", web::delete().to(remove_package)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_response_success() {
        let response = ApiResponse::success("test data".to_string());
        assert!(response.success);
        assert!(!response.request_id.is_empty());
        assert!(response.data.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn test_api_response_error() {
        let response: ApiResponse<()> =
            ApiResponse::error("TEST_CODE", "Test message", None, false);
        assert!(!response.success);
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, "TEST_CODE");
    }
}
