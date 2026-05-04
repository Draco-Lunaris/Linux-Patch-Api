//! Job Manager - Async job queue management
//!
//! Manages async job execution with concurrency limits and timeout enforcement.
//! Broadcasts job status events via tokio broadcast channel for WebSocket streaming.

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, broadcast};
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
}

/// Job Manager - handles async job queue with limits and WebSocket broadcast
pub struct JobManager {
    max_concurrent: usize,
    timeout_minutes: u64,
    jobs: Arc<RwLock<HashMap<Uuid, Job>>>,
    /// Broadcast sender for job status events
    event_sender: broadcast::Sender<JobStatusEvent>,
}

impl JobManager {
    /// Create a new job manager
    pub fn new(max_concurrent: usize, timeout_minutes: u64) -> Result<Self> {
        let (event_sender, _) = broadcast::channel(256);
        Ok(Self {
            max_concurrent,
            timeout_minutes,
            jobs: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
        })
    }

    /// Get the timeout duration
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_minutes * 60)
    }

    /// Get max concurrent jobs
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Subscribe to job status events
    /// Returns a broadcast receiver that will receive JobStatusEvent messages
    pub fn subscribe(&self) -> broadcast::Receiver<JobStatusEvent> {
        self.event_sender.subscribe()
    }

    /// Emit a job status event to all subscribers
    fn emit_event(
        &self,
        event_type: &str,
        job_id: &Uuid,
        status: &JobStatus,
        progress: u8,
        message: &str,
    ) {
        let event = JobStatusEvent {
            event: event_type.to_string(),
            job_id: *job_id,
            status: status.as_str().to_string(),
            progress,
            message: message.to_string(),
            timestamp: Utc::now().to_rfc3339(),
        };
        // Ignore send errors (no receivers is fine)
        let _ = self.event_sender.send(event);
    }

    /// Create a new job and return its ID
    pub async fn create_job(&self, operation: JobOperation, packages: Vec<String>) -> Result<Uuid> {
        let job = Job::new(operation, packages);
        let job_id = job.id;
        let status = job.status.clone();
        let progress = job.progress;
        let message = job.message.clone();

        let mut jobs = self.jobs.write().await;
        jobs.insert(job_id, job);
        drop(jobs); // Release lock before emitting event

        self.emit_event("job_status", &job_id, &status, progress, &message);

        Ok(job_id)
    }

    /// Get a job by ID
    pub async fn get_job(&self, job_id: &Uuid) -> Option<Job> {
        let jobs = self.jobs.read().await;
        jobs.get(job_id).cloned()
    }

    /// Update a job's status
    pub async fn update_job(
        &self,
        job_id: &Uuid,
        status: JobStatus,
        progress: Option<u8>,
        message: Option<String>,
    ) -> Result<()> {
        let event_data;
        {
            let mut jobs = self.jobs.write().await;

            if let Some(job) = jobs.get_mut(job_id) {
                job.status = status;
                if let Some(p) = progress {
                    job.progress = p;
                }
                if let Some(m) = message {
                    job.message = m;
                }
                job.updated_at = Utc::now();

                event_data = Some((job.status.clone(), job.progress, job.message.clone()));
            } else {
                event_data = None;
            }
        } // Write lock dropped here

        if let Some((status, progress, message)) = event_data {
            self.emit_event("job_status", job_id, &status, progress, &message);
        }

        Ok(())
    }

    /// Add a log entry to a job
    pub async fn add_job_log(&self, job_id: &Uuid, message: String) -> Result<()> {
        let mut jobs = self.jobs.write().await;

        if let Some(job) = jobs.get_mut(job_id) {
            job.add_log(message);
        }

        Ok(())
    }

    /// Mark a job as completed
    pub async fn complete_job(&self, job_id: &Uuid) -> Result<()> {
        let event_data;
        {
            let mut jobs = self.jobs.write().await;

            if let Some(job) = jobs.get_mut(job_id) {
                job.complete();
                event_data = Some((
                    job.status.clone(),
                    job.progress,
                    job.message.clone(),
                ));
            } else {
                event_data = None;
            }
        }

        if let Some((status, progress, message)) = event_data {
            self.emit_event("job_status", job_id, &status, progress, &message);
        }

        Ok(())
    }

    /// Mark a job as failed
    pub async fn fail_job(&self, job_id: &Uuid, error: String) -> Result<()> {
        let event_data;
        {
            let mut jobs = self.jobs.write().await;

            if let Some(job) = jobs.get_mut(job_id) {
                job.fail(error);
                event_data = Some((
                    job.status.clone(),
                    job.progress,
                    job.message.clone(),
                ));
            } else {
                event_data = None;
            }
        }

        if let Some((status, progress, message)) = event_data {
            self.emit_event("job_status", job_id, &status, progress, &message);
        }

        Ok(())
    }

    /// List all jobs with optional status filter
    pub async fn list_jobs(&self, status_filter: Option<JobStatus>, limit: usize) -> Vec<Job> {
        // FIX: Clone under lock, then release before sorting to reduce lock contention
        let mut result = {
            let jobs = self.jobs.read().await;
            jobs.values().cloned().collect::<Vec<Job>>()
        }; // Lock released here

        // Filter by status if provided
        if let Some(status) = status_filter {
            result.retain(|j| j.status == status);
        }

        // Sort by created_at descending (newest first)
        result.sort_by_key(|b| std::cmp::Reverse(b.created_at));

        // Apply limit
        result.truncate(limit);

        result
    }

    /// Get count of running jobs
    pub async fn running_count(&self) -> usize {
        let jobs = self.jobs.read().await;
        jobs.values()
            .filter(|j| j.status == JobStatus::Running)
            .count()
    }

    /// Check if can accept new job (respecting max_concurrent)
    pub async fn can_accept_job(&self) -> bool {
        self.running_count().await < self.max_concurrent
    }

    /// Delete a completed/failed job from history
    pub async fn delete_job(&self, job_id: &Uuid) -> Result<bool> {
        let mut jobs = self.jobs.write().await;

        if let Some(job) = jobs.get(job_id) {
            // Only allow deletion of completed/failed/cancelled jobs
            if matches!(
                job.status,
                JobStatus::Completed
                    | JobStatus::Failed
                    | JobStatus::Cancelled
                    | JobStatus::TimedOut
            ) {
                jobs.remove(job_id);
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Create a rollback job for a failed job
    pub async fn create_rollback_job(&self, original_job_id: &Uuid) -> Result<Option<Uuid>> {
        let original_job = {
            let jobs = self.jobs.read().await;
            jobs.get(original_job_id).cloned()
        };

        if let Some(original_job) = original_job {
            // Only allow rollback of failed/completed jobs
            if matches!(
                original_job.status,
                JobStatus::Failed | JobStatus::Completed
            ) {
                let rollback_job_id = self
                    .create_job(JobOperation::Rollback, original_job.packages.clone())
                    .await?;

                // Mark as exclusive mode
                {
                    let mut jobs = self.jobs.write().await;
                    if let Some(rollback_job) = jobs.get_mut(&rollback_job_id) {
                        rollback_job.exclusive_mode = true;
                        rollback_job.rollback_job_id = Some(*original_job_id);
                    }
                }

                return Ok(Some(rollback_job_id));
            }
        }

        Ok(None)
    }
}

// Thread-safe clone for sharing across handlers
impl Clone for JobManager {
    fn clone(&self) -> Self {
        Self {
            max_concurrent: self.max_concurrent,
            timeout_minutes: self.timeout_minutes,
            jobs: self.jobs.clone(),
            event_sender: self.event_sender.clone(),
        }
    }
}
