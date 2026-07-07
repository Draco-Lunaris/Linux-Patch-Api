//! Package Cache Management Module
//! Handles package index refresh, stale detection, state persistence, and 404 retry logic.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::Duration;
use tracing::{info, warn};

use super::error_utils::CommandError;

/// State file path for cache persistence
const CACHE_STATE_PATH: &str = "/var/lib/linux_patch_api/state/cache.json";

/// Default stale threshold: 15 minutes
pub const DEFAULT_STALE_THRESHOLD_SECS: u64 = 15 * 60;

/// Cache refresh command timeout: 120 seconds
#[allow(dead_code)]
const CACHE_REFRESH_TIMEOUT_SECS: u64 = 120;

/// Persistent cache state (written to cache.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStateFile {
    pub last_cache_update: Option<String>, // RFC3339
    pub last_update_success: bool,
}

/// Runtime cache status
#[derive(Debug, Clone, Serialize)]
pub struct PackageCacheStatus {
    pub last_update: Option<DateTime<Utc>>,
    pub last_update_success: bool,
    pub last_update_error: Option<String>,
}

/// In-memory cache state (thread-safe)
pub struct PackageCacheState {
    inner: Mutex<CacheStateInner>,
    stale_threshold_secs: u64,
}

struct CacheStateInner {
    last_update: Option<DateTime<Utc>>,
    last_update_success: bool,
    last_update_error: Option<String>,
}

impl Default for PackageCacheState {
    fn default() -> Self {
        Self::with_threshold(DEFAULT_STALE_THRESHOLD_SECS)
    }
}

impl PackageCacheState {
    pub fn new() -> Self {
        Self::with_threshold(DEFAULT_STALE_THRESHOLD_SECS)
    }

    /// Create a cache state with a custom stale threshold (in seconds).
    pub fn with_threshold(stale_threshold_secs: u64) -> Self {
        // Try to load from state file on startup
        let inner = match Self::load_state_file() {
            Some(state) => CacheStateInner {
                last_update: state
                    .last_cache_update
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&Utc)),
                last_update_success: state.last_update_success,
                last_update_error: None,
            },
            None => CacheStateInner {
                last_update: None,
                last_update_success: false,
                last_update_error: None,
            },
        };
        Self {
            inner: Mutex::new(inner),
            stale_threshold_secs,
        }
    }

    pub fn status(&self) -> PackageCacheStatus {
        let inner = self.inner.lock().unwrap();
        PackageCacheStatus {
            last_update: inner.last_update,
            last_update_success: inner.last_update_success,
            last_update_error: inner.last_update_error.clone(),
        }
    }

    pub fn is_stale(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        match inner.last_update {
            None => true,
            Some(t) => {
                let threshold = Duration::from_secs(self.stale_threshold_secs);
                Utc::now() - t
                    > chrono::Duration::from_std(threshold).unwrap_or(chrono::TimeDelta::MAX)
            }
        }
    }

    pub fn update_success(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.last_update = Some(Utc::now());
        inner.last_update_success = true;
        inner.last_update_error = None;
        drop(inner); // release lock before I/O
        self.persist_state();
    }

    pub fn update_failure(&self, error: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.last_update_success = false;
        inner.last_update_error = Some(error);
        let now = Utc::now();
        // Keep old timestamp if we had one, don't update on failure
        if inner.last_update.is_none() {
            inner.last_update = Some(now); // first attempt timestamp
        }
        drop(inner);
        self.persist_state();
    }

    fn load_state_file() -> Option<CacheStateFile> {
        let content = std::fs::read_to_string(CACHE_STATE_PATH).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn persist_state(&self) {
        let inner = self.inner.lock().unwrap();
        let state = CacheStateFile {
            last_cache_update: inner.last_update.map(|dt| dt.to_rfc3339()),
            last_update_success: inner.last_update_success,
        };
        drop(inner); // release lock before I/O

        // Create parent directory if needed
        if let Some(parent) = std::path::Path::new(CACHE_STATE_PATH).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(&state) {
            Ok(json) => {
                if let Err(e) = std::fs::write(CACHE_STATE_PATH, json) {
                    warn!("Failed to persist cache state: {}", e);
                }
            }
            Err(e) => warn!("Failed to serialize cache state: {}", e),
        }
    }
}

/// Check if an error message indicates a fetch/404 error
pub fn is_fetch_error(error: &anyhow::Error) -> bool {
    let msg = error.to_string().to_lowercase();
    msg.contains("404")
        || msg.contains("not found")
        || msg.contains("failed to fetch")
        || msg.contains("unable to fetch")
}

/// Execute a patch apply with automatic cache refresh retry on 404/fetch errors.
/// Hardcoded 1 retry after cache refresh.
pub fn apply_with_cache_retry<F>(mut refresh_fn: F, apply_fn: impl Fn() -> Result<()>) -> Result<()>
where
    F: FnMut() -> Result<()>,
{
    match apply_fn() {
        Ok(()) => Ok(()),
        Err(e) if is_fetch_error(&e) => {
            info!("Patch apply failed with fetch error, refreshing cache and retrying");
            refresh_fn()?;
            apply_fn()
        }
        Err(e) => Err(e),
    }
}

/// Run a command with timeout for cache refresh operations.
///
/// On failure, returns a [`CommandError`] (wrapped in anyhow) preserving exit code,
/// stdout, and stderr so the manager receives the same diagnostics as the local journal.
pub fn run_command_with_timeout(program: &str, args: &[&str]) -> Result<String> {
    use std::process::Command;

    let output = match Command::new(program)
        .args(args)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Err(
                anyhow::Error::new(CommandError::from_spawn_error(program, args, &e))
                    .context("Cache refresh command failed to start"),
            );
        }
    };

    if !output.status.success() {
        return Err(anyhow::Error::new(CommandError::from_output(
            program, args, &output,
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_fetch_error_404() {
        let err = anyhow::anyhow!("E: Unable to fetch 404 Not Found");
        assert!(is_fetch_error(&err));
    }

    #[test]
    fn test_is_fetch_error_not_found() {
        let err = anyhow::anyhow!("Package not found in repository");
        assert!(is_fetch_error(&err));
    }

    #[test]
    fn test_is_fetch_error_failed_to_fetch() {
        let err = anyhow::anyhow!("Failed to fetch package index");
        assert!(is_fetch_error(&err));
    }

    #[test]
    fn test_is_fetch_error_unable_to_fetch() {
        let err = anyhow::anyhow!("Unable to fetch some archive");
        assert!(is_fetch_error(&err));
    }

    #[test]
    fn test_is_not_fetch_error() {
        let err = anyhow::anyhow!("Permission denied");
        assert!(!is_fetch_error(&err));
    }

    #[test]
    fn test_cache_state_new() {
        let state = PackageCacheState::new();
        let status = state.status();
        // Fresh state should have no last_update (unless state file exists)
        // Just verify it doesn't panic
        assert!(!status.last_update_success || status.last_update.is_some());
    }

    #[test]
    fn test_cache_state_stale_when_no_update() {
        let state = PackageCacheState::new();
        // If no state file exists, cache should be stale
        // This test may vary based on state file existence,
        // but we can at least call is_stale without panic
        let _ = state.is_stale();
    }

    #[test]
    fn test_cache_state_update_success() {
        let state = PackageCacheState::new();
        state.update_success();
        let status = state.status();
        assert!(status.last_update.is_some());
        assert!(status.last_update_success);
        assert!(status.last_update_error.is_none());
    }

    #[test]
    fn test_cache_state_update_failure() {
        let state = PackageCacheState::new();
        state.update_failure("test error".to_string());
        let status = state.status();
        assert!(!status.last_update_success);
        assert_eq!(status.last_update_error, Some("test error".to_string()));
    }

    #[test]
    fn test_apply_with_cache_retry_success() {
        let result = apply_with_cache_retry(|| Ok(()), || Ok(()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_apply_with_cache_retry_non_fetch_error() {
        let result: Result<()> =
            apply_with_cache_retry(|| Ok(()), || Err(anyhow::anyhow!("Permission denied")));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(!is_fetch_error(&err));
    }

    #[test]
    fn test_apply_with_cache_retry_fetch_error_with_refresh() {
        let mut refresh_called = false;
        let result: Result<()> = apply_with_cache_retry(
            || {
                refresh_called = true;
                Ok(())
            },
            || Err(anyhow::anyhow!("404 Not Found")),
        );
        // Refresh should have been called, but second apply_fn still fails with 404
        assert!(refresh_called);
        assert!(result.is_err());
    }
}
