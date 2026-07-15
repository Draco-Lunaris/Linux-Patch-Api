//! Concurrency Invariant Tests for Self-Update Admission
//!
//! These tests verify the actual dangerous interleavings that the self-update
//! guard must prevent. They use barriers and channels to force deterministic
//! interleavings rather than relying on timing sleeps.
//!
//! Tests:
//! - Normal job cannot be admitted after self-update reservation
//! - Self-update cannot be admitted after a normal job is admitted
//! - Only one concurrent self-update is admitted (even with simultaneous calls)
//! - Queued (pending) jobs prevent self-update admission
//! - Restart-pending state blocks jobs after install finishes
//! - Startup recovers restart-pending state from persistent storage
//! - Wrong-owner release does not clear the self-update lock

use std::sync::Arc;

use linux_patch_api::jobs::manager::{
    JobAdmissionError, JobManager, JobOperation, JobStatus, SelfUpdateAdmissionError,
};
use linux_patch_api::jobs::upgrade_state::{self, UpgradeState};
use tempfile::TempDir;
use uuid::Uuid;

// =============================================================================
// Admission Ordering Tests
// =============================================================================

/// A normal job cannot be admitted after a self-update has been reserved.
///
/// Interleaving:
///   Self-update handler calls try_reserve_self_update() and succeeds.
///   Patch handler calls admit_job() — must be rejected.
#[actix_web::test]
async fn normal_job_cannot_be_admitted_after_self_update_reservation() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    // Self-update reserves first
    let su_result = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await;
    assert!(su_result.is_ok(), "Self-update reservation should succeed");

    // Normal job admission must be rejected
    let result = jm
        .admit_job(JobOperation::PatchApply, vec!["pkg1".to_string()])
        .await;
    assert!(
        result.is_err(),
        "Normal job must be rejected after self-update reservation"
    );
    assert_eq!(result.unwrap_err(), JobAdmissionError::SelfUpdateInProgress);
}

/// A self-update cannot be reserved after a normal job has been admitted.
///
/// Interleaving:
///   Patch handler calls admit_job() and succeeds (job is pending).
///   Self-update handler calls try_reserve_self_update() — must be rejected
///   because there is an active (pending) job.
#[actix_web::test]
async fn self_update_cannot_be_admitted_after_normal_job_reservation() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    // Normal job is admitted first
    let result = jm
        .admit_job(JobOperation::PatchApply, vec!["pkg1".to_string()])
        .await;
    assert!(result.is_ok(), "Normal job admission should succeed");

    // Self-update must be rejected because there's a pending job
    let su_result = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await;
    assert!(
        su_result.is_err(),
        "Self-update must be rejected when jobs are active"
    );
    match su_result.unwrap_err() {
        SelfUpdateAdmissionError::JobsInProgress { count } => {
            assert_eq!(count, 1, "Should report 1 active job");
        }
        other => panic!("Expected JobsInProgress, got {:?}", other),
    }
}

/// Only one concurrent self-update is admitted, even when two calls race.
///
/// Two tasks call try_reserve_self_update() simultaneously. Exactly one
/// must succeed and the other must be rejected with AlreadyInProgress.
/// We use a barrier to ensure both tasks reach the call at the same time.
#[actix_web::test]
async fn only_one_concurrent_self_update_is_admitted() {
    let jm = Arc::new(JobManager::new(5, 30, 100).unwrap());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let jm1 = jm.clone();
    let jm2 = jm.clone();
    let b1 = barrier.clone();
    let b2 = barrier.clone();

    let task1 = tokio::spawn(async move {
        b1.wait().await;
        jm1.try_reserve_self_update(vec!["linux-patch-api".to_string()])
            .await
    });

    let task2 = tokio::spawn(async move {
        b2.wait().await;
        jm2.try_reserve_self_update(vec!["linux-patch-api".to_string()])
            .await
    });

    let result1 = task1.await.expect("task 1 panicked");
    let result2 = task2.await.expect("task 2 panicked");

    // Exactly one must succeed, the other must fail with AlreadyInProgress
    let success_count = result1.is_ok() as u32 + result2.is_ok() as u32;
    assert_eq!(
        success_count, 1,
        "Exactly one self-update should succeed, got {} successes",
        success_count
    );

    let failure = if result1.is_err() { &result1 } else { &result2 };
    assert_eq!(
        failure.as_ref().unwrap_err(),
        &SelfUpdateAdmissionError::AlreadyInProgress,
        "The rejected call should be AlreadyInProgress"
    );
}

/// Queued (pending) jobs prevent self-update admission.
///
/// A job is admitted (it's in Pending state, not yet Running).
/// try_reserve_self_update must reject it because active_count includes
/// pending jobs.
#[actix_web::test]
async fn queued_jobs_prevent_self_update_admission() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    // Admit a job — it starts in Pending state (no spawned task to run it)
    let result = jm
        .admit_job(JobOperation::Install, vec!["some-pkg".to_string()])
        .await;
    assert!(result.is_ok());

    // Verify the job is pending, not running
    let job = jm.get_job(&result.unwrap()).await;
    assert!(job.is_some());
    assert_eq!(job.unwrap().status, JobStatus::Pending);

    // Self-update must be rejected because of the pending job
    let su_result = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await;
    assert!(su_result.is_err());
    match su_result.unwrap_err() {
        SelfUpdateAdmissionError::JobsInProgress { count } => {
            assert_eq!(count, 1, "Should count the pending job");
        }
        other => panic!("Expected JobsInProgress, got {:?}", other),
    }
}

// =============================================================================
// Restart-Pending State Tests
// =============================================================================

/// Restart-pending state blocks jobs after install finishes.
///
/// After a self-update succeeds, the persistent state transitions to
/// RestartPending. The in-memory flag remains set (the old process doesn't
/// clear it). This test verifies that while the flag is set, normal jobs
/// are rejected — simulating the 30s window between install completion
/// and restart.
#[actix_web::test]
async fn restart_pending_state_blocks_jobs_after_install_finishes() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    // Simulate: self-update was reserved and succeeded
    let su_job_id = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await
        .expect("reservation should succeed");

    // The flag is still set (we don't clear it on success — the restart will
    // kill the process)
    assert!(jm.is_self_update_in_progress().await);

    // Normal jobs must be rejected during the restart-pending window
    let result = jm
        .admit_job(JobOperation::PatchApply, vec!["pkg1".to_string()])
        .await;
    assert!(
        result.is_err(),
        "Normal jobs must be rejected during restart-pending window"
    );
    assert_eq!(result.unwrap_err(), JobAdmissionError::SelfUpdateInProgress);

    // The flag is NOT cleared on success (only on failure or by the new process)
    assert!(
        jm.is_self_update_in_progress().await,
        "Self-update flag must remain set after successful install — cleared by new process on startup"
    );

    // Cleanup
    jm.release_self_update(&su_job_id).await;
}

/// Startup recovers restart-pending state from persistent storage.
///
/// This test simulates a process restart:
/// 1. Write a restart-pending state file (as the old process would)
/// 2. Create a new JobManager (as the new process would)
/// 3. Call reconcile_startup_state — should return true
/// 4. Set the self-update flag (as main.rs does)
/// 5. Verify normal jobs are rejected
/// 6. Call force_clear_self_update (as main.rs does after init)
/// 7. Verify normal jobs are now accepted
#[actix_web::test]
async fn startup_recovers_restart_pending_state() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");

    // 1. Write restart-pending state (simulating old process before crash/restart)
    let mut state = UpgradeState::installing("job-abc", "2.1.0", "2.2.0");
    state.to_restart_pending();
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();

    // 2. New JobManager (new process)
    let jm = JobManager::new(5, 30, 100).unwrap();

    // 3. Reconcile — should return RestartInProgress (restart pending, deadline not expired)
    let marker_path = dir.path().join("upgrade-pending");
    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert!(
        result == upgrade_state::StartupReconciliation::RestartInProgress,
        "reconcile should return RestartInProgress for restart-pending state with valid deadline"
    );

    // 4. Set the flag (as main.rs does — uses random UUID, not nil)
    jm.set_self_update_in_progress(Uuid::new_v4()).await;

    // 5. Normal jobs must be rejected
    let result = jm
        .admit_job(JobOperation::Install, vec!["test-pkg".to_string()])
        .await;
    assert!(
        result.is_err(),
        "Jobs must be rejected during restart-pending state"
    );
    assert_eq!(result.unwrap_err(), JobAdmissionError::SelfUpdateInProgress);

    // 6. Force-clear (as main.rs does after successful initialization)
    jm.force_clear_self_update().await;
    upgrade_state::clear_state_at(&state_path);

    // 7. Normal jobs are now accepted
    let result = jm
        .admit_job(JobOperation::Install, vec!["test-pkg".to_string()])
        .await;
    assert!(
        result.is_ok(),
        "Jobs should be accepted after clearing restart-pending state"
    );
}

/// Startup recovers from an interrupted install (Installing state).
///
/// If the process crashed during apt-get install, the state file shows
/// "installing". The new process should clear the state and allow normal
/// operation (the dpkg pre-flight will clean up).
#[actix_web::test]
async fn startup_recovers_from_interrupted_install() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");

    // Write installing state (simulating crash during apt-get)
    let state = UpgradeState::installing("job-xyz", "2.1.0", "2.2.0");
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();

    // Reconcile — should return InterruptedInstall (installing = interrupted)
    let marker_path = dir.path().join("upgrade-pending");
    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert!(
        result == upgrade_state::StartupReconciliation::InterruptedInstall,
        "reconcile should return InterruptedInstall for interrupted install"
    );

    // State file should NOT be cleared — it's kept until finalize_successful_restart
    // is called after the new process completes initialization.
    assert!(
        state_path.exists(),
        "State file should remain after reconcile for InterruptedInstall — cleared only after init"
    );

    // In production, main.rs sets the self-update flag for InterruptedInstall,
    // which blocks job admission until finalize. This test uses a fresh
    // JobManager (no flag set) to verify the reconcile return value only —
    // the flag-setting behavior is tested in the startup recovery test below.
    let jm = JobManager::new(5, 30, 100).unwrap();

    // Simulate what main.rs does: set the flag based on reconcile result
    jm.set_self_update_in_progress(Uuid::new_v4()).await;

    // Jobs should be rejected while the flag is set
    let result = jm
        .admit_job(JobOperation::Install, vec!["test-pkg".to_string()])
        .await;
    assert!(
        result.is_err(),
        "Jobs should be rejected while self-update flag is set for InterruptedInstall"
    );
    assert_eq!(result.unwrap_err(), JobAdmissionError::SelfUpdateInProgress);

    // After force-clearing the flag (as main.rs does after listener bind)
    jm.force_clear_self_update().await;
    upgrade_state::clear_state_at(&state_path);

    // Jobs should now be accepted
    let result = jm
        .admit_job(JobOperation::Install, vec!["test-pkg".to_string()])
        .await;
    assert!(
        result.is_ok(),
        "Jobs should be accepted after flag is cleared and state is finalized"
    );
}

/// Startup recovers from an expired restart-pending deadline.
///
/// If the restart timer failed and the deadline passed, the new process
/// should clear the state and allow recovery.
#[actix_web::test]
async fn startup_recovers_from_expired_restart_deadline() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");

    // Write restart-pending state with an expired deadline
    let mut state = UpgradeState::installing("job-expired", "2.1.0", "2.2.0");
    state.to_restart_pending();
    state.restart_deadline =
        Some((chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339());
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();

    // Reconcile — should return RecoveryMode (deadline expired)
    let marker_path = dir.path().join("upgrade-pending");
    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert!(
        result == upgrade_state::StartupReconciliation::RecoveryMode,
        "reconcile should return RecoveryMode for expired restart deadline"
    );

    // State file should NOT be cleared in recovery mode — preserved for diagnosis
    assert!(
        state_path.exists(),
        "State file should be preserved in RecoveryMode for diagnosis"
    );
}

// =============================================================================
// Ownership Permit Tests
// =============================================================================

/// Wrong-owner release does not clear the self-update lock.
///
/// If two self-updates somehow existed (e.g., a bug), Update A finishing
/// and calling release with its job_id must NOT clear Update B's lock.
#[actix_web::test]
async fn wrong_owner_release_does_not_clear_lock() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    // Update A reserves
    let job_a = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await
        .expect("Update A should reserve");

    // Simulate Update B somehow taking over (force-set with a different job_id)
    let job_b = Uuid::new_v4();
    jm.set_self_update_in_progress(job_b).await;

    // Update A tries to release — must fail (wrong owner)
    let released = jm.release_self_update(&job_a).await;
    assert!(
        !released,
        "Update A must not be able to release Update B's lock"
    );

    // Lock is still held by Update B
    assert!(
        jm.is_self_update_in_progress().await,
        "Lock must still be held by Update B after Update A's failed release"
    );

    // Update B can release
    let released = jm.release_self_update(&job_b).await;
    assert!(released, "Update B should be able to release its own lock");
    assert!(!jm.is_self_update_in_progress().await);
}

/// Release after force_clear is a no-op (the lock is already cleared).
#[actix_web::test]
async fn release_after_force_clear_is_noop() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    let job_id = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await
        .expect("reservation should succeed");

    // Force-clear (as the new process does on startup)
    jm.force_clear_self_update().await;
    assert!(!jm.is_self_update_in_progress().await);

    // Old owner tries to release — no-op (already cleared)
    let released = jm.release_self_update(&job_id).await;
    assert!(!released, "release after force_clear should be a no-op");
    assert!(!jm.is_self_update_in_progress().await);
}

// =============================================================================
// Concurrent Admission Race Tests (using barriers)
// =============================================================================

/// Self-update and normal job admission race: self-update must win or
/// normal job must win, but both cannot be admitted simultaneously.
///
/// Uses a barrier to ensure both calls happen at the same time. After
/// both complete, verify that at most one succeeded (either the self-update
/// or the normal job, not both).
#[actix_web::test]
async fn self_update_and_normal_job_cannot_both_be_admitted() {
    let jm = Arc::new(JobManager::new(5, 30, 100).unwrap());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let jm_su = jm.clone();
    let jm_job = jm.clone();
    let b1 = barrier.clone();
    let b2 = barrier.clone();

    let su_task = tokio::spawn(async move {
        b1.wait().await;
        jm_su
            .try_reserve_self_update(vec!["linux-patch-api".to_string()])
            .await
    });

    let job_task = tokio::spawn(async move {
        b2.wait().await;
        jm_job
            .admit_job(JobOperation::PatchApply, vec!["pkg1".to_string()])
            .await
    });

    let su_result = su_task.await.expect("su task panicked");
    let job_result = job_task.await.expect("job task panicked");

    // At most one can succeed. If self-update wins, the normal job is rejected.
    // If the normal job wins (admitted first), the self-update is rejected
    // because there's a pending job.
    let su_ok = su_result.is_ok();
    let job_ok = job_result.is_ok();

    assert!(
        !(su_ok && job_ok),
        "Both self-update and normal job were admitted — this is the race we're preventing"
    );

    // At least one should succeed (unless queue is full, which it isn't)
    assert!(
        su_ok || job_ok,
        "Neither was admitted — at least one should succeed"
    );

    if su_ok {
        // Self-update won — normal job should be rejected
        assert_eq!(
            job_result.unwrap_err(),
            JobAdmissionError::SelfUpdateInProgress,
            "Normal job should be rejected with SelfUpdateInProgress"
        );
    } else {
        // Normal job won — self-update should be rejected
        match su_result.unwrap_err() {
            SelfUpdateAdmissionError::JobsInProgress { .. } => {}
            other => panic!("Expected JobsInProgress, got {:?}", other),
        }
    }
}

/// Two normal jobs can be admitted concurrently (no self-update involved).
///
/// This verifies that the self-update read lock doesn't prevent concurrent
/// normal job admission — multiple read locks can be held simultaneously.
#[actix_web::test]
async fn two_normal_jobs_can_be_admitted_concurrently() {
    let jm = Arc::new(JobManager::new(5, 30, 100).unwrap());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let jm1 = jm.clone();
    let jm2 = jm.clone();
    let b1 = barrier.clone();
    let b2 = barrier.clone();

    let task1 = tokio::spawn(async move {
        b1.wait().await;
        jm1.admit_job(JobOperation::Install, vec!["pkg1".to_string()])
            .await
    });

    let task2 = tokio::spawn(async move {
        b2.wait().await;
        jm2.admit_job(JobOperation::Install, vec!["pkg2".to_string()])
            .await
    });

    let result1 = task1.await.expect("task 1 panicked");
    let result2 = task2.await.expect("task 2 panicked");

    assert!(
        result1.is_ok(),
        "First job should be admitted: {:?}",
        result1
    );
    assert!(
        result2.is_ok(),
        "Second job should be admitted: {:?}",
        result2
    );
    assert_ne!(
        result1.unwrap(),
        result2.unwrap(),
        "Each job should have a unique ID"
    );
}

// =============================================================================
// Crash-Recovery: Marker-Without-State Scenarios
// =============================================================================

/// Manager-initiated package upgrade (not self-update) creates a marker via
/// postinst, but the agent never wrote a state file because it wasn't a
/// self-update operation. On restart, marker-without-state must enter
/// RecoveryMode (fail-closed), not Clean.
#[actix_web::test]
async fn marker_without_state_enters_recovery_mode() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // Postinst created the marker, but no state file exists
    std::fs::write(&marker_path, "").unwrap();
    assert!(!state_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::RecoveryMode,
        "marker without state must enter RecoveryMode (fail-closed)"
    );

    // Marker must NOT be cleared — preserved for diagnosis
    assert!(
        marker_path.exists(),
        "marker should be preserved in RecoveryMode"
    );
}

/// write_state fails (disk full, permissions) but the postinst still creates
/// the marker. On restart, marker-without-state → RecoveryMode.
#[actix_web::test]
async fn write_state_failure_leaves_marker_without_state() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // Simulate: state write failed (no state file), but postinst created marker
    std::fs::write(&marker_path, "").unwrap();
    assert!(!state_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::RecoveryMode,
        "marker without state (write_state failure) must enter RecoveryMode"
    );
}

/// Agent crashes after write_state(Installing) but before apt-get runs.
/// State exists, marker does NOT yet exist (postinst hasn't run).
/// On restart: state says Installing → InterruptedInstall (correct).
#[actix_web::test]
async fn crash_after_write_state_before_postinst() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // State written, but marker not yet created (postinst hasn't run)
    let state = UpgradeState::installing("job-crash", "2.1.0", "2.2.0");
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();
    assert!(!marker_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::InterruptedInstall,
        "state=Installing with no marker should return InterruptedInstall"
    );
    // State must be preserved (not cleared)
    assert!(
        state_path.exists(),
        "state must be preserved for InterruptedInstall"
    );
}

/// Agent crashes during apt-get install (after postinst created marker).
/// Both state (Installing) and marker exist. On restart → InterruptedInstall.
#[actix_web::test]
async fn crash_during_install_with_marker() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // State written, marker created by postinst during install
    let state = UpgradeState::installing("job-crash", "2.1.0", "2.2.0");
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();
    std::fs::write(&marker_path, "").unwrap();

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::InterruptedInstall,
        "state=Installing with marker should return InterruptedInstall"
    );
    // Both must be preserved
    assert!(state_path.exists(), "state must be preserved");
    assert!(marker_path.exists(), "marker must be preserved");
}

/// write_state(RestartPending) fails after Verifying succeeded.
/// State still says Verifying, marker exists. On restart → InterruptedInstall
/// (correct — install completed but we don't know if restart was scheduled).
#[actix_web::test]
async fn write_state_restart_pending_failure_leaves_verifying() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // Verifying state (write_state(RestartPending) failed, so state stays Verifying)
    let mut state = UpgradeState::installing("job-verify", "2.1.0", "2.2.0");
    state.to_verifying();
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();
    std::fs::write(&marker_path, "").unwrap();

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::InterruptedInstall,
        "state=Verifying with marker should return InterruptedInstall"
    );
    assert!(state_path.exists());
    assert!(marker_path.exists());
}

/// clear_marker succeeds but restart_own_service fails.
/// Marker is gone, state says RestartPending. On restart → RestartInProgress
/// (correct — the new process starts and finalizes).
#[actix_web::test]
async fn clear_marker_succeeds_restart_fails() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // RestartPending state, marker already cleared (clear_marker succeeded)
    let mut state = UpgradeState::installing("job-restart", "2.1.0", "2.2.0");
    state.to_restart_pending();
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();
    // Marker does NOT exist (was cleared before restart_own_service failed)
    assert!(!marker_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::RestartInProgress,
        "state=RestartPending without marker should return RestartInProgress"
    );
    assert!(
        state_path.exists(),
        "state must be preserved for RestartInProgress"
    );
}

/// finalize_successful_restart clears state but fails to clear marker
/// (permissions error). On next restart: marker-without-state → RecoveryMode.
/// This is fail-closed — correct behavior.
#[actix_web::test]
async fn finalize_clears_state_but_not_marker() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // Simulate: finalize cleared state, but marker remains
    std::fs::write(&marker_path, "").unwrap();
    assert!(!state_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::RecoveryMode,
        "marker without state after finalize failure must enter RecoveryMode"
    );
    assert!(
        marker_path.exists(),
        "marker should be preserved for diagnosis"
    );
}

/// write_recovering_state fails. If the process then crashes, the next
/// startup sees marker (if it exists) without state → RecoveryMode (correct).
/// If no marker, Clean (correct — recovery completed and cleared everything).
#[actix_web::test]
async fn recovering_state_write_fails_with_marker() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // write_recovering_state failed (no state file), but marker exists
    std::fs::write(&marker_path, "").unwrap();
    assert!(!state_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::RecoveryMode,
        "failed write_recovering_state with marker must enter RecoveryMode"
    );
}

/// write_recovering_state fails and no marker exists. On restart → Clean
/// (correct — nothing happened, no upgrade in progress).
#[actix_web::test]
async fn recovering_state_write_fails_no_marker() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // No state file, no marker
    assert!(!state_path.exists());
    assert!(!marker_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::Clean,
        "no state and no marker must return Clean"
    );
}

/// Full lifecycle: RecoveryMode → finalize clears state and marker →
/// next restart is Clean. Verifies recovery is self-healing.
#[actix_web::test]
async fn recovery_mode_self_heals_after_finalize() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // Step 1: marker without state → RecoveryMode
    std::fs::write(&marker_path, "").unwrap();
    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(result, upgrade_state::StartupReconciliation::RecoveryMode);

    // Step 2: simulate finalize_successful_restart (clears both)
    upgrade_state::finalize_successful_restart_at(&state_path, &marker_path);
    assert!(!state_path.exists());
    assert!(!marker_path.exists());

    // Step 3: next restart → Clean
    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::Clean,
        "after finalize, next restart should be Clean"
    );
}

/// Reserving state with marker: reconcile clears both (self-update was
/// interrupted before install started — nothing happened, clean slate).
#[actix_web::test]
async fn reserving_with_marker_clears_both() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // Reserving state + marker (postinst ran before the agent wrote Installing)
    let state = UpgradeState::reserving("job-reserve", "2.1.0", "2.2.0");
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();
    std::fs::write(&marker_path, "").unwrap();

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::Clean,
        "Reserving state should return Clean (nothing happened)"
    );
    assert!(!state_path.exists(), "state should be cleared");
    assert!(!marker_path.exists(), "marker should be cleared");
}

/// Ready state without marker: reconcile clears state and returns Clean.
/// (Marker was already cleared by finalize, but state wasn't — edge case
/// where finalize cleared marker but failed to clear state.)
#[actix_web::test]
async fn ready_without_marker_clears_state() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // Ready state, no marker (marker was cleared, state wasn't)
    let mut state = UpgradeState::installing("job-ready", "2.1.0", "2.2.0");
    state.to_ready();
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();
    assert!(!marker_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::Clean,
        "Ready state should return Clean"
    );
    assert!(!state_path.exists(), "state should be cleared");
}

/// Recovering state with marker: reconcile returns RecoveryMode and
/// preserves both (continuing recovery).
#[actix_web::test]
async fn recovering_state_with_marker_continues_recovery() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    // Recovering state + marker
    let state = UpgradeState {
        state: upgrade_state::UpgradePhase::Recovering,
        job_id: String::new(),
        from_version: String::new(),
        target_version: String::new(),
        started_at: chrono::Utc::now().to_rfc3339(),
        restart_deadline: None,
        generation: 0,
    };
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();
    std::fs::write(&marker_path, "").unwrap();

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::RecoveryMode,
        "Recovering state should return RecoveryMode"
    );
    assert!(state_path.exists(), "state should be preserved");
    assert!(marker_path.exists(), "marker should be preserved");
}

/// Recovering state without marker: reconcile returns RecoveryMode and
/// preserves state. (Marker was cleared but state wasn't — the process
/// should still enter recovery mode to be safe.)
#[actix_web::test]
async fn recovering_state_without_marker() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("upgrade-state.json");
    let marker_path = dir.path().join("upgrade-pending");

    let state = UpgradeState {
        state: upgrade_state::UpgradePhase::Recovering,
        job_id: String::new(),
        from_version: String::new(),
        target_version: String::new(),
        started_at: chrono::Utc::now().to_rfc3339(),
        restart_deadline: None,
        generation: 0,
    };
    let json = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_path, &json).unwrap();
    assert!(!marker_path.exists());

    let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
    assert_eq!(
        result,
        upgrade_state::StartupReconciliation::RecoveryMode,
        "Recovering state without marker should still return RecoveryMode"
    );
    assert!(state_path.exists(), "state should be preserved");
}

// =============================================================================
// Property-based fuzz: exhaustive + randomized marker/state combinations
// =============================================================================

use linux_patch_api::jobs::upgrade_state::UpgradePhase;
use rand::Rng;

/// Safety invariant: reconcile must NEVER return `Clean` when a marker exists
/// and the state file was not explicitly in a phase that clears it (Reserving
/// or Ready). Returning `Clean` with a marker present would mean the agent
/// starts normal operation while an upgrade may be in progress — unsafe.
///
/// Additionally, `Clean` is only safe when BOTH state and marker are cleared
/// (or were never present). Any other combination that returns `Clean` must
/// have cleared both files.
fn assert_safe_outcome(
    result: &upgrade_state::StartupReconciliation,
    state_path: &std::path::Path,
    marker_path: &std::path::Path,
    context: &str,
) {
    match result {
        upgrade_state::StartupReconciliation::Clean => {
            // Clean is only safe if both files are gone (or never existed).
            // Reserving and Ready reconcile paths clear both — verify they did.
            assert!(
                !state_path.exists(),
                "{}: Clean result but state file still exists — unsafe",
                context
            );
            // Marker may exist if the phase was Reserving/Ready (which clear it)
            // or if no marker was present. Either way, after a Clean result,
            // the marker should NOT exist (it should have been cleared or
            // never present).
            assert!(
                !marker_path.exists(),
                "{}: Clean result but marker still exists — unsafe",
                context
            );
        }
        upgrade_state::StartupReconciliation::RestartInProgress => {
            // State must be preserved (not cleared) — it's needed for finalize.
            assert!(
                state_path.exists(),
                "{}: RestartInProgress but state file missing — state must be preserved",
                context
            );
        }
        upgrade_state::StartupReconciliation::InterruptedInstall => {
            // State must be preserved — it's needed for finalize after init.
            assert!(
                state_path.exists(),
                "{}: InterruptedInstall but state file missing — state must be preserved",
                context
            );
        }
        upgrade_state::StartupReconciliation::RecoveryMode => {
            // RecoveryMode is always safe (fail-closed). No file assertions —
            // state may or may not exist, marker may or may not exist.
            // The key invariant is that the self-update flag will be set,
            // blocking all operations.
        }
    }
}

/// Exhaustive enumeration: every (phase, marker_present, deadline_expired)
/// combination. 6 phases × 2 marker × 2 deadline = 24 base cases, plus
/// no-state-file × 2 marker = 2, plus corrupt-state × 2 marker = 2.
/// Total: 28 deterministic cases.
#[test]
fn fuzz_exhaustive_all_phase_marker_combinations() {
    let phases = [
        UpgradePhase::Reserving,
        UpgradePhase::Installing,
        UpgradePhase::Verifying,
        UpgradePhase::RestartPending,
        UpgradePhase::Ready,
        UpgradePhase::Recovering,
    ];

    for phase in &phases {
        for marker_present in [false, true] {
            for deadline_expired in [false, true] {
                let dir = TempDir::new().unwrap();
                let state_path = dir.path().join("upgrade-state.json");
                let marker_path = dir.path().join("upgrade-pending");

                // Write state file
                let mut state = UpgradeState::installing("job-fuzz", "1.0.0", "2.0.0");
                state.state = phase.clone();
                if *phase == UpgradePhase::RestartPending {
                    if deadline_expired {
                        state.restart_deadline =
                            Some((chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339());
                    } else {
                        state.restart_deadline =
                            Some((chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339());
                    }
                }
                let json = serde_json::to_string_pretty(&state).unwrap();
                std::fs::write(&state_path, &json).unwrap();

                if marker_present {
                    std::fs::write(&marker_path, "").unwrap();
                }

                let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
                let ctx = format!(
                    "phase={:?} marker={} deadline_expired={}",
                    phase, marker_present, deadline_expired
                );
                assert_safe_outcome(&result, &state_path, &marker_path, &ctx);
            }
        }
    }

    // No state file cases
    for marker_present in [false, true] {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("upgrade-state.json");
        let marker_path = dir.path().join("upgrade-pending");

        if marker_present {
            std::fs::write(&marker_path, "").unwrap();
        }

        let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
        let ctx = format!("no-state marker={}", marker_present);
        assert_safe_outcome(&result, &state_path, &marker_path, &ctx);
    }

    // Corrupt state file cases
    for marker_present in [false, true] {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("upgrade-state.json");
        let marker_path = dir.path().join("upgrade-pending");

        std::fs::write(&state_path, "not valid json {{{{").unwrap();
        if marker_present {
            std::fs::write(&marker_path, "").unwrap();
        }

        let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
        let ctx = format!("corrupt-state marker={}", marker_present);
        assert_safe_outcome(&result, &state_path, &marker_path, &ctx);
    }
}

/// Randomized fuzz: generate random combinations of state content
/// (valid JSON with random fields, invalid JSON, empty file, binary garbage)
/// and marker presence. Verify the safety invariant holds for every case.
///
/// Iteration count is configurable via `LPA_FUZZ_RANDOM_ITER` env var
/// (default 500, CI sets 5000).
#[test]
fn fuzz_randomized_combinations() {
    let iterations: u32 = std::env::var("LPA_FUZZ_RANDOM_ITER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    let mut rng = rand::thread_rng();

    for i in 0..iterations {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("upgrade-state.json");
        let marker_path = dir.path().join("upgrade-pending");

        // Randomly decide marker presence
        let marker_present: bool = rng.gen();
        if marker_present {
            std::fs::write(&marker_path, "").unwrap();
        }

        // Pick a random state-file content strategy
        let strategy: u8 = rng.gen_range(0..6);
        match strategy {
            0 => {
                // Valid JSON with random phase
                let phases = [
                    UpgradePhase::Reserving,
                    UpgradePhase::Installing,
                    UpgradePhase::Verifying,
                    UpgradePhase::RestartPending,
                    UpgradePhase::Ready,
                    UpgradePhase::Recovering,
                ];
                let phase = phases[rng.gen_range(0..phases.len())].clone();
                let mut state = UpgradeState::installing(
                    &format!("job-{}", i),
                    &format!(
                        "{}.{}.{}",
                        rng.gen_range(0..10),
                        rng.gen_range(0..10),
                        rng.gen_range(0..10)
                    ),
                    &format!(
                        "{}.{}.{}",
                        rng.gen_range(0..10),
                        rng.gen_range(0..10),
                        rng.gen_range(0..10)
                    ),
                );
                state.state = phase;
                // Random deadline: past, future, or none
                let deadline_choice: u8 = rng.gen_range(0..3);
                state.restart_deadline = match deadline_choice {
                    0 => Some(
                        (chrono::Utc::now() - chrono::Duration::seconds(rng.gen_range(1..3600)))
                            .to_rfc3339(),
                    ),
                    1 => Some(
                        (chrono::Utc::now() + chrono::Duration::seconds(rng.gen_range(1..3600)))
                            .to_rfc3339(),
                    ),
                    _ => None,
                };
                let json = serde_json::to_string_pretty(&state).unwrap();
                std::fs::write(&state_path, &json).unwrap();
            }
            1 => {
                // Empty file
                std::fs::write(&state_path, "").unwrap();
            }
            2 => {
                // Random binary garbage
                let garbage: Vec<u8> = (0..rng.gen_range(1..256)).map(|_| rng.gen()).collect();
                std::fs::write(&state_path, &garbage).unwrap();
            }
            3 => {
                // Valid JSON but wrong structure (missing required fields)
                let bad_json = format!(
                    r#"{{"random_key": "{}", "number": {}}}"#,
                    (0..10).map(|_| rng.gen::<char>()).collect::<String>(),
                    rng.gen::<i32>()
                );
                std::fs::write(&state_path, &bad_json).unwrap();
            }
            4 => {
                // No state file at all (don't write anything)
            }
            5 => {
                // Valid JSON with extra fields (should still parse)
                let state = UpgradeState::installing("job-extra", "1.0", "2.0");
                let mut json = serde_json::to_string_pretty(&state).unwrap();
                // Add a random extra field
                json = json.trim_end_matches('}').to_string();
                json.push_str(&format!(r#", "extra_field_{}": "unexpected_value"}}"#, i));
                std::fs::write(&state_path, &json).unwrap();
            }
            _ => unreachable!(),
        }

        let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
        let ctx = format!("iter={} strategy={} marker={}", i, strategy, marker_present);
        assert_safe_outcome(&result, &state_path, &marker_path, &ctx);
    }
}

/// Fuzz with partially-written state files (simulating crash mid-write).
/// The file may be truncated JSON — valid start but incomplete.
///
/// Iteration count is configurable via `LPA_FUZZ_TRUNCATED_ITER` env var
/// (default 100, nightly CI sets 10000).
#[test]
fn fuzz_truncated_state_files() {
    let iterations: u32 = std::env::var("LPA_FUZZ_TRUNCATED_ITER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let mut rng = rand::thread_rng();

    for i in 0..iterations {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("upgrade-state.json");
        let marker_path = dir.path().join("upgrade-pending");

        // Write a valid state, then truncate it at a random point
        let state = UpgradeState::installing("job-trunc", "1.0.0", "2.0.0");
        let full_json = serde_json::to_string_pretty(&state).unwrap();
        let truncate_at = rng.gen_range(0..=full_json.len());
        let truncated = &full_json[..truncate_at];
        std::fs::write(&state_path, truncated).unwrap();

        // Random marker
        if rng.gen() {
            std::fs::write(&marker_path, "").unwrap();
        }

        let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
        let ctx = format!("truncated iter={} truncate_at={}", i, truncate_at);
        assert_safe_outcome(&result, &state_path, &marker_path, &ctx);
    }
}

/// Fuzz with state files that have valid JSON but unexpected enum values
/// for the `state` field (e.g. "idle", "unknown", "foobar"). These should
/// fail to deserialize and enter RecoveryMode (fail-closed).
#[test]
fn fuzz_invalid_phase_values() {
    let invalid_phases = [
        "idle",
        "unknown",
        "foobar",
        "INSTALLING",   // wrong case
        "installing ",  // trailing space
        "installing\0", // null byte
        "",
    ];

    for (i, phase) in invalid_phases.iter().enumerate() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("upgrade-state.json");
        let marker_path = dir.path().join("upgrade-pending");

        // Write JSON with an invalid phase value
        let json = format!(
            r#"{{
  "state": "{}",
  "job_id": "job-{}",
  "from_version": "1.0.0",
  "target_version": "2.0.0",
  "started_at": "2026-01-01T00:00:00Z",
  "restart_deadline": null
}}"#,
            phase, i
        );
        std::fs::write(&state_path, &json).unwrap();

        let result = upgrade_state::reconcile_startup_state_at(&state_path, &marker_path);
        let ctx = format!("invalid_phase={} iter={}", phase, i);

        // Invalid phase should either fail to parse (→ RecoveryMode) or
        // be handled safely. Either way, the safety invariant must hold.
        assert_safe_outcome(&result, &state_path, &marker_path, &ctx);
    }
}
