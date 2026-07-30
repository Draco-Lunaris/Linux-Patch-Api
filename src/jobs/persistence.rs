//! Job persistence — survives agent reboot.
//!
//! When the agent reboots (e.g. after auto-reboot from patching), all
//! in-memory job state is lost. The manager polls for the job and gets
//! JOB_NOT_FOUND forever.
//!
//! This module writes running jobs to a JSON file on disk. On startup,
//! any persisted jobs are recovered and marked as failed with an
//! "agent rebooted" error so the manager can see the terminal status.

use crate::jobs::manager::{Job, JobStatus};
use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

/// Directory for persistent state files.
const STATE_DIR: &str = "/var/lib/linux_patch_api/state";

/// Filename for persisted running jobs.
const JOBS_FILE: &str = "running_jobs.json";

fn jobs_path() -> PathBuf {
    PathBuf::from(STATE_DIR).join(JOBS_FILE)
}

/// Persist the current set of running jobs to disk.
///
/// Called whenever a job transitions to `Running` or leaves `Running`
/// (completes, fails, is cancelled). Writes atomically via a temp file.
pub async fn persist_running_jobs(jobs: &[Job]) {
    let path = jobs_path();

    // Only persist jobs that are currently running or pending.
    let running: Vec<&Job> = jobs
        .iter()
        .filter(|j| j.status == JobStatus::Running || j.status == JobStatus::Pending)
        .collect();

    if running.is_empty() {
        // Remove the file if no running jobs — clean slate.
        let _ = fs::remove_file(&path).await;
        return;
    }

    let json = match serde_json::to_string_pretty(&running) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "persist_running_jobs: failed to serialize");
            return;
        }
    };

    // Atomic write: write to temp then rename.
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = fs::write(&tmp, &json).await {
        tracing::warn!(error = %e, "persist_running_jobs: failed to write temp file");
        return;
    }
    if let Err(e) = fs::rename(&tmp, &path).await {
        tracing::warn!(error = %e, "persist_running_jobs: failed to rename temp file");
        let _ = fs::remove_file(&tmp).await;
    }
}

/// Load persisted running jobs from disk on startup.
///
/// Returns the job IDs that were running when the agent last shut down.
/// These jobs are orphaned — the agent has no way to know if they completed
/// or not. The caller should mark them as failed.
pub async fn load_orphaned_jobs() -> Vec<Uuid> {
    let path = jobs_path();

    let json = match fs::read_to_string(&path).await {
        Ok(j) => j,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No persisted jobs — clean startup.
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!(error = %e, "load_orphaned_jobs: failed to read file");
            return Vec::new();
        }
    };

    let jobs: Vec<Job> = match serde_json::from_str(&json) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "load_orphaned_jobs: failed to deserialize");
            return Vec::new();
        }
    };

    let ids: Vec<Uuid> = jobs.iter().map(|j| j.id).collect();

    // Clean up the file — these jobs are now being handled.
    let _ = fs::remove_file(&path).await;

    ids
}

/// Clear the persisted jobs file. Called after orphaned jobs have been
/// processed to ensure a clean state.
pub async fn clear_persisted_jobs() {
    let _ = fs::remove_file(jobs_path()).await;
}
