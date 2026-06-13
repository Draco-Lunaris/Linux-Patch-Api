//! File Install API Handler
//!
//! Implements REST endpoint for uploading and installing a package from a file:
//! - POST /api/v1/packages/install-file (multipart upload)

use actix_multipart::Multipart;
use actix_web::{web, HttpResponse, Responder};
use chrono::Utc;
use futures_util::StreamExt;
use std::path::PathBuf;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::loader::AppConfig;
use crate::jobs::manager::{JobManager, JobOperation, JobStatus};
use crate::packages::{validate_file_extension, PackageManagerBackend, MAX_FILE_SIZE};

use super::packages::{ApiError, ApiResponse, JobResponseData};

/// Sanitize a filename to prevent path traversal attacks.
/// Strips directory components and rejects names containing `..`.
fn sanitize_filename(name: &str) -> Option<String> {
    // Reject any input containing path traversal before stripping
    if name.contains("..") {
        return None;
    }
    let name = name.replace('\\', "/");
    let file_name = name.rsplit('/').next()?;
    if file_name.is_empty() {
        return None;
    }
    Some(file_name.to_string())
}

/// Install a package from an uploaded file (async operation)
pub async fn install_file(
    mut payload: Multipart,
    config: web::Data<AppConfig>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    job_manager: web::Data<JobManager>,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();

    // Config gate: reject if file_install is disabled
    if !config.file_install.enabled {
        warn!(request_id = %request_id, "File install attempted but feature is disabled");
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "FILE_INSTALL_DISABLED".to_string(),
                message: "File install feature is disabled. Enable file_install in config.".to_string(),
                details: None,
                retryable: false,
            }),
        };
        return HttpResponse::Forbidden().json(response);
    }

    // Read the multipart field named "package"
    let mut file_data: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;

    while let Some(Ok(mut field)) = payload.next().await {
        let content_disposition = match field.content_disposition() {
            Some(cd) => cd,
            None => continue,
        };
        let field_name = content_disposition.get_name().unwrap_or("");

        if field_name != "package" {
            continue;
        }

        let name = content_disposition
            .get_filename()
            .unwrap_or("")
            .to_string();

        // Read all chunks with size limit enforcement
        let mut data = Vec::new();
        while let Some(Ok(chunk)) = field.next().await {
            data.extend_from_slice(&chunk);
            if data.len() > MAX_FILE_SIZE {
                let response = ApiResponse::<()> {
                    success: false,
                    request_id: request_id.clone(),
                    timestamp: Utc::now().to_rfc3339(),
                    data: None,
                    error: Some(ApiError {
                        code: "FILE_TOO_LARGE".to_string(),
                        message: format!(
                            "Uploaded file exceeds maximum size of {} bytes",
                            MAX_FILE_SIZE
                        ),
                        details: None,
                        retryable: false,
                    }),
                };
                return HttpResponse::PayloadTooLarge().json(response);
            }
        }

        file_data = Some(data);
        file_name = Some(name);
        break; // Only process the first "package" field
    }

    let (data, name) = match (file_data, file_name) {
        (Some(d), Some(n)) => (d, n),
        _ => {
            let response = ApiResponse::<()> {
                success: false,
                request_id: request_id.clone(),
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "MISSING_FILE".to_string(),
                    message: "No file uploaded. Use multipart field named 'package'.".to_string(),
                    details: None,
                    retryable: false,
                }),
            };
            return HttpResponse::BadRequest().json(response);
        }
    };

    // Sanitize filename
    let safe_name = match sanitize_filename(&name) {
        Some(n) => n,
        None => {
            let response = ApiResponse::<()> {
                success: false,
                request_id: request_id.clone(),
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "INVALID_FILENAME".to_string(),
                    message: "Filename contains invalid characters or path traversal.".to_string(),
                    details: None,
                    retryable: false,
                }),
            };
            return HttpResponse::BadRequest().json(response);
        }
    };

    // Validate file extension against backend allowlist
    let backend_name = backend.backend_name();
    if let Err(e) = validate_file_extension(&safe_name, backend_name) {
        let response = ApiResponse::<()> {
            success: false,
            request_id: request_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "INVALID_EXTENSION".to_string(),
                message: e,
                details: None,
                retryable: false,
            }),
        };
        return HttpResponse::BadRequest().json(response);
    }

    // Stage the file to staging_dir
    let staging_dir = &config.file_install.staging_dir;
    let staging_path = PathBuf::from(staging_dir).join(&safe_name);
    let staging_path_str = staging_path.display().to_string();

    if let Err(e) = std::fs::write(&staging_path, &data) {
        error!(
            request_id = %request_id,
            path = %staging_path_str,
            error = %e,
            "Failed to stage uploaded file"
        );
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "STAGING_ERROR".to_string(),
                message: format!("Failed to stage file: {}", e),
                details: None,
                retryable: true,
            }),
        };
        return HttpResponse::InternalServerError().json(response);
    }

    info!(
        request_id = %request_id,
        file = %safe_name,
        size = data.len(),
        staging_path = %staging_path_str,
        "File staged for installation"
    );

    // Check job queue capacity
    if !job_manager.can_accept_job().await {
        // Clean up staged file
        let _ = std::fs::remove_file(&staging_path);
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "QUEUE_FULL".to_string(),
                message: "Job queue is at capacity. Please retry later.".to_string(),
                details: None,
                retryable: true,
            }),
        };
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(response);
    }

    // Create async job
    let file_name_for_job = safe_name.clone();
    match job_manager
        .create_job(JobOperation::FileInstall, vec![safe_name.clone()])
        .await
    {
        Ok(job_id) => {
            // Spawn background task to execute the installation
            let backend_clone = backend.clone();
            let job_manager_clone = job_manager.clone();
            let staging_path_for_cleanup = staging_path.clone();

            tokio::spawn(async move {
                let job_id_clone = job_id;

                // Update job to running
                let _ = job_manager_clone
                    .update_job(
                        &job_id_clone,
                        JobStatus::Running,
                        Some(0),
                        Some("Starting file installation...".to_string()),
                    )
                    .await;
                let _ = job_manager_clone
                    .add_job_log(&job_id_clone, "Job started".to_string())
                    .await;

                // Execute installation
                match backend_clone.install_file(&staging_path_for_cleanup.display().to_string()) {
                    Ok(_) => {
                        // Clean up staged file on success
                        if let Err(e) = std::fs::remove_file(&staging_path_for_cleanup) {
                            warn!(
                                job_id = %job_id_clone,
                                path = %staging_path_for_cleanup.display(),
                                error = %e,
                                "Failed to clean up staged file after successful install"
                            );
                        }
                        let _ = job_manager_clone.complete_job(&job_id_clone).await;
                        info!(job_id = %job_id_clone, "File installation completed");
                    }
                    Err(e) => {
                        // Clean up staged file on failure
                        if let Err(cleanup_err) = std::fs::remove_file(&staging_path_for_cleanup) {
                            warn!(
                                job_id = %job_id_clone,
                                path = %staging_path_for_cleanup.display(),
                                error = %cleanup_err,
                                "Failed to clean up staged file after failed install"
                            );
                        }
                        let _ = job_manager_clone
                            .fail_job(&job_id_clone, e.to_string())
                            .await;
                        error!(job_id = %job_id_clone, error = %e, "File installation failed");
                    }
                }
            });

            let response = ApiResponse::success(JobResponseData {
                job_id: job_id.to_string(),
                status: "pending".to_string(),
                operation: "file_install".to_string(),
                packages: Some(vec![file_name_for_job]),
                package: None,
            });

            HttpResponse::Accepted().json(response)
        }
        Err(e) => {
            // Clean up staged file
            let _ = std::fs::remove_file(&staging_path);
            error!(request_id = %request_id, error = %e, "Failed to create job");
            let response = ApiResponse::<()> {
                success: false,
                request_id,
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "JOB_CREATE_ERROR".to_string(),
                    message: format!("Failed to create job: {}", e),
                    details: None,
                    retryable: true,
                }),
            };
            HttpResponse::InternalServerError().json(response)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename_normal() {
        assert_eq!(sanitize_filename("test.deb"), Some("test.deb".to_string()));
    }

    #[test]
    fn test_sanitize_filename_strips_path() {
        assert_eq!(sanitize_filename("/tmp/test.deb"), Some("test.deb".to_string()));
    }

    #[test]
    fn test_sanitize_filename_rejects_traversal() {
        assert_eq!(sanitize_filename("../../etc/passwd"), None);
    }

    #[test]
    fn test_sanitize_filename_empty() {
        assert_eq!(sanitize_filename(""), None);
    }

    #[test]
    fn test_sanitize_filename_backslash_path() {
        assert_eq!(
            sanitize_filename("C:\\Users\\test\\pkg.deb"),
            Some("pkg.deb".to_string())
        );
    }
}
