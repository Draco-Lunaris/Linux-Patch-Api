//! Job Management API Handlers
//!
//! Implements REST endpoints for job management operations:
//! - GET /api/v1/jobs - List all jobs
//! - GET /api/v1/jobs/{id} - Get job status/details
//! - POST /api/v1/jobs/{id}/rollback - Rollback failed job
//! - DELETE /api/v1/jobs/{id} - Clear completed job from history

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::jobs::manager::{Job, JobManager, JobStatus};

use super::packages::ApiResponse;

/// Job list response data
#[derive(Debug, Serialize)]
pub struct JobListData {
    pub jobs: Vec<JobSummary>,
    pub total: usize,
}

/// Job summary for list view
#[derive(Debug, Serialize)]
pub struct JobSummary {
    pub job_id: String,
    pub operation: String,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub packages: Vec<String>,
    /// Stable error code — present only for failed jobs. New field; old clients ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Job detail response data
#[derive(Debug, Serialize)]
pub struct JobDetailData {
    pub job_id: String,
    pub operation: String,
    pub status: String,
    pub progress: u8,
    pub message: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub packages: Vec<String>,
    pub logs: Vec<String>,
    pub error: Option<String>,
    /// Stable error code — present only for failed jobs. New field; old clients ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Exit code of the underlying command — present only for failed jobs with a CommandError.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Captured stdout of the underlying command — present only for failed jobs with a CommandError.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_stdout: Option<String>,
    /// Captured stderr of the underlying command — present only for failed jobs with a CommandError.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_stderr: Option<String>,
    pub rollback_job_id: Option<String>,
    pub exclusive_mode: bool,
}

/// Query parameters for job listing
#[derive(Debug, Deserialize)]
pub struct JobListQuery {
    pub status: Option<String>,
    pub limit: Option<usize>,
}

impl JobSummary {
    pub fn from_job(job: &Job) -> Self {
        Self {
            job_id: job.id.to_string(),
            operation: format!("{:?}", job.operation).to_lowercase(),
            status: format!("{:?}", job.status).to_lowercase(),
            created_at: job.created_at.to_rfc3339(),
            completed_at: job.completed_at.map(|t| t.to_rfc3339()),
            packages: job.packages.clone(),
            error_code: job.error_code.clone(),
        }
    }
}

impl JobDetailData {
    pub fn from_job(job: &Job) -> Self {
        Self {
            job_id: job.id.to_string(),
            operation: format!("{:?}", job.operation).to_lowercase(),
            status: format!("{:?}", job.status).to_lowercase(),
            progress: job.progress,
            message: job.message.clone(),
            created_at: job.created_at.to_rfc3339(),
            completed_at: job.completed_at.map(|t| t.to_rfc3339()),
            packages: job.packages.clone(),
            logs: job.logs.clone(),
            error: job.error.clone(),
            error_code: job.error_code.clone(),
            exit_code: job.exit_code,
            command_stdout: job.command_stdout.clone(),
            command_stderr: job.command_stderr.clone(),
            rollback_job_id: job.rollback_job_id.map(|id| id.to_string()),
            exclusive_mode: job.exclusive_mode,
        }
    }
}

/// Parse job status from string
fn parse_job_status(status_str: &str) -> Option<JobStatus> {
    match status_str.to_lowercase().as_str() {
        "pending" => Some(JobStatus::Pending),
        "running" => Some(JobStatus::Running),
        "completed" => Some(JobStatus::Completed),
        "failed" => Some(JobStatus::Failed),
        "cancelled" => Some(JobStatus::Cancelled),
        "timedout" => Some(JobStatus::TimedOut),
        _ => None,
    }
}

/// List all jobs with optional filtering
pub async fn list_jobs(
    query: web::Query<JobListQuery>,
    job_manager: web::Data<JobManager>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();

    let status_filter = query.status.as_ref().and_then(|s| parse_job_status(s));
    let limit = query.limit.unwrap_or(50);

    info!(
        request_id = %request_id,
        status_filter = ?status_filter,
        limit = limit,
        "Listing jobs"
    );

    let jobs = job_manager.list_jobs(status_filter, limit).await;
    let total = jobs.len();
    let job_summaries: Vec<JobSummary> = jobs.iter().map(JobSummary::from_job).collect();

    let response = ApiResponse::success(JobListData {
        jobs: job_summaries,
        total,
    });

    HttpResponse::Ok().json(response)
}

/// Get specific job status and details
pub async fn get_job(
    path: web::Path<String>,
    job_manager: web::Data<JobManager>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let job_id_str = path.into_inner();

    info!(request_id = %request_id, job_id = %job_id_str, "Getting job details");

    // Parse job ID
    let job_id = match Uuid::parse_str(&job_id_str) {
        Ok(id) => id,
        Err(_) => {
            let response = ApiResponse::<()>::error(
                "INVALID_JOB_ID",
                "Invalid job ID format. Expected UUID.",
                None,
                false,
            );
            return HttpResponse::BadRequest().json(response);
        }
    };

    match job_manager.get_job(&job_id).await {
        Some(job) => {
            let response = ApiResponse::success(JobDetailData::from_job(&job));
            HttpResponse::Ok().json(response)
        }
        None => {
            warn!(request_id = %request_id, job_id = %job_id_str, "Job not found");
            let response = ApiResponse::<()>::error(
                "JOB_NOT_FOUND",
                &format!("Job '{}' not found", job_id_str),
                None,
                false,
            );
            HttpResponse::NotFound().json(response)
        }
    }
}

/// Rollback a failed/completed job (async operation)
pub async fn rollback_job(
    path: web::Path<String>,
    job_manager: web::Data<JobManager>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let job_id_str = path.into_inner();

    info!(request_id = %request_id, job_id = %job_id_str, "Initiating job rollback");

    // Block new jobs while a self-update is in progress
    if job_manager.is_self_update_in_progress().await {
        warn!(request_id = %request_id, "Rollback rejected — self-update in progress");
        let response = ApiResponse::<()>::error(
            "SELF_UPDATE_IN_PROGRESS",
            "Cannot accept new jobs while a self-update is in progress. Retry after it completes.",
            None,
            true,
        );
        return HttpResponse::Conflict()
            .insert_header(("Retry-After", "60"))
            .json(response);
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

    // Parse job ID
    let job_id = match Uuid::parse_str(&job_id_str) {
        Ok(id) => id,
        Err(_) => {
            let response = ApiResponse::<()>::error(
                "INVALID_JOB_ID",
                "Invalid job ID format. Expected UUID.",
                None,
                false,
            );
            return HttpResponse::BadRequest().json(response);
        }
    };

    match job_manager.create_rollback_job(&job_id).await {
        Ok(Some(rollback_job_id)) => {
            info!(
                request_id = %request_id,
                original_job_id = %job_id_str,
                rollback_job_id = %rollback_job_id,
                "Rollback job created"
            );

            let response = ApiResponse::success(serde_json::json!({
                "job_id": rollback_job_id.to_string(),
                "status": "pending",
                "operation": "rollback",
                "original_job_id": job_id_str,
                "exclusive_mode": true,
            }));

            HttpResponse::Accepted().json(response)
        }
        Ok(None) => {
            warn!(request_id = %request_id, job_id = %job_id_str, "Job not eligible for rollback");
            let response = ApiResponse::<()>::error(
                "ROLLBACK_NOT_ALLOWED",
                "Job is not eligible for rollback. Only failed or completed jobs can be rolled back.",
                Some(serde_json::json!({"job_id": job_id_str})),
                false,
            );
            HttpResponse::BadRequest().json(response)
        }
        Err(e) => {
            error!(request_id = %request_id, job_id = %job_id_str, error = ?e, "Failed to create rollback job");
            let response = ApiResponse::<()>::error(
                "JOB_CREATE_ERROR",
                &format!("Failed to create rollback job: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Delete a completed/failed job from history
pub async fn delete_job(
    path: web::Path<String>,
    job_manager: web::Data<JobManager>,
    _req: HttpRequest,
) -> impl Responder {
    let request_id = Uuid::new_v4().to_string();
    let _timestamp = Utc::now().to_rfc3339();
    let job_id_str = path.into_inner();

    info!(request_id = %request_id, job_id = %job_id_str, "Deleting job from history");

    // Parse job ID
    let job_id = match Uuid::parse_str(&job_id_str) {
        Ok(id) => id,
        Err(_) => {
            let response = ApiResponse::<()>::error(
                "INVALID_JOB_ID",
                "Invalid job ID format. Expected UUID.",
                None,
                false,
            );
            return HttpResponse::BadRequest().json(response);
        }
    };

    match job_manager.delete_job(&job_id).await {
        Ok(true) => {
            info!(request_id = %request_id, job_id = %job_id_str, "Job deleted successfully");
            let response = ApiResponse::success(serde_json::json!({
                "deleted": true,
                "job_id": job_id_str,
            }));
            HttpResponse::Ok().json(response)
        }
        Ok(false) => {
            // Check if job exists but is not deletable
            if let Some(job) = job_manager.get_job(&job_id).await {
                warn!(
                    request_id = %request_id,
                    job_id = %job_id_str,
                    status = ?job.status,
                    "Cannot delete job - not in terminal state"
                );
                let response = ApiResponse::<()>::error(
                    "DELETE_NOT_ALLOWED",
                    "Cannot delete job that is not in a terminal state (completed/failed/cancelled).",
                    Some(serde_json::json!({"job_id": job_id_str, "status": format!("{:?}", job.status).to_lowercase()})),
                    false,
                );
                HttpResponse::Conflict().json(response)
            } else {
                warn!(request_id = %request_id, job_id = %job_id_str, "Job not found");
                let response = ApiResponse::<()>::error(
                    "JOB_NOT_FOUND",
                    &format!("Job '{}' not found", job_id_str),
                    None,
                    false,
                );
                HttpResponse::NotFound().json(response)
            }
        }
        Err(e) => {
            error!(request_id = %request_id, job_id = %job_id_str, error = ?e, "Failed to delete job");
            let response = ApiResponse::<()>::error(
                "JOB_DELETE_ERROR",
                &format!("Failed to delete job: {}", e),
                None,
                true,
            );
            HttpResponse::InternalServerError().json(response)
        }
    }
}

/// Configure all job routes
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/jobs")
            .route("", web::get().to(list_jobs))
            .route("/{id}", web::get().to(get_job))
            .route("/{id}/rollback", web::post().to(rollback_job))
            .route("/{id}", web::delete().to(delete_job)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_job_status() {
        assert_eq!(parse_job_status("pending"), Some(JobStatus::Pending));
        assert_eq!(parse_job_status("PENDING"), Some(JobStatus::Pending));
        assert_eq!(parse_job_status("running"), Some(JobStatus::Running));
        assert_eq!(parse_job_status("completed"), Some(JobStatus::Completed));
        assert_eq!(parse_job_status("failed"), Some(JobStatus::Failed));
        assert_eq!(parse_job_status("invalid"), None);
    }

    #[test]
    fn test_job_list_query_default() {
        let json = r#"{}"#;
        let query: JobListQuery = serde_json::from_str(json).unwrap();
        assert!(query.status.is_none());
        assert!(query.limit.is_none());
    }

    #[test]
    fn test_job_list_query_full() {
        let json = r#"{"status": "running", "limit": 10}"#;
        let query: JobListQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.status, Some("running".to_string()));
        assert_eq!(query.limit, Some(10));
    }
}
