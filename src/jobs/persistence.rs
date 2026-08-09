//! Job persistence — durable job history that survives agent reboot.
//!
//! When the agent reboots (e.g. after auto-reboot from patching), all
//! in-memory job state is lost. Previously only running/pending jobs were
//! persisted, which meant terminal jobs vanished on restart and the manager
//! got `JOB_NOT_FOUND` forever — including for a completed patch job and the
//! reboot job that was never persisted at all.
//!
//! This module now writes the **full** job history (all statuses) to a JSON
//! file on disk. On startup every persisted job is loaded back:
//!   - terminal jobs (Completed/Failed/Cancelled/TimedOut) are reloaded
//!     as-is so the manager can still query them;
//!   - non-terminal jobs (Pending/Running/Rebooting) are handed to orphan
//!     recovery, which assigns a terminal status (a `Rebooting` job means
//!     the reboot fired and is marked `Completed`; a genuine `Running`
//!     orphan is marked `Failed` with `AGENT_REBOOTED`).
//!
//! History is bounded by `HISTORY_RETENTION_COUNT` (most recent jobs by
//! `created_at`) to prevent unbounded growth.

use crate::jobs::manager::Job;
use std::path::{Path, PathBuf};
use tokio::fs;

/// Directory for persistent state files. Overridable in tests.
const STATE_DIR: &str = "/var/lib/linux_patch_api/state";

/// Filename for the durable job history (all statuses).
const JOBS_HISTORY_FILE: &str = "jobs_history.json";

/// Legacy filename (pre-2.7) that held only running/pending jobs. Migrated
/// to `JOBS_HISTORY_FILE` on first load if the new file is absent.
const LEGACY_JOBS_FILE: &str = "running_jobs.json";

/// Maximum number of jobs kept in the history file. The most recent jobs
/// by `created_at` are retained; older jobs are trimmed on each write.
const HISTORY_RETENTION_COUNT: usize = 500;

/// Test-only override for the state directory. When set, `jobs_path()` and
/// `legacy_jobs_path()` resolve under this directory instead of `STATE_DIR`,
/// letting persistence tests run against a tempdir.
#[cfg(any(test, feature = "test-utils"))]
static TEST_STATE_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

fn state_dir() -> PathBuf {
    #[cfg(any(test, feature = "test-utils"))]
    {
        if let Ok(guard) = TEST_STATE_DIR.lock() {
            if let Some(ref p) = *guard {
                return p.clone();
            }
        }
    }
    PathBuf::from(STATE_DIR)
}

fn jobs_path() -> PathBuf {
    state_dir().join(JOBS_HISTORY_FILE)
}

fn legacy_jobs_path() -> PathBuf {
    state_dir().join(LEGACY_JOBS_FILE)
}

/// Set the state directory for persistence tests. Must point at an existing
/// directory (tests create a tempdir first).
#[cfg(any(test, feature = "test-utils"))]
pub fn set_state_dir_for_testing(dir: PathBuf) {
    let mut guard = TEST_STATE_DIR
        .lock()
        .expect("TEST_STATE_DIR mutex poisoned");
    *guard = Some(dir);
}

#[cfg(any(test, feature = "test-utils"))]
pub fn clear_state_dir_for_testing() {
    let mut guard = TEST_STATE_DIR
        .lock()
        .expect("TEST_STATE_DIR mutex poisoned");
    *guard = None;
}

/// Persist the full set of jobs to disk as a durable history.
///
/// Called on every job transition. Writes ALL jobs (including terminal
/// ones) so the manager can query completed jobs after an agent restart.
/// History is trimmed to `HISTORY_RETENTION_COUNT` most recent jobs by
/// `created_at`. Atomic write via a uniquely-named temp file then rename,
/// so concurrent `persist_all_jobs` calls cannot clobber each other.
pub async fn persist_all_jobs(jobs: &[Job]) {
    let path = jobs_path();

    // Trim to the most recent N jobs by created_at (newest first).
    let mut kept: Vec<&Job> = jobs.iter().collect();

    // Internal tracking jobs (e.g. `__health_refresh__`, `__patch_list_refresh__`)
    // are ephemeral bookkeeping, not real mutations. Never persist them while
    // non-terminal: a non-terminal internal job written to disk would be
    // orphan-recovered on the next restart as a scary `AGENT_REBOOTED` failure —
    // the exact source of the false "Agent rebooted during job execution" noise
    // (a process restart mid-cache-refresh left a Running internal job on disk).
    // Terminal internal jobs (a genuinely Completed/Failed refresh) are kept so
    // the history reflects real cache-refresh outcomes.
    kept.retain(|j| !j.is_internal() || j.status.is_terminal());

    if kept.len() > HISTORY_RETENTION_COUNT {
        kept.sort_by_key(|j| std::cmp::Reverse(j.created_at));
        kept.truncate(HISTORY_RETENTION_COUNT);
    }

    let json = match serde_json::to_string_pretty(&kept) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "persist_all_jobs: failed to serialize");
            return;
        }
    };

    // Ensure the state directory exists (best-effort).
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent).await;
    }

    // Unique temp filename so overlapping writes don't race on the same
    // temp path. Derive uniqueness from a process-local counter + thread id.
    let tmp = unique_temp_path(&path);
    if let Err(e) = fs::write(&tmp, &json).await {
        tracing::warn!(error = %e, "persist_all_jobs: failed to write temp file");
        return;
    }
    if let Err(e) = fs::rename(&tmp, &path).await {
        tracing::warn!(error = %e, "persist_all_jobs: failed to rename temp file");
        let _ = fs::remove_file(&tmp).await;
    }
}

fn unique_temp_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("jobs");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("json");
    let tid = std::thread::current().id();
    let tid_hash = format!("{:?}", tid)
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    path.with_file_name(format!("{}.{}.{}.{}.tmp", stem, tid_hash, n, ext))
}

/// Load all persisted jobs from disk on startup.
///
/// Performs a one-shot migration from the legacy `running_jobs.json` if the
/// history file is absent. Does NOT delete the file after load — the caller
/// re-persists the resolved state after orphan recovery, which overwrites it.
///
/// Returns the full `Job` records. The caller partitions them into terminal
/// (reload as history) and non-terminal (run orphan recovery).
pub async fn load_all_jobs() -> Vec<Job> {
    let path = jobs_path();

    // One-shot migration from the legacy filename.
    if !path.exists() {
        let legacy = legacy_jobs_path();
        if legacy.exists() {
            match fs::read_to_string(&legacy).await {
                Ok(json) => {
                    let parsed: Result<Vec<Job>, _> = serde_json::from_str(&json);
                    if let Ok(jobs) = parsed {
                        // Write to the new path; remove the old file.
                        persist_all_jobs(&jobs).await;
                        let _ = fs::remove_file(&legacy).await;
                        tracing::info!(
                            migrated = jobs.len(),
                            "Migrated legacy running_jobs.json to jobs_history.json"
                        );
                        return jobs;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "load_all_jobs: failed to read legacy file");
                }
            }
        }
    }

    let json = match fs::read_to_string(&path).await {
        Ok(j) => j,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No persisted jobs — clean startup.
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!(error = %e, "load_all_jobs: failed to read file");
            return Vec::new();
        }
    };

    match serde_json::from_str(&json) {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!(error = %e, "load_all_jobs: failed to deserialize");
            Vec::new()
        }
    }
}

/// Clear the persisted jobs file. Called after orphaned jobs have been
/// processed to ensure a clean state, or for explicit resets.
pub async fn clear_persisted_jobs() {
    let _ = fs::remove_file(jobs_path()).await;
}
