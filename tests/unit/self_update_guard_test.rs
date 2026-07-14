//! Self-Update Bidirectional Guard Tests
//!
//! Tests that:
//! - The self_update_in_progress flag starts false
//! - Setting it blocks new job acceptance checks
//! - Clearing it restores job acceptance
//! - The flag is shared across cloned JobManager instances (same Arc)

use linux_patch_api::jobs::manager::{JobManager, JobOperation};

#[actix_web::test]
async fn test_self_update_flag_starts_false() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    assert!(
        !jm.is_self_update_in_progress().await,
        "Flag should be false on a fresh JobManager"
    );
}

#[actix_web::test]
async fn test_set_and_clear_self_update_flag() {
    let jm = JobManager::new(5, 30, 100).unwrap();

    jm.set_self_update_in_progress().await;
    assert!(
        jm.is_self_update_in_progress().await,
        "Flag should be true after set_self_update_in_progress"
    );

    jm.clear_self_update().await;
    assert!(
        !jm.is_self_update_in_progress().await,
        "Flag should be false after clear_self_update"
    );
}

#[actix_web::test]
async fn test_flag_shared_across_clones() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    let jm_clone = jm.clone();

    jm.set_self_update_in_progress().await;
    assert!(
        jm_clone.is_self_update_in_progress().await,
        "Clone should see the flag set by the original"
    );

    jm_clone.clear_self_update().await;
    assert!(
        !jm.is_self_update_in_progress().await,
        "Original should see the flag cleared by the clone"
    );
}

#[actix_web::test]
async fn test_can_accept_job_still_works_with_flag_set() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    jm.set_self_update_in_progress().await;

    // can_accept_job checks queue depth, not the self-update flag.
    // The handler is responsible for checking the flag separately.
    assert!(
        jm.can_accept_job().await,
        "can_accept_job should still return true — the flag is checked by the handler, not can_accept_job"
    );
}

#[actix_web::test]
async fn test_running_count_unaffected_by_flag() {
    let jm = JobManager::new(5, 30, 100).unwrap();
    jm.set_self_update_in_progress().await;

    assert_eq!(
        jm.running_count().await,
        0,
        "running_count should be 0 — the flag doesn't create jobs"
    );
}

#[actix_web::test]
async fn test_flag_does_not_block_job_creation_directly() {
    // The flag is advisory — handlers check it before calling create_job.
    // create_job itself does not check the flag. This is by design: the
    // self-update handler needs to create its own job while the flag is set.
    let jm = JobManager::new(5, 30, 100).unwrap();
    jm.set_self_update_in_progress().await;

    let result = jm.create_job(JobOperation::Update, vec!["linux-patch-api".to_string()]).await;
    assert!(result.is_ok(), "create_job should succeed even with flag set — handlers enforce the guard");
}