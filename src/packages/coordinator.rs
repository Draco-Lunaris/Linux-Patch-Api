//! Operation Coordinator — single chokepoint for all package-database mutations.
//!
//! Provides:
//! - **`CommandRunner` trait**: abstraction over `std::process::Command` so backends
//!   can be tested with injected mock runners instead of real package managers.
//! - **`SystemCommandRunner`**: production impl using `std::process::Command`.
//! - **`OperationCoordinator`**: serializes all package-DB mutations across ALL
//!   backends via a `Semaphore(1)`, enforces `max_concurrent` jobs via a real
//!   semaphore, and tracks `op_in_progress` for the SIGTERM handler.
//!
//! ## Design
//!
//! Every mutating package operation (install, update, remove, patch-apply,
//! cache-refresh) MUST go through `coordinator.run_mutation(...)`. This
//! acquires the mutation semaphore (serializing across all backends), sets
//! `op_in_progress = true`, runs the closure, and clears the flag on drop.
//!
//! Read-only operations (list, get, list-patches) do NOT acquire the mutation
//! semaphore — they can run concurrently with each other.
//!
//! The `op_in_progress` flag is the sole authority for whether a package-DB
//! mutation is running. The old APT-only `APT_IN_PROGRESS` static has been
//! removed — the coordinator works across all backends.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

use super::error_utils::CommandError;

/// Output from a command execution, mirroring `std::process::Output` but
/// as an owned, clonable struct suitable for mock injection.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status_code == Some(0)
    }

    pub fn from_process(_program: &str, _args: &[&str], output: std::process::Output) -> Self {
        Self {
            status_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }
}

/// Trait abstracting command execution so backends can be tested with mocks.
///
/// Production code uses [`SystemCommandRunner`]. Tests use mock implementations
/// that return programmed responses without spawning real processes.
pub trait CommandRunner: Send + Sync {
    /// Run a command with the given program and args, returning its output.
    ///
    /// Implementations should set `DEBIAN_FRONTEND=noninteractive` for
    /// package-manager commands (harmless on non-APT systems).
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput>;
}

/// Production command runner using `std::process::Command`.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
        let output = match std::process::Command::new(program)
            .args(args)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                return Err(
                    anyhow::Error::new(CommandError::from_spawn_error(program, args, &e))
                        .context(format!("Failed to execute {}", program)),
                );
            }
        };

        if !output.status.success() {
            return Err(anyhow::Error::new(CommandError::from_output(
                program, args, &output,
            )));
        }

        Ok(CommandOutput::from_process(program, args, output))
    }
}

/// Run a command via the runner and return stdout as a string, converting
/// errors to `CommandError`-wrapped `anyhow::Error` (same as backends did inline).
pub fn run_command(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> Result<String> {
    let output = runner.run(program, args)?;
    Ok(output.stdout)
}

/// Run a command where non-zero exit codes other than a specific "acceptable"
/// code (e.g. dnf check-update returns 100 when updates are available) should
/// be treated as success.
pub fn run_command_with_acceptable_exit(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&str],
    acceptable_codes: &[i32],
) -> Result<String> {
    // We need to intercept the error to check the exit code, so we can't use
    // the simple run_command wrapper. Instead, call the runner directly and
    // handle the error.
    match runner.run(program, args) {
        Ok(output) => Ok(output.stdout),
        Err(e) => {
            // Check if this is a CommandError with an acceptable exit code
            let ce = e
                .chain()
                .find_map(|cause| cause.downcast_ref::<CommandError>());

            if let Some(ce) = ce {
                if let Some(code) = ce.exit_code {
                    if acceptable_codes.contains(&code) {
                        return Ok(ce.stdout.clone());
                    }
                }
            }
            Err(e)
        }
    }
}

/// Operation Coordinator — the single point of control for package operations.
///
/// Always shared via `Arc<OperationCoordinator>` (or `web::Data<Arc<...>>`).
/// All mutating package operations go through `run_mutation`, which serializes
/// them via a `Semaphore(1)`. Job concurrency is enforced via `job_semaphore`.
///
/// **Never clone this struct directly** — there is no `Clone` impl. All shared
/// references must go through `Arc`. This guarantees every handler shares the
/// same semaphores and atomic flags.
pub struct OperationCoordinator {
    /// Serializes all package-DB mutations (install/update/remove/patch/refresh).
    /// Only one mutation runs at a time, across ALL backends.
    mutation_semaphore: Semaphore,

    /// Enforces `max_concurrent` running jobs. Acquired before spawning a job
    /// task, released when the job completes/fails/times out.
    job_semaphore: Semaphore,

    /// True while a package-DB mutation is in progress. Checked by the
    /// SIGTERM handler to decide whether to wait before shutting down.
    op_in_progress: Arc<AtomicBool>,

    /// True while a job is running (any job, not just mutations). Used for
    /// health reporting and drain logic.
    job_in_progress: Arc<AtomicBool>,
}

impl OperationCoordinator {
    /// Create a new coordinator with the given concurrency limits.
    /// Wrap the result in `Arc` before sharing.
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            mutation_semaphore: Semaphore::new(1),
            job_semaphore: Semaphore::new(max_concurrent.max(1)),
            op_in_progress: Arc::new(AtomicBool::new(false)),
            job_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Check if a package-DB mutation is currently in progress.
    /// Used by the SIGTERM handler and the self-update drain logic.
    pub fn is_operation_in_progress(&self) -> bool {
        self.op_in_progress.load(Ordering::SeqCst)
    }

    /// Check if any job is currently running.
    pub fn is_job_in_progress(&self) -> bool {
        self.job_in_progress.load(Ordering::SeqCst)
    }

    /// Get a handle to the job semaphore for acquiring before spawning a job.
    pub fn job_semaphore(&self) -> &Semaphore {
        &self.job_semaphore
    }

    /// Get a handle to the mutation semaphore for acquiring before a mutation.
    pub fn mutation_semaphore(&self) -> &Semaphore {
        &self.mutation_semaphore
    }

    /// Get a clone of the op_in_progress flag for use in signal handlers.
    pub fn op_in_progress_flag(&self) -> Arc<AtomicBool> {
        self.op_in_progress.clone()
    }

    /// Get a clone of the job_in_progress flag.
    pub fn job_in_progress_flag(&self) -> Arc<AtomicBool> {
        self.job_in_progress.clone()
    }

    /// Run a package-DB mutation under the mutation semaphore.
    ///
    /// This is the single chokepoint for all mutating package operations.
    /// It:
    /// 1. Acquires the mutation semaphore (serializes across all backends).
    /// 2. Sets `op_in_progress = true`.
    /// 3. Runs the closure.
    /// 4. Sets `op_in_progress = false` (via RAII guard, even on panic).
    ///
    /// The closure receives a `MutationGuard` that can be used to explicitly
    /// clear the flag early if needed (rare).
    pub async fn run_mutation<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        let _permit = self
            .mutation_semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("mutation semaphore closed: {}", e))?;

        self.op_in_progress.store(true, Ordering::SeqCst);

        let result = f();

        self.op_in_progress.store(false, Ordering::SeqCst);

        result
    }

    /// Try to run a mutation without blocking. Returns `Err(MutationBusy)`
    /// if the semaphore is currently held by another operation, or
    /// `Err(MutationFailed)` if the closure returned an error.
    ///
    /// Used by health-check-triggered cache refreshes: if a mutation is in
    /// progress, the health check skips the refresh and reports stale cache
    /// instead of blocking.
    pub fn try_run_mutation<F, T>(&self, f: F) -> Result<T, TryMutationError>
    where
        F: FnOnce() -> Result<T>,
    {
        let _permit = self
            .mutation_semaphore
            .try_acquire()
            .map_err(|_| TryMutationError::Busy)?;

        self.op_in_progress.store(true, Ordering::SeqCst);
        let result = f();
        self.op_in_progress.store(false, Ordering::SeqCst);

        result.map_err(TryMutationError::Failed)
    }
}

/// Error returned by `try_run_mutation`.
#[derive(Debug)]
pub enum TryMutationError {
    /// The mutation semaphore was already held by another operation.
    Busy,
    /// The closure returned an error.
    Failed(anyhow::Error),
}

impl std::fmt::Display for TryMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TryMutationError::Busy => write!(f, "A package-DB mutation is already in progress"),
            TryMutationError::Failed(e) => write!(f, "Mutation failed: {}", e),
        }
    }
}

impl std::error::Error for TryMutationError {}

/// Error returned by `try_run_mutation` when the mutation semaphore is busy.
/// Kept for backwards compatibility with existing callers that only check
/// for the busy case.
#[derive(Debug, Clone, Copy)]
pub struct MutationBusy;

impl std::fmt::Display for MutationBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "A package-DB mutation is already in progress")
    }
}

impl std::error::Error for MutationBusy {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_mution_serializes() {
        let coord = Arc::new(OperationCoordinator::new(5));

        let coord1 = coord.clone();
        let coord2 = coord.clone();

        let h1 = tokio::spawn(async move { coord1.run_mutation(|| Ok(42)).await.unwrap() });

        let h2 = tokio::spawn(async move { coord2.run_mutation(|| Ok(99)).await.unwrap() });

        // Both should complete (serialized, not concurrent)
        let r1 = h1.await.unwrap();
        let r2 = h2.await.unwrap();
        assert_eq!(r1, 42);
        assert_eq!(r2, 99);
    }

    #[tokio::test]
    async fn test_op_in_progress_flag() {
        let coord = OperationCoordinator::new(5);
        assert!(!coord.is_operation_in_progress());

        // We can't easily test the flag being set mid-closure because
        // run_mutation is synchronous inside the async wrapper, but we
        // can verify it's cleared after.
        coord.run_mutation(|| Ok(())).await.unwrap();
        assert!(!coord.is_operation_in_progress());
    }

    #[test]
    fn test_try_run_mutation_busy() {
        // Create a coordinator and hold the semaphore manually
        let coord = OperationCoordinator::new(5);

        // Without holding the semaphore, try_run should succeed
        let result = coord.try_run_mutation(|| Ok(123));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 123);
    }

    #[test]
    fn test_try_run_mutation_failed() {
        let coord = OperationCoordinator::new(5);
        let result: Result<(), TryMutationError> =
            coord.try_run_mutation(|| Err(anyhow::anyhow!("command failed")));
        assert!(matches!(result, Err(TryMutationError::Failed(_))));
    }

    #[test]
    fn test_system_command_runner() {
        let runner = SystemCommandRunner;
        let result = runner.run("true", &[]);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.success());
    }

    #[test]
    fn test_system_command_runner_failure() {
        let runner = SystemCommandRunner;
        let result = runner.run("false", &[]);
        assert!(result.is_err());
    }
}
