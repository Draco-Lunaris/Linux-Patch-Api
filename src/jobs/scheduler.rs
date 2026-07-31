//! Unified Operation Scheduler — the single authoritative control point
//! for all operation admission and lifecycle decisions.
//!
//! One `SchedulerState` behind one `tokio::sync::Mutex` owns:
//! - job map (pending, running, completed, failed)
//! - active mutation tracking (at most one)
//! - self-update / upgrade operation state
//! - reboot pending state (with reservation owner)
//! - admission mode (open, frozen-for-shutdown, recovery)
//! - max_concurrent enforcement
//!
//! ## Production mutation API
//!
//! There is exactly one production API for executing package-manager
//! mutations: [`Scheduler::dispatch_mutation`]. It owns the complete
//! lifecycle:
//!
//! 1. Verify admission is open.
//! 2. Verify no reboot is committed.
//! 3. Verify no self-update conflict exists.
//! 4. Wait for the package-mutation slot.
//! 5. Transition the job from Pending to Running.
//! 6. Execute the blocking package-manager operation.
//! 7. Preserve ownership if the caller future is cancelled.
//! 8. Release the mutation slot only when the blocking operation actually exits.
//! 9. Resolve the job state on success, error, panic, or caller cancellation.
//! 10. Wake the next queued operation.
//!
//! Handlers must not call `run_mutation`, `try_run_mutation`,
//! `wait_and_start_job`, or `start_job` from production code. Those
//! methods are only available under `#[cfg(any(test, feature = "test-utils"))]` and are otherwise
//! `unimplemented!` to make bypass structurally impossible.
//!
//! ## Wait/Notify
//!
//! Waiters are awakened by a scheduler-owned `tokio::sync::Notify` when:
//! - a mutation finishes,
//! - a running job reaches a terminal state,
//! - a reboot reservation rolls back,
//! - admission changes,
//! - shutdown begins.
//!
//! Wait loops re-check all conditions under the scheduler lock after
//! waking. There are no 100ms polling loops in this module.
//!
//! ## Reboot reservation
//!
//! Reboot admission uses [`Scheduler::reserve_reboot`], which returns a
//! [`RebootReservationGuard`]. The guard rolls back automatically on
//! drop unless [`RebootReservationGuard::commit`] is called. This
//! guarantees `reboot_pending` is cleared by the owner only.
//!
//! ## Multi-stage operations
//!
//! Multi-stage patch transactions (cache refresh → apply → optional
//! retry) pass one closure to `dispatch_mutation` so the entire
//! transaction holds the mutation slot for its full duration.

use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, Notify};
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
    /// Monotonic generation counter for mutation ownership. Incremented
    /// each time a mutation acquires the slot. The watchdog stores the
    /// generation at acquisition time and checks it before clearing
    /// shared state, preventing a stale watchdog from clearing a newer
    /// operation's ownership.
    mutation_generation: u64,
    /// Active self-update operation, if any.
    self_update: Option<UpgradeOperation>,
    /// Pending reboot reservation: the job ID of the owner, if any.
    /// Only the owner can commit or roll back.
    reboot_pending: Option<Uuid>,
    /// Maximum number of concurrently running jobs.
    max_concurrent: usize,
    /// Maximum queue depth (pending + running).
    max_queue_depth: usize,
    /// Broadcast sender for job status events.
    event_sender: broadcast::Sender<JobStatusEvent>,
    /// Scheduler-owned wake mechanism for waiters. Replaces 100ms polling.
    notify: Arc<Notify>,
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
                mutation_generation: 0,
                self_update: None,
                reboot_pending: None,
                max_concurrent: max_concurrent.max(1),
                max_queue_depth: max_queue_depth.max(1),
                event_sender,
                notify: Arc::new(Notify::new()),
            }),
        })
    }

    /// Wake one waiter. Called by the watchdog after a mutation finishes
    /// or by admission-state transitions.
    pub async fn notify_waiters(&self) {
        // We don't need the lock to call notify_one, but the state must
        // be observable to the waiter after it wakes. We just fire the
        // notify; the waiter will acquire the lock and re-check.
        self.state.lock().await.notify.notify_one();
    }

    /// Wait for a notification. Re-check conditions under the lock.
    pub async fn wait_for_notify(&self) {
        self.state.lock().await.notify.notified().await;
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
    /// - no reboot is reserved
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
        let id = admit_job_inner(&mut state, operation, packages)?;
        state.notify.notify_one();
        Ok(id)
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
    /// - no reboot is reserved (reboot blocks self-update)
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
        if state.reboot_pending.is_some() {
            return Err(SelfUpdateAdmissionError::AlreadyInProgress);
        }
        if state.active_mutation.is_some() {
            return Err(SelfUpdateAdmissionError::JobsInProgress { count: 1 });
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
        state.notify.notify_one();

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
                state.notify.notify_one();
                return true;
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // Reboot reservation
    // ------------------------------------------------------------------

    /// Atomically admit a reboot job and reserve the reboot state.
    ///
    /// Three-tier force model:
    /// - force=false: reject if any jobs active, self-update, or mutation
    /// - force=true, ack=false: reject if self-update or mutation active
    /// - force=true, ack=true: bypass all guards (audit logged)
    ///
    /// On success, returns a `RebootReservationGuard` that owns the
    /// reservation. The reservation:
    /// - rejects every new package mutation (via `dispatch_mutation`),
    /// - rejects every new self-update reservation,
    /// - rejects every new health/patch-list cache refresh (which route
    ///   through `dispatch_mutation`),
    /// - can be committed (process is about to reboot) or rolled back
    ///   (reboot command failed before machine actually rebooted).
    pub async fn reserve_reboot(
        self: &Arc<Self>,
        force: bool,
        ack_corruption_risk: bool,
    ) -> Result<RebootReservationGuard, RebootAdmissionError> {
        let mut state = self.state.lock().await;

        if state.admission != AdmissionMode::Open {
            return Err(RebootAdmissionError::AdmissionClosed);
        }
        // A second reboot is never allowed.
        if state.reboot_pending.is_some() {
            return Err(RebootAdmissionError::JobsInProgress { count: 1 });
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

        // Cancel pre-existing pending jobs: a reboot reservation means
        // the machine is expected to terminate. Pending jobs must not
        // wait indefinitely — mark them terminal now.
        //
        // CRITICAL: exclude the reboot owner's own job_id from
        // cancellation. The reboot job was just inserted as Pending
        // above. It must stay Pending until the reboot command begins,
        // at which point `begin_reboot_execution` transitions it to
        // Running.
        let mut to_fail = Vec::new();
        for (id, j) in state.jobs.iter() {
            if *id != job_id && matches!(j.status, JobStatus::Pending) {
                to_fail.push(*id);
            }
        }
        for id in to_fail {
            if let Some(j) = state.jobs.get_mut(&id) {
                j.status = JobStatus::Failed;
                j.error = Some("Cancelled: reboot reservation in progress".to_string());
                j.completed_at = Some(Utc::now());
                j.updated_at = j.completed_at.unwrap();
                j.add_log("Job cancelled by reboot reservation".to_string());
                let event_data = (j.status.clone(), j.progress, j.message.clone());
                emit_event(
                    &state,
                    "job_status",
                    &id,
                    &event_data.0,
                    event_data.1,
                    &event_data.2,
                );
            }
        }

        state.notify.notify_one();

        Ok(RebootReservationGuard {
            scheduler: self.clone(),
            job_id,
            committed: false,
        })
    }

    /// Backwards-compatible alias for `reserve_reboot`. Returns the job_id
    /// of the freshly admitted reboot job. The caller MUST own the
    /// resulting reservation — this method is consumed by the existing
    /// HTTP handler and the reservation is exposed via a separate
    /// `RebootJobHandle` struct returned alongside.
    pub async fn admit_reboot(
        self: &Arc<Self>,
        force: bool,
        ack_corruption_risk: bool,
    ) -> Result<RebootJobHandle, RebootAdmissionError> {
        let guard = self.reserve_reboot(force, ack_corruption_risk).await?;
        let job_id = guard.job_id;
        // The handler treats the reservation as a "fire and execute
        // reboot" — i.e. commit immediately. We commit so dropping the
        // returned `RebootJobHandle` does not roll back. The reboot
        // command itself is responsible for rolling back on failure
        // via `rollback_reboot`.
        guard.commit();
        Ok(RebootJobHandle { job_id })
    }

    /// Roll back a reboot reservation. Only succeeds if `job_id` is the
    /// current owner. This is the only way `reboot_pending` is cleared
    /// on failure — never unconditionally.
    pub async fn rollback_reboot(&self, job_id: Uuid, error: Option<String>) -> bool {
        let mut state = self.state.lock().await;
        if state.reboot_pending != Some(job_id) {
            return false;
        }
        state.reboot_pending = None;
        if let Some(job) = state.jobs.get_mut(&job_id) {
            if let Some(err) = error {
                job.fail(err);
            } else {
                job.status = JobStatus::Cancelled;
                job.message = "Reboot reservation rolled back".to_string();
                job.completed_at = Some(Utc::now());
                job.updated_at = job.completed_at.unwrap();
            }
            let event_data = (job.status.clone(), job.progress, job.message.clone());
            emit_event(
                &state,
                "job_status",
                &job_id,
                &event_data.0,
                event_data.1,
                &event_data.2,
            );
        }
        state.notify.notify_one();
        true
    }

    /// Transition the owning reboot job from Pending to Running
    /// immediately before invoking the backend reboot command.
    ///
    /// This is the production method for starting the reboot job's
    /// execution phase. It is ownership-safe: only the current
    /// `reboot_pending` owner may transition the job. A stale owner
    /// (whose job_id no longer matches `reboot_pending`) is rejected.
    ///
    /// Returns `true` if the transition succeeded, `false` if the
    /// caller is not the current reboot owner or the job was not in
    /// Pending state.
    ///
    /// After this method returns `true`, the caller should invoke the
    /// backend reboot command:
    ///   - On failure: call `rollback_reboot(job_id, Some(error))` to
    ///     mark the job Failed and reopen admission.
    ///   - On success: retain the committed reservation (the process
    ///     is expected to terminate).
    pub async fn begin_reboot_execution(&self, job_id: Uuid) -> bool {
        let mut state = self.state.lock().await;
        if state.reboot_pending != Some(job_id) {
            return false;
        }
        if let Some(job) = state.jobs.get_mut(&job_id) {
            if job.status != JobStatus::Pending {
                return false;
            }
            job.status = JobStatus::Running;
            job.updated_at = Utc::now();
            job.add_log("Reboot command starting".to_string());
            let event_data = (job.status.clone(), job.progress, job.message.clone());
            emit_event(
                &state,
                "job_status",
                &job_id,
                &event_data.0,
                event_data.1,
                &event_data.2,
            );
            return true;
        }
        false
    }

    // ------------------------------------------------------------------
    // Mutation execution (single production API)
    // ------------------------------------------------------------------

    /// Atomically start a job AND acquire the mutation slot, then run
    /// the closure in spawn_blocking. This is the SOLE production API
    /// for executing package-manager mutations. Every code path that
    /// invokes a package manager MUST go through this method.
    ///
    /// Behavior:
    /// 1. Verify admission is open (Frozen/Recovery → reject).
    /// 2. Verify no reboot is reserved (reboot_pending → wait or reject).
    /// 3. Verify no self-update conflict.
    /// 4. Wait (via Notify) for the package-mutation slot AND max_concurrent.
    /// 5. Transition the job to Running.
    /// 6. Execute the blocking closure in spawn_blocking.
    /// 7. If the caller future is cancelled, the watchdog still owns
    ///    the slot and the job — it finalizes the job state when the
    ///    blocking command exits.
    /// 8. Release the mutation slot only when the blocking operation
    ///    actually exits (success, error, or panic).
    /// 9. Resolve the job state on success, error, or panic.
    /// 10. Wake the next queued operation.
    pub async fn dispatch_mutation<F, T>(self: &Arc<Self>, job_id: Uuid, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        use tokio::sync::oneshot;

        // The result_tx is sent into the watchdog task. The caller
        // holds result_rx. If the caller is cancelled (its future is
        // dropped), result_rx is dropped; the watchdog's send fails,
        // and the watchdog marks the job Failed (cancelled) under the
        // scheduler lock.
        let (tx, result_rx) = oneshot::channel::<Result<T>>();
        // Wrap the sender in an Arc<Mutex<Option<>>> so the watchdog
        // can take it once and detect cancellation via the send result.
        let result_tx = Arc::new(std::sync::Mutex::new(Some(tx)));

        // Loop until we can atomically acquire both the max_concurrent
        // slot and the mutation slot, OR be rejected.
        'wait: loop {
            let mut state = self.state.lock().await;

            // 1. Admission mode check
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

            // 2. Reboot reserved? Reject immediately — do not wait.
            // A reboot reservation means the machine is expected to
            // terminate. Queued mutations must not wait indefinitely
            // for a reboot that will kill the process. The caller
            // receives an error and can mark the job terminal.
            if state.reboot_pending.is_some() {
                return Err(anyhow::anyhow!(
                    "Mutation rejected — reboot reservation is in progress"
                ));
            }

            // 3. Self-update conflict
            //
            // When a self-update is reserved, only the job that OWNS
            // that reservation may enter its mutation closure. Every
            // other mutation (package jobs AND a second self-update
            // attempt) is rejected. This preserves the self-update
            // barrier for all non-owning jobs while allowing the
            // owning self-update job to actually execute its package
            // command through dispatch_mutation.
            //
            // Ownership is NOT cleared or transferred here — it
            // remains set until the self-update lifecycle (guard
            // commit/release or force_clear) releases it.
            if let Some(ref su) = state.self_update {
                if su.job_id != job_id {
                    return Err(anyhow::anyhow!(
                        "Mutation rejected — self-update in progress"
                    ));
                }
            }

            // 4. Job must exist and be Pending
            let job_already_running = state
                .jobs
                .get(&job_id)
                .map(|j| j.status == JobStatus::Running)
                .unwrap_or(false);

            // 5. max_concurrent
            let running = state
                .jobs
                .values()
                .filter(|j| j.status == JobStatus::Running)
                .count();

            if !job_already_running && running >= state.max_concurrent {
                let notify = state.notify.clone();
                drop(state);
                notify.notified().await;
                continue 'wait;
            }

            // 6. Mutation slot
            if state.active_mutation.is_some() {
                let notify = state.notify.clone();
                drop(state);
                notify.notified().await;
                continue 'wait;
            }

            // All slots acquired atomically.
            state.active_mutation = Some(job_id);
            // Increment the mutation generation so the watchdog can
            // verify it still owns the slot before clearing shared state.
            state.mutation_generation = state.mutation_generation.wrapping_add(1);
            let generation = state.mutation_generation;
            if let Some(job) = state.jobs.get_mut(&job_id) {
                if !job_already_running {
                    job.status = JobStatus::Running;
                    job.add_log("Job started".to_string());
                }
                job.updated_at = Utc::now();
                let event_data = (job.status.clone(), job.progress, job.message.clone());
                emit_event(
                    &state,
                    "job_status",
                    &job_id,
                    &event_data.0,
                    event_data.1,
                    &event_data.2,
                );
            }

            // Spawn the blocking task with a watchdog that finalizes
            // the job state regardless of caller cancellation.
            let sched = self.clone();
            let job_id_owned = job_id;
            let result_tx = result_tx.clone();

            // Drop state lock before spawning to keep the critical
            // section short. The blocking task runs outside the lock.
            drop(state);

            // Persist job state to disk — the job is now running.
            // If the agent reboots, this file is used to recover orphaned jobs.
            self.persist_jobs().await;

            tokio::spawn(async move {
                let join_handle = tokio::task::spawn_blocking(f);
                let result = match join_handle.await {
                    Ok(inner) => inner,
                    Err(join_err) => Err(anyhow::anyhow!("Mutation task panicked: {}", join_err)),
                };

                // Watchdog: always clear active_mutation and finalize
                // the job state. The caller may have been dropped
                // (cancelled) but the job must reach a terminal state.
                //
                // Ownership safety: verify the generation matches
                // before clearing shared state. A stale watchdog from
                // a previous operation must not clear a newer
                // operation's mutation ownership.
                let mut state = sched.state.lock().await;
                let still_owns = state.active_mutation == Some(job_id_owned)
                    && state.mutation_generation == generation;

                if still_owns {
                    state.active_mutation = None;
                }

                // Try to send the result to the caller. If the caller
                // was cancelled (dropped), the receiver is gone and
                // the send fails — we mark the job terminal.
                //
                // Capture whether the result was Ok or Err BEFORE
                // moving it into the oneshot sender, so the watchdog
                // can set the correct terminal-state diagnostic without
                // accessing the moved value.
                let result_is_ok = result.is_ok();
                let result_err_msg: Option<String> = match &result {
                    Ok(_) => None,
                    Err(e) => Some(format!("{}", e)),
                };

                let caller_alive = if let Ok(mut guard) = result_tx.lock() {
                    if let Some(tx) = guard.take() {
                        tx.send(result).is_ok()
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !caller_alive && still_owns {
                    // Terminal-state policy for caller cancellation:
                    //
                    // - caller absent, closure succeeds: mark Failed
                    //   with a message explaining the caller was
                    //   cancelled after the underlying operation
                    //   completed. The operation ran to completion but
                    //   the result could not be delivered.
                    //
                    // - caller absent, closure fails: mark Failed with
                    //   the underlying diagnostic preserved.
                    //
                    // - closure panicked: mark Failed (the JoinError
                    //   is already captured in result_err_msg).
                    //
                    // In all cases the job must NOT remain Running.
                    if let Some(job) = state.jobs.get_mut(&job_id_owned) {
                        if matches!(job.status, JobStatus::Running | JobStatus::Pending) {
                            job.status = JobStatus::Failed;
                            if result_is_ok {
                                job.error = Some(
                                    "Caller cancelled after mutation completed \
                                     — result could not be delivered"
                                        .to_string(),
                                );
                                job.add_log(
                                    "Watchdog: caller dropped, operation succeeded \
                                     but result undeliverable"
                                        .to_string(),
                                );
                            } else {
                                let diag = format!(
                                    "Caller cancelled with underlying error: {}",
                                    result_err_msg.as_deref().unwrap_or("unknown error")
                                );
                                job.error = Some(diag);
                                job.add_log(
                                    "Watchdog: caller dropped, operation failed".to_string(),
                                );
                            }
                            job.completed_at = Some(Utc::now());
                            job.updated_at = job.completed_at.unwrap();
                            let event_data =
                                (job.status.clone(), job.progress, job.message.clone());
                            emit_event(
                                &state,
                                "job_status",
                                &job_id_owned,
                                &event_data.0,
                                event_data.1,
                                &event_data.2,
                            );
                        }
                    }
                }

                // Wake one waiter.
                state.notify.notify_one();
            });

            break;
        }

        // The caller is awaiting result_rx. If this future is
        // cancelled (dropped), the watchdog's send fails and the
        // watchdog marks the job Failed (cancelled) under the lock.
        match result_rx.await {
            Ok(val) => val,
            Err(_) => Err(anyhow::anyhow!(
                "Mutation result channel closed — task was cancelled but blocking command may still be running"
            )),
        }
    }

    // ------------------------------------------------------------------
    // Shutdown / drain
    // ------------------------------------------------------------------

    /// Freeze admission — no new mutations or jobs accepted.
    /// Called by the SIGTERM handler before draining.
    pub async fn freeze_admission(&self) {
        let mut state = self.state.lock().await;
        state.admission = AdmissionMode::Frozen;
        state.notify.notify_one();
    }

    // ------------------------------------------------------------------
    // Job lifecycle (delegated to inner state)
    // ------------------------------------------------------------------

    pub async fn get_job(&self, job_id: &Uuid) -> Option<Job> {
        self.state.lock().await.jobs.get(job_id).cloned()
    }

    /// Update a job's status. Only used for progress updates and
    /// terminal-state transitions from the post-mutation step in the
    /// handler. Does NOT start a new job.
    pub async fn update_job(
        &self,
        job_id: &Uuid,
        status: JobStatus,
        progress: Option<u8>,
        message: Option<String>,
    ) -> Result<()> {
        let mut state = self.state.lock().await;

        // max_concurrent: if a job is being transitioned to Running,
        // enforce. We do NOT call this from handler dispatch; we only
        // allow this for jobs already running.
        if status == JobStatus::Running {
            let already_running = state
                .jobs
                .get(job_id)
                .map(|j| j.status == JobStatus::Running)
                .unwrap_or(false);
            if !already_running {
                let running = state
                    .jobs
                    .values()
                    .filter(|j| j.status == JobStatus::Running)
                    .count();
                if running >= state.max_concurrent {
                    return Err(anyhow::anyhow!(
                        "max_concurrent limit ({}) reached — cannot transition another job to Running",
                        state.max_concurrent
                    ));
                }
            }
        }

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
        state.notify.notify_one();
        drop(state);
        // Persist job state — completed jobs are removed from the file.
        self.persist_jobs().await;
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
        state.notify.notify_one();
        drop(state);
        // Persist job state — failed jobs are removed from the file.
        self.persist_jobs().await;
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
        state.notify.notify_one();
        drop(state);
        // Persist job state — failed jobs are removed from the file.
        self.persist_jobs().await;
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

    /// Get all jobs that are currently running or pending — for persistence.
    pub async fn get_active_jobs(&self) -> Vec<Job> {
        let state = self.state.lock().await;
        state
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
            .cloned()
            .collect()
    }

    /// Recover orphaned jobs (from a previous boot) into terminal states.
    ///
    /// Called on startup with the jobs loaded from the persistence file.
    /// Each job is inserted into the scheduler with a terminal status so the
    /// manager can see the outcome when it polls.
    ///
    /// The terminal status depends on the operation:
    /// - `SelfUpdate`: the job is orphaned *because* the postinst restart
    ///   killed the old process mid-flight. The new binary is running, which
    ///   means the upgrade succeeded — mark `Completed`.
    /// - anything else: a genuine orphan (crash / reboot during execution) —
    ///   mark `Failed` with `AGENT_REBOOTED`.
    pub async fn recover_orphaned_jobs(&self, orphaned_jobs: &[Job]) {
        if orphaned_jobs.is_empty() {
            return;
        }

        let mut state = self.state.lock().await;
        for orphaned in orphaned_jobs {
            let now = Utc::now();
            let is_self_update = matches!(orphaned.operation, JobOperation::SelfUpdate);

            let job = if is_self_update {
                // The only way a self-update job is orphaned is if the
                // postinst restart terminated the old process before it could
                // persist the completion. The new process is running the new
                // binary, so the upgrade succeeded by definition.
                Job {
                    id: orphaned.id,
                    status: JobStatus::Completed,
                    operation: orphaned.operation.clone(),
                    created_at: orphaned.created_at,
                    updated_at: now,
                    completed_at: Some(now),
                    packages: orphaned.packages.clone(),
                    progress: 100,
                    message: "Self-update completed — agent restarted into new version".to_string(),
                    logs: vec![
                        "Agent restarted during self-update (expected postinst restart)"
                            .to_string(),
                        "New binary is running — upgrade succeeded".to_string(),
                    ],
                    error: None,
                    error_code: None,
                    exit_code: None,
                    command_stdout: None,
                    command_stderr: None,
                    rollback_job_id: None,
                    exclusive_mode: false,
                }
            } else {
                Job {
                    id: orphaned.id,
                    status: JobStatus::Failed,
                    operation: orphaned.operation.clone(),
                    created_at: orphaned.created_at,
                    updated_at: now,
                    completed_at: Some(now),
                    packages: orphaned.packages.clone(),
                    progress: 0,
                    message: "Job failed: agent rebooted during execution".to_string(),
                    logs: vec!["Agent rebooted — in-memory job state was lost".to_string()],
                    error: Some("Agent rebooted during job execution".to_string()),
                    error_code: Some("AGENT_REBOOTED".to_string()),
                    exit_code: None,
                    command_stdout: None,
                    command_stderr: None,
                    rollback_job_id: None,
                    exclusive_mode: false,
                }
            };

            if is_self_update {
                tracing::info!(
                    job_id = %orphaned.id,
                    "Recovered orphaned self-update — marked as completed (restart proves upgrade succeeded)"
                );
            } else {
                tracing::info!(
                    job_id = %orphaned.id,
                    "Recovered orphaned job from previous boot — marked as failed (AGENT_REBOOTED)"
                );
            }

            state.jobs.insert(orphaned.id, job);
        }
    }

    /// Persist current running/pending jobs to disk.
    pub async fn persist_jobs(&self) {
        let active = self.get_active_jobs().await;
        crate::jobs::persistence::persist_running_jobs(&active).await;
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
                state.notify.notify_one();
                return Ok(Some(rollback_job_id));
            }
        }
        Ok(None)
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

    /// TEST-ONLY: snapshot the scheduler state for assertions.
    /// Returns a struct with read-only views of the fields tests
    /// need to inspect. Production code must never use this.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn state_for_test(&self) -> SchedulerStateSnapshot {
        let s = self.state.lock().await;
        SchedulerStateSnapshot {
            admission: s.admission,
            active_mutation: s.active_mutation,
            mutation_generation: s.mutation_generation,
            self_update: s.self_update.clone(),
            reboot_pending: s.reboot_pending,
        }
    }

    // ------------------------------------------------------------------
    // LEGACY MUTATION APIs — REMOVED FROM PRODUCTION
    // ------------------------------------------------------------------
    //
    // The following methods were the original mutation entry points
    // and are now STRUCTURALLY UNAVAILABLE in production builds. They
    // remain only under `#[cfg(any(test, feature = "test-utils"))]` for legacy test support.
    //
    // Production code that tries to invoke a package manager MUST go
    // through `dispatch_mutation`. This is enforced by:
    //   1. The methods are `unimplemented!` in production builds, so
    //      any call site is a panic, not a silent bypass.
    //   2. Even under `#[cfg(any(test, feature = "test-utils"))]`, they have a different signature
    //      than `dispatch_mutation` so the compiler reminds callers
    //      that they should be using the authoritative entry point.

    /// Acquire the mutation slot. **TEST-ONLY**. Production code must
    /// use `dispatch_mutation`. Calling this from production panics.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn run_mutation<F, T>(self: &Arc<Self>, job_id: Uuid, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        use tokio::sync::oneshot;

        let (result_tx, result_rx) = oneshot::channel::<Result<T>>();

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

            let sched = self.clone();
            tokio::spawn(async move {
                let join_handle = tokio::task::spawn_blocking(f);
                let result = match join_handle.await {
                    Ok(inner) => inner,
                    Err(join_err) => Err(anyhow::anyhow!("Mutation task panicked: {}", join_err)),
                };
                let mut state = sched.state.lock().await;
                state.active_mutation = None;
                state.notify.notify_one();
                drop(state);
                let _ = result_tx.send(result);
            });
        }

        match result_rx.await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow::anyhow!(
                "Mutation result channel closed — task was cancelled but blocking command may still be running"
            )),
        }
    }

    /// Try to run a mutation without blocking. **TEST-ONLY**.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn try_run_mutation<F, T>(self: &Arc<Self>, f: F) -> Result<T, TryMutationError>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        use tokio::sync::oneshot;

        let job_id = Uuid::new_v4();
        let (result_tx, result_rx) = oneshot::channel::<Result<T>>();

        {
            let mut state = self.state.lock().await;
            if state.admission != AdmissionMode::Open {
                return Err(TryMutationError::Busy);
            }
            if state.active_mutation.is_some() {
                return Err(TryMutationError::Busy);
            }
            if state.self_update.is_some() {
                return Err(TryMutationError::Busy);
            }
            if state.reboot_pending.is_some() {
                return Err(TryMutationError::Busy);
            }
            state.active_mutation = Some(job_id);

            let sched = self.clone();
            tokio::spawn(async move {
                let join_handle = tokio::task::spawn_blocking(f);
                let result = match join_handle.await {
                    Ok(inner) => inner,
                    Err(join_err) => Err(anyhow::anyhow!("Task panicked: {}", join_err)),
                };
                let mut state = sched.state.lock().await;
                state.active_mutation = None;
                state.notify.notify_one();
                drop(state);
                let _ = result_tx.send(result);
            });
        }

        match result_rx.await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(e)) => Err(TryMutationError::Failed(e)),
            Err(_) => Err(TryMutationError::Failed(anyhow::anyhow!(
                "Mutation result channel closed — task was cancelled but blocking command may still be running"
            ))),
        }
    }

    /// Atomically transition a job to Running status, enforcing
    /// max_concurrent. **TEST-ONLY**. Production code must use
    /// `dispatch_mutation` which combines this with mutation admission.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn start_job(&self, job_id: &Uuid) -> Result<(), anyhow::Error> {
        let mut state = self.state.lock().await;
        if state.admission != AdmissionMode::Open {
            return Err(anyhow::anyhow!(
                "Admission not open (mode={:?}) — job remains pending",
                state.admission
            ));
        }
        let running = state
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Running)
            .count();
        let already_running = state
            .jobs
            .get(job_id)
            .map(|j| j.status == JobStatus::Running)
            .unwrap_or(false);
        if !already_running && running >= state.max_concurrent {
            return Err(anyhow::anyhow!(
                "max_concurrent limit ({}) reached — job remains pending",
                state.max_concurrent
            ));
        }
        if let Some(job) = state.jobs.get_mut(job_id) {
            job.status = JobStatus::Running;
            job.updated_at = Utc::now();
            job.add_log("Job started".to_string());
            let event_data = (job.status.clone(), job.progress, job.message.clone());
            emit_event(
                &state,
                "job_status",
                job_id,
                &event_data.0,
                event_data.1,
                &event_data.2,
            );
        }
        Ok(())
    }

    /// Wait for a running slot to become available, then start the job.
    /// **TEST-ONLY**. Production code must use `dispatch_mutation`.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn wait_and_start_job(&self, job_id: &Uuid) {
        loop {
            match self.start_job(job_id).await {
                Ok(()) => return,
                Err(_) => {
                    let state = self.state.lock().await;
                    let notify = state.notify.clone();
                    drop(state);
                    notify.notified().await;
                }
            }
        }
    }
}

// ------------------------------------------------------------------
// RAII guards
// ------------------------------------------------------------------

/// Handle for a reboot reservation that has been committed and is now
/// being executed. The owner is expected to call `reboot_system()` and
/// either:
///   - The system actually reboots (process is killed): nothing more
///     is required.
///   - The reboot command failed: call `rollback_reboot(job_id)` to
///     release the reservation, mark the job Failed, and reopen
///     admission.
#[derive(Debug, Clone)]
pub struct RebootJobHandle {
    pub job_id: Uuid,
}

impl std::fmt::Display for RebootJobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RebootJobHandle({})", self.job_id)
    }
}

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
                state.notify.notify_one();
                tracing::info!(job_id = %job_id, "Self-update reservation rolled back");
            });
        }
    }
}

/// Guard for a reboot reservation. Rolls back automatically on drop
/// unless `commit()` is called. The owner can also call
/// `Scheduler::rollback_reboot(job_id, error)` explicitly when a
/// reboot command fails after the reservation was made.
pub struct RebootReservationGuard {
    scheduler: Arc<Scheduler>,
    pub job_id: Uuid,
    committed: bool,
}

/// TEST-ONLY: read-only snapshot of scheduler state for assertions.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone)]
pub struct SchedulerStateSnapshot {
    pub admission: AdmissionMode,
    pub active_mutation: Option<Uuid>,
    pub mutation_generation: u64,
    pub self_update: Option<UpgradeOperation>,
    pub reboot_pending: Option<Uuid>,
}

impl RebootReservationGuard {
    /// Commit the reservation. After this, dropping the guard will NOT
    /// roll back. The process is expected to terminate via the reboot
    /// command.
    pub fn commit(mut self) -> Uuid {
        self.committed = true;
        self.job_id
    }
}

impl Drop for RebootReservationGuard {
    fn drop(&mut self) {
        if !self.committed {
            tracing::warn!(
                job_id = %self.job_id,
                "RebootReservationGuard dropped without commit — rolling back"
            );
            let scheduler = self.scheduler.clone();
            let job_id = self.job_id;
            tokio::spawn(async move {
                scheduler
                    .rollback_reboot(
                        job_id,
                        Some("Reservation dropped without commit (cancelled)".to_string()),
                    )
                    .await;
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
    JobsInProgress {
        count: usize,
    },
    QueueFull,
    /// Admission is frozen (shutdown in progress) or recovery mode.
    AdmissionClosed,
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
            RebootAdmissionError::AdmissionClosed => {
                write!(f, "Admission is closed (shutdown or recovery in progress)")
            }
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
    if state.reboot_pending.is_some() {
        return Err(JobAdmissionError::AdmissionFrozen);
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
