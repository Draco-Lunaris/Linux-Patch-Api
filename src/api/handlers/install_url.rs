//! URL Install API Handler
//!
//! Implements REST endpoint for installing a package from a URL:
//! - POST /api/v1/packages/install-url (JSON body)
//!
//! The API validates the request, creates a lock file, and forks a detached
//! shell script that downloads and installs the package. The install script
//! owns the entire download+install process to avoid the race condition
//! where the API downloads a file but gets killed before the install script
//! can use it.
//!
//! This design solves the self-upgrade problem: when the API installs
//! its own package, the package's pre-remove script stops the API
//! service, which would kill an in-process download and leave a staged
//! file that gets cleaned up on SIGTERM. By passing the URL to the
//! install script instead of a file path, the script downloads the
//! package itself before stopping the API, eliminating the race.

use actix_web::{web, HttpResponse, Responder};
use chrono::Utc;
use serde::Deserialize;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::loader::AppConfig;
use crate::jobs::manager::{JobManager, JobOperation, JobStatus};

use super::packages::{ApiError, ApiResponse, JobResponseData};
use super::self_upgrade::{
    check_lock_file, create_lock_file, detect_package_type, fork_install_script,
    generate_install_script,
};

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
/// Returns the last path segment, percent-decoded and sanitized.
/// Rejects filenames containing shell metacharacters or other dangerous characters
/// as a defense-in-depth measure (the install script also single-quotes all values).
fn filename_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let filename = parsed.path_segments()?.next_back()?;
    if filename.is_empty() {
        return None;
    }
    // Fully percent-decode the filename so that encoded dangerous characters
    // (e.g. %60 for backtick, %22 for double quote) are caught by the
    // DANGEROUS_CHARS check below.
    let decoded = percent_encoding::percent_decode_str(filename)
        .decode_utf8()
        .ok()?;
    // Reject filenames containing shell metacharacters or dangerous characters.
    // This is defense-in-depth: the install script single-quotes all values,
    // but we reject obviously dangerous filenames at the API level too.
    const DANGEROUS_CHARS: &[char] = &[
        ';', '|', '&', '$', '`', '(', ')', '<', '>', ' ', '\t', '\n', '\r', '\\', '\'', '"', '#',
        '!', '*', '?', '{', '}', '[', ']', '~',
    ];
    if decoded.contains(DANGEROUS_CHARS) {
        return None;
    }
    Some(decoded.into_owned())
}

/// Install a package from a URL (detached process for self-upgrade safety).
///
/// Validates the request, creates a lock file, and forks a detached shell
/// script that downloads the package from the URL, verifies the checksum
/// (if provided), stops the API service, and installs the package.
///
/// Returns 202 Accepted immediately, before the download begins.
/// The Manager can detect completion by polling the health endpoint or by
/// checking the install status on the next API startup.
pub async fn install_url(
    req: web::Json<InstallUrlRequest>,
    config: web::Data<AppConfig>,
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

    // Check for existing lock file (concurrent install prevention)
    if let Err(lock_error) = check_lock_file(&config.file_install.state_dir) {
        warn!(
            request_id = %request_id,
            lock_error = %lock_error,
            "Install rejected: lock file exists"
        );
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "INSTALL_IN_PROGRESS".to_string(),
                message: lock_error,
                details: None,
                retryable: true,
            }),
        };
        return HttpResponse::Conflict().json(response);
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

    // Detect package type from filename
    let package_type = match detect_package_type(&safe_name) {
        Some(pt) => pt,
        None => {
            let response = ApiResponse::<()> {
                success: false,
                request_id,
                timestamp: Utc::now().to_rfc3339(),
                data: None,
                error: Some(ApiError {
                    code: "INVALID_EXTENSION".to_string(),
                    message: format!(
                        "Could not determine package type from filename '{}'. \
                         Supported extensions: .deb, .rpm, .apk, .tar.zst",
                        safe_name
                    ),
                    details: None,
                    retryable: false,
                }),
            };
            return HttpResponse::BadRequest().json(response);
        }
    };

    info!(
        request_id = %request_id,
        url = %url,
        filename = %safe_name,
        package_type = %package_type,
        "Processing URL install request"
    );

    // Check job queue capacity
    if !job_manager.can_accept_job().await {
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

    // Create a job for tracking
    let job_id = match job_manager
        .create_job(JobOperation::FileInstall, vec![safe_name.clone()])
        .await
    {
        Ok(id) => id,
        Err(e) => {
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
            return HttpResponse::InternalServerError().json(response);
        }
    };

    // Create lock file to prevent concurrent installs
    let state_dir = &config.file_install.state_dir;
    if let Err(lock_err) = create_lock_file(state_dir, &job_id.to_string(), url) {
        // Clean up job
        let _ = job_manager.fail_job(&job_id, lock_err.clone()).await;
        error!(request_id = %request_id, error = %lock_err, "Failed to create lock file");
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "LOCK_ERROR".to_string(),
                message: format!("Failed to create install lock: {}", lock_err),
                details: None,
                retryable: true,
            }),
        };
        return HttpResponse::InternalServerError().json(response);
    }

    // Update job to running
    let _ = job_manager
        .update_job(
            &job_id,
            JobStatus::Running,
            Some(0),
            Some("Forking detached install script...".to_string()),
        )
        .await;

    // Generate the detached install script
    // The script will download the package itself, verify checksum, stop the
    // service, and install — all without the API needing to stage the file.
    let script_content = generate_install_script(
        url,
        req.checksum.as_deref(),
        &safe_name,
        package_type,
        &job_id.to_string(),
        state_dir,
    );

    // Fork the detached install script
    if let Err(fork_err) = fork_install_script(&script_content, state_dir) {
        // Clean up: remove lock file and fail the job
        let lock_path = std::path::PathBuf::from(state_dir).join("install.lock");
        let _ = std::fs::remove_file(&lock_path);
        let _ = job_manager.fail_job(&job_id, fork_err.clone()).await;

        error!(request_id = %request_id, error = %fork_err, "Failed to fork install script");
        let response = ApiResponse::<()> {
            success: false,
            request_id,
            timestamp: Utc::now().to_rfc3339(),
            data: None,
            error: Some(ApiError {
                code: "FORK_ERROR".to_string(),
                message: format!("Failed to start installation: {}", fork_err),
                details: None,
                retryable: true,
            }),
        };
        return HttpResponse::InternalServerError().json(response);
    }

    info!(
        request_id = %request_id,
        job_id = %job_id,
        url = %url,
        "Install script forked successfully, returning 202 Accepted"
    );

    let response = ApiResponse::success(JobResponseData {
        job_id: job_id.to_string(),
        status: "installing".to_string(),
        operation: "file_install".to_string(),
        packages: Some(vec![safe_name]),
        package: None,
    });

    HttpResponse::Accepted().json(response)
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
    fn test_filename_from_url_dangerous_chars() {
        // Shell metacharacters should be rejected
        assert_eq!(
            filename_from_url("https://example.com/pkg;whoami.deb"),
            None
        );
        assert_eq!(filename_from_url("https://example.com/pkg$(cmd).deb"), None);
        assert_eq!(filename_from_url("https://example.com/pkg`cmd`.deb"), None);
        assert_eq!(filename_from_url("https://example.com/pkg|cmd.deb"), None);
        assert_eq!(filename_from_url("https://example.com/pkg&cmd.deb"), None);
        assert_eq!(filename_from_url("https://example.com/pkg file.deb"), None);
        assert_eq!(filename_from_url("https://example.com/pkg'file.deb"), None);
        assert_eq!(filename_from_url("https://example.com/pkg\"file.deb"), None);
    }

    #[test]
    fn test_filename_from_url_safe_chars() {
        // Hyphens, underscores, dots, and alphanumerics should be allowed
        assert_eq!(
            filename_from_url("https://example.com/linux-patch-api_1.5.0_amd64.deb"),
            Some("linux-patch-api_1.5.0_amd64.deb".to_string())
        );
        assert_eq!(
            filename_from_url("https://example.com/v2.0.0-beta.1+x86_64.rpm"),
            Some("v2.0.0-beta.1+x86_64.rpm".to_string())
        );
    }
}
