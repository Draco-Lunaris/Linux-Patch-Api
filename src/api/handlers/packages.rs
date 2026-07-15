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
use crate::jobs::manager::{JobManager, JobOperation, JobStatus};
use crate::packages::coordinator::OperationCoordinator;
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
pub fn admission_error_response(err: &crate::jobs::manager::JobAdmissionError) -> HttpResponse {
    match err {
        crate::jobs::manager::JobAdmissionError::SelfUpdateInProgress => HttpResponse::Conflict()
            .insert_header(("Retry-After", "60"))
            .json(ApiResponse::<()>::error(
                "SELF_UPDATE_IN_PROGRESS",
                "Cannot accept new jobs while a self-update is in progress. Retry after it completes.",
                None,
                true,
            )),
        crate::jobs::manager::JobAdmissionError::QueueFull => HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(ApiResponse::<()>::error(
                "QUEUE_FULL",
                "Job queue is at capacity. Please retry later.",
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
    job_manager: web::Data<JobManager>,
    coordinator: web::Data<Arc<OperationCoordinator>>,
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
    match job_manager
        .admit_job(JobOperation::Install, package_names.clone())
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the installation
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
            let coordinator_clone = coordinator.clone();
            let options = body.options.clone();
            let packages = body.packages.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Update job to running
                let _ = job_manager_clone
                    .update_job(
                        &job_id_clone,
                        JobStatus::Running,
                        Some(0),
                        Some("Starting installation...".to_string()),
                    )
                    .await;
                let _ = job_manager_clone
                    .add_job_log(&job_id_clone, "Job started".to_string())
                    .await;

                // Execute installation through the coordinator's mutation
                // semaphore — this serializes all package-DB mutations across
                // ALL backends (APT, DNF, YUM, APK, Pacman).
                let install_result = coordinator_clone
                    .run_mutation(|| backend_clone.install_packages(&packages, &options))
                    .await;

                match install_result {
                    Ok(_) => {
                        let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, "Package installation completed");
                    }
                    Err(e) => {
                        let _ = job_manager_clone
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
    job_manager: web::Data<JobManager>,
    coordinator: web::Data<Arc<OperationCoordinator>>,
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

        // Atomically reserve the self-update slot. This performs all checks
        // (no running jobs, no existing self-update, queue capacity) and
        // state changes (set flag, create job) under a single lock
        // acquisition, preventing the check-then-set race where a competing
        // patch/install/remove request interleaves between the running-count
        // check and the flag set.
        match job_manager
            .try_reserve_self_update(vec![package_name.clone()])
            .await
        {
            Ok(reservation) => {
                let job_id = reservation.job_id;
                info!(
                    request_id = %request_id,
                    job_id = %job_id,
                    "Self-update reserved atomically — flag set, job created, other endpoints rejecting new jobs"
                );

                // Write persistent upgrade state — start in Installing phase.
                // The state file survives process restarts, unlike the in-memory flag.
                // FAIL-CLOSED: If we cannot persist the Installing state, we MUST
                // abort the self-update before invoking the package manager. If we
                // proceeded and the process crashed, the next startup would not
                // know an upgrade was in progress.
                //
                // Section 9: Query the package manager for the installed version,
                // not CARGO_PKG_VERSION. The package manager is the authoritative
                // source for the installed package version.
                let from_version = backend
                    .get_installed_version(&package_name)
                    .unwrap_or(None)
                    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
                // Try to resolve the target version before installing.
                let target_version = backend
                    .get_candidate_version(&package_name)
                    .unwrap_or(None)
                    .unwrap_or_else(|| from_version.clone());
                if target_version != from_version {
                    info!(
                        from_version = %from_version,
                        target_version = %target_version,
                        "Resolved target version for self-update"
                    );
                } else {
                    warn!("Could not resolve target version — using from_version as placeholder");
                }
                let upgrade_state = crate::jobs::upgrade_state::UpgradeState::installing(
                    &job_id.to_string(),
                    &from_version,
                    &target_version,
                );
                if let Err(e) = crate::jobs::upgrade_state::write_state(&upgrade_state) {
                    // FAIL-CLOSED: abort the self-update
                    // The reservation will be dropped here, rolling back
                    // the owner and job automatically.
                    error!(error = %e, "Failed to write persistent Installing state — aborting self-update before invoking package manager");
                    job_manager.release_self_update(&job_id).await;
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
                let job_manager_clone = job_manager.clone();
                let coordinator_clone = coordinator.clone();
                let pkg_name = package_name.clone();

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

                    // Execute update through the coordinator's mutation semaphore
                    let update_result = coordinator_clone
                        .run_mutation(|| backend_clone.update_package(&pkg_name))
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
                                let _ = job_manager_clone
                                    .fail_job(&job_id_clone, format!(
                                        "Failed to persist Verifying state: {}. Entered recovery mode — manual intervention required.", e
                                    ))
                                    .await;
                                return;
                            }

                            // Verify the installed version changed
                            let installed_version = backend_clone
                                .get_installed_version(&pkg_name)
                                .unwrap_or(None);

                            match &installed_version {
                                Some(v) if v != &from_version => {
                                    info!(
                                        job_id = %job_id_clone,
                                        from_version = %from_version,
                                        installed_version = %v,
                                        target_version = %target_version,
                                        "Self-update verified — installed version changed"
                                    );
                                    let _ = job_manager_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            format!("Updated from {} to {}", from_version, v),
                                        )
                                        .await;
                                }
                                Some(v) if v == &from_version => {
                                    warn!(
                                        job_id = %job_id_clone,
                                        installed_version = %v,
                                        "Self-update was a no-op — installed version unchanged. Not restarting."
                                    );
                                    let _ = job_manager_clone
                                        .add_job_log(
                                            &job_id_clone,
                                            "No update available — installed version unchanged"
                                                .to_string(),
                                        )
                                        .await;
                                    let _ = job_manager_clone.complete_job(&job_id_clone).await;
                                    // Release the self-update lock, clear state and marker
                                    job_manager_clone.release_self_update(&job_id_clone).await;
                                    crate::jobs::upgrade_state::clear_state();
                                    crate::jobs::upgrade_state::clear_marker();
                                    return;
                                }
                                _ => {
                                    warn!(
                                        job_id = %job_id_clone,
                                        "Could not verify installed version after update — proceeding with restart"
                                    );
                                }
                            }

                            let _ = job_manager_clone.complete_job(&job_id_clone).await;

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
                            let _ = job_manager_clone
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
                    // 3. Wait for is_operation_in_progress() == false (no
                    //    apt/dpkg child process running).
                    // 4. Call restart_own_service() to restart immediately.
                    //
                    // The 30s timer in the postinst remains as a fallback
                    // safety net — if this process crashes before completing
                    // the drain, the timer ensures the restart still happens.
                    //
                    // On FAILURE: Clear the flag and persistent state so the
                    // system can recover.
                    let job = job_manager_clone.get_job(&job_id_clone).await;
                    let is_failed = job
                        .as_ref()
                        .map(|j| j.status == JobStatus::Failed)
                        .unwrap_or(true);
                    if is_failed {
                        // Release the self-update lock using the job_id as
                        // the ownership permit. This only clears the lock if
                        // this job still owns it — if a second self-update
                        // somehow took over, this is a no-op.
                        job_manager_clone.release_self_update(&job_id_clone).await;
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
                            let active = job_manager_clone.active_count().await;
                            let mutation_busy = coordinator_clone.is_operation_in_progress();

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

                        // Transition to StartingNewProcess before issuing
                        // the restart command. This persists the state so
                        // the new process knows it's the replacement.
                        let mut restart_state =
                            crate::jobs::upgrade_state::UpgradeState::installing(
                                &job_id_clone.to_string(),
                                &from_version,
                                &target_version,
                            );
                        restart_state.to_starting_new_process();
                        if let Err(e) = crate::jobs::upgrade_state::write_state(&restart_state) {
                            error!(
                                error = %e,
                                "Failed to write StartingNewProcess state — keeping fallback timer armed, not cancelling marker"
                            );
                            // Don't cancel the marker — the fallback timer
                            // is still needed since we can't persist state.
                        }

                        // Trigger the restart immediately. restart_own_service
                        // is fire-and-forget (spawn, not output) so it doesn't
                        // block a tokio worker thread. The process will be
                        // killed by the restart.
                        info!(job_id = %job_id_clone, "Initiating service restart after self-update drain");
                        match backend_clone.restart_own_service() {
                            Ok(_) => {
                                info!(job_id = %job_id_clone, "Service restart command spawned — process will be replaced");
                                // Cancel the fallback timer only AFTER
                                // successful restart launch. The marker
                                // file is the cancellation signal.
                                info!(job_id = %job_id_clone, "Cancelling fallback restart timer by removing marker");
                                crate::jobs::upgrade_state::clear_marker();
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
                    crate::jobs::manager::SelfUpdateAdmissionError::AlreadyInProgress => (
                        "SELF_UPDATE_IN_PROGRESS",
                        "A self-update is already in progress. Retry after it completes.".to_string(),
                        None,
                        true,
                    ),
                    crate::jobs::manager::SelfUpdateAdmissionError::JobsInProgress { count } => (
                        "SELF_UPDATE_BLOCKED",
                        format!("Cannot self-update while {} jobs are in progress. Retry after jobs complete.", count),
                        Some(serde_json::json!({"running_jobs": count})),
                        true,
                    ),
                    crate::jobs::manager::SelfUpdateAdmissionError::QueueFull => (
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
    match job_manager
        .admit_job(JobOperation::Update, vec![package_name.clone()])
        .await
    {
        Ok(job_id) => {
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
            let coordinator_clone = coordinator.clone();
            let pkg_name = package_name.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                let _ = job_manager_clone
                    .update_job(
                        &job_id_clone,
                        JobStatus::Running,
                        Some(0),
                        Some("Starting update...".to_string()),
                    )
                    .await;
                let _ = job_manager_clone
                    .add_job_log(&job_id_clone, "Job started".to_string())
                    .await;

                // Execute update through the coordinator's mutation semaphore
                let update_result = coordinator_clone
                    .run_mutation(|| backend_clone.update_package(&pkg_name))
                    .await;

                match update_result {
                    Ok(_) => {
                        let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, package = %pkg_name, "Package update completed");
                    }
                    Err(e) => {
                        let _ = job_manager_clone
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
    job_manager: web::Data<JobManager>,
    coordinator: web::Data<Arc<OperationCoordinator>>,
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
    match job_manager
        .admit_job(JobOperation::Remove, vec![package_name.clone()])
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the removal
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
            let coordinator_clone = coordinator.clone();
            let pkg_name = package_name.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Update job to running
                let _ = job_manager_clone
                    .update_job(
                        &job_id_clone,
                        JobStatus::Running,
                        Some(0),
                        Some("Starting removal...".to_string()),
                    )
                    .await;
                let _ = job_manager_clone
                    .add_job_log(&job_id_clone, "Job started".to_string())
                    .await;

                // Execute removal through the coordinator's mutation semaphore
                let remove_result = coordinator_clone
                    .run_mutation(|| backend_clone.remove_package(&pkg_name, false))
                    .await;

                match remove_result {
                    Ok(_) => {
                        let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, package = %pkg_name, "Package removal completed");
                    }
                    Err(e) => {
                        let _ = job_manager_clone
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
