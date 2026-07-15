//! Persistent upgrade state for self-update lifecycle tracking.
//!
//! The in-memory `self_update_in_progress` flag is volatile — it disappears
//! on crash or restart. This module provides a persistent state file that
//! survives process restarts, allowing the new process to reconcile its
//! upgrade state on startup.
//!
//! ## State file
//!
//! Located at `/var/lib/linux_patch_api/upgrade-state.json`. Written
//! atomically (temp file → fsync → rename) to prevent corruption on
//! crash mid-write. Temp files use `O_EXCL` (create_new) to prevent
//! symlink attacks.
//!
//! ## Lifecycle
//!
//! ```text
//! Idle
//!   → write state { "state": "reserving", ... }
//!   → set self_update_owner
//!   → write state { "state": "installing", ... }
//!   → apt-get installs the package
//!   → write state { "state": "verifying", ... }
//!   → verify installed version changed
//!   → write state { "state": "restart_pending", ... }
//!   → postinst schedules delayed restart
//!   → old process killed by restart
//!   → new process starts, reads state file
//!   → if restart_pending and deadline not expired:
//!       keep self_update flag set, continue initialization
//!   → after successful initialization (listener bound, READY=1 sent):
//!       verify running version == target_version
//!       clear self_update flag, delete state file and marker
//!   → if restart_pending and deadline expired:
//!       enter recovery mode
//! ```
//!
//! ## Crash recovery
//!
//! If the process crashes during `installing` (apt-get was running),
//! the next startup sees `installing` state. The state is NOT cleared
//! immediately — the new process keeps the self_update flag set, runs
//! dpkg --configure -a via the pre-flight, and then clears the state.
//!
//! If the process crashes during `restart_pending` (after install,
//! before restart), the restart timer should still fire and restart
//! the service. If the timer also failed, the deadline check on
//! startup will detect the stale state and enter recovery mode.
//!
//! ## Fail-closed state handling
//!
//! A missing state file with an upgrade marker present, or a corrupt
//! (unparseable) state file, enters **recovery mode** — not normal
//! operation. In recovery mode, all package operations are blocked,
//! dpkg --configure -a is run, and health reports degraded status.

use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Path to the persistent upgrade state file.
pub const UPGRADE_STATE_PATH: &str = "/var/lib/linux_patch_api/upgrade-state.json";

/// Path to the upgrade-pending marker file created by postinst.
pub const UPGRADE_MARKER_PATH: &str = "/var/lib/linux_patch_api/upgrade-pending";

/// How long after a self-update restart is initiated before we consider
/// the restart to have failed. The postinst schedules a delayed restart,
/// so we allow 120s for the new process to start.
const RESTART_DEADLINE_SECS: i64 = 120;

/// The phase of a self-update lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UpgradePhase {
    /// No upgrade in progress. Normal operation.
    Idle,
    /// Self-update reserved, before apt starts. If process dies here,
    /// the reservation is orphaned — recovery clears it.
    Reserving,
    /// apt-get is actively installing the new package.
    Installing,
    /// apt install finished, verifying the installed version changed.
    /// If process dies here, the install may have succeeded — recovery
    /// should check the installed version.
    Verifying,
    /// Package installed and verified, waiting for the delayed restart.
    RestartPending,
    /// Restart command has been issued; the new process is starting.
    /// If the old process dies here, the new process should appear and
    /// transition to Ready after initialization.
    StartingNewProcess,
    /// New process started, listener bound, READY=1 sent. Marker and
    /// state can be safely cleared.
    Ready,
    /// State file is missing/corrupt but marker exists, or state is
    /// inconsistent. All ops blocked, dpkg cleanup runs, health degraded.
    Recovering,
}

/// Persistent upgrade state, written to disk and read on startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeState {
    /// Current phase of the upgrade.
    pub state: UpgradePhase,
    /// Job ID of the self-update job (for manager correlation).
    pub job_id: String,
    /// Version the agent is upgrading from.
    pub from_version: String,
    /// Version the agent is upgrading to (best known at write time).
    pub target_version: String,
    /// When the upgrade was initiated (RFC3339).
    pub started_at: String,
    /// When the restart should have completed (RFC3339).
    /// Only set when state == RestartPending.
    pub restart_deadline: Option<String>,
    /// Monotonic generation/operation ID. Incremented on each state
    /// transition. Used to prevent stale timers from acting on a
    /// later upgrade. The restart marker file contains the same
    /// generation — if they don't match, the marker is stale.
    #[serde(default)]
    pub generation: u64,
}

impl UpgradeState {
    /// Create a new `Reserving` state.
    pub fn reserving(job_id: &str, from_version: &str, target_version: &str) -> Self {
        Self {
            state: UpgradePhase::Reserving,
            job_id: job_id.to_string(),
            from_version: from_version.to_string(),
            target_version: target_version.to_string(),
            started_at: Utc::now().to_rfc3339(),
            restart_deadline: None,
            generation: next_generation(),
        }
    }

    /// Create a new `Installing` state.
    pub fn installing(job_id: &str, from_version: &str, target_version: &str) -> Self {
        Self {
            state: UpgradePhase::Installing,
            job_id: job_id.to_string(),
            from_version: from_version.to_string(),
            target_version: target_version.to_string(),
            started_at: Utc::now().to_rfc3339(),
            restart_deadline: None,
            generation: next_generation(),
        }
    }

    /// Create a new `Reserving` state with a specific generation.
    /// Used when the caller needs to control the generation (e.g. when
    /// transitioning from Reserving to Installing — same generation).
    pub fn reserving_with_generation(
        job_id: &str,
        from_version: &str,
        target_version: &str,
        generation: u64,
    ) -> Self {
        Self {
            state: UpgradePhase::Reserving,
            job_id: job_id.to_string(),
            from_version: from_version.to_string(),
            target_version: target_version.to_string(),
            started_at: Utc::now().to_rfc3339(),
            restart_deadline: None,
            generation,
        }
    }

    /// Create a new `Installing` state with a specific generation.
    pub fn installing_with_generation(
        job_id: &str,
        from_version: &str,
        target_version: &str,
        generation: u64,
    ) -> Self {
        Self {
            state: UpgradePhase::Installing,
            job_id: job_id.to_string(),
            from_version: from_version.to_string(),
            target_version: target_version.to_string(),
            started_at: Utc::now().to_rfc3339(),
            restart_deadline: None,
            generation,
        }
    }

    /// Transition to `Verifying` state (preserves generation).
    pub fn to_verifying(&mut self) {
        self.state = UpgradePhase::Verifying;
    }

    /// Transition to `RestartPending` state with a deadline (preserves generation).
    pub fn to_restart_pending(&mut self) {
        self.state = UpgradePhase::RestartPending;
        self.restart_deadline =
            Some((Utc::now() + Duration::seconds(RESTART_DEADLINE_SECS)).to_rfc3339());
    }

    /// Transition to `Ready` state (preserves generation).
    pub fn to_ready(&mut self) {
        self.state = UpgradePhase::Ready;
    }

    /// Transition to `StartingNewProcess` state (preserves generation).
    /// Called after the restart command is issued but before the new
    /// process has completed initialization.
    pub fn to_starting_new_process(&mut self) {
        self.state = UpgradePhase::StartingNewProcess;
    }

    /// Transition to `Recovering` state (preserves generation).
    pub fn to_recovering(&mut self) {
        self.state = UpgradePhase::Recovering;
    }

    /// Get the generation/operation ID.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Check if the restart deadline has passed.
    /// Returns false if no deadline is set or if it hasn't been reached.
    pub fn is_deadline_expired(&self) -> bool {
        match &self.restart_deadline {
            Some(deadline_str) => match DateTime::parse_from_rfc3339(deadline_str) {
                Ok(deadline) => Utc::now() > deadline.with_timezone(&Utc),
                Err(_) => true,
            },
            None => false,
        }
    }
}

/// Global monotonic generation counter. Uses a static AtomicU64 so each
/// state transition gets a unique generation within the process lifetime.
static GENERATION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_generation() -> u64 {
    GENERATION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// Result of reconciling upgrade state on startup.
#[derive(Debug, Clone, PartialEq)]
pub enum StartupReconciliation {
    /// Clean state — no upgrade in progress. Normal operation.
    Clean,
    /// Restart in progress — keep self_update flag set until initialization
    /// completes. The state file should NOT be cleared until finalize.
    RestartInProgress,
    /// An interrupted install was detected. The self_update flag should be
    /// set to block jobs, and dpkg --configure -a should run via pre-flight.
    /// The state file should NOT be cleared until after cleanup succeeds.
    InterruptedInstall,
    /// Recovery mode — state file is corrupt/missing but marker exists, or
    /// state is inconsistent. All ops blocked, health degraded.
    RecoveryMode,
}

/// Error type for state file reading (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum StateError {
    /// No state file — clean state (marker check is done by reconcile).
    Clean,
    /// State file exists but is unparseable.
    Corrupt(String),
    /// State file says Idle/Ready but marker exists — inconsistent.
    Inconsistent(String),
}

/// Atomically write the upgrade state to disk.
///
/// Uses a unique temp filename (including generation) with `O_EXCL`
/// (create_new) to prevent symlink attacks and collisions with stale
/// temp files from prior crashes. Writes to temp file → fsync → rename
/// for crash safety. Directory is fsynced after rename.
///
/// If a temp file from a prior generation exists, it is removed only
/// if its filename doesn't match the current generation (stale cleanup).
pub fn write_state(state: &UpgradeState) -> std::io::Result<()> {
    let path = Path::new(UPGRADE_STATE_PATH);
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("/var/lib/linux_patch_api"));

    std::fs::create_dir_all(dir)?;

    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;

    // Use a unique temp filename including the generation to prevent
    // collisions with stale temp files from prior crashes.
    let tmp_path = path.with_extension(format!("json.tmp.{}", state.generation));

    {
        use std::io::Write;
        // Use create_new (O_EXCL) to prevent symlink attacks.
        // If the temp file already exists (same generation — shouldn't
        // happen in normal flow), return an error rather than silently
        // removing it.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp_path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "Temp file {} already exists — possible concurrent write or stale temp from same generation",
                            tmp_path.display()
                        ),
                    )
                } else {
                    e
                }
            })?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
    }

    std::fs::rename(&tmp_path, path)?;

    // fsync the directory to ensure the rename is durable
    if let Ok(dir_file) = std::fs::File::open(dir) {
        let _ = dir_file.sync_all();
    }

    info!(path = %path.display(), state = ?state.state, generation = state.generation, "Upgrade state persisted");
    Ok(())
}

/// Clean up stale temp files from prior generations.
/// Called on startup to remove any leftover .json.tmp.* files that
/// don't match the current state's generation.
pub fn cleanup_stale_temp_files() {
    let dir = Path::new(UPGRADE_STATE_PATH)
        .parent()
        .unwrap_or_else(|| Path::new("/var/lib/linux_patch_api"));

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("upgrade-state.json.tmp") {
                warn!(path = %entry.path().display(), "Removing stale upgrade state temp file");
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Read the upgrade state from disk.
/// Returns the parsed state, or a `StateError` describing what went wrong.
/// This function only checks the state file — it does NOT check the marker.
/// Use `reconcile_startup_state` for full startup reconciliation including
/// marker checks.
pub fn read_state() -> Result<UpgradeState, StateError> {
    let path = Path::new(UPGRADE_STATE_PATH);
    read_state_from(path)
}

/// Read the upgrade state from a specific path (for testing).
/// Does NOT check the marker file — only reads and parses the state file.
pub fn read_state_from(path: &Path) -> Result<UpgradeState, StateError> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StateError::Clean);
        }
        Err(e) => {
            return Err(StateError::Corrupt(format!(
                "Failed to read state file: {}",
                e
            )));
        }
    };

    match serde_json::from_str::<UpgradeState>(&content) {
        Ok(state) => Ok(state),
        Err(e) => {
            warn!(error = %e, path = %path.display(), "Failed to parse upgrade state file — entering recovery mode");
            Err(StateError::Corrupt(format!(
                "Failed to parse state file: {}",
                e
            )))
        }
    }
}

/// Delete the upgrade state file.
pub fn clear_state() {
    let path = Path::new(UPGRADE_STATE_PATH);
    clear_state_at(path);
}

/// Delete the upgrade state file at a specific path (for testing).
pub fn clear_state_at(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(_) => info!(path = %path.display(), "Upgrade state file removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            warn!(error = %e, path = %path.display(), "Failed to remove upgrade state file");
        }
    }
}

/// Check if the upgrade-pending marker file exists.
pub fn marker_exists() -> bool {
    Path::new(UPGRADE_MARKER_PATH).exists()
}

/// Remove the upgrade-pending marker file.
pub fn clear_marker() {
    let marker = Path::new(UPGRADE_MARKER_PATH);
    match std::fs::remove_file(marker) {
        Ok(_) => info!("Upgrade-pending marker file removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            warn!(error = %e, "Failed to remove upgrade-pending marker file");
        }
    }
}

/// Reconcile upgrade state on startup.
///
/// Called early in the startup sequence, before the job manager accepts
/// any jobs. Returns a `StartupReconciliation` indicating what action
/// the caller should take.
///
/// **Fail-closed**: A corrupt state file or a missing state file with a
/// marker present enters recovery mode. The state is NOT silently cleared.
///
/// **No early clearing**: The state file is NOT cleared by this function
/// for `InterruptedInstall` or `RestartInProgress` — the caller must call
/// `finalize_successful_restart()` after initialization completes.
pub fn reconcile_startup_state() -> StartupReconciliation {
    let path = Path::new(UPGRADE_STATE_PATH);
    let marker = Path::new(UPGRADE_MARKER_PATH);
    reconcile_startup_state_at(path, marker)
}

/// Reconcile upgrade state from specific paths (for testing).
pub fn reconcile_startup_state_at(state_path: &Path, marker_path: &Path) -> StartupReconciliation {
    let state = match read_state_from(state_path) {
        Ok(s) => s,
        Err(StateError::Clean) => {
            // No state file — check if marker exists
            if marker_path.exists() {
                warn!(
                    "Upgrade marker exists but state file is missing — entering recovery mode. \
                     An upgrade may have been interrupted. dpkg --configure -a will run via pre-flight."
                );
                return StartupReconciliation::RecoveryMode;
            }
            return StartupReconciliation::Clean;
        }
        Err(StateError::Corrupt(msg)) => {
            // Corrupt state file — enter recovery mode, do NOT clear.
            warn!(
                error = %msg,
                "Upgrade state file is corrupt — entering recovery mode. \
                 dpkg --configure -a will run via pre-flight. State file preserved for diagnosis."
            );
            return StartupReconciliation::RecoveryMode;
        }
        Err(StateError::Inconsistent(msg)) => {
            warn!(
                error = %msg,
                "Upgrade state is inconsistent — entering recovery mode."
            );
            return StartupReconciliation::RecoveryMode;
        }
    };

    match state.state {
        UpgradePhase::Idle => {
            // Idle state — no upgrade in progress. If marker exists, it's
            // inconsistent (marker without an active upgrade). Enter recovery.
            if marker_path.exists() {
                warn!(
                    "Upgrade state is 'idle' but marker exists — inconsistent, entering recovery mode"
                );
                return StartupReconciliation::RecoveryMode;
            }
            info!("Upgrade state is 'idle' — normal startup");
            clear_state_at(state_path);
            StartupReconciliation::Clean
        }
        UpgradePhase::Reserving => {
            warn!(
                job_id = %state.job_id,
                "Found upgrade state in 'reserving' phase — self-update was interrupted before install started. \
                 Clearing state and marker to allow retry."
            );
            clear_state_at(state_path);
            clear_marker_at(marker_path);
            StartupReconciliation::Clean
        }
        UpgradePhase::Installing => {
            warn!(
                job_id = %state.job_id,
                from_version = %state.from_version,
                target_version = %state.target_version,
                started_at = %state.started_at,
                "Found upgrade state in 'installing' phase — apt-get was interrupted. \
                 Keeping self_update flag set. The dpkg pre-flight will clean up. \
                 State will be cleared after successful initialization."
            );
            StartupReconciliation::InterruptedInstall
        }
        UpgradePhase::Verifying => {
            warn!(
                job_id = %state.job_id,
                from_version = %state.from_version,
                target_version = %state.target_version,
                "Found upgrade state in 'verifying' phase — install completed but verification was interrupted. \
                 Keeping self_update flag set. Will verify installed version on startup."
            );
            StartupReconciliation::InterruptedInstall
        }
        UpgradePhase::RestartPending => {
            if state.is_deadline_expired() {
                warn!(
                    job_id = %state.job_id,
                    from_version = %state.from_version,
                    target_version = %state.target_version,
                    started_at = %state.started_at,
                    "Found upgrade state in 'restart_pending' phase but deadline has expired. \
                     The delayed restart may have failed. Entering recovery mode."
                );
                StartupReconciliation::RecoveryMode
            } else {
                info!(
                    job_id = %state.job_id,
                    from_version = %state.from_version,
                    target_version = %state.target_version,
                    "Found upgrade state in 'restart_pending' phase — restart in progress. \
                     Keeping self_update flag set until initialization completes."
                );
                StartupReconciliation::RestartInProgress
            }
        }
        UpgradePhase::StartingNewProcess => {
            // The old process issued the restart command and transitioned
            // to StartingNewProcess. We are the new process. Keep the
            // admission block set until initialization completes.
            info!(
                job_id = %state.job_id,
                from_version = %state.from_version,
                target_version = %state.target_version,
                "Found upgrade state in 'starting_new_process' phase — new process is initializing. \
                 Keeping self_update flag set until initialization completes."
            );
            StartupReconciliation::RestartInProgress
        }
        UpgradePhase::Ready => {
            // State says Ready but we're starting up — this means the previous
            // process wrote Ready but didn't finish clearing. Clear now.
            info!("Found upgrade state in 'ready' phase — clearing state and marker");
            clear_state_at(state_path);
            clear_marker_at(marker_path);
            StartupReconciliation::Clean
        }
        UpgradePhase::Recovering => {
            warn!("Found upgrade state in 'recovering' phase — continuing recovery mode");
            StartupReconciliation::RecoveryMode
        }
    }
}

/// Clear the marker file at a specific path (for testing).
fn clear_marker_at(marker_path: &Path) {
    match std::fs::remove_file(marker_path) {
        Ok(_) => info!(path = %marker_path.display(), "Upgrade marker removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            warn!(error = %e, path = %marker_path.display(), "Failed to remove upgrade marker");
        }
    }
}

/// Clean up the upgrade state after successful initialization.
///
/// Called AFTER the new process has fully initialized (config loaded,
/// backend ready, server listening, READY=1 sent to systemd). Clears
/// the state file and the marker file.
pub fn finalize_successful_restart() {
    let state_path = Path::new(UPGRADE_STATE_PATH);
    clear_state_at(state_path);
    clear_marker();
}

/// Finalize with specific paths (for testing).
pub fn finalize_successful_restart_at(state_path: &Path, marker_path: &Path) {
    clear_state_at(state_path);
    clear_marker_at(marker_path);
}

/// Write a `Recovering` state to disk so the next startup (if this process
/// crashes during recovery) also enters recovery mode.
pub fn write_recovering_state() {
    let state = UpgradeState {
        state: UpgradePhase::Recovering,
        job_id: String::new(),
        from_version: String::new(),
        target_version: String::new(),
        started_at: Utc::now().to_rfc3339(),
        restart_deadline: None,
        generation: next_generation(),
    };
    if let Err(e) = write_state(&state) {
        warn!(error = %e, "Failed to write recovering state — next startup may not detect recovery mode");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn test_paths(dir: &TempDir) -> (PathBuf, PathBuf) {
        (
            dir.path().join("upgrade-state.json"),
            dir.path().join("upgrade-pending"),
        )
    }

    #[test]
    fn test_write_and_read_state() {
        let dir = TempDir::new().unwrap();
        let (state_path, _) = test_paths(&dir);

        let state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        write_state_at(&state, &state_path).unwrap();

        let read = read_state_from(&state_path).unwrap();
        assert_eq!(read.state, UpgradePhase::Installing);
        assert_eq!(read.job_id, "job-123");
        assert_eq!(read.from_version, "2.1.0");
        assert_eq!(read.target_version, "2.2.0");
    }

    fn write_state_at(state: &UpgradeState, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
        let tmp_path = path.with_extension(format!("json.tmp.{}", state.generation));
        if tmp_path.exists() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    #[test]
    fn test_read_missing_file_returns_clean() {
        let dir = TempDir::new().unwrap();
        let (state_path, _) = test_paths(&dir);
        let result = read_state_from(&state_path);
        assert_eq!(result.unwrap_err(), StateError::Clean);
    }

    #[test]
    fn test_read_corrupt_file_returns_corrupt() {
        let dir = TempDir::new().unwrap();
        let (state_path, _) = test_paths(&dir);
        fs::write(&state_path, "not valid json").unwrap();
        let result = read_state_from(&state_path);
        assert!(matches!(result, Err(StateError::Corrupt(_))));
    }

    #[test]
    fn test_to_restart_pending_sets_deadline() {
        let mut state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        assert!(state.restart_deadline.is_none());

        state.to_restart_pending();
        assert_eq!(state.state, UpgradePhase::RestartPending);
        assert!(state.restart_deadline.is_some());

        let deadline = DateTime::parse_from_rfc3339(state.restart_deadline.as_ref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        let now = Utc::now();
        let diff = deadline - now;
        assert!(diff.num_seconds() > 100 && diff.num_seconds() < 130);
    }

    #[test]
    fn test_deadline_not_expired_when_recent() {
        let mut state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        state.to_restart_pending();
        assert!(!state.is_deadline_expired());
    }

    #[test]
    fn test_deadline_expired_when_in_past() {
        let mut state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        state.to_restart_pending();
        state.restart_deadline = Some((Utc::now() - Duration::seconds(10)).to_rfc3339());
        assert!(state.is_deadline_expired());
    }

    #[test]
    fn test_deadline_expired_when_unparseable() {
        let mut state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        state.to_restart_pending();
        state.restart_deadline = Some("not-a-date".to_string());
        assert!(state.is_deadline_expired());
    }

    #[test]
    fn test_reconcile_no_state_file() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);
        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::Clean
        );
    }

    #[test]
    fn test_reconcile_missing_state_with_marker_enters_recovery() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);
        fs::write(&marker_path, "").unwrap();

        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::RecoveryMode
        );
        // State file should NOT be cleared in recovery mode
        assert!(!state_path.exists());
        // Marker should NOT be cleared in recovery mode
        assert!(marker_path.exists());
    }

    #[test]
    fn test_reconcile_corrupt_state_enters_recovery() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);
        fs::write(&state_path, "garbage").unwrap();

        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::RecoveryMode
        );
        // State file should NOT be cleared — preserved for diagnosis
        assert!(state_path.exists());
    }

    #[test]
    fn test_reconcile_reserving_clears_and_returns_clean() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);

        let state = UpgradeState::reserving("job-123", "2.1.0", "2.2.0");
        write_state_at(&state, &state_path).unwrap();
        fs::write(&marker_path, "").unwrap();

        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::Clean
        );
        assert!(!state_path.exists(), "state file should be cleared");
        assert!(!marker_path.exists(), "marker should be cleared");
    }

    #[test]
    fn test_reconcile_installing_returns_interrupted_install() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);

        let state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        write_state_at(&state, &state_path).unwrap();

        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::InterruptedInstall
        );
        // State file should NOT be cleared — kept until finalize
        assert!(state_path.exists());
    }

    #[test]
    fn test_reconcile_verifying_returns_interrupted_install() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);

        let mut state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        state.to_verifying();
        write_state_at(&state, &state_path).unwrap();

        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::InterruptedInstall
        );
        assert!(state_path.exists());
    }

    #[test]
    fn test_reconcile_restart_pending_not_expired_returns_restart_in_progress() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);

        let mut state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        state.to_restart_pending();
        write_state_at(&state, &state_path).unwrap();

        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::RestartInProgress
        );
        assert!(state_path.exists());
    }

    #[test]
    fn test_reconcile_restart_pending_expired_enters_recovery() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);

        let mut state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        state.to_restart_pending();
        state.restart_deadline = Some((Utc::now() - Duration::seconds(10)).to_rfc3339());
        write_state_at(&state, &state_path).unwrap();

        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::RecoveryMode
        );
        // State file should NOT be cleared in recovery mode
        assert!(state_path.exists());
    }

    #[test]
    fn test_reconcile_ready_clears_and_returns_clean() {
        let dir = TempDir::new().unwrap();
        let (state_path, marker_path) = test_paths(&dir);

        let mut state = UpgradeState::installing("job-123", "2.1.0", "2.2.0");
        state.to_ready();
        write_state_at(&state, &state_path).unwrap();
        fs::write(&marker_path, "").unwrap();

        assert_eq!(
            reconcile_startup_state_at(&state_path, &marker_path),
            StartupReconciliation::Clean
        );
        assert!(!state_path.exists());
        assert!(!marker_path.exists());
    }

    #[test]
    fn test_clear_state_removes_file() {
        let dir = TempDir::new().unwrap();
        let (state_path, _) = test_paths(&dir);

        fs::write(&state_path, "{}").unwrap();
        assert!(state_path.exists());

        clear_state_at(&state_path);
        assert!(!state_path.exists());
    }

    #[test]
    fn test_clear_state_missing_file_is_noop() {
        let dir = TempDir::new().unwrap();
        let (state_path, _) = test_paths(&dir);
        clear_state_at(&state_path);
    }

    #[test]
    fn test_write_state_atomic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("upgrade-state.json");

        let state = UpgradeState::installing("job-456", "1.0.0", "2.0.0");
        write_state_at(&state, &path).unwrap();

        assert!(path.exists());
        let read = read_state_from(&path).unwrap();
        assert_eq!(read.job_id, "job-456");

        // The temp file should have been renamed (not left behind)
        assert!(!path.with_extension("json.tmp.0").exists());
    }

    #[test]
    fn test_reserving_state() {
        let state = UpgradeState::reserving("job-1", "1.0", "2.0");
        assert_eq!(state.state, UpgradePhase::Reserving);
        assert_eq!(state.job_id, "job-1");
    }

    #[test]
    fn test_to_verifying() {
        let mut state = UpgradeState::installing("job-1", "1.0", "2.0");
        state.to_verifying();
        assert_eq!(state.state, UpgradePhase::Verifying);
    }

    #[test]
    fn test_to_ready() {
        let mut state = UpgradeState::installing("job-1", "1.0", "2.0");
        state.to_ready();
        assert_eq!(state.state, UpgradePhase::Ready);
    }

    #[test]
    fn test_to_recovering() {
        let mut state = UpgradeState::installing("job-1", "1.0", "2.0");
        state.to_recovering();
        assert_eq!(state.state, UpgradePhase::Recovering);
    }
}
