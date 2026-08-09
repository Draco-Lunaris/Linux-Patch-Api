//! Tests for the durable job history persistence layer.
//!
//! These tests share a process-global test state-dir override, so they are
//! marked `#[serial]` to avoid clobbering each other when run in parallel.

use std::fs;

use linux_patch_api::jobs::manager::{Job, JobOperation, JobStatus};
use linux_patch_api::jobs::persistence::{
    clear_state_dir_for_testing, load_all_jobs, persist_all_jobs, set_state_dir_for_testing,
};
use serial_test::serial;
use tempfile::TempDir;

/// Build a job in the requested status.
fn make_job(status: JobStatus, op: JobOperation) -> Job {
    let mut job = Job::new(op, vec![]);
    match status {
        JobStatus::Pending => {}
        JobStatus::Running => job.start(),
        JobStatus::Rebooting => {
            job.start();
            job.set_rebooting("awaiting reboot".to_string());
        }
        JobStatus::Completed => {
            job.start();
            job.complete();
        }
        JobStatus::Failed => {
            job.start();
            job.fail("boom".to_string());
        }
        JobStatus::Cancelled => {
            job.status = JobStatus::Cancelled;
            job.message = "cancelled".to_string();
        }
        JobStatus::TimedOut => {
            job.status = JobStatus::TimedOut;
            job.message = "timed out".to_string();
        }
    }
    job
}

/// Persist a mix of statuses, reload, and verify every job survives the
/// round-trip with its status intact (including terminal jobs, which were
/// previously dropped on persist).
#[tokio::test]
#[serial]
async fn persist_all_jobs_round_trips_every_status() {
    let dir = TempDir::new().unwrap();
    set_state_dir_for_testing(dir.path().to_path_buf());

    let jobs = vec![
        make_job(JobStatus::Pending, JobOperation::Install),
        make_job(JobStatus::Running, JobOperation::Update),
        make_job(JobStatus::Rebooting, JobOperation::PatchApply),
        make_job(JobStatus::Completed, JobOperation::Remove),
        make_job(JobStatus::Failed, JobOperation::Install),
        make_job(JobStatus::Cancelled, JobOperation::Reboot),
        make_job(JobStatus::TimedOut, JobOperation::SelfUpdate),
    ];
    let ids: Vec<_> = jobs.iter().map(|j| j.id).collect();

    persist_all_jobs(&jobs).await;
    let loaded = load_all_jobs().await;

    assert_eq!(loaded.len(), jobs.len(), "all statuses must round-trip");
    for id in &ids {
        assert!(
            loaded.iter().any(|j| j.id == *id),
            "job {id} missing after reload"
        );
    }
    // Statuses preserved exactly.
    let find = |id| loaded.iter().find(|j| j.id == id).unwrap().status.clone();
    assert_eq!(find(ids[0]), JobStatus::Pending);
    assert_eq!(find(ids[1]), JobStatus::Running);
    assert_eq!(find(ids[2]), JobStatus::Rebooting);
    assert_eq!(find(ids[3]), JobStatus::Completed);
    assert_eq!(find(ids[4]), JobStatus::Failed);
    assert_eq!(find(ids[5]), JobStatus::Cancelled);
    assert_eq!(find(ids[6]), JobStatus::TimedOut);

    clear_state_dir_for_testing();
}

/// An empty job set writes an (effectively) empty history that loads back
/// as empty, not as a missing file.
#[tokio::test]
#[serial]
async fn persist_empty_then_load_empty() {
    let dir = TempDir::new().unwrap();
    set_state_dir_for_testing(dir.path().to_path_buf());

    persist_all_jobs(&[]).await;
    let loaded = load_all_jobs().await;
    assert!(loaded.is_empty());

    clear_state_dir_for_testing();
}

/// History is trimmed to the retention cap (500). Seeding 600 jobs must
/// load back exactly 500.
#[tokio::test]
#[serial]
async fn retention_trims_to_cap() {
    let dir = TempDir::new().unwrap();
    set_state_dir_for_testing(dir.path().to_path_buf());

    let jobs: Vec<Job> = (0..600)
        .map(|_| make_job(JobStatus::Completed, JobOperation::Install))
        .collect();
    persist_all_jobs(&jobs).await;
    let loaded = load_all_jobs().await;
    assert_eq!(
        loaded.len(),
        500,
        "history must be trimmed to the 500-job retention cap"
    );

    clear_state_dir_for_testing();
}

/// A legacy `running_jobs.json` (pre-2.7) is migrated to `jobs_history.json`
/// on first load, and the old file is removed.
#[tokio::test]
#[serial]
async fn migrates_legacy_running_jobs_file() {
    let dir = TempDir::new().unwrap();
    set_state_dir_for_testing(dir.path().to_path_buf());

    // Write a legacy file with a couple of running jobs.
    let legacy_jobs = vec![
        make_job(JobStatus::Running, JobOperation::Install),
        make_job(JobStatus::Pending, JobOperation::Update),
    ];
    let legacy_path = dir.path().join("running_jobs.json");
    fs::write(
        &legacy_path,
        serde_json::to_string_pretty(&legacy_jobs).unwrap(),
    )
    .unwrap();
    // No jobs_history.json yet.
    assert!(!dir.path().join("jobs_history.json").exists());

    let loaded = load_all_jobs().await;
    assert_eq!(loaded.len(), 2, "legacy jobs must be migrated and loaded");
    assert!(
        !legacy_path.exists(),
        "legacy running_jobs.json must be removed after migration"
    );
    assert!(
        dir.path().join("jobs_history.json").exists(),
        "jobs_history.json must be written after migration"
    );

    clear_state_dir_for_testing();
}

/// `delete_job` (via the scheduler) removes the job from the durable
/// history on disk.
#[tokio::test]
#[serial]
async fn delete_job_removes_from_disk() {
    use linux_patch_api::jobs::scheduler::Scheduler;

    let dir = TempDir::new().unwrap();
    set_state_dir_for_testing(dir.path().to_path_buf());

    let scheduler = Scheduler::new(5, 10);
    let job_id = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await
        .unwrap();
    scheduler
        .run_mutation(job_id, || Ok::<(), anyhow::Error>(()))
        .await
        .unwrap();
    scheduler.complete_job(&job_id).await.unwrap(); // persists

    // The completed job is in the history file.
    assert!(load_all_jobs().await.iter().any(|j| j.id == job_id));

    // Delete it — must sync to disk.
    let removed = scheduler.delete_job(&job_id).await.unwrap();
    assert!(removed);
    assert!(
        !load_all_jobs().await.iter().any(|j| j.id == job_id),
        "deleted job must not remain on disk"
    );

    clear_state_dir_for_testing();
}

/// Non-terminal internal tracking jobs (e.g. `__health_refresh__`) are NOT
/// persisted, so they can never be orphan-recovered as `AGENT_REBOOTED` on the
/// next restart (the source of the false "Agent rebooted during job
/// execution"). Terminal internal jobs (a genuinely completed/failed refresh)
/// are still kept for history.
#[tokio::test]
#[serial]
async fn persist_all_jobs_drops_nonterminal_internal_jobs() {
    let dir = TempDir::new().unwrap();
    set_state_dir_for_testing(dir.path().to_path_buf());

    let mut running_internal = Job::new(
        JobOperation::Install,
        vec!["__health_refresh__".to_string()],
    );
    running_internal.start();
    let mut completed_internal = Job::new(
        JobOperation::Install,
        vec!["__patch_list_refresh__".to_string()],
    );
    completed_internal.start();
    completed_internal.complete();
    let mut running_real = Job::new(JobOperation::PatchApply, vec!["kernel".to_string()]);
    running_real.start();

    let jobs = vec![running_internal, completed_internal, running_real];
    let running_internal_id = jobs[0].id;
    let completed_internal_id = jobs[1].id;
    let running_real_id = jobs[2].id;

    persist_all_jobs(&jobs).await;
    let loaded = load_all_jobs().await;
    let has = |id| loaded.iter().any(|j| j.id == id);

    assert!(
        !has(running_internal_id),
        "a non-terminal internal job must NOT be persisted (would be orphan-recovered as AGENT_REBOOTED)"
    );
    assert!(
        has(completed_internal_id),
        "a terminal internal job must be kept for history"
    );
    assert!(
        has(running_real_id),
        "a non-internal Running job must be kept"
    );

    clear_state_dir_for_testing();
}
