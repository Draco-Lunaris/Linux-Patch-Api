//! Job data types — shared by the scheduler and API handlers.
//!
//! This module contains the data types for jobs (Job, JobStatus, JobOperation,
//! JobStatusEvent). The actual job management logic lives in the Scheduler
//! (src/jobs/scheduler.rs).

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Job status
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

/// Convert JobStatus to lowercase string for WebSocket events
impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
            JobStatus::TimedOut => "timed_out",
        }
    }
}

/// Job operation type
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum JobOperation {
    Install,
    Update,
    Remove,
    PatchApply,
    Reboot,
    SelfUpdate,
    Rollback,
}

/// Job information
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub status: JobStatus,
    pub operation: JobOperation,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub packages: Vec<String>,
    pub progress: u8,
    pub message: String,
    pub logs: Vec<String>,
    pub error: Option<String>,
    /// Stable machine-readable error code (one of `error_utils::error_code::*`).
    /// Set on failure, `None` for non-failed jobs. The manager uses this to
    /// classify and route failures programmatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Exit code of the underlying package-manager command, when available.
    /// `None` for non-failed jobs or when the command could not be spawned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Captured stdout of the underlying command, when available.
    /// Truncated to [`error_utils::MAX_OUTPUT_LINES`] lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_stdout: Option<String>,
    /// Captured stderr of the underlying command, when available.
    /// Truncated to [`error_utils::MAX_OUTPUT_LINES`] lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_stderr: Option<String>,
    pub rollback_job_id: Option<Uuid>,
    pub exclusive_mode: bool,
}

impl Job {
    /// Create a new pending job
    pub fn new(operation: JobOperation, packages: Vec<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            status: JobStatus::Pending,
            operation,
            created_at: now,
            updated_at: now,
            completed_at: None,
            packages,
            progress: 0,
            message: String::from("Job created"),
            logs: Vec::new(),
            error: None,
            error_code: None,
            exit_code: None,
            command_stdout: None,
            command_stderr: None,
            rollback_job_id: None,
            exclusive_mode: false,
        }
    }

    /// Add a log entry
    pub fn add_log(&mut self, message: String) {
        self.logs.push(message);
        self.updated_at = Utc::now();
    }

    /// Update progress
    pub fn update_progress(&mut self, progress: u8, message: String) {
        self.progress = progress;
        self.message = message;
        self.updated_at = Utc::now();
    }

    /// Mark job as running
    pub fn start(&mut self) {
        self.status = JobStatus::Running;
        self.updated_at = Utc::now();
        self.add_log(String::from("Job started"));
    }

    /// Mark job as completed
    pub fn complete(&mut self) {
        self.status = JobStatus::Completed;
        self.progress = 100;
        self.completed_at = Some(Utc::now());
        self.updated_at = self.completed_at.unwrap();
        self.add_log(String::from("Job completed successfully"));
    }

    /// Mark job as failed
    pub fn fail(&mut self, error: String) {
        self.status = JobStatus::Failed;
        self.error = Some(error.clone());
        self.completed_at = Some(Utc::now());
        self.updated_at = self.completed_at.unwrap();
        self.add_log(format!("Job failed: {}", error));
    }

    /// Mark job as failed with full structured diagnostics.
    ///
    /// Populates `error` (full chain), `error_code` (stable classification),
    /// `exit_code`/`command_stdout`/`command_stderr` (from a `CommandError` in
    /// the chain, if any), and appends diagnostic log lines.
    pub fn fail_with_diagnostics(&mut self, error: &anyhow::Error) {
        use crate::packages::error_utils;

        // Full error chain for the `error` field.
        let error_chain = error_utils::format_error_chain(error);
        self.error = Some(error_chain.clone());
        self.error_code = Some(error_utils::classify_error(error).to_string());
        self.exit_code = error_utils::extract_exit_code(error);
        self.command_stdout = error_utils::extract_stdout(error);
        self.command_stderr = error_utils::extract_stderr(error);

        self.status = JobStatus::Failed;
        self.completed_at = Some(Utc::now());
        self.updated_at = self.completed_at.unwrap();
        self.add_log(format!("Job failed: {}", error_chain));

        // Append diagnostic lines (chain + captured command output) to logs.
        for line in error_utils::diagnostic_log_lines(error) {
            self.add_log(line);
        }
    }
}

/// Job status event broadcast to WebSocket clients
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JobStatusEvent {
    pub event: String,
    pub job_id: Uuid,
    pub status: String,
    pub progress: u8,
    pub message: String,
    pub timestamp: String,
    /// Error message (full chain) — only present on failure events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Stable error code — only present on failure events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}
