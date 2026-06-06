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
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::jobs::manager::{JobManager, JobOperation, JobStatus};
use crate::packages::{
    validate_package_name, validate_version_string, InstallOptions, Package, PackageManagerBackend,
    PackageSpec,
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
            error!(request_id = %request_id, error = %e, "Failed to list packages");
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
            error!(request_id = %request_id, package = %package_name, error = %e, "Failed to get package");
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
    match job_manager
        .create_job(JobOperation::Install, package_names.clone())
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the installation
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
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

                // Execute installation
                match backend_clone.install_packages(&packages, &options) {
                    Ok(_) => {
                        let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, "Package installation completed");
                    }
                    Err(e) => {
                        let _ = job_manager_clone
                            .fail_job(&job_id_clone, e.to_string())
                            .await;
                        error!(job_id = %job_id_clone, error = %e, "Package installation failed");
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

/// Update a package (async operation)
pub async fn update_package(
    path: web::Path<String>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    job_manager: web::Data<JobManager>,
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
    match job_manager
        .create_job(JobOperation::Update, vec![package_name.clone()])
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the update
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
            let pkg_name = package_name.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Update job to running
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

                // Execute update
                match backend_clone.update_package(&pkg_name) {
                    Ok(_) => {
                        let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, package = %pkg_name, "Package update completed");
                    }
                    Err(e) => {
                        let _ = job_manager_clone
                            .fail_job(&job_id_clone, e.to_string())
                            .await;
                        error!(job_id = %job_id_clone, package = %pkg_name, error = %e, "Package update failed");
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

/// Remove a package (async operation)
pub async fn remove_package(
    path: web::Path<String>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    job_manager: web::Data<JobManager>,
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

    match job_manager
        .create_job(JobOperation::Remove, vec![package_name.clone()])
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the removal
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
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

                // Execute removal (purge=false for standard removal)
                match backend_clone.remove_package(&pkg_name, false) {
                    Ok(_) => {
                        let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, package = %pkg_name, "Package removal completed");
                    }
                    Err(e) => {
                        let _ = job_manager_clone
                            .fail_job(&job_id_clone, e.to_string())
                            .await;
                        error!(job_id = %job_id_clone, package = %pkg_name, error = %e, "Package removal failed");
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
