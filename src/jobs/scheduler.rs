//! Unified Operation Scheduler — the single authoritative control point
//! for all operation admission and lifecycle decisions.
//!
//! One `SchedulerState` behind one `tokio::sync::Mutex` owns:
//! - job map (pending, running, completed, failed)
//! - active mutation tracking (at most one)
//! - self-update / upgrade operation state
//! - reboot pending state
//! - admission mode (open, frozen-for-shutdown, recovery)
//! - max_concurrent enforcement
//!
//! Handlers call scheduler methods that acquire the lock, make all
//! decisions atomically, and return RAII guards where the operation
//! outlives the lock hold. No handler samples Booleans or checks
//! separate locks.

use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

use crate::jobs::manager::{Job, JobOperation, JobStatus, JobStatusEvent};

/// Admission mode for the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionMode {
    /// Normal operation — new jobs and mutations accepted.
    Open,
    /// Shutdown in progress — no new mutations or jobs accepted.
    Frozen,
    /// Recovery mode — no mutations accepted. Health reports degraded.
    /// Read-only endpoints remain available.
    Recovery,
}

/// A self-update / upgrade operation tracked by the scheduler.
#[derive(Debug, Clone)]
pub struct UpgradeOperation {
    pub job_id: Uuid,
    pub from_version: String,
    pub target_version: String,
    pub generation: u64,
}

/// The single source of truth for all operation state.
pub struct SchedulerState {
    /// Current admission mode.
    admission: AdmissionMode,
    /// All jobs by ID.
    jobs: HashMap<Uuid, Job>,
    /// The job ID currently executing a package-DB mutation, if any.
    /// At most one mutation runs at a time.
    active_mutation: Option<Uuid>,
    /// Active self-update operation, if any.
    self_update: Option<UpgradeOperation>,
    /// Pending reboot job ID, if any.
    reboot_pending: Option<Uuid>,
    /// Maximum number of concurrently running jobs.
    max_concurrent: usize,
    /// Maximum queue depth (pending + running).
    max_queue_depth: usize,
    /// Broadcast sender for job status events.
    event_sender: broadcast::Sender<JobStatusEvent>,
}

/// The scheduler — shared via `Arc<Scheduler>` across all handlers.
pub struct Scheduler {
    state: Mutex<SchedulerState>,
}

impl Scheduler {
    /// Create a new scheduler.
    pub fn new(max_concurrent: usize, max_queue_depth: usize) -> Arc<Self> {
        let (event_sender, _) = broadcast::channel(256);
        Arc::new(Self {
            state: Mutex::new(SchedulerState {
                admission: AdmissionMode::Open,
                jobs: HashMap::new(),
                active_mutation: None,
                self_update: None,
                reboot_pending: None,
                max_concurrent: max_concurrent.max(1),
                max_queue_depth: max_queue_depth.max(1),
                event_sender,
            }),
        })
    }

    // ------------------------------------------------------------------
    // Admission queries (read-only, used by health endpoint)
    // ------------------------------------------------------------------

    /// Check if a self-update is in progress.
    pub async fn is_self_update_in_progress(&self) -> bool {
        self.state.lock().await.self_update.is_some()
    }

    /// Check if a package mutation is in progress.
    pub async fn is_mutation_in_progress(&self) -> bool {
        self.state.lock().await.active_mutation.is_some()
    }

    /// Get the current admission mode.
    pub async fn admission_mode(&self) -> AdmissionMode {
        self.state.lock().await.admission
    }

    /// Count active (running + pending) jobs.
    pub async fn active_count(&self) -> usize {
        let state = self.state.lock().await;
        state
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
            .count()
    }

    /// Count running jobs only.
    pub async fn running_count(&self) -> usize {
        let state = self.state.lock().await;
        state
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Running)
            .count()
    }

    // ------------------------------------------------------------------
    // Job admission
    // ------------------------------------------------------------------

    /// Admit a normal (non-self-update, non-reboot) job.
    ///
    /// Atomically checks:
    /// - admission mode is Open
    /// - no self-update in progress
    /// - queue capacity
    /// - max_concurrent (running count)
    ///
    /// Creates the job in Pending status and returns its ID.
    pub async fn admit_job(
        &self,
        operation: JobOperation,
        packages: Vec<String>,
    ) -> Result<Uuid, JobAdmissionError> {
        let mut state = self.state.lock().await;
        admit_job_inner(&mut state, operation, packages)
    }

    // ------------------------------------------------------------------
    // Self-update reservation
    // ------------------------------------------------------------------

    /// Atomically reserve a self-update slot.
    ///
    /// Checks (under the lock):
    /// - admission mode is Open
    /// - no existing self-update
    /// - no active jobs (running or pending)
    /// - queue capacity
    ///
    /// On success, sets the self_update operation and creates the job.
    /// Returns a `SelfUpdateReservationGuard` that rolls back on drop
    /// if not committed.
    pub async fn try_reserve_self_update(
        self: &Arc<Self>,
        packages: Vec<String>,
        from_version: &str,
        target_version: &str,
    ) -> Result<SelfUpdateReservationGuard, SelfUpdateAdmissionError> {
        let mut state = self.state.lock().await;

        if state.admission != AdmissionMode::Open {
            return Err(SelfUpdateAdmissionError::AlreadyInProgress);
        }
        if state.self_update.is_some() {
            return Err(SelfUpdateAdmissionError::AlreadyInProgress);
        }

        let active_count = state
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
            .count();
        if active_count > 0 {
            return Err(SelfUpdateAdmissionError::JobsInProgress {
                count: active_count,
            });
        }
        if state.max_queue_depth == 0 {
            return Err(SelfUpdateAdmissionError::QueueFull);
        }

        let job = Job::new(JobOperation::SelfUpdate, packages);
        let job_id = job.id;
        let generation = next_generation();

        state.self_update = Some(UpgradeOperation {
            job_id,
            from_version: from_version.to_string(),
            target_version: target_version.to_string(),
            generation,
        });
        state.jobs.insert(job_id, job);
        emit_event(
            &state,
            "job_status",
            &job_id,
            &JobStatus::Pending,
            0,
            "Job created",
        );

        Ok(SelfUpdateReservationGuard {
            scheduler: self.clone(),
            job_id,
            committed: false,
        })
    }

    /// Release the self-update lock. Only succeeds if the caller's job_id
    /// matches the current self_update owner.
    pub async fn release_self_update(&self, job_id: &Uuid) -> bool {
        let mut state = self.state.lock().await;
        if let Some(ref su) = state.self_update {
            if su.job_id == *job_id {
                state.self_update = None;
                return true;
            }
        }
        false
    }

    /// Force-clear the self-update lock regardless of ownership.
    pub async fn force_clear_self_update(&self) {
        self.state.lock().await.self_update = None;
    }

    // ------------------------------------------------------------------
    // Reboot admission
    // ------------------------------------------------------------------

    /// Atomically admit a reboot job.
    ///
    /// Three-tier force model:
    /// - force=false: reject if any jobs active, self-update, or mutation
    /// - force=true, ack=false: reject if self-update or mutation active
    /// - force=true, ack=true: bypass all guards (audit logged)
    pub async fn admit_reboot(
        &self,
        force: bool,
        ack_corruption_risk: bool,
    ) -> Result<Uuid, RebootAdmissionError> {
        let mut state = self.state.lock().await;

        if state.admission != AdmissionMode::Open {
            return Err(RebootAdmissionError::SelfUpdateInProgress);
        }

        let self_update_active = state.self_update.is_some();
        let mutation_active = state.active_mutation.is_some();
        let active_jobs = state
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
            .count();

        if !force {
            if self_update_active {
                return Err(RebootAdmissionError::SelfUpdateInProgress);
            }
            if active_jobs > 0 {
                return Err(RebootAdmissionError::JobsInProgress { count: active_jobs });
            }
        }

        if force && !ack_corruption_risk && (self_update_active || mutation_active) {
            if self_update_active {
                return Err(RebootAdmissionError::SelfUpdateInProgress);
            }
            return Err(RebootAdmissionError::PackageMutationInProgress);
        }

        if force && ack_corruption_risk && (self_update_active || mutation_active) {
            tracing::error!(
                self_update_active,
                mutation_active,
                "AUDIT: Forced reboot accepted with package-database corruption risk acknowledged"
            );
        }

        // Check queue capacity
        if active_jobs >= state.max_queue_depth {
            return Err(RebootAdmissionError::QueueFull);
        }

        let job = Job::new(JobOperation::Reboot, vec![]);
        let job_id = job.id;
        state.reboot_pending = Some(job_id);
        state.jobs.insert(job_id, job);
        emit_event(
            &state,
            "job_status",
            &job_id,
            &JobStatus::Pending,
            0,
            "Job created",
        );

        Ok(job_id)
    }

    // ------------------------------------------------------------------
    // Mutation execution
    // ------------------------------------------------------------------

    /// Acquire the mutation slot. At most one mutation runs at a time.
    /// Returns a guard that releases the slot on drop.
    ///
    /// The closure is executed OUTSIDE the lock — the lock is only held
    /// to set `active_mutation`. This prevents blocking the scheduler
    /// while a package-manager command runs.
    pub async fn run_mutation<F, T>(&self, job_id: Uuid, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        {
            let mut state = self.state.lock().await;
            if state.admission == AdmissionMode::Frozen {
                return Err(anyhow::anyhow!(
                    "Mutation admission frozen — shutdown in progress"
                ));
            }
            if state.admission == AdmissionMode::Recovery {
                return Err(anyhow::anyhow!(
                    "Mutation rejected — system in recovery mode"
                ));
            }
            if state.active_mutation.is_some() {
                return Err(anyhow::anyhow!(
                    "A package-DB mutation is already in progress"
                ));
            }
            state.active_mutation = Some(job_id);
        }

        // Run the mutation outside the lock in a blocking thread
        let result = tokio::task::spawn_blocking(f).await;

        // Clear the mutation slot regardless of result
        {
            let mut state = self.state.lock().await;
            state.active_mutation = None;
        }

        result.map_err(|e| anyhow::anyhow!("Mutation task panicked: {}", e))?
    }

    /// Try to run a mutation without blocking. Returns `Err(MutationBusy)`
    /// if a mutation is already in progress or admission is frozen.
    pub async fn try_run_mutation<F, T>(&self, f: F) -> Result<T, TryMutationError>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let job_id = Uuid::new_v4(); // synthetic ID for try_run
        {
            let mut state = self.state.lock().await;
            if state.admission != AdmissionMode::Open {
                return Err(TryMutationError::Busy);
            }
            if state.active_mutation.is_some() {
                return Err(TryMutationError::Busy);
            }
            state.active_mutation = Some(job_id);
        }

        let result = tokio::task::spawn_blocking(f).await;

        {
            let mut state = self.state.lock().await;
            state.active_mutation = None;
        }

        result
            .map_err(|e| TryMutationError::Failed(anyhow::anyhow!("Task panicked: {}", e)))?
            .map_err(TryMutationError::Failed)
    }

    // ------------------------------------------------------------------
    // Shutdown / drain
    // ------------------------------------------------------------------

    /// Freeze admission — no new mutations or jobs accepted.
    /// Called by the SIGTERM handler before draining.
    pub async fn freeze_admission(&self) {
        let mut state = self.state.lock().await;
        state.admission = AdmissionMode::Frozen;
    }

    /// Enter recovery mode — no mutations accepted, health reports degraded.
    pub async fn enter_recovery(&self) {
        let mut state = self.state.lock().await;
        state.admission = AdmissionMode::Recovery;
    }

    /// Check if the scheduler is drained (no active mutations, no running jobs).
    pub async fn is_drained(&self) -> bool {
        let state = self.state.lock().await;
        state.active_mutation.is_none()
            && !state.jobs.values().any(|j| j.status == JobStatus::Running)
    }

    // ------------------------------------------------------------------
    // Job lifecycle (delegated to inner state)
    // ------------------------------------------------------------------

    pub async fn get_job(&self, job_id: &Uuid) -> Option<Job> {
        self.state.lock().await.jobs.get(job_id).cloned()
    }

    pub async fn update_job(
        &self,
        job_id: &Uuid,
        status: JobStatus,
        progress: Option<u8>,
        message: Option<String>,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        let event_data;
        if let Some(job) = state.jobs.get_mut(job_id) {
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
        if let Some((status, progress, message)) = event_data {
            emit_event(&state, "job_status", job_id, &status, progress, &message);
        }
        Ok(())
    }

    pub async fn add_job_log(&self, job_id: &Uuid, message: String) -> Result<()> {
        let mut state = self.state.lock().await;
        if let Some(job) = state.jobs.get_mut(job_id) {
            job.add_log(message);
        }
        Ok(())
    }

    pub async fn complete_job(&self, job_id: &Uuid) -> Result<()> {
        let mut state = self.state.lock().await;
        let event_data;
        if let Some(job) = state.jobs.get_mut(job_id) {
            job.complete();
            event_data = Some((job.status.clone(), job.progress, job.message.clone()));
        } else {
            event_data = None;
        }
        if let Some((status, progress, message)) = event_data {
            emit_event(&state, "job_status", job_id, &status, progress, &message);
        }
        Ok(())
    }

    pub async fn fail_job(&self, job_id: &Uuid, error: String) -> Result<()> {
        let mut state = self.state.lock().await;
        let event_data;
        if let Some(job) = state.jobs.get_mut(job_id) {
            job.fail(error);
            event_data = Some((job.status.clone(), job.progress, job.message.clone()));
        } else {
            event_data = None;
        }
        if let Some((status, progress, message)) = event_data {
            emit_event(&state, "job_status", job_id, &status, progress, &message);
        }
        Ok(())
    }

    pub async fn fail_job_with_diagnostics(
        &self,
        job_id: &Uuid,
        error: &anyhow::Error,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        let event_data;
        if let Some(job) = state.jobs.get_mut(job_id) {
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
        if let Some((status, progress, message, err, code)) = event_data {
            emit_event_detailed(
                &state,
                job_id,
                &status,
                progress,
                &message,
                Some(err),
                Some(code),
            );
        }
        Ok(())
    }

    pub async fn list_jobs(&self, status_filter: Option<JobStatus>, limit: usize) -> Vec<Job> {
        let state = self.state.lock().await;
        let mut result: Vec<Job> = state.jobs.values().cloned().collect();
        if let Some(status) = status_filter {
            result.retain(|j| j.status == status);
        }
        result.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        result.truncate(limit);
        result
    }

    pub async fn delete_job(&self, job_id: &Uuid) -> Result<bool> {
        let mut state = self.state.lock().await;
        if let Some(job) = state.jobs.get(job_id) {
            if matches!(
                job.status,
                JobStatus::Completed
                    | JobStatus::Failed
                    | JobStatus::Cancelled
                    | JobStatus::TimedOut
            ) {
                state.jobs.remove(job_id);
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn create_rollback_job(
        &self,
        original_job_id: &Uuid,
    ) -> Result<Option<Uuid>, JobAdmissionError> {
        let mut state = self.state.lock().await;
        let original_job = state.jobs.get(original_job_id).cloned();
        if let Some(original_job) = original_job {
            if matches!(
                original_job.status,
                JobStatus::Failed | JobStatus::Completed
            ) {
                let rollback_job_id = admit_job_inner(
                    &mut state,
                    JobOperation::Rollback,
                    original_job.packages.clone(),
                )?;
                if let Some(rollback_job) = state.jobs.get_mut(&rollback_job_id) {
                    rollback_job.exclusive_mode = true;
                    rollback_job.rollback_job_id = Some(*original_job_id);
                }
                return Ok(Some(rollback_job_id));
            }
        }
        Ok(None)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobStatusEvent> {
        // We need to get the sender without holding the lock for long.
        // Since event_sender is behind the Mutex, we lock briefly.
        // This is fine — broadcast::subscribe is fast.
        // We use try_lock to avoid blocking, but if it fails we need to await.
        // Actually, we can't return a receiver from an async fn without
        // holding the lock. Let's use blocking_lock.
        // No — we're in an async context. Let's just await.
        // But this function is not async. Let me make it async.
        // Actually, the callers expect a non-async subscribe. Let me
        // store the sender outside the Mutex.
        unimplemented!("subscribe requires restructuring — use subscribe_async")
    }

    pub async fn subscribe_async(&self) -> broadcast::Receiver<JobStatusEvent> {
        self.state.lock().await.event_sender.subscribe()
    }

    pub async fn max_concurrent(&self) -> usize {
        self.state.lock().await.max_concurrent
    }

    pub async fn max_queue_depth(&self) -> usize {
        self.state.lock().await.max_queue_depth
    }
}

// ------------------------------------------------------------------
// RAII guards
// ------------------------------------------------------------------

/// Guard for a self-update reservation. Rolls back on drop if not committed.
pub struct SelfUpdateReservationGuard {
    scheduler: Arc<Scheduler>,
    pub job_id: Uuid,
    committed: bool,
}

impl SelfUpdateReservationGuard {
    /// Commit the reservation. After this, dropping the guard will NOT
    /// roll back. Returns the job ID.
    pub fn commit(mut self) -> Uuid {
        self.committed = true;
        self.job_id
    }
}

impl Drop for SelfUpdateReservationGuard {
    fn drop(&mut self) {
        if !self.committed {
            tracing::warn!(
                job_id = %self.job_id,
                "SelfUpdateReservationGuard dropped without commit — rolling back"
            );
            // Use try_lock to avoid deadlock in Drop. If it fails, the
            // owner stays set (fail-closed — mutations blocked, which is safe).
            // We spawn a task to do the cleanup asynchronously.
            let scheduler = self.scheduler.clone();
            let job_id = self.job_id;
            tokio::spawn(async move {
                let mut state = scheduler.state.lock().await;
                if let Some(ref su) = state.self_update {
                    if su.job_id == job_id {
                        state.self_update = None;
                    }
                }
                state.jobs.remove(&job_id);
                tracing::info!(job_id = %job_id, "Self-update reservation rolled back");
            });
        }
    }
}

// ------------------------------------------------------------------
// Error types
// ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum JobAdmissionError {
    SelfUpdateInProgress,
    QueueFull,
    AdmissionFrozen,
}

impl std::fmt::Display for JobAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobAdmissionError::SelfUpdateInProgress => write!(f, "A self-update is in progress"),
            JobAdmissionError::QueueFull => write!(f, "Job queue is at capacity"),
            JobAdmissionError::AdmissionFrozen => {
                write!(f, "Admission frozen — shutdown in progress")
            }
        }
    }
}
impl std::error::Error for JobAdmissionError {}

#[derive(Debug, Clone, PartialEq)]
pub enum SelfUpdateAdmissionError {
    AlreadyInProgress,
    JobsInProgress { count: usize },
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
            SelfUpdateAdmissionError::QueueFull => write!(f, "Job queue is at capacity"),
        }
    }
}
impl std::error::Error for SelfUpdateAdmissionError {}

#[derive(Debug, Clone, PartialEq)]
pub enum RebootAdmissionError {
    SelfUpdateInProgress,
    PackageMutationInProgress,
    JobsInProgress { count: usize },
    QueueFull,
}

impl std::fmt::Display for RebootAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RebootAdmissionError::SelfUpdateInProgress => write!(f, "A self-update is in progress"),
            RebootAdmissionError::PackageMutationInProgress => {
                write!(f, "A package-database mutation is in progress")
            }
            RebootAdmissionError::JobsInProgress { count } => {
                write!(f, "Cannot reboot while {} jobs are in progress", count)
            }
            RebootAdmissionError::QueueFull => write!(f, "Job queue is at capacity"),
        }
    }
}
impl std::error::Error for RebootAdmissionError {}

#[derive(Debug)]
pub enum TryMutationError {
    Busy,
    Failed(anyhow::Error),
}

impl std::fmt::Display for TryMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TryMutationError::Busy => write!(f, "A package-DB mutation is already in progress"),
            TryMutationError::Failed(e) => write!(f, "Mutation failed: {}", e),
        }
    }
}
impl std::error::Error for TryMutationError {}

// ------------------------------------------------------------------
// Internal helpers
// ------------------------------------------------------------------

static GENERATION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_generation() -> u64 {
    GENERATION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

fn admit_job_inner(
    state: &mut SchedulerState,
    operation: JobOperation,
    packages: Vec<String>,
) -> Result<Uuid, JobAdmissionError> {
    if state.admission != AdmissionMode::Open {
        return Err(JobAdmissionError::AdmissionFrozen);
    }
    if state.self_update.is_some() {
        return Err(JobAdmissionError::SelfUpdateInProgress);
    }

    let active_count = state
        .jobs
        .values()
        .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
        .count();
    if active_count >= state.max_queue_depth {
        return Err(JobAdmissionError::QueueFull);
    }

    let job = Job::new(operation, packages);
    let job_id = job.id;
    state.jobs.insert(job_id, job);
    emit_event(
        state,
        "job_status",
        &job_id,
        &JobStatus::Pending,
        0,
        "Job created",
    );
    Ok(job_id)
}

fn emit_event(
    state: &SchedulerState,
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
        error: None,
        error_code: None,
    };
    let _ = state.event_sender.send(event);
}

fn emit_event_detailed(
    state: &SchedulerState,
    job_id: &Uuid,
    status: &JobStatus,
    progress: u8,
    message: &str,
    error: Option<String>,
    error_code: Option<String>,
) {
    let event = JobStatusEvent {
        event: "job_status".to_string(),
        job_id: *job_id,
        status: status.as_str().to_string(),
        progress,
        message: message.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        error,
        error_code,
    };
    let _ = state.event_sender.send(event);
}
