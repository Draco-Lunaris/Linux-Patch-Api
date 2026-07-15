//! Job Manager - Async job queue management
//!
//! Manages async job execution with concurrency limits and timeout enforcement.
//! Broadcasts job status events via tokio broadcast channel for WebSocket streaming.

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

/// Error returned by `try_reserve_self_update` when a self-update request
/// cannot be admitted. Each variant maps to a specific HTTP response in the
/// handler.
#[derive(Debug, Clone, PartialEq)]
pub enum SelfUpdateAdmissionError {
    /// A self-update is already in progress (duplicate request).
    AlreadyInProgress,
    /// One or more jobs are currently running or pending.
    JobsInProgress { count: usize },
    /// The job queue is at capacity.
    QueueFull,
}

impl std::fmt::Display for SelfUpdateAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelfUpdateAdmissionError::AlreadyInProgress => {
                write!(f, "A self-update is already in progress")
            }
            SelfUpdateAdmissionError::JobsInProgress { count } => {
                write!(f, "Cannot self-update while {} jobs are in progress", count)
            }
            SelfUpdateAdmissionError::QueueFull => {
                write!(f, "Job queue is at capacity")
            }
        }
    }
}

impl std::error::Error for SelfUpdateAdmissionError {}

/// Reservation guard for self-update. Automatically rolls back on drop
/// unless explicitly committed. This ensures cancellation or panic before
/// task spawn cannot leave an orphaned owner.
pub struct SelfUpdateReservation {
    pub job_id: Uuid,
    owner_handle: Arc<RwLock<Option<Uuid>>>,
    jobs_handle: Arc<RwLock<HashMap<Uuid, Job>>>,
    committed: bool,
}

impl std::fmt::Debug for SelfUpdateReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelfUpdateReservation")
            .field("job_id", &self.job_id)
            .field("committed", &self.committed)
            .finish()
    }
}

impl SelfUpdateReservation {
    /// Commit the reservation — transfer ownership to the execution task.
    /// After commit, the guard will NOT roll back on drop.
    pub fn commit(mut self) -> Uuid {
        self.committed = true;
        self.job_id
    }
}

impl Drop for SelfUpdateReservation {
    fn drop(&mut self) {
        if !self.committed {
            tracing::warn!(
                job_id = %self.job_id,
                "SelfUpdateReservation dropped without commit — rolling back owner and job"
            );
            // We can't await in Drop, so we use try_write and blocking
            // operations. In practice, if the reservation is dropped
            // before being committed, it means the task was cancelled
            // before spawn, so there shouldn't be lock contention.
            if let Ok(mut owner) = self.owner_handle.try_write() {
                if let Some(current) = &*owner {
                    if current == &self.job_id {
                        *owner = None;
                    }
                }
            }
            if let Ok(mut jobs) = self.jobs_handle.try_write() {
                jobs.remove(&self.job_id);
            }
        }
    }
}

/// Error returned by `admit_reboot` when a reboot request cannot be admitted.
#[derive(Debug, Clone, PartialEq)]
pub enum RebootAdmissionError {
    /// A self-update is in progress — reboot blocked.
    SelfUpdateInProgress,
    /// A package-database mutation is in progress — reboot blocked.
    PackageMutationInProgress,
    /// One or more jobs are currently running or pending.
    JobsInProgress { count: usize },
    /// The job queue is at capacity.
    QueueFull,
}

impl std::fmt::Display for RebootAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RebootAdmissionError::SelfUpdateInProgress => {
                write!(f, "A self-update is in progress")
            }
            RebootAdmissionError::PackageMutationInProgress => {
                write!(f, "A package-database mutation is in progress")
            }
            RebootAdmissionError::JobsInProgress { count } => {
                write!(f, "Cannot reboot while {} jobs are in progress", count)
            }
            RebootAdmissionError::QueueFull => {
                write!(f, "Job queue is at capacity")
            }
        }
    }
}

impl std::error::Error for RebootAdmissionError {}

/// Parameters for atomic reboot admission.
#[derive(Debug, Clone)]
pub struct RebootAdmission {
    pub force: bool,
    pub acknowledge_package_database_corruption_risk: bool,
}

/// Error returned by `admit_job` when a normal (non-self-update) job
/// cannot be admitted. Each variant maps to a specific HTTP response.
#[derive(Debug, Clone, PartialEq)]
pub enum JobAdmissionError {
    /// A self-update is in progress — no new jobs accepted.
    SelfUpdateInProgress,
    /// The job queue is at capacity.
    QueueFull,
}

impl std::fmt::Display for JobAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobAdmissionError::SelfUpdateInProgress => {
                write!(f, "A self-update is in progress")
            }
            JobAdmissionError::QueueFull => {
                write!(f, "Job queue is at capacity")
            }
        }
    }
}

impl std::error::Error for JobAdmissionError {}

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

/// Bundles the error fields for [`JobStatusEvent`] emission, so `emit_event` stays
/// under clippy's argument-count limit. `None` means "not a failure event".
#[derive(Debug, Clone)]
struct EventError {
    error: String,
    code: String,
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

/// Job Manager - handles async job queue with limits and WebSocket broadcast
pub struct JobManager {
    max_concurrent: usize,
    timeout_minutes: u64,
    max_queue_depth: usize,
    jobs: Arc<RwLock<HashMap<Uuid, Job>>>,
    /// Broadcast sender for job status events
    event_sender: broadcast::Sender<JobStatusEvent>,
    /// Self-update ownership state. `None` = no self-update in progress (idle).
    /// `Some(job_id)` = a self-update is in progress, owned by the given job.
    /// The job ID is the permit: only the owner can release the lock.
    /// This prevents one self-update from clearing another's flag.
    self_update_owner: Arc<RwLock<Option<Uuid>>>,
}

impl JobManager {
    /// Create a new job manager
    pub fn new(
        max_concurrent: usize,
        timeout_minutes: u64,
        max_queue_depth: usize,
    ) -> Result<Self> {
        let (event_sender, _) = broadcast::channel(256);
        Ok(Self {
            max_concurrent,
            timeout_minutes,
            max_queue_depth,
            jobs: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            self_update_owner: Arc::new(RwLock::new(None)),
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

    /// Get max queue depth
    pub fn max_queue_depth(&self) -> usize {
        self.max_queue_depth
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
        error_info: Option<EventError>,
    ) {
        let event = JobStatusEvent {
            event: event_type.to_string(),
            job_id: *job_id,
            status: status.as_str().to_string(),
            progress,
            message: message.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            error: error_info.as_ref().map(|e| e.error.clone()),
            error_code: error_info.as_ref().map(|e| e.code.clone()),
        };
        // Ignore send errors (no receivers is fine)
        let _ = self.event_sender.send(event);
    }

    /// Create a new job and return its ID.
    ///
    /// **WARNING**: This bypasses the self-update guard and queue capacity
    /// check. Only use this when you have already performed admission checks
    /// via `admit_job` or `try_reserve_self_update`, or in the emergency
    /// reboot path where the caller has explicitly acknowledged the risk.
    /// All normal job creation MUST go through `admit_job`.
    pub async fn create_job_unchecked(
        &self,
        operation: JobOperation,
        packages: Vec<String>,
    ) -> Result<Uuid> {
        let job = Job::new(operation, packages);
        let job_id = job.id;
        let status = job.status.clone();
        let progress = job.progress;
        let message = job.message.clone();

        let mut jobs = self.jobs.write().await;
        jobs.insert(job_id, job);
        drop(jobs); // Release lock before emitting event

        self.emit_event("job_status", &job_id, &status, progress, &message, None);

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
            self.emit_event("job_status", job_id, &status, progress, &message, None);
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
                event_data = Some((job.status.clone(), job.progress, job.message.clone()));
            } else {
                event_data = None;
            }
        }

        if let Some((status, progress, message)) = event_data {
            self.emit_event("job_status", job_id, &status, progress, &message, None);
        }

        Ok(())
    }

    /// Mark a job as failed.
    pub async fn fail_job(&self, job_id: &Uuid, error: String) -> Result<()> {
        let event_data;
        {
            let mut jobs = self.jobs.write().await;

            if let Some(job) = jobs.get_mut(job_id) {
                job.fail(error);
                event_data = Some((job.status.clone(), job.progress, job.message.clone()));
            } else {
                event_data = None;
            }
        }

        if let Some((status, progress, message)) = event_data {
            self.emit_event("job_status", job_id, &status, progress, &message, None);
        }

        Ok(())
    }

    /// Mark a job as failed with rich diagnostics.
    ///
    /// This is the preferred failure path for async package operations. It:
    /// - Sets `job.error` to the full anyhow error chain (all `.context()` layers +
    ///   root cause), so the manager sees the same depth as the local journal.
    /// - Sets `job.error_code` to a stable classification
    ///   (one of [`crate::packages::error_utils::error_code`]::*).
    /// - Sets `job.exit_code`/`command_stdout`/`command_stderr` from a
    ///   [`crate::packages::error_utils::CommandError`] in the chain, if present.
    /// - Appends diagnostic log lines to `job.logs`: the error chain plus any captured
    ///   command output (stdout/stderr with stream prefixes, exit code).
    /// - Emits a WebSocket `job_status` event with `error` and `error_code` populated,
    ///   so the manager receives the failure in real time (not just on poll).
    ///
    /// The manager retrieves full details via `GET /api/v1/jobs/{id}`. The WebSocket
    /// event carries the error code + a short error string for real-time alerting.
    pub async fn fail_job_with_diagnostics(
        &self,
        job_id: &Uuid,
        error: &anyhow::Error,
    ) -> Result<()> {
        let event_data;
        {
            let mut jobs = self.jobs.write().await;

            if let Some(job) = jobs.get_mut(job_id) {
                // Capture the error code + short message before fail_with_diagnostics
                // consumes the error reference for the chain/logs.
                let code = crate::packages::error_utils::classify_error(error).to_string();
                let short_error = crate::packages::error_utils::format_error_for_cache(error);

                job.fail_with_diagnostics(error);

                event_data = Some((
                    job.status.clone(),
                    job.progress,
                    job.message.clone(),
                    short_error,
                    code,
                ));
            } else {
                event_data = None;
            }
        }

        if let Some((status, progress, message, err, code)) = event_data {
            self.emit_event(
                "job_status",
                job_id,
                &status,
                progress,
                &message,
                Some(EventError { error: err, code }),
            );
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

    /// Get count of active jobs (running + pending).
    ///
    /// This is the count that matters for safety checks: a pending job can
    /// transition to running at any time, so both must be zero before a
    /// self-update or reboot can proceed safely.
    pub async fn active_count(&self) -> usize {
        let jobs = self.jobs.read().await;
        jobs.values()
            .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
            .count()
    }

    /// Check if can accept new job (respecting max_queue_depth)
    /// Returns false when the total number of pending + running jobs
    /// equals or exceeds the configured queue depth cap.
    pub async fn can_accept_job(&self) -> bool {
        let jobs = self.jobs.read().await;
        let active_count = jobs
            .values()
            .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
            .count();
        drop(jobs);
        active_count < self.max_queue_depth
    }

    /// Returns true if a self-update (linux-patch-api upgrade) is in progress.
    /// While true, all non-self-update job endpoints should reject new jobs
    /// with 409 Conflict to prevent the delayed restart from killing a
    /// concurrent package operation.
    pub async fn is_self_update_in_progress(&self) -> bool {
        self.self_update_owner.read().await.is_some()
    }

    /// Mark that a self-update is in progress, owned by `job_id`.
    /// Used by the startup reconciliation path (main.rs) when the persistent
    /// state file indicates a restart is pending.
    pub async fn set_self_update_in_progress(&self, job_id: Uuid) {
        *self.self_update_owner.write().await = Some(job_id);
    }

    /// Release the self-update lock. Only succeeds if the caller's `job_id`
    /// matches the current owner. This prevents one self-update from clearing
    /// another's lock — if Update A finishes and calls release, but Update B
    /// now owns the lock, the release is a no-op (logged as a warning).
    ///
    /// Returns true if the lock was released, false if the caller did not own it.
    pub async fn release_self_update(&self, job_id: &Uuid) -> bool {
        let mut owner = self.self_update_owner.write().await;
        match &*owner {
            Some(current) if current == job_id => {
                *owner = None;
                true
            }
            Some(current) => {
                tracing::warn!(
                    caller_job_id = %job_id,
                    owner_job_id = %current,
                    "release_self_update: caller does not own the lock — ignoring"
                );
                false
            }
            None => {
                tracing::warn!(
                    caller_job_id = %job_id,
                    "release_self_update: no self-update in progress — ignoring"
                );
                false
            }
        }
    }

    /// Force-clear the self-update lock regardless of ownership.
    ///
    /// Used ONLY by the startup path (main.rs) when the new process has
    /// successfully initialized and needs to clear any stale reservation
    /// from the old process.
    pub async fn force_clear_self_update(&self) {
        *self.self_update_owner.write().await = None;
    }

    /// Get a clone of the self_update_owner Arc for external manipulation.
    ///
    /// This allows the startup path to clear the self-update flag after
    /// the JobManager has been moved into web::Data, without needing
    /// a reference to the JobManager itself.
    pub fn self_update_owner_handle(&self) -> Arc<RwLock<Option<Uuid>>> {
        self.self_update_owner.clone()
    }

    /// Atomically reserve a self-update slot.
    ///
    /// This is the single admission point for self-update requests. It
    /// performs all checks and state changes under one exclusive lock
    /// acquisition, preventing the check-then-set race where a competing
    /// patch/install/remove request interleaves between the running-count
    /// check and the flag set.
    ///
    /// Under the `self_update_owner` write lock, this method:
    /// 1. Rejects if a self-update is already in progress (duplicate request)
    /// 2. Rejects if any jobs are running or pending (concurrent operations)
    /// 3. Rejects if the job queue is at capacity
    /// 4. Sets the self-update owner to the new job's UUID (permit)
    /// 5. Creates the self-update job in the jobs map
    ///
    /// Because the `self_update_owner` write lock is held for the
    /// entire duration, no other handler can read `is_self_update_in_progress()`
    /// (it will block until we release, then see Some) and no second
    /// self-update can pass the owner check.
    ///
    /// Returns a `SelfUpdateReservation` guard that owns the job ID and
    /// automatically rolls back (removes owner and job) on drop unless
    /// explicitly committed via `commit()`. This ensures cancellation
    /// or panic before task spawn cannot leave an orphaned owner.
    pub async fn try_reserve_self_update(
        &self,
        packages: Vec<String>,
    ) -> Result<SelfUpdateReservation, SelfUpdateAdmissionError> {
        // Create the job first to get the job_id (the ownership permit).
        let job = Job::new(JobOperation::SelfUpdate, packages);
        let job_id = job.id;

        // Acquire the self-update write lock FIRST and hold it for the
        // entire operation. This prevents any other handler from reading
        // is_self_update_in_progress() (they'll block until we release,
        // then see Some) and prevents a second self-update from passing
        // the owner check.
        let mut su_guard = self.self_update_owner.write().await;

        // 1. Reject duplicate self-update
        if su_guard.is_some() {
            return Err(SelfUpdateAdmissionError::AlreadyInProgress);
        }

        // 2. Check for running/pending jobs under the jobs read lock.
        //    We hold the self-update write lock, so no other handler can
        //    start a new job between this check and the owner set below.
        {
            let jobs = self.jobs.read().await;
            let active_count = jobs
                .values()
                .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
                .count();
            if active_count > 0 {
                return Err(SelfUpdateAdmissionError::JobsInProgress {
                    count: active_count,
                });
            }
            // Queue capacity check: if max_queue_depth is 0 (misconfiguration),
            // reject. Otherwise, active_count == 0 means we're below capacity.
            if self.max_queue_depth == 0 {
                return Err(SelfUpdateAdmissionError::QueueFull);
            }
        }

        // 4. Set the self-update owner to this job's UUID. All other
        //    handlers will now see Some when they check is_self_update_in_progress().
        *su_guard = Some(job_id);
        drop(su_guard);

        // 5. Insert the job into the jobs map. The owner is already set,
        //    so no other handler can create a job.
        let status = job.status.clone();
        let progress = job.progress;
        let message = job.message.clone();

        {
            let mut jobs = self.jobs.write().await;
            jobs.insert(job_id, job);
        }

        self.emit_event("job_status", &job_id, &status, progress, &message, None);

        // Return a reservation guard. If dropped without commit(),
        // it rolls back the owner and removes the job.
        Ok(SelfUpdateReservation {
            job_id,
            owner_handle: self.self_update_owner.clone(),
            jobs_handle: self.jobs.clone(),
            committed: false,
        })
    }

    /// Atomically admit a normal (non-self-update) job.
    ///
    /// This is the single admission point for all non-self-update job-creating
    /// endpoints (install, update, remove, patch-apply, reboot, rollback). It
    /// performs the self-update flag check and job creation under one lock
    /// acquisition, preventing the check-then-create race where a self-update
    /// reservation interleaves between the flag check and `create_job()`.
    ///
    /// Under the `self_update_owner` read lock, this method:
    /// 1. Rejects if a self-update is in progress (no new jobs during self-update)
    /// 2. Acquires the `jobs` write lock and checks queue capacity
    /// 3. Creates the job in the jobs map
    ///
    /// The read lock on `self_update_owner` blocks the self-update handler
    /// from acquiring its write lock (in `try_reserve_self_update`) while we're
    /// creating the job. This means a self-update cannot set its owner between
    /// our flag check and our job creation.
    ///
    /// No handler should call `create_job()` directly. All job admission must
    /// go through either `admit_job` (normal) or `try_reserve_self_update`
    /// (self-update).
    pub async fn admit_job(
        &self,
        operation: JobOperation,
        packages: Vec<String>,
    ) -> Result<Uuid, JobAdmissionError> {
        // Acquire the self-update READ lock and hold it for the entire
        // operation. This blocks try_reserve_self_update from acquiring
        // its write lock while we're checking and creating.
        let su_guard = self.self_update_owner.read().await;

        // 1. Reject if a self-update is in progress
        if su_guard.is_some() {
            return Err(JobAdmissionError::SelfUpdateInProgress);
        }

        // 2. Check queue capacity and create the job atomically under
        //    the jobs write lock. We still hold the self-update read lock,
        //    so no self-update can set its flag between these steps.
        let job = Job::new(operation, packages);
        let job_id = job.id;
        let status = job.status.clone();
        let progress = job.progress;
        let message = job.message.clone();

        {
            let mut jobs = self.jobs.write().await;
            let active_count = jobs
                .values()
                .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
                .count();
            if active_count >= self.max_queue_depth {
                return Err(JobAdmissionError::QueueFull);
            }
            jobs.insert(job_id, job);
        }

        // Release the self-update read lock before emitting the event
        // (event emission doesn't need the lock, and releasing early
        // reduces lock contention).
        drop(su_guard);

        self.emit_event("job_status", &job_id, &status, progress, &message, None);

        Ok(job_id)
    }
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

    /// Atomically admit a reboot job with coordinated checks.
    ///
    /// This is the single admission point for reboot requests. It performs
    /// all safety checks and job creation under one lock acquisition,
    /// preventing races between the reboot check and concurrent job/self-update
    /// creation.
    ///
    /// **Tier 1 — force=false**: All guards active. Rejects if any jobs are
    /// active, a self-update is in progress, or a package mutation is in
    /// progress.
    ///
    /// **Tier 2 — force=true, ack=false**: Bypasses ordinary active-job
    /// protection but does NOT bypass the self-update or package-mutation
    /// guard. A forced reboot during a package-DB mutation can corrupt
    /// dpkg/rpm/pacman state.
    ///
    /// **Tier 3 — force=true, ack=true**: Bypasses all guards. The caller
    /// has explicitly acknowledged the risk. A durable audit event is logged.
    /// The job is still created atomically.
    ///
    /// `pkg_mutation_in_progress` is checked externally (from the coordinator)
    /// and passed in to avoid the JobManager needing a reference to the
    /// coordinator.
    pub async fn admit_reboot(
        &self,
        admission: RebootAdmission,
        pkg_mutation_in_progress: bool,
    ) -> Result<Uuid, RebootAdmissionError> {
        let force = admission.force;
        let ack = admission.acknowledge_package_database_corruption_risk;

        // Acquire the self-update write lock and hold it for the entire
        // operation. This prevents any other handler from creating jobs
        // or reserving self-update while we're checking and creating.
        let su_guard = self.self_update_owner.write().await;

        let self_update_active = su_guard.is_some();

        // Tier 1: force=false — check all guards
        if !force {
            if self_update_active {
                return Err(RebootAdmissionError::SelfUpdateInProgress);
            }
            // Check jobs under the jobs read lock
            let active_count = {
                let jobs = self.jobs.read().await;
                jobs.values()
                    .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
                    .count()
            };
            if active_count > 0 {
                return Err(RebootAdmissionError::JobsInProgress {
                    count: active_count,
                });
            }
        }

        // Tier 2/3: force=true — check package mutation guard
        if force && !ack && (self_update_active || pkg_mutation_in_progress) {
            if self_update_active {
                return Err(RebootAdmissionError::SelfUpdateInProgress);
            }
            return Err(RebootAdmissionError::PackageMutationInProgress);
        }

        // Tier 3: force=true + ack=true — log durable audit event
        if force && ack && (self_update_active || pkg_mutation_in_progress) {
            tracing::error!(
                self_update_active = self_update_active,
                pkg_mutation_in_progress = pkg_mutation_in_progress,
                "AUDIT: Forced reboot accepted with package-database corruption risk acknowledged. \
                 This may corrupt dpkg/rpm/pacman state and leave the system unbootable."
            );
        }

        // Create the reboot job atomically under the jobs write lock.
        // We still hold the self_update write lock, so no self-update can
        // set its flag between our check and job creation.
        let job = Job::new(JobOperation::Reboot, vec![]);
        let job_id = job.id;
        let status = job.status.clone();
        let progress = job.progress;
        let message = job.message.clone();

        {
            let mut jobs = self.jobs.write().await;
            let active_count = jobs
                .values()
                .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
                .count();
            if active_count >= self.max_queue_depth {
                return Err(RebootAdmissionError::QueueFull);
            }
            jobs.insert(job_id, job);
        }

        // Release the self-update write lock before emitting the event
        drop(su_guard);

        self.emit_event("job_status", &job_id, &status, progress, &message, None);

        Ok(job_id)
    }

    /// Create a rollback job for a failed job.
    ///
    /// Uses `admit_job` internally to atomically check the self-update flag
    /// and queue capacity before creating the rollback job.
    pub async fn create_rollback_job(
        &self,
        original_job_id: &Uuid,
    ) -> Result<Option<Uuid>, JobAdmissionError> {
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
                    .admit_job(JobOperation::Rollback, original_job.packages.clone())
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
            max_queue_depth: self.max_queue_depth,
            jobs: self.jobs.clone(),
            event_sender: self.event_sender.clone(),
            self_update_owner: self.self_update_owner.clone(),
        }
    }
}
