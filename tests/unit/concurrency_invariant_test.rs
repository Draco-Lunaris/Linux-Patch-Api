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

    // 3. Reconcile — should return true (restart pending, deadline not expired)
    let should_block = upgrade_state::reconcile_startup_state_at(&state_path);
    assert!(
        should_block,
        "reconcile should return true for restart-pending state with valid deadline"
    );

    // 4. Set the flag (as main.rs does)
    jm.set_self_update_in_progress(Uuid::nil()).await;

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

    // Reconcile — should return false (installing = interrupted, not restart-pending)
    let should_block = upgrade_state::reconcile_startup_state_at(&state_path);
    assert!(
        !should_block,
        "reconcile should return false for interrupted install — dpkg pre-flight will clean up"
    );

    // State file should be cleared
    assert!(
        !state_path.exists(),
        "State file should be removed after reconcile"
    );

    // Jobs should be accepted (no flag set)
    let jm = JobManager::new(5, 30, 100).unwrap();
    let result = jm
        .admit_job(JobOperation::Install, vec!["test-pkg".to_string()])
        .await;
    assert!(
        result.is_ok(),
        "Jobs should be accepted after interrupted install recovery"
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

    // Reconcile — should return false (deadline expired)
    let should_block = upgrade_state::reconcile_startup_state_at(&state_path);
    assert!(
        !should_block,
        "reconcile should return false for expired restart deadline"
    );

    assert!(
        !state_path.exists(),
        "State file should be removed after expired deadline"
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
