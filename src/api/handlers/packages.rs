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

use crate::jobs::manager::JobOperation;
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
    cache_state: web::Data<crate::packages::cache::PackageCacheState>,
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
    // completes. The postinst restart after self-update would kill any
    // concurrent package operation mid-transaction, leaving the package
    // manager in a broken state.
    let is_self_update = package_name == SELF_PACKAGE_NAME;

    if is_self_update {
        // ── Simplified self-update ─────────────────────────────────────────
        //
        // The self-update is just a package upgrade. The flow is:
        //   1. Check no jobs are running (simple boolean)
        //   2. Refresh the package cache (apt-get update)
        //   3. Install the package (apt-get install -y linux-patch-api)
        //   4. The postinst script restarts the service
        //   5. The manager's health poll sees the new version
        //
        // The package manager handles atomicity. The postinst handles
        // restart. The manager handles version detection via its existing
        // health poll.

        // Atomically reserve the self-update slot. This checks no running
        // jobs, no existing self-update, and sets the self_update flag —
        // all under a single lock. This prevents concurrent package
        // operations from being admitted while the self-update is in
        // progress, and makes the health endpoint report degraded.
        match scheduler
            .try_reserve_self_update(
                vec![package_name.clone()],
                "",
                "", // from/target versions not needed — no version verification
            )
            .await
        {
            Ok(reservation) => {
                let job_id = reservation.commit();
                let backend_clone = backend.clone();
                let scheduler_clone = scheduler.clone();
                let pkg_name = package_name.clone();
                let cache_state_clone = cache_state.clone();

                tokio::spawn(async move {
                    let job_id_clone = job_id;

                    // Execute cache refresh + package upgrade through
                    // dispatch_mutation — atomically acquires the
                    // mutation slot so the SIGTERM handler knows a
                    // mutation is in progress.
                    let backend_for_mutation = backend_clone.clone();
                    let pkg_name_for_mutation = pkg_name.clone();
                    let cache_state_for_mutation = cache_state_clone.clone();

                    let update_result = scheduler_clone
                        .dispatch_mutation(job_id_clone, move || {
                            // Refresh cache first, then upgrade
                            if let Err(e) = backend_for_mutation
                                .refresh_package_cache(cache_state_for_mutation.get_ref())
                            {
                                warn!(
                                    package = %pkg_name_for_mutation,
                                    error = %e,
                                    "Pre-self-update cache refresh failed — proceeding with stale cache"
                                );
                            }
                            backend_for_mutation.update_package(&pkg_name_for_mutation)
                        })
                        .await;

                    match update_result {
                        Ok(_) => {
                            info!(
                                job_id = %job_id_clone,
                                package = %pkg_name,
                                "Self-update install completed — postinst will restart the service"
                            );
                            let _ = scheduler_clone.complete_job(&job_id_clone).await;
                            // Release the self-update flag so new jobs can
                            // be admitted. The postinst's --no-block restart
                            // will kill this process shortly.
                            scheduler_clone.release_self_update(&job_id_clone).await;
                        }
                        Err(e) => {
                            error!(
                                job_id = %job_id_clone,
                                package = %pkg_name,
                                error = ?e,
                                "Self-update failed"
                            );
                            let _ = scheduler_clone
                                .fail_job_with_diagnostics(&job_id_clone, &e)
                                .await;
                            scheduler_clone.release_self_update(&job_id_clone).await;
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
