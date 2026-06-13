//! URL Install API Handler
//!
//! Implements REST endpoint for installing a package from a URL:
//! - POST /api/v1/packages/install-url (JSON body)
//!
//! The client downloads the package directly from the provided URL,
//! verifies the checksum (if provided), and installs it.
//! This avoids routing the package through the Manager, which is
//! more efficient for fleet upgrades.

use actix_web::{web, HttpResponse, Responder};
use chrono::Utc;
use serde::Deserialize;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::loader::AppConfig;
use crate::jobs::manager::{JobManager, JobOperation, JobStatus};
use crate::packages::{validate_file_extension, PackageManagerBackend};

use super::packages::{ApiError, ApiResponse, JobResponseData};

/// Request body for URL-based package installation.
#[derive(Debug, Deserialize)]
pub struct InstallUrlRequest {
    /// URL to download the package from (HTTPS or HTTP).
    pub url: String,
    /// Optional SHA-256 checksum for verification (format: "sha256:<hex>" or raw hex).
    #[serde(default)]
    pub checksum: Option<String>,
}

/// Extract filename from a URL path.
/// Returns the last path segment, percent-decoded.
fn filename_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let path = parsed.path();
    let filename = path.rsplit('/').next()?;
    if filename.is_empty() {
        return None;
    }
    // Simple percent-decoding for common cases
    let decoded = filename
        .replace("%20", " ")
        .replace("%28", "(")
        .replace("%29", ")")
        .replace("%5B", "[")
        .replace("%5D", "]")
        .replace("%2B", "+");
    Some(decoded)
}

/// Verify SHA-256 checksum of data against an expected hex digest.
fn verify_checksum(data: &[u8], expected_hex: &str) -> bool {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let actual = hex::encode(result);
    // Strip optional "sha256:" prefix
    let expected = expected_hex.strip_prefix("sha256:").unwrap_or(expected_hex);
    actual.eq_ignore_ascii_case(expected)
}

/// Install a package from a URL (async operation).
///
/// Downloads the package from the provided URL, verifies the checksum
/// (if provided), stages it, and creates a background job to install it.
pub async fn install_url(
    req: web::Json<InstallUrlRequest>,
    config: web::Data<AppConfig>,
    backend: web::Data<Box<dyn PackageManagerBackend>>,
    job_manager: web::Data<JobManager>,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();

    // Config gate: reject if file_install is disabled
    if !config.file_install.enabled {
        warn!(request_id = %request_id, "URL install attempted but feature is disabled");
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "FILE_INSTALL_DISABLED".to_string(),
                message: "File install feature is disabled. Enable file_install in config."
                    .to_string(),
                details: None,
                retryable: false,
            }),
        };
        return HttpResponse::Forbidden().json(response);
    }

    // Validate URL
    let url = req.url.trim();
    if url.is_empty() {
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "INVALID_URL".to_string(),
                message: "URL cannot be empty.".to_string(),
                details: None,
                retryable: false,
            }),
        };
        return HttpResponse::BadRequest().json(response);
    }

    // Only allow http:// and https:// schemes
    if !url.starts_with("https://") && !url.starts_with("http://") {
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "INVALID_URL".to_string(),
                message: "URL must start with http:// or https://.".to_string(),
                details: None,
                retryable: false,
            }),
        };
        return HttpResponse::BadRequest().json(response);
    }

    // Extract filename from URL
    let safe_name = match filename_from_url(url) {
        Some(name) => {
            // Sanitize: reject path traversal
            if name.contains("..") {
                let response = ApiResponse::<()> {
                    success: false,
                    request_id,
                    timestamp: Utc::now().to_rfc3339(),
                    data: None,
                    error: Some(ApiError {
                        code: "INVALID_FILENAME".to_string(),
                        message: "Filename from URL contains path traversal.".to_string(),
                        details: None,
                        retryable: false,
                    }),
                };
                return HttpResponse::BadRequest().json(response);
            }
            name
        }
        None => {
            let response = ApiResponse::<()> {
                success: false,
                request_id,
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "INVALID_FILENAME".to_string(),
                    message: "Could not extract filename from URL.".to_string(),
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
            request_id,
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

    // Download the file from the URL
    info!(
        request_id = %request_id,
        url = %url,
        filename = %safe_name,
        "Downloading package from URL"
    );

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300)) // 5 min timeout for large packages
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!(request_id = %request_id, error = %e, "Failed to build HTTP client");
            let response = ApiResponse::<()> {
                success: false,
                request_id,
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "DOWNLOAD_ERROR".to_string(),
                    message: format!("Failed to build HTTP client: {}", e),
                    details: None,
                    retryable: true,
                }),
            };
            return HttpResponse::InternalServerError().json(response);
        }
    };

    let download_response = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            error!(request_id = %request_id, url = %url, error = %e, "Failed to download package");
            let response = ApiResponse::<()> {
                success: false,
                request_id,
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "DOWNLOAD_ERROR".to_string(),
                    message: format!("Failed to download package: {}", e),
                    details: None,
                    retryable: true,
                }),
            };
            return HttpResponse::BadGateway().json(response);
        }
    };

    if !download_response.status().is_success() {
        let status = download_response.status();
        error!(
            request_id = %request_id,
            url = %url,
            status = %status,
            "Download returned non-success status"
        );
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "DOWNLOAD_ERROR".to_string(),
                message: format!("Download returned HTTP {}", status),
                details: None,
                retryable: true,
            }),
        };
        return HttpResponse::BadGateway().json(response);
    }

    let data = match download_response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            error!(request_id = %request_id, error = %e, "Failed to read download body");
            let response = ApiResponse::<()> {
                success: false,
                request_id,
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "DOWNLOAD_ERROR".to_string(),
                    message: format!("Failed to read download body: {}", e),
                    details: None,
                    retryable: true,
                }),
            };
            return HttpResponse::BadGateway().json(response);
        }
    };

    // Check file size
    use crate::packages::MAX_FILE_SIZE;
    if data.len() > MAX_FILE_SIZE {
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "FILE_TOO_LARGE".to_string(),
                message: format!(
                    "Downloaded file exceeds maximum size of {} bytes",
                    MAX_FILE_SIZE
                ),
                details: None,
                retryable: false,
            }),
        };
        return HttpResponse::PayloadTooLarge().json(response);
    }

    // Verify checksum if provided
    if let Some(ref expected) = req.checksum {
        if !verify_checksum(&data, expected) {
            let response = ApiResponse::<()> {
                success: false,
                request_id,
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "CHECKSUM_MISMATCH".to_string(),
                    message: "Downloaded file checksum does not match the provided checksum."
                        .to_string(),
                    details: None,
                    retryable: false,
                }),
            };
            return HttpResponse::BadRequest().json(response);
        }
        info!(
            request_id = %request_id,
            filename = %safe_name,
            "Checksum verified successfully"
        );
    }

    // Stage the file
    let staging_dir = &config.file_install.staging_dir;
    let staging_path = std::path::PathBuf::from(staging_dir).join(&safe_name);
    let staging_path_str = staging_path.display().to_string();

    if let Err(e) = std::fs::write(&staging_path, &data) {
        error!(
            request_id = %request_id,
            path = %staging_path_str,
            error = %e,
            "Failed to stage downloaded file"
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
        "Downloaded package staged for installation"
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
    fn test_filename_from_url_normal() {
        assert_eq!(
            filename_from_url("https://example.com/path/to/package.deb"),
            Some("package.deb".to_string())
        );
    }

    #[test]
    fn test_filename_from_url_no_path() {
        assert_eq!(
            filename_from_url("https://example.com/package.rpm"),
            Some("package.rpm".to_string())
        );
    }

    #[test]
    fn test_filename_from_url_trailing_slash() {
        assert_eq!(filename_from_url("https://example.com/path/"), None);
    }

    #[test]
    fn test_verify_checksum_sha256() {
        let data = b"hello world";
        // SHA-256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_checksum(data, expected));
        assert!(verify_checksum(data, &format!("sha256:{}", expected)));
    }

    #[test]
    fn test_verify_checksum_mismatch() {
        let data = b"hello world";
        assert!(!verify_checksum(data, "0000000000000000"));
    }
}
