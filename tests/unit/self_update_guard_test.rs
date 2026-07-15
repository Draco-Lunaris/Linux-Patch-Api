//! Self-Update Guard Tests
//!
//! Tests that:
//! - The self_update_owner state starts as None (no self-update in progress)
//! - Setting it with a job_id blocks new job acceptance
//! - Releasing it with the correct job_id restores job acceptance
//! - Releasing it with the wrong job_id is a no-op (ownership validation)
//! - The state is shared across cloned JobManager instances (same Arc)
//! - force_clear clears regardless of ownership (startup path)

use linux_patch_api::jobs::manager::{JobManager, JobOperation};
use uuid::Uuid;

#[actix_web::test]
async fn test_self_update_starts_idle() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    assert!(
        !jm.is_self_update_in_progress().await,
        "No self-update should be in progress on a fresh JobManager"
    );
}

#[actix_web::test]
async fn test_set_and_release_with_correct_owner() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    let job_id = Uuid::new_v4();

    jm.set_self_update_in_progress(job_id).await;
    assert!(
        jm.is_self_update_in_progress().await,
        "Self-update should be in progress after set"
    );

    let released = jm.release_self_update(&job_id).await;
    assert!(released, "release should succeed with correct owner");
    assert!(
        !jm.is_self_update_in_progress().await,
        "Self-update should be cleared after release with correct owner"
    );
}

#[actix_web::test]
async fn test_release_with_wrong_owner_is_noop() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    let owner_id = Uuid::new_v4();
    let wrong_id = Uuid::new_v4();

    jm.set_self_update_in_progress(owner_id).await;
    assert!(jm.is_self_update_in_progress().await);

    // Try to release with the wrong job_id
    let released = jm.release_self_update(&wrong_id).await;
    assert!(
        !released,
        "release should fail with wrong owner — the lock must not be cleared"
    );
    assert!(
        jm.is_self_update_in_progress().await,
        "Self-update should still be in progress after failed release"
    );

    // The correct owner can still release
    let released = jm.release_self_update(&owner_id).await;
    assert!(released, "release should succeed with correct owner");
    assert!(!jm.is_self_update_in_progress().await);
}

#[actix_web::test]
async fn test_release_when_idle_is_noop() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    let job_id = Uuid::new_v4();

    let released = jm.release_self_update(&job_id).await;
    assert!(
        !released,
        "release should be a no-op when no self-update is in progress"
    );
}

#[actix_web::test]
async fn test_force_clear_regardless_of_owner() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    let job_id = Uuid::new_v4();

    jm.set_self_update_in_progress(job_id).await;
    assert!(jm.is_self_update_in_progress().await);

    // force_clear doesn't need the job_id — used by startup path
    jm.force_clear_self_update().await;
    assert!(
        !jm.is_self_update_in_progress().await,
        "force_clear should clear regardless of ownership"
    );
}

#[actix_web::test]
async fn test_state_shared_across_clones() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    let jm_clone = jm.clone();
    let job_id = Uuid::new_v4();

    jm.set_self_update_in_progress(job_id).await;
    assert!(
        jm_clone.is_self_update_in_progress().await,
        "Clone should see the state set by the original"
    );

    let released = jm_clone.release_self_update(&job_id).await;
    assert!(
        released,
        "Clone should be able to release with correct owner"
    );
    assert!(
        !jm.is_self_update_in_progress().await,
        "Original should see the state cleared by the clone"
    );
}

#[actix_web::test]
async fn test_try_reserve_sets_owner() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    let result = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await;

    assert!(result.is_ok(), "try_reserve should succeed when idle");
    let job_id = result.unwrap().commit();
    assert!(
        jm.is_self_update_in_progress().await,
        "Self-update should be in progress after try_reserve"
    );

    // The returned job_id is the ownership permit
    let released = jm.release_self_update(&job_id).await;
    assert!(
        released,
        "release should succeed with the job_id from try_reserve"
    );
    assert!(!jm.is_self_update_in_progress().await);
}

#[actix_web::test]
async fn test_try_reserve_rejects_duplicate() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    let result1 = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await;
    assert!(result1.is_ok());

    let result2 = jm
        .try_reserve_self_update(vec!["linux-patch-api".to_string()])
        .await;
    assert!(result2.is_err(), "Second try_reserve should be rejected");
    assert_eq!(
        result2.unwrap_err(),
        linux_patch_api::jobs::manager::SelfUpdateAdmissionError::AlreadyInProgress
    );
}

#[actix_web::test]
async fn test_admit_job_rejected_when_self_update_in_progress() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    let job_id = Uuid::new_v4();

    jm.set_self_update_in_progress(job_id).await;

    let result = jm
        .admit_job(JobOperation::Install, vec!["test-pkg".to_string()])
        .await;
    assert!(
        result.is_err(),
        "admit_job should be rejected during self-update"
    );
    assert_eq!(
        result.unwrap_err(),
        linux_patch_api::jobs::manager::JobAdmissionError::SelfUpdateInProgress
    );
}

#[actix_web::test]
async fn test_admit_job_succeeds_when_idle() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    let result = jm
        .admit_job(JobOperation::Install, vec!["test-pkg".to_string()])
        .await;
    assert!(result.is_ok(), "admit_job should succeed when idle");
}
