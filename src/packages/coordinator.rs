//! Command Runner — abstraction over process execution for package backends.
//!
//! Provides:
//! - **`CommandRunner` trait**: abstraction over `std::process::Command` so backends
//!   can be tested with injected mock runners instead of real package managers.
//! - **`SystemCommandRunner`**: production impl using `tokio::process::Command` with
//!   per-call timeouts, SIGTERM grace, and SIGKILL escalation.
//! - **Timeout constants**: `CACHE_REFRESH_TIMEOUT`, `PACKAGE_OP_TIMEOUT`,
//!   `QUICK_OP_TIMEOUT` — conservative upper bounds for package operations.

use anyhow::Result;
use std::os::unix::process::CommandExt;
use std::time::Duration;
use tokio::process::Command as TokioCommand;
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
            .stderr(std::process::Stdio::piped());

        // Isolate package-manager processes into their own process group.
        //
        // `process_group(0)` calls `setpgid(0, 0)` in the child before exec,
        // putting apt-get/dnf/apk/pacman (and their children — dpkg, rpm, etc.)
        // into a new process group outside the agent's service cgroup.
        //
        // This is critical for self-updates: when the package's postinst calls
        // `systemctl restart` (or the OpenRC equivalent), the init system sends
        // SIGTERM/SIGKILL to the agent's cgroup. Without process-group isolation,
        // this kills apt-get/dpkg mid-transaction — leaving the package
        // half-installed and the self-update job marked as failed.
        //
        // With isolation, the package-manager process survives the agent restart
        // and completes the upgrade. The new agent binary detects the completed
        // upgrade on startup.
        //
        // We intentionally do NOT use `kill_on_drop(true)`: if the agent is
        // stopped/restarted while a package operation is in flight, the child
        // handle is dropped, and `kill_on_drop` would SIGKILL the very process
        // we're trying to protect. Package-manager processes must be allowed to
        // run to completion independently of the agent's lifecycle.
        cmd.as_std_mut().process_group(0);

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
        // The child is in its own process group (process_group(0) was set
        // before spawn). To kill the entire group — the child plus any
        // subprocesses it spawned (dpkg, rpm, postinst hooks, etc.) — we
        // send signals to the negative PID (the process group ID).
        let pgid = child.id().map(|p| p as i32);

        // Try SIGTERM first for graceful shutdown of the whole group.
        if let Some(pgid) = pgid {
            let ret = unsafe { libc::kill(-pgid, libc::SIGTERM) };
            if ret != 0 {
                warn!(
                    error = ret,
                    "failed to SIGTERM process group during timeout"
                );
            }
        } else if let Err(e) = child.start_kill() {
            warn!(error = %e, "failed to SIGTERM child during timeout");
        }

        // Give the child a 5-second grace period to exit cleanly.
        let grace = Duration::from_secs(5);
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            // Still alive — escalate to SIGKILL the whole group.
            warn!("child did not exit after SIGTERM, escalating to SIGKILL");
            if let Some(pgid) = pgid {
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let runner = SystemCommandRunner;
        let result = runner.run_with_timeout("true", &[], Duration::from_secs(5));
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.success());
        assert!(!output.timed_out);
    }

    #[test]
    fn test_system_command_runner_with_timeout_kills_hung_child() {
        let runner = SystemCommandRunner;
        let result = runner.run_with_timeout("sleep", &["30"], Duration::from_secs(1));
        assert!(result.is_err());
        let err = result.unwrap_err();
        let ce = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CommandError>());
        assert!(ce.is_some(), "error chain should contain a CommandError");
        let ce = ce.unwrap();
        assert!(ce.timed_out, "CommandError should be marked as timed_out");
        assert_eq!(ce.program, "sleep");
    }
}
