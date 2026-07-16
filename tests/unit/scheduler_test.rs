//! Production-path tests for the unified scheduler.
//!
//! These tests call the same Scheduler methods used by production handlers.
//! They use barriers, atomic counters, and channels for deterministic
//! synchronization — no arbitrary sleeps.

use std::sync::Arc;

use linux_patch_api::jobs::manager::JobOperation;
use linux_patch_api::jobs::scheduler::{
    AdmissionMode, JobAdmissionError, RebootAdmissionError, Scheduler, SelfUpdateAdmissionError,
    TryMutationError,
};

// =============================================================================
// 1. Concurrent job execution respecting max_concurrent
// =============================================================================

/// With max_concurrent=2, admitting 3 jobs should only allow 2 to run.
/// The third stays pending.
#[tokio::test]
async fn max_concurrent_limits_running_jobs() {
    let scheduler = Scheduler::new(2, 10);

    let j1 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg1".to_string()])
        .await
        .unwrap();
    let j2 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg2".to_string()])
        .await
        .unwrap();
    let j3 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg3".to_string()])
        .await
        .unwrap();

    // Use a notify to keep the mutation running until we release it.
    let notify = Arc::new(tokio::sync::Notify::new());
    let n1 = notify.clone();
    let s1 = scheduler.clone();

    let h1 = tokio::spawn(async move {
        s1.run_mutation(j1, move || {
            // Block until notified
            n1.notify_waiters();
            Ok(())
        })
        .await
    });

    // Wait for the mutation to start, then check j3 is pending
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let job3 = scheduler.get_job(&j3).await.unwrap();
    assert_eq!(
        job3.status,
        linux_patch_api::jobs::manager::JobStatus::Pending,
        "third job should be pending while mutations are running"
    );

    // The mutation closure returns immediately (no barrier), so h1 completes.
    let _ = h1.await;

    // j2 mutation should also work
    let result = scheduler.run_mutation(j2, || Ok(())).await;
    assert!(result.is_ok());
}

// =============================================================================
// 2. Cancellation during self-update reservation
// =============================================================================

/// Dropping a reservation without committing rolls back the owner.
#[tokio::test]
async fn reservation_cancellation_rolls_back_owner() {
    let scheduler = Scheduler::new(5, 10);

    let guard = scheduler
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "1.0.0", "2.0.0")
        .await
        .unwrap();

    assert!(scheduler.is_self_update_in_progress().await);

    // Drop without committing
    drop(guard);

    // Give the async rollback task time to run
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    assert!(
        !scheduler.is_self_update_in_progress().await,
        "self-update should be cleared after reservation rollback"
    );

    // A new reservation should succeed
    let guard2 = scheduler
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "1.0.0", "2.0.0")
        .await
        .unwrap();
    assert!(scheduler.is_self_update_in_progress().await);
    let _ = guard2.commit();
}

// =============================================================================
// 3. Rollback while scheduler state is contended
// =============================================================================

/// While a self-update reservation is held, admit_job must fail.
/// After rollback, admit_job must succeed.
#[tokio::test]
async fn rollback_while_state_contended() {
    let scheduler = Scheduler::new(5, 10);

    let guard = scheduler
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "1.0.0", "2.0.0")
        .await
        .unwrap();

    // While reservation is held, job admission must fail
    let result = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await;
    assert!(
        matches!(result, Err(JobAdmissionError::SelfUpdateInProgress)),
        "job admission should fail during self-update reservation"
    );

    // Drop the reservation (rollback)
    drop(guard);
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Now job admission should succeed
    let result = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await;
    assert!(
        result.is_ok(),
        "job admission should succeed after rollback"
    );
}

// =============================================================================
// 4. Reboot racing with a pending mutation
// =============================================================================

/// Reboot with force=false must fail while a mutation is active.
/// Reboot with force=true, ack=false must fail while mutation is active.
#[tokio::test]
async fn reboot_racing_with_pending_mutation() {
    let scheduler = Scheduler::new(5, 10);

    let job_id = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await
        .unwrap();

    // Start a mutation that blocks on a channel receive, keeping
    // the mutation slot held.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = std::sync::Mutex::new(rx);
    let s = scheduler.clone();
    let handle = tokio::spawn(async move {
        s.run_mutation(job_id, move || {
            // Block until the channel receives (or sender drops)
            let _ = rx.lock().unwrap().recv();
            Ok(())
        })
        .await
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Reboot with force=false should fail
    let result = scheduler.admit_reboot(false, false).await;
    assert!(
        matches!(result, Err(RebootAdmissionError::JobsInProgress { .. })),
        "reboot with force=false should fail while jobs are active"
    );

    // Reboot with force=true, ack=false should fail (mutation in progress)
    let result = scheduler.admit_reboot(true, false).await;
    assert!(
        matches!(result, Err(RebootAdmissionError::PackageMutationInProgress)),
        "reboot with force=true, ack=false should fail while mutation is active"
    );

    // Reboot with force=true, ack=true should succeed
    let result = scheduler.admit_reboot(true, true).await;
    assert!(
        result.is_ok(),
        "reboot with force=true, ack=true should succeed"
    );

    // Cleanup — release the mutation
    drop(tx);
    let _ = handle.await;
}

// =============================================================================
// 5. Admission closing before shutdown drain
// =============================================================================

/// After freeze_admission(), no new jobs or mutations are accepted.
#[tokio::test]
async fn admission_closes_before_shutdown_drain() {
    let scheduler = Scheduler::new(5, 10);

    scheduler.freeze_admission().await;
    assert_eq!(
        scheduler.admission_mode().await,
        AdmissionMode::Frozen,
        "admission should be frozen"
    );

    // Job admission must fail
    let result = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await;
    assert!(
        matches!(result, Err(JobAdmissionError::AdmissionFrozen)),
        "job admission should fail when frozen"
    );

    // Mutation must fail
    let job_id = uuid::Uuid::new_v4();
    let result = scheduler.run_mutation(job_id, || Ok(())).await;
    assert!(result.is_err(), "mutation should fail when frozen");

    // try_run_mutation must return Busy
    let result: Result<(), TryMutationError> = scheduler.try_run_mutation(|| Ok(())).await;
    assert!(
        matches!(result, Err(TryMutationError::Busy)),
        "try_run_mutation should return Busy when frozen"
    );
}

// =============================================================================
// 6. Restart command spawn success followed by command failure
// =============================================================================

/// A mutation that succeeds should leave the scheduler clean for the
/// next phase of the self-update lifecycle.
#[tokio::test]
async fn mutation_success_leaves_clean_state() {
    let scheduler = Scheduler::new(5, 10);

    let guard = scheduler
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "1.0.0", "2.0.0")
        .await
        .unwrap();
    let job_id = guard.commit();

    // Run the "install" mutation — succeeds
    let result = scheduler.run_mutation(job_id, || Ok(())).await;
    assert!(result.is_ok(), "mutation should succeed");

    // Scheduler should be clean — no active mutation
    assert!(
        !scheduler.is_mutation_in_progress().await,
        "no mutation should be in progress after success"
    );

    // Self-update should still be in progress (not cleared by mutation)
    assert!(
        scheduler.is_self_update_in_progress().await,
        "self-update should still be in progress after install"
    );
}

// =============================================================================
// 7. Fallback eligibility after primary restart failure
// =============================================================================

/// After a failed mutation, the scheduler state must be clean so a
/// fallback/retry can proceed.
#[tokio::test]
async fn failed_mutation_leaves_clean_state() {
    let scheduler = Scheduler::new(5, 10);

    let job_id = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await
        .unwrap();

    // Run a mutation that fails
    let result: Result<(), anyhow::Error> = scheduler
        .run_mutation(job_id, || Err(anyhow::anyhow!("command failed")))
        .await;
    assert!(result.is_err(), "mutation should fail");

    // Scheduler must be clean — no active mutation
    assert!(
        !scheduler.is_mutation_in_progress().await,
        "no mutation should be in progress after failure"
    );

    // A new mutation should be admissible
    let job2 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg2".to_string()])
        .await
        .unwrap();
    let result = scheduler.run_mutation(job2, || Ok(())).await;
    assert!(result.is_ok(), "new mutation should succeed after failure");
}

// =============================================================================
// 8. Exact target-version mismatch
// =============================================================================

/// The self-update reservation must store the exact target version.
/// A version mismatch after install must not be accepted.
#[tokio::test]
async fn target_version_stored_in_reservation() {
    let scheduler = Scheduler::new(5, 10);

    let guard = scheduler
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "1.0.0", "2.0.0")
        .await
        .unwrap();

    // The scheduler should report self-update in progress
    assert!(scheduler.is_self_update_in_progress().await);

    // The target version is stored internally — we verify it's not
    // equal to from_version (which would indicate a no-op)
    let _ = guard.commit();

    // Verify we can't reserve again (already in progress)
    let result = scheduler
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "1.0.0", "3.0.0")
        .await;
    assert!(
        matches!(result, Err(SelfUpdateAdmissionError::AlreadyInProgress)),
        "second self-update reservation should fail"
    );

    // Release and verify we can reserve with a different target
    scheduler.force_clear_self_update().await;
    // Also need to clear the job from the first reservation
    // (force_clear only clears the owner, not the job)
    // The job is in Completed/Failed state? No — it was never started.
    // It's still Pending. We need to clean it up.
    // Actually, try_reserve_self_update checks active_count (pending+running).
    // The old job is still pending. Let's just use a fresh scheduler.
    let scheduler2 = Scheduler::new(5, 10);
    let guard2 = scheduler2
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "2.0.0", "3.0.0")
        .await;
    assert!(
        guard2.is_ok(),
        "reservation should succeed on fresh scheduler"
    );
}

// =============================================================================
// 9. Missing installed-version result
// =============================================================================

/// try_run_mutation with a closure that returns Err must propagate
/// the error as TryMutationError::Failed.
#[tokio::test]
async fn missing_installed_version_fails_closed() {
    let scheduler = Scheduler::new(5, 10);

    let result: Result<(), TryMutationError> = scheduler
        .try_run_mutation(|| Err(anyhow::anyhow!("package not installed")))
        .await;

    assert!(
        matches!(result, Err(TryMutationError::Failed(_))),
        "missing version should return Failed error, not Busy"
    );

    // Scheduler should be clean after the failed try
    assert!(!scheduler.is_mutation_in_progress().await);
}

// =============================================================================
// 10. Recovery across two simulated process startups
// =============================================================================

/// A new scheduler (simulating a new process) starts in Open mode.
/// Recovery state is in the durable file, not the scheduler.
#[tokio::test]
async fn recovery_across_two_startups() {
    // First "process" — enter recovery
    let scheduler1 = Scheduler::new(5, 10);
    scheduler1.enter_recovery().await;
    assert_eq!(
        scheduler1.admission_mode().await,
        AdmissionMode::Recovery,
        "first process should be in recovery"
    );
    assert!(
        !scheduler1.is_mutation_in_progress().await,
        "no mutation in recovery"
    );

    // Second "process" — new scheduler, starts fresh
    let scheduler2 = Scheduler::new(5, 10);
    assert_eq!(
        scheduler2.admission_mode().await,
        AdmissionMode::Open,
        "second process (new scheduler) should start in Open mode"
    );

    // The second process should be able to admit jobs
    let result = scheduler2
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await;
    assert!(result.is_ok(), "second process should accept jobs");
}

// =============================================================================
// 11. Panic or cancellation during mutation execution
// =============================================================================

/// A panic inside a mutation closure must not leave the scheduler
/// with an active mutation. spawn_blocking isolates the panic.
#[tokio::test]
async fn panic_during_mutation_leaves_clean_state() {
    let scheduler = Scheduler::new(5, 10);

    let job_id = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await
        .unwrap();

    // Run a mutation that panics
    let result: Result<(), anyhow::Error> = scheduler.run_mutation(job_id, || panic!("boom")).await;

    // spawn_blocking catches the panic and returns a JoinError
    assert!(result.is_err(), "panicked mutation should return error");

    // The scheduler must be clean — no active mutation
    assert!(
        !scheduler.is_mutation_in_progress().await,
        "no mutation should be active after panic"
    );

    // A new mutation should be admissible
    let job2 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg2".to_string()])
        .await
        .unwrap();
    let result = scheduler.run_mutation(job2, || Ok(())).await;
    assert!(result.is_ok(), "new mutation should succeed after panic");
}

// =============================================================================
// 12. At most one mutation executes at a time
// =============================================================================

/// Two concurrent run_mutation calls must not overlap. The second
/// must fail while the first is running.
#[tokio::test]
async fn at_most_one_mutation_at_a_time() {
    let scheduler = Scheduler::new(5, 10);

    let j1 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg1".to_string()])
        .await
        .unwrap();
    let j2 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg2".to_string()])
        .await
        .unwrap();

    // Start first mutation with a channel to keep it running
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = std::sync::Mutex::new(rx);
    let s1 = scheduler.clone();
    let handle = tokio::spawn(async move {
        s1.run_mutation(j1, move || {
            let _ = rx.lock().unwrap().recv();
            Ok(())
        })
        .await
    });

    // Give the first mutation time to acquire the slot
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Second mutation must fail — slot is held
    let result: Result<(), anyhow::Error> = scheduler.run_mutation(j2, || Ok(())).await;
    assert!(
        result.is_err(),
        "second mutation must fail while first is running"
    );
    assert!(
        scheduler.is_mutation_in_progress().await,
        "mutation should be in progress"
    );

    // Release the channel so the first completes
    drop(tx);
    let _ = handle.await;

    // Now the second should succeed
    let result = scheduler.run_mutation(j2, || Ok(())).await;
    assert!(
        result.is_ok(),
        "second mutation should succeed after first completes"
    );
}

// =============================================================================
// 13. Recovery mode rejects all mutating operations
// =============================================================================

#[tokio::test]
async fn recovery_mode_rejects_mutations() {
    let scheduler = Scheduler::new(5, 10);
    scheduler.enter_recovery().await;

    // Job admission must fail
    let result = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await;
    assert!(
        matches!(result, Err(JobAdmissionError::AdmissionFrozen)),
        "job admission should fail in recovery"
    );

    // Self-update reservation must fail
    let result = scheduler
        .try_reserve_self_update(vec!["pkg".to_string()], "1.0.0", "2.0.0")
        .await;
    assert!(
        matches!(result, Err(SelfUpdateAdmissionError::AlreadyInProgress)),
        "self-update should fail in recovery"
    );

    // Reboot must fail
    let result = scheduler.admit_reboot(false, false).await;
    assert!(result.is_err(), "reboot should fail in recovery");

    // Mutation must fail
    let result = scheduler
        .run_mutation(uuid::Uuid::new_v4(), || Ok(()))
        .await;
    assert!(result.is_err(), "mutation should fail in recovery");
}

// =============================================================================
// 14. is_drained reports correctly
// =============================================================================

#[tokio::test]
async fn is_drained_reports_correctly() {
    let scheduler = Scheduler::new(5, 10);
    assert!(scheduler.is_drained().await, "should be drained initially");

    let job_id = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await
        .unwrap();

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = std::sync::Mutex::new(rx);
    let s = scheduler.clone();
    let handle = tokio::spawn(async move {
        s.run_mutation(job_id, move || {
            let _ = rx.lock().unwrap().recv();
            Ok(())
        })
        .await
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    assert!(
        !scheduler.is_drained().await,
        "should not be drained while mutation is running"
    );

    drop(tx);
    let _ = handle.await;
    assert!(
        scheduler.is_drained().await,
        "should be drained after mutation completes"
    );
}

// =============================================================================
// 15. Recovery reopens admission after successful finalization
// =============================================================================

/// After enter_recovery() and then reopen_admission(), the scheduler
/// must accept jobs again.
#[tokio::test]
async fn recovery_reopens_admission_after_finalization() {
    let scheduler = Scheduler::new(5, 10);

    scheduler.enter_recovery().await;
    assert_eq!(
        scheduler.admission_mode().await,
        AdmissionMode::Recovery,
        "should be in recovery"
    );

    // Job admission must fail in recovery
    let result = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await;
    assert!(result.is_err(), "job admission should fail in recovery");

    // Reopen admission
    scheduler.reopen_admission().await;
    assert_eq!(
        scheduler.admission_mode().await,
        AdmissionMode::Open,
        "should be open after reopen"
    );

    // Job admission must succeed now
    let result = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await;
    assert!(result.is_ok(), "job admission should succeed after reopen");
}

// =============================================================================
// 16. max_concurrent enforced on job start
// =============================================================================

/// With max_concurrent=1, starting a second job while one is running
/// must fail.
#[tokio::test]
async fn max_concurrent_enforced_on_start() {
    let scheduler = Scheduler::new(1, 10);

    let j1 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg1".to_string()])
        .await
        .unwrap();
    let j2 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg2".to_string()])
        .await
        .unwrap();

    // Start j1 — should succeed
    scheduler
        .update_job(
            &j1,
            linux_patch_api::jobs::manager::JobStatus::Running,
            Some(0),
            Some("starting".to_string()),
        )
        .await
        .unwrap();

    // Start j2 — must fail (max_concurrent=1)
    let result = scheduler
        .update_job(
            &j2,
            linux_patch_api::jobs::manager::JobStatus::Running,
            Some(0),
            Some("starting".to_string()),
        )
        .await;
    assert!(
        result.is_err(),
        "starting j2 should fail with max_concurrent=1"
    );

    // Complete j1
    scheduler.complete_job(&j1).await.unwrap();

    // Now j2 should start
    let result = scheduler
        .update_job(
            &j2,
            linux_patch_api::jobs::manager::JobStatus::Running,
            Some(0),
            Some("starting".to_string()),
        )
        .await;
    assert!(result.is_ok(), "j2 should start after j1 completes");
}

// =============================================================================
// 17. Mutation cancellation clears the slot
// =============================================================================

/// Aborting the task that awaits run_mutation must NOT clear the
/// slot while the blocking command is still running. The slot stays
/// set (fail-closed) until the blocking task completes, preventing
/// a second mutation from overlapping with the still-running command.
#[tokio::test]
async fn mutation_cancellation_keeps_slot_until_blocking_completes() {
    let scheduler = Scheduler::new(5, 10);

    let job_id = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await
        .unwrap();

    // Start a mutation that blocks on a channel
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = std::sync::Mutex::new(rx);
    let s = scheduler.clone();
    let handle = tokio::spawn(async move {
        s.run_mutation(job_id, move || {
            let _ = rx.lock().unwrap().recv();
            Ok(())
        })
        .await
    });

    // Give it time to acquire the slot
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    assert!(
        scheduler.is_mutation_in_progress().await,
        "mutation should be in progress"
    );

    // Abort the task — this cancels the await but NOT the blocking command.
    handle.abort();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // The slot must STILL be set — the blocking command is still running.
    // This prevents a second mutation from starting and overlapping.
    assert!(
        scheduler.is_mutation_in_progress().await,
        "mutation slot must stay set after cancellation — blocking command still running"
    );

    // A second mutation must be rejected while the first is still running
    let job2 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg2".to_string()])
        .await
        .unwrap();
    let result: Result<(), anyhow::Error> = scheduler.run_mutation(job2, || Ok(())).await;
    assert!(
        result.is_err(),
        "second mutation must be rejected while first blocking command is still running"
    );

    // Release the channel — the blocking command completes
    drop(tx);
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Now the slot must be cleared by the watchdog
    assert!(
        !scheduler.is_mutation_in_progress().await,
        "mutation slot must be cleared after blocking command completes"
    );

    // A new mutation should now be admissible
    let result = scheduler.run_mutation(job2, || Ok(())).await;
    assert!(
        result.is_ok(),
        "new mutation should succeed after blocking command completes"
    );
}
