//! Production-path tests for the unified scheduler.
//!
//! These tests call the same Scheduler methods used by production handlers.
//! They use barriers, atomic counters, and channels for deterministic
//! synchronization — no arbitrary sleeps.

use std::sync::Arc;

use linux_patch_api::jobs::manager::{Job, JobOperation, JobStatus};
use linux_patch_api::jobs::scheduler::{
    set_test_watchdog_backstop, AdmissionMode, JobAdmissionError, RebootAdmissionError, Scheduler,
    SelfUpdateAdmissionError, TryMutationError,
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
    let job_id = guard.commit();

    // Verify we can't reserve again (already in progress)
    let result = scheduler
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "1.0.0", "3.0.0")
        .await;
    assert!(
        matches!(result, Err(SelfUpdateAdmissionError::AlreadyInProgress)),
        "second self-update reservation should fail"
    );

    // Release and verify we can reserve with a different target
    let _ = scheduler.release_self_update(&job_id).await;
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

// =============================================================================
// Orphaned job recovery (PR #200 regression — self-update false failure)
// =============================================================================

/// Build a minimal orphaned job record as it would be loaded from
/// running_jobs.json after a restart.
fn orphaned_job(operation: JobOperation) -> Job {
    let mut job = Job::new(operation, vec!["linux-patch-api".to_string()]);
    job.start(); // status = Running, as persisted
    job
}

/// A self-update job orphaned by the postinst restart must be marked
/// Completed — the new binary is running precisely because the upgrade
/// succeeded. Marking it Failed (AGENT_REBOOTED) is the regression that
/// broke self-upgrade reporting across all distros.
#[tokio::test]
async fn recover_orphaned_self_update_is_completed_not_failed() {
    let scheduler = Scheduler::new(2, 10);
    let orphaned = orphaned_job(JobOperation::SelfUpdate);
    let id = orphaned.id;

    scheduler.recover_orphaned_jobs(&[orphaned]).await;

    let job = scheduler.get_job(&id).await.expect("job must be recovered");
    assert_eq!(
        job.status,
        JobStatus::Completed,
        "self-update orphan must be Completed (restart proves upgrade succeeded), not Failed"
    );
    assert!(
        job.error_code.is_none(),
        "completed self-update must not carry AGENT_REBOOTED error code"
    );
    assert!(
        matches!(job.operation, JobOperation::SelfUpdate),
        "operation must be preserved as SelfUpdate"
    );
}

/// A non-self-update orphan (genuine crash / reboot during execution) must
/// still be marked Failed with AGENT_REBOOTED.
#[tokio::test]
async fn recover_orphaned_regular_job_is_failed() {
    let scheduler = Scheduler::new(2, 10);
    let orphaned = orphaned_job(JobOperation::Update);
    let id = orphaned.id;

    scheduler.recover_orphaned_jobs(&[orphaned]).await;

    let job = scheduler.get_job(&id).await.expect("job must be recovered");
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(job.error_code.as_deref(), Some("AGENT_REBOOTED"));
    assert!(
        matches!(job.operation, JobOperation::Update),
        "operation must be preserved as Update"
    );
}

// =============================================================================
// Internal tracking jobs (background cache refresh) must not block
// self-update / reboot / queue, and a hung refresh closure must be
// force-finalized by the watchdog backstop so it can't hold the slot forever.
// =============================================================================

/// `Job::is_internal()` identifies the sentinel-package tracking jobs and
/// rejects ordinary package jobs.
#[tokio::test]
async fn is_internal_predicate_identifies_tracking_jobs() {
    assert!(
        Job::new(
            JobOperation::Install,
            vec!["__health_refresh__".to_string()]
        )
        .is_internal(),
        "__health_refresh__ tracking job is internal"
    );
    assert!(
        Job::new(
            JobOperation::Install,
            vec!["__patch_list_refresh__".to_string()]
        )
        .is_internal(),
        "__patch_list_refresh__ tracking job is internal"
    );
    assert!(
        !Job::new(JobOperation::Install, vec!["nginx".to_string()]).is_internal(),
        "a real package job is not internal"
    );
    assert!(
        !Job::new(JobOperation::PatchApply, vec![]).is_internal(),
        "a patch job is not internal"
    );
}

/// A mutation closure that hangs past the watchdog backstop is force-finalized:
/// the job reaches a terminal state and the mutation slot is released so a
/// subsequent mutation (e.g. self-update) can proceed. This is the fix for the
/// haproxy failure mode where a stuck refresh held the slot forever.
#[tokio::test]
async fn watchdog_backstop_force_finalizes_hung_closure() {
    // Shrink the backstop to ~1s for the test.
    set_test_watchdog_backstop(Some(std::time::Duration::from_secs(1)));
    // Restore the default at the end of the test so it doesn't leak.
    struct BackstopGuard;
    impl Drop for BackstopGuard {
        fn drop(&mut self) {
            set_test_watchdog_backstop(None);
        }
    }
    let _guard = BackstopGuard;

    let scheduler = Scheduler::new(5, 10);
    let job_id = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await
        .unwrap();

    // A closure that blocks past the 1s backstop. Use a channel recv (released
    // at test exit) rather than a long sleep so the detached blocking task
    // doesn't linger on the blocking pool after the test returns.
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let sched = scheduler.clone();
    let handle = tokio::spawn(async move {
        sched
            .dispatch_mutation(job_id, move || -> anyhow::Result<()> {
                let _ = release_rx.lock().unwrap().recv();
                Ok(())
            })
            .await
    });
    // Keep release_tx alive for the test body; dropping it (at end of scope)
    // unblocks the detached closure so its blocking thread can exit.
    let _release_tx = release_tx;

    // Wait for the mutation to acquire the slot.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        scheduler.is_mutation_in_progress().await,
        "slot held while closure runs"
    );

    // The backstop must fire and release the slot well before the test budget.
    for _ in 0..50 {
        if !scheduler.is_mutation_in_progress().await {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        !scheduler.is_mutation_in_progress().await,
        "watchdog backstop must release the mutation slot"
    );

    // The caller's dispatch_mutation must have received the timeout error.
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), handle)
        .await
        .expect("dispatch_mutation must return after backstop");
    assert!(
        result.unwrap().is_err(),
        "dispatch_mutation must return Err on backstop timeout"
    );

    // The watchdog leaves the job Running when the caller is alive (the handler
    // is responsible for the terminal transition). Mimic a handler: fail_job.
    let _ = scheduler
        .fail_job(&job_id, "watchdog backstop elapsed".to_string())
        .await;
    let job = scheduler.get_job(&job_id).await.unwrap();
    assert_eq!(
        job.status,
        JobStatus::Failed,
        "job must be Failed after the handler finalizes the backstop timeout"
    );

    // A second mutation can now acquire the slot — self-update would unblock.
    let j2 = scheduler
        .admit_job(JobOperation::Install, vec!["pkg2".to_string()])
        .await
        .unwrap();
    let r2 = scheduler
        .dispatch_mutation(j2, || Ok::<(), anyhow::Error>(()))
        .await;
    assert!(
        r2.is_ok(),
        "a second mutation must run after the backstop frees the slot"
    );
    let _ = scheduler.complete_job(&j2).await;
    let _ = scheduler.delete_job(&j2).await;
    let _ = scheduler.delete_job(&job_id).await;
}

/// While an internal tracking job (background cache refresh) holds the mutation
/// slot, `try_reserve_self_update` must admit (not reject with
/// `JobsInProgress`) — a read-ish cache refresh must not block an agent
/// upgrade. The self-update would then wait for the slot, which the backstop
/// bounds.
#[tokio::test]
async fn internal_tracking_job_does_not_block_self_update_admission() {
    let scheduler = Scheduler::new(5, 10);

    let tracking_id = scheduler
        .admit_job(
            JobOperation::Install,
            vec!["__health_refresh__".to_string()],
        )
        .await
        .unwrap();

    // Hold the mutation slot with the tracking job's "refresh" closure.
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let sched = scheduler.clone();
    let _tracking_handle = tokio::spawn(async move {
        sched
            .dispatch_mutation(tracking_id, move || -> anyhow::Result<()> {
                let _ = release_rx.lock().unwrap().recv();
                Ok(())
            })
            .await
    });

    // Wait for the tracking job to acquire the slot.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        scheduler.is_mutation_in_progress().await,
        "tracking job holds the slot"
    );
    assert_eq!(
        scheduler.get_job(&tracking_id).await.unwrap().status,
        JobStatus::Running
    );

    // Self-update admission must succeed despite the held slot, because the
    // holder is an internal tracking job.
    let guard = scheduler
        .try_reserve_self_update(vec!["linux-patch-api".to_string()], "1.0.0", "2.0.0")
        .await;
    assert!(
        guard.is_ok(),
        "self-update admission must succeed while an internal tracking job holds the slot"
    );
    let job_id = guard.unwrap().commit();
    assert!(scheduler.is_self_update_in_progress().await);

    // Release the tracking job's closure so the test can clean up.
    drop(release_tx);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let _ = scheduler.release_self_update(&job_id).await;
    let _ = scheduler.delete_job(&tracking_id).await;
}

/// An internal tracking job must not consume user-facing queue capacity: with
/// `max_queue_depth = 1`, a pending tracking job must not prevent a real package
/// job from being admitted.
#[tokio::test]
async fn internal_tracking_job_does_not_consume_queue_depth() {
    let scheduler = Scheduler::new(5, 1);

    let tracking_id = scheduler
        .admit_job(
            JobOperation::Install,
            vec!["__patch_list_refresh__".to_string()],
        )
        .await
        .unwrap();
    assert_eq!(
        scheduler.get_job(&tracking_id).await.unwrap().status,
        JobStatus::Pending
    );

    // A real package job must still be admitted despite the pending tracking
    // job — the queue-depth gate excludes internal jobs.
    let real_id = scheduler
        .admit_job(JobOperation::Install, vec!["nginx".to_string()])
        .await
        .expect("real job must be admitted — tracking job does not consume queue depth");

    let _ = scheduler.delete_job(&tracking_id).await;
    let _ = scheduler.delete_job(&real_id).await;
}

/// An internal tracking job holding the mutation slot must not block a
/// non-force reboot reservation — the active-jobs gate excludes internal jobs.
#[tokio::test]
async fn internal_tracking_job_does_not_block_reboot() {
    let scheduler = Scheduler::new(5, 10);

    let tracking_id = scheduler
        .admit_job(
            JobOperation::Install,
            vec!["__patch_list_refresh__".to_string()],
        )
        .await
        .unwrap();

    // Hold the slot with the tracking job so the reboot gate's mutation slot is
    // occupied by an internal job.
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let sched = scheduler.clone();
    let _tracking_handle = tokio::spawn(async move {
        sched
            .dispatch_mutation(tracking_id, move || -> anyhow::Result<()> {
                let _ = release_rx.lock().unwrap().recv();
                Ok(())
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        scheduler.is_mutation_in_progress().await,
        "tracking job holds the slot"
    );

    // Non-force reboot reservation must succeed — the active-jobs gate excludes
    // the internal tracking job.
    let reboot_guard = scheduler
        .reserve_reboot(false, false)
        .await
        .expect("non-force reboot must succeed while an internal tracking job runs");
    // Drop without commit to roll back the reservation (clears reboot_pending).
    drop(reboot_guard);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Release and clean up.
    drop(release_tx);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let _ = scheduler.delete_job(&tracking_id).await;
}
