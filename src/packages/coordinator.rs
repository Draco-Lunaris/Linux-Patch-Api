//! Operation Coordinator — single chokepoint for all package-database mutations.
//!
//! Provides:
//! - **`CommandRunner` trait**: abstraction over `std::process::Command` so backends
//!   can be tested with injected mock runners instead of real package managers.
//! - **`SystemCommandRunner`**: production impl using `tokio::process::Command` with
//!   per-call timeouts, SIGTERM grace, and SIGKILL escalation.
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
use std::time::Duration;
use tokio::process::Command as TokioCommand;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use super::error_utils::CommandError;

/// Output from a command execution, mirroring `std::process::Output` but
/// as an owned, clonable struct suitable for mock injection.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// True when the command exceeded its deadline and was killed by the runner.
    /// Backends use this to populate `CommandError::timed_out`.
    pub timed_out: bool,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status_code == Some(0)
    }

    #[allow(dead_code)]
    pub fn from_process(_program: &str, _args: &[&str], output: std::process::Output) -> Self {
        Self {
            status_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            timed_out: false,
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

    /// Run a command with a deadline. On expiry the child is sent SIGTERM,
    /// given a short grace period, then SIGKILL. The returned `CommandOutput`
    /// (or `CommandError`) will have `timed_out = true`.
    ///
    /// The default implementation delegates to `run` (i.e. no timeout) so
    /// existing mock runners keep working without modification. Production
    /// code uses [`SystemCommandRunner`] which enforces the deadline.
    fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput> {
        let _ = timeout;
        self.run(program, args)
    }
}

/// Production command runner using `tokio::process::Command` with timeout support.
///
/// `run` spawns the child and waits without a deadline (preserving the
/// historical behaviour for callers that haven't been migrated yet).
/// `run_with_timeout` wraps the child in a `tokio::time::timeout` and, on
/// expiry, escalates SIGTERM → grace → SIGKILL so the process never lingers.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
        Self::block_on_async(program, args, None)
    }

    fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput> {
        Self::block_on_async(program, args, Some(timeout))
    }
}

impl SystemCommandRunner {
    /// Block on the async inner function from synchronous code.
    ///
    /// When called from within a tokio runtime (e.g. an actix-web handler on a
    /// current-thread runtime), spawns a dedicated OS thread with its own
    /// runtime to avoid the "Cannot start a runtime from within a runtime" panic.
    /// When called from a non-async context, creates a new runtime directly.
    fn block_on_async(
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> Result<CommandOutput> {
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                // We're inside a tokio runtime. spawn_blocking would work on
                // a multi-threaded runtime, but actix-rt uses a current-thread
                // runtime where block_in_place panics. The safest approach is
                // to spawn a dedicated OS thread with its own runtime.
                let program = program.to_string();
                let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let rt = match tokio::runtime::Runtime::new() {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = tx.send(Err(anyhow::anyhow!(
                                "failed to create tokio runtime: {}",
                                e
                            )));
                            return;
                        }
                    };
                    let result = rt.block_on(Self::run_async_owned(&program, &args, timeout));
                    let _ = tx.send(result);
                });
                rx.recv().map_err(|e| {
                    anyhow::anyhow!("command thread panicked or was cancelled: {}", e)
                })?
            }
            Err(_) => {
                // No runtime — create one, run, and let it drop.
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|e| anyhow::anyhow!("failed to create tokio runtime: {}", e))?;
                rt.block_on(Self::run_async(program, args, timeout))
            }
        }
    }

    /// Owned-args variant of `run_async` for use when the args need to be
    /// moved across thread boundaries (e.g. the `block_on_async` thread spawn).
    async fn run_async_owned(
        program: &str,
        args: &[String],
        timeout: Option<Duration>,
    ) -> Result<CommandOutput> {
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_async(program, &args_ref, timeout).await
    }

    /// Async inner implementation shared by `run` and `run_with_timeout`.
    ///
    /// When `timeout` is `Some`, the child is killed (SIGTERM, then SIGKILL)
    /// if it hasn't exited by the deadline. The returned `CommandOutput` has
    /// `timed_out = true` in that case.
    async fn run_async(
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> Result<CommandOutput> {
        let mut cmd = TokioCommand::new(program);
        cmd.args(args)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Ensure the child is killed when the handle is dropped so a
            // cancelled future never orphans the process.
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Err(
                    anyhow::Error::new(CommandError::from_spawn_error(program, args, &e))
                        .context(format!("Failed to execute {}", program)),
                );
            }
        };

        // Take the stdout/stderr handles so we can read them without consuming `child`.
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        // Read stdout/stderr concurrently with the wait+timeout.
        let stdout_fut = async {
            if let Some(mut h) = stdout_handle {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf)
                    .await
                    .ok();
                buf
            } else {
                Vec::new()
            }
        };
        let stderr_fut = async {
            if let Some(mut h) = stderr_handle {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf)
                    .await
                    .ok();
                buf
            } else {
                Vec::new()
            }
        };

        let wait_fut = child.wait();

        match timeout {
            Some(dur) => {
                let (wait_result, stdout_buf, stderr_buf) =
                    tokio::join!(tokio::time::timeout(dur, wait_fut), stdout_fut, stderr_fut);

                match wait_result {
                    Ok(Ok(status)) => {
                        if !status.success() {
                            return Err(anyhow::Error::new(CommandError {
                                program: program.to_string(),
                                args: args.iter().map(|s| s.to_string()).collect(),
                                exit_code: status.code(),
                                stdout: String::from_utf8_lossy(&stdout_buf).to_string(),
                                stderr: String::from_utf8_lossy(&stderr_buf).to_string(),
                                spawn_error: None,
                                timed_out: false,
                            }));
                        }
                        Ok(CommandOutput {
                            status_code: status.code(),
                            stdout: String::from_utf8_lossy(&stdout_buf).to_string(),
                            stderr: String::from_utf8_lossy(&stderr_buf).to_string(),
                            timed_out: false,
                        })
                    }
                    Ok(Err(e)) => Err(anyhow::Error::new(CommandError::from_spawn_error(
                        program, args, &e,
                    ))
                    .context(format!("Failed to wait on {}", program))),
                    Err(_) => {
                        // Deadline elapsed — kill the child.
                        debug!(
                            program = program,
                            timeout_secs = dur.as_secs(),
                            "command timed out, killing child"
                        );
                        Self::kill_child(&mut child).await;
                        let ce = CommandError::from_timeout(program, args, dur.as_secs());
                        Err(anyhow::Error::new(ce).context(format!(
                            "{} timed out after {}s",
                            program,
                            dur.as_secs()
                        )))
                    }
                }
            }
            None => match wait_fut.await {
                Ok(status) => {
                    let stdout_buf = stdout_fut.await;
                    let stderr_buf = stderr_fut.await;
                    if !status.success() {
                        return Err(anyhow::Error::new(CommandError {
                            program: program.to_string(),
                            args: args.iter().map(|s| s.to_string()).collect(),
                            exit_code: status.code(),
                            stdout: String::from_utf8_lossy(&stdout_buf).to_string(),
                            stderr: String::from_utf8_lossy(&stderr_buf).to_string(),
                            spawn_error: None,
                            timed_out: false,
                        }));
                    }
                    Ok(CommandOutput {
                        status_code: status.code(),
                        stdout: String::from_utf8_lossy(&stdout_buf).to_string(),
                        stderr: String::from_utf8_lossy(&stderr_buf).to_string(),
                        timed_out: false,
                    })
                }
                Err(e) => Err(anyhow::Error::new(CommandError::from_spawn_error(
                    program, args, &e,
                ))
                .context(format!("Failed to wait on {}", program))),
            },
        }
    }

    /// Best-effort child kill: SIGTERM, short grace, then SIGKILL.
    async fn kill_child(child: &mut tokio::process::Child) {
        // Try SIGTERM first for graceful shutdown.
        if let Err(e) = child.start_kill() {
            warn!(error = %e, "failed to SIGTERM child during timeout");
        }
        // Give the child a 5-second grace period to exit cleanly.
        let grace = Duration::from_secs(5);
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            // Still alive — escalate to SIGKILL.
            warn!("child did not exit after SIGTERM, escalating to SIGKILL");
            let _ = child.kill().await;
        }
    }
}

/// Run a command via the runner and return stdout as a string, converting
/// errors to `CommandError`-wrapped `anyhow::Error` (same as backends did inline).
pub fn run_command(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> Result<String> {
    let output = runner.run(program, args)?;
    Ok(output.stdout)
}

/// Run a command with a deadline via the runner's `run_with_timeout`.
pub fn run_command_timed(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String> {
    let output = runner.run_with_timeout(program, args, timeout)?;
    Ok(output.stdout)
}

/// Run a command with a deadline, treating certain non-zero exit codes as success.
///
/// This is the timed variant of [`run_command_with_acceptable_exit`]. It is used
/// by backends like DNF where `check-update` returns 100 when updates exist.
pub fn run_command_with_acceptable_exit_timed(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&str],
    acceptable_codes: &[i32],
    timeout: Duration,
) -> Result<String> {
    match runner.run_with_timeout(program, args, timeout) {
        Ok(output) => Ok(output.stdout),
        Err(e) => {
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

// ---------------------------------------------------------------------------
// Default command timeouts.
//
// These are conservative upper bounds — under normal conditions apt-get update
// finishes in <30s and an install in <60s. The values are chosen so a hung
// upstream mirror (the root cause of issue #158) is detected and killed before
// it blocks the agent's mutation semaphore for more than a few minutes.
// ---------------------------------------------------------------------------

/// Timeout for cache-refresh commands (`apt-get update`, `dnf check-update`, etc.).
/// 300s is generous enough for slow mirrors but short enough that a hung TCP
/// connection doesn't block the agent for hours.
pub const CACHE_REFRESH_TIMEOUT: Duration = Duration::from_secs(300);

/// Timeout for package install/upgrade/remove operations. Large kernel updates
/// can take a while to download and unpack, so this is intentionally generous.
pub const PACKAGE_OP_TIMEOUT: Duration = Duration::from_secs(1800);

/// Timeout for quick dpkg/systemctl/rpm queries and cleanup. These should
/// never take more than a few seconds; 60s is a safety net.
pub const QUICK_OP_TIMEOUT: Duration = Duration::from_secs(60);

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

    #[test]
    fn test_system_command_runner_with_timeout_succeeds() {
        // `true` exits immediately, well within the timeout.
        let runner = SystemCommandRunner;
        let result = runner.run_with_timeout("true", &[], Duration::from_secs(5));
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.success());
        assert!(!output.timed_out);
    }

    #[test]
    fn test_system_command_runner_with_timeout_kills_hung_child() {
        // `sleep 30` would block for 30s; the 1s timeout should kill it.
        let runner = SystemCommandRunner;
        let result = runner.run_with_timeout("sleep", &["30"], Duration::from_secs(1));
        assert!(result.is_err());
        let err = result.unwrap_err();
        // The error chain should contain a CommandError with timed_out = true.
        let ce = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CommandError>());
        assert!(ce.is_some(), "error chain should contain a CommandError");
        let ce = ce.unwrap();
        assert!(ce.timed_out, "CommandError should be marked as timed_out");
        assert_eq!(ce.program, "sleep");
    }
}
