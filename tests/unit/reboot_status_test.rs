//! Tests for the new `Rebooting` status, the patch-job-timing fix, and
//! orphan recovery of `Rebooting` jobs after an agent restart.

use linux_patch_api::jobs::manager::{current_boot_id, Job, JobOperation, JobStatus};
use linux_patch_api::jobs::scheduler::Scheduler;

/// `set_job_rebooting` transitions a Running job to `Rebooting` (not
/// `Completed`), leaving it non-terminal so the manager sees an accurate
/// in-progress state.
#[tokio::test]
async fn set_job_rebooting_transitions_running_to_rebooting() {
    let scheduler = Scheduler::new(5, 10);

    let patch_id = scheduler
        .admit_job(JobOperation::PatchApply, vec![])
        .await
        .unwrap();
    // Run the mutation to completion of the closure; the job stays Running
    // (the handler calls complete_job separately).
    scheduler
        .dispatch_mutation(patch_id, || Ok::<(), anyhow::Error>(()))
        .await
        .unwrap();
    assert_eq!(
        scheduler.get_job(&patch_id).await.unwrap().status,
        JobStatus::Running
    );

    scheduler
        .set_job_rebooting(&patch_id, "Patches applied; awaiting reboot".to_string())
        .await
        .unwrap();

    let job = scheduler.get_job(&patch_id).await.unwrap();
    assert_eq!(
        job.status,
        JobStatus::Rebooting,
        "patch job should be Rebooting"
    );
    assert!(
        job.completed_at.is_none(),
        "Rebooting job must not be completed"
    );
    assert_ne!(
        job.progress, 100,
        "Rebooting job must not report progress=100"
    );
    assert!(
        job.logs.iter().any(|l| l.contains("awaiting reboot")),
        "Rebooting transition should log"
    );
}

/// `set_job_rebooting` rejects a job that is not Running (e.g. still
/// Pending), so it cannot be used to skip the mutation phase.
#[tokio::test]
async fn set_job_rebooting_rejects_non_running_job() {
    let scheduler = Scheduler::new(5, 10);
    let patch_id = scheduler
        .admit_job(JobOperation::PatchApply, vec![])
        .await
        .unwrap();
    // Still Pending — no dispatch yet.
    let result = scheduler
        .set_job_rebooting(&patch_id, "awaiting reboot".to_string())
        .await;
    assert!(
        result.is_err(),
        "set_job_rebooting must reject a Pending job"
    );
    assert_eq!(
        scheduler.get_job(&patch_id).await.unwrap().status,
        JobStatus::Pending
    );
}

/// A `Rebooting` `PatchApply` orphan is recovered as `Completed` — the
/// restart proves the reboot fired. No `error_code`.
#[tokio::test]
async fn recover_rebooting_patch_job_as_completed() {
    let scheduler = Scheduler::new(5, 10);

    let mut orphan = Job::new(JobOperation::PatchApply, vec![]);
    orphan.start();
    orphan.set_rebooting("Patches applied; awaiting reboot".to_string());
    let orphan_id = orphan.id;
    // Simulate a log that should survive recovery.
    orphan.add_log("Reboot command executed".to_string());

    scheduler.recover_orphaned_jobs(&[orphan]).await;

    let recovered = scheduler.get_job(&orphan_id).await.unwrap();
    assert_eq!(recovered.status, JobStatus::Completed);
    assert!(
        recovered.error_code.is_none(),
        "Rebooting orphan must not have an error_code"
    );
    assert_eq!(recovered.progress, 100);
    // The accumulated log must be preserved, plus the recovery marker.
    assert!(
        recovered
            .logs
            .iter()
            .any(|l| l.contains("Reboot command executed")),
        "preserved logs must survive recovery"
    );
    assert!(
        recovered
            .logs
            .iter()
            .any(|l| l.contains("marking completed")),
        "recovery must append a completion marker log"
    );
}

/// A `Rebooting` `Reboot` job (manual reboot) is also recovered as
/// `Completed` — the restart proves the reboot fired.
#[tokio::test]
async fn recover_rebooting_reboot_job_as_completed() {
    let scheduler = Scheduler::new(5, 10);

    let mut orphan = Job::new(JobOperation::Reboot, vec![]);
    orphan.start();
    orphan.set_rebooting("Reboot command starting".to_string());
    let orphan_id = orphan.id;

    scheduler.recover_orphaned_jobs(&[orphan]).await;

    let recovered = scheduler.get_job(&orphan_id).await.unwrap();
    assert_eq!(recovered.status, JobStatus::Completed);
    assert!(recovered.error_code.is_none());
}

/// A genuine `Running` orphan whose persisted `boot_id` differs from the
/// current boot_id (i.e. the machine actually rebooted) is recovered as
/// `Failed` with `AGENT_REBOOTED`. We simulate the reboot by overwriting the
/// orphan's boot_id with a value that cannot match the running host.
#[tokio::test]
async fn recover_running_orphan_different_boot_id_is_reboot() {
    let scheduler = Scheduler::new(5, 10);

    let mut orphan = Job::new(JobOperation::Install, vec![]);
    orphan.start();
    // A real reboot changes boot_id — pretend the job was created on a
    // previous boot.
    orphan.boot_id = Some("00000000-0000-0000-0000-differentboot".to_string());
    let orphan_id = orphan.id;

    scheduler.recover_orphaned_jobs(&[orphan]).await;

    let recovered = scheduler.get_job(&orphan_id).await.unwrap();
    assert_eq!(recovered.status, JobStatus::Failed);
    assert_eq!(
        recovered.error_code.as_deref(),
        Some("AGENT_REBOOTED"),
        "a real reboot (boot_id changed) must keep AGENT_REBOOTED"
    );
}

/// A `Running` orphan whose `boot_id` matches the current boot_id was NOT
/// rebooted — the agent process restarted (crash / OOM / `systemctl restart`
/// / dpkg-postinst self-restart) on the same boot. This is recovered as
/// `Failed` with the distinct `AGENT_RESTARTED` code, NOT `AGENT_REBOOTED`,
/// because a process restart is not the boot-brick failure mode.
#[tokio::test]
async fn recover_running_orphan_same_boot_id_is_process_restart() {
    let scheduler = Scheduler::new(5, 10);

    let mut orphan = Job::new(JobOperation::PatchApply, vec!["pkg".to_string()]);
    orphan.start();
    // Same boot as recovery → process restart, not a reboot.
    orphan.boot_id = current_boot_id();
    assert!(
        orphan.boot_id.is_some(),
        "test host must expose /proc boot_id"
    );
    let orphan_id = orphan.id;

    scheduler.recover_orphaned_jobs(&[orphan]).await;

    let recovered = scheduler.get_job(&orphan_id).await.unwrap();
    assert_eq!(recovered.status, JobStatus::Failed);
    assert_eq!(
        recovered.error_code.as_deref(),
        Some("AGENT_RESTARTED"),
        "a same-boot process restart must be AGENT_RESTARTED, not AGENT_REBOOTED"
    );
    assert!(
        recovered.message.contains("process restart"),
        "message should clarify it was a process restart: {}",
        recovered.message
    );
}

/// An internal tracking job (e.g. `__health_refresh__`) orphaned by a restart
/// is recovered as `Cancelled`, NOT `Failed/AGENT_REBOOTED` — it is ephemeral
/// bookkeeping and must never surface as a scary reboot failure.
#[tokio::test]
async fn recover_internal_tracking_job_is_cancelled() {
    let scheduler = Scheduler::new(5, 10);

    let mut orphan = Job::new(
        JobOperation::Install,
        vec!["__health_refresh__".to_string()],
    );
    orphan.start();
    let orphan_id = orphan.id;

    scheduler.recover_orphaned_jobs(&[orphan]).await;

    let recovered = scheduler.get_job(&orphan_id).await.unwrap();
    assert_eq!(recovered.status, JobStatus::Cancelled);
    assert!(
        recovered.error_code.is_none(),
        "internal tracking job must not get an error code"
    );
}

/// While a patch job is `Rebooting` (reboot reserved), new package jobs
/// must be rejected — the system is mid-reboot.
#[tokio::test]
async fn admission_rejects_new_jobs_while_patch_is_rebooting() {
    let scheduler = Scheduler::new(5, 10);

    let patch_id = scheduler
        .admit_job(JobOperation::PatchApply, vec![])
        .await
        .unwrap();
    scheduler
        .dispatch_mutation(patch_id, || Ok::<(), anyhow::Error>(()))
        .await
        .unwrap();

    // Reserve a reboot (force=true, no active mutation/self-update).
    let guard = scheduler.reserve_reboot(true, false).await.unwrap();
    scheduler
        .set_job_rebooting(&patch_id, "awaiting reboot".to_string())
        .await
        .unwrap();

    // A new install must be rejected while reboot_pending is set.
    let result = scheduler
        .admit_job(JobOperation::Install, vec!["pkg".to_string()])
        .await;
    assert!(
        result.is_err(),
        "new jobs must be rejected while a reboot is reserved"
    );

    // Patch job is Rebooting and counts as active.
    assert_eq!(
        scheduler.get_job(&patch_id).await.unwrap().status,
        JobStatus::Rebooting
    );
    let _ = guard.commit();
}

/// `active_count` includes `Rebooting` jobs, so a self-update cannot start
/// while a patch is mid-reboot.
#[tokio::test]
async fn active_count_includes_rebooting() {
    let scheduler = Scheduler::new(5, 10);

    let patch_id = scheduler
        .admit_job(JobOperation::PatchApply, vec![])
        .await
        .unwrap();
    scheduler
        .dispatch_mutation(patch_id, || Ok::<(), anyhow::Error>(()))
        .await
        .unwrap();
    let guard = scheduler.reserve_reboot(true, false).await.unwrap();
    scheduler
        .set_job_rebooting(&patch_id, "awaiting reboot".to_string())
        .await
        .unwrap();

    // active_count should be 2: the Rebooting patch job + the Pending/Rebooting
    // reboot job. The key assertion: Rebooting is counted as active.
    assert!(
        scheduler.active_count().await >= 2,
        "Rebooting jobs must count as active"
    );
    let _ = guard.commit();
}
