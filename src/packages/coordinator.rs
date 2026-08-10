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
        // This is critical whenever the agent's own package is being upgraded
        // (explicit self-update OR patch_apply via dist-upgrade): the
        // package's postinst calls `systemctl restart` (or the OpenRC
        // equivalent), and the init system sends SIGTERM/SIGKILL to the
        // agent's cgroup. Without process-group isolation, this kills
        // apt-get/dpkg mid-transaction — leaving the package half-installed
        // and the job marked as failed.
        //
        // With isolation, the package-manager process survives the agent
        // restart and completes the upgrade. The new agent binary starts
        // fresh and detects the completed upgrade on startup.
        //
        // We intentionally do NOT use `kill_on_drop(true)`: if the agent is
        // stopped/restarted while a package operation is in flight, the child
        // handle is dropped, and `kill_on_drop` would SIGKILL the very process
        // we're trying to protect. Package-manager processes must be allowed
        // to run to completion independently of the agent's lifecycle.
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

        // Spawn the stdout/stderr reads as independent drain tasks.
        //
        // Two reasons:
        // (1) Prevent the OS pipe-buffer deadlock. A child that writes more
        //     than ~64KB before exiting fills the pipe, blocks on its next
        //     write, never exits, and `child.wait()` never resolves. Draining
        //     concurrently keeps the pipe clear.
        // (2) Let a *timed-out* child's pipes close promptly. The drain tasks
        //     reach EOF once the child (and any subprocesses it spawned) has
        //     been killed. We collect the drained buffers AFTER killing, so
        //     the deadline actually bounds the wall-clock time the runner
        //     blocks — see the timeout arm below.
        //
        // The previous implementation used `tokio::join!(timeout(wait),
        // stdout_fut, stderr_fut)`. `join!` waits for ALL three futures and
        // does not short-circuit when the timeout resolves, so a child that
        // held its pipes open (e.g. `apt-get update` stuck on `gpgv` against a
        // dead network connection) kept `join!` waiting for the pipe reads
        // forever — `kill_child` never ran and the deadline was meaningless.
        // That was the root cause of agents hanging until manual restart.
        let stdout_task = child.stdout.take().map(|mut h| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf)
                    .await
                    .ok();
                buf
            })
        });
        let stderr_task = child.stderr.take().map(|mut h| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf)
                    .await
                    .ok();
                buf
            })
        });

        // Wait for the child, bounded by the deadline. On timeout, kill the
        // process group FIRST (closing the pipes so the drain tasks complete),
        // then reap and collect. Collection is itself bounded — if the kill
        // signal somehow doesn't reach the child (e.g. systemd cgroup
        // isolation) we still return rather than block forever.
        match timeout {
            Some(dur) => match tokio::time::timeout(dur, child.wait()).await {
                Ok(Ok(status)) => {
                    let stdout_str = Self::collect_drain(stdout_task, Duration::from_secs(5)).await;
                    let stderr_str = Self::collect_drain(stderr_task, Duration::from_secs(5)).await;
                    if !status.success() {
                        return Err(anyhow::Error::new(CommandError {
                            program: program.to_string(),
                            args: args.iter().map(|s| s.to_string()).collect(),
                            exit_code: status.code(),
                            stdout: stdout_str,
                            stderr: stderr_str,
                            spawn_error: None,
                            timed_out: false,
                        }));
                    }
                    Ok(CommandOutput {
                        status_code: status.code(),
                        stdout: stdout_str,
                        stderr: stderr_str,
                        timed_out: false,
                    })
                }
                Ok(Err(e)) => Err(anyhow::Error::new(CommandError::from_spawn_error(
                    program, args, &e,
                ))
                .context(format!("Failed to wait on {}", program))),
                Err(_) => {
                    debug!(
                        program = program,
                        timeout_secs = dur.as_secs(),
                        "command timed out, killing child"
                    );
                    // Kill the process group FIRST (closing the pipes so the
                    // drain tasks can complete), then reap.
                    let killed = Self::kill_child(&mut child).await;
                    let reaped_code = match killed {
                        Some(s) => s.code(),
                        None => {
                            // kill_child SIGKILLed the group but didn't reap
                            // during its grace window. Reap now, but bound it:
                            // if the kill signal somehow didn't reach the
                            // child (e.g. systemd cgroup isolation) we must
                            // still return rather than block forever. The OS
                            // will reap any orphaned child on its own.
                            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                                Ok(Ok(s)) => s.code(),
                                _ => {
                                    warn!(
                                        program = program,
                                        "child did not reap after timeout kill; \
                                         abandoning (OS will reap the orphan)"
                                    );
                                    None
                                }
                            }
                        }
                    };
                    // Collect whatever the child printed before the kill
                    // (bounded — the pipes close once the group is dead).
                    let stdout_str = Self::collect_drain(stdout_task, Duration::from_secs(2)).await;
                    let stderr_str = Self::collect_drain(stderr_task, Duration::from_secs(2)).await;
                    let ce = CommandError {
                        program: program.to_string(),
                        args: args.iter().map(|s| s.to_string()).collect(),
                        exit_code: reaped_code,
                        stdout: stdout_str,
                        stderr: if stderr_str.is_empty() {
                            format!("{} timed out after {}s", program, dur.as_secs())
                        } else {
                            stderr_str
                        },
                        spawn_error: None,
                        timed_out: true,
                    };
                    Err(anyhow::Error::new(ce).context(format!(
                        "{} timed out after {}s",
                        program,
                        dur.as_secs()
                    )))
                }
            },
            None => match child.wait().await {
                Ok(status) => {
                    let stdout_str = Self::collect_drain(stdout_task, Duration::from_secs(5)).await;
                    let stderr_str = Self::collect_drain(stderr_task, Duration::from_secs(5)).await;
                    if !status.success() {
                        return Err(anyhow::Error::new(CommandError {
                            program: program.to_string(),
                            args: args.iter().map(|s| s.to_string()).collect(),
                            exit_code: status.code(),
                            stdout: stdout_str,
                            stderr: stderr_str,
                            spawn_error: None,
                            timed_out: false,
                        }));
                    }
                    Ok(CommandOutput {
                        status_code: status.code(),
                        stdout: stdout_str,
                        stderr: stderr_str,
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

    /// Collect a drained pipe buffer, bounded so a kill that didn't reach the
    /// child can't pin the runner forever. Returns whatever was drained
    /// (possibly empty) when the drain task finishes within `bound`, lossy
    /// converted to a string (matching the rest of the runner).
    async fn collect_drain(
        task: Option<tokio::task::JoinHandle<Vec<u8>>>,
        bound: Duration,
    ) -> String {
        let buf = match task {
            Some(t) => match tokio::time::timeout(bound, t).await {
                Ok(Ok(buf)) => buf,
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        String::from_utf8_lossy(&buf).to_string()
    }

    /// Best-effort child kill: SIGTERM the whole process group, short grace,
    /// then SIGKILL. Returns the child's exit status if it was reaped during
    /// the SIGTERM grace window; otherwise the caller must reap the (now
    /// SIGKILLed) child itself.
    ///
    /// The child is in its own process group (process_group(0) was set before
    /// spawn). To kill the entire group — the child plus any subprocesses it
    /// spawned (dpkg, rpm, postinst hooks, etc.) — signals target the negative
    /// PID (the process-group ID). SIGTERM-first lets package managers clean
    /// up (e.g. dpkg finishing its state write); a direct SIGKILL risks leaving
    /// dpkg half-configured.
    async fn kill_child(child: &mut tokio::process::Child) -> Option<std::process::ExitStatus> {
        let pgid = child.id().map(|p| p as i32);

        // SIGTERM the whole group.
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

        // Wait up to 5s for a graceful exit. If the child reaps here, hand its
        // status back so the caller doesn't need to wait again.
        let grace = Duration::from_secs(5);
        match tokio::time::timeout(grace, child.wait()).await {
            Ok(Ok(status)) => Some(status),
            Ok(Err(_)) => None,
            Err(_) => {
                // Still alive after SIGTERM — escalate to SIGKILL the group.
                // The caller reaps the now-dead child.
                warn!("child did not exit after SIGTERM, escalating to SIGKILL");
                if let Some(pgid) = pgid {
                    unsafe { libc::kill(-pgid, libc::SIGKILL) };
                }
                None
            }
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

    #[test]
    fn test_system_command_runner_timeout_bounded_in_wallclock() {
        // A child that holds its stdout pipe open without writing (a stand-in
        // for `apt-get update` stuck on `gpgv` against a dead connection) must
        // be killed near the deadline — NOT held until its natural exit. The
        // previous `tokio::join!` of wait+stdout+stderr waited for the pipe
        // read to EOF, so this child pinned the runner for its full lifetime
        // and the deadline was meaningless. This test pins wall-clock time.
        let runner = SystemCommandRunner;
        let start = std::time::Instant::now();
        let result = runner.run_with_timeout("sleep", &["30"], Duration::from_secs(1));
        let elapsed = start.elapsed();
        assert!(result.is_err(), "should error on timeout");
        let err = result.unwrap_err();
        let ce = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CommandError>())
            .expect("error chain should contain a CommandError");
        assert!(ce.timed_out, "should be marked timed_out");
        assert_eq!(ce.program, "sleep");
        // deadline 1s + SIGTERM grace 5s + reap/collect bound ~5s ≈ 11s worst
        // case. Must be well under sleep's 30s natural exit — that is the
        // whole point: the timeout now actually bounds wall-clock time.
        assert!(
            elapsed.as_secs() < 20,
            "timeout should bound wall-clock time, took {:?}",
            elapsed
        );
    }
}
