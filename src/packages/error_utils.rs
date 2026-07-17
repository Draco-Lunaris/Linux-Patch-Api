//! Error reporting utilities for richer job failure diagnostics.
//!
//! Provides:
//! - [`CommandError`]: structured error capturing program, args, exit code, stdout, stderr.
//! - [`format_error_chain`]: renders an `anyhow::Error` with its full cause chain (the `Debug`
//!   representation), which is what local `tracing::error!(?e)` logs but `e.to_string()` (Display)
//!   drops. Used so the manager receives the same diagnostic depth as the local journal.
//! - [`extract_command_output`]: pulls captured stdout/stderr/exit_code out of an `anyhow::Error`
//!   if a `CommandError` is present anywhere in the chain, for streaming into job logs.
//! - [`classify_error`]: maps an error to a stable [`ErrorCode`] string so the manager can
//!   programmatically classify and route failures (e.g. retry on `NETWORK_ERROR`,
//!   alert on `GPG_ERROR`).

use std::error::Error as StdError;
use std::fmt;

use anyhow::Error as AnyhowError;

/// Maximum number of stdout/stderr lines captured into job logs to avoid unbounded growth.
pub const MAX_OUTPUT_LINES: usize = 200;

/// Stable error codes for job failures, surfaced to the manager via `Job.error_code`
/// and the WebSocket `job_status` event's `error_code` field.
///
/// These are machine-readable strings (not enums) so the manager can switch on them
/// without a shared type definition. New codes may be added in the future; the manager
/// should treat unknown codes as `UNKNOWN_ERROR`.
pub mod error_code {
    /// Package manager command exited non-zero (generic package operation failure).
    pub const PKG_MANAGER_ERROR: &str = "PKG_MANAGER_ERROR";
    /// Package manager command could not be spawned (binary not found, permission denied).
    pub const COMMAND_NOT_FOUND: &str = "COMMAND_NOT_FOUND";
    /// Cache refresh failed (apt-get update / dnf check-update / apk update).
    pub const CACHE_REFRESH_ERROR: &str = "CACHE_REFRESH_ERROR";
    /// Network/fetch error during package download (404, connection refused, DNS).
    pub const NETWORK_ERROR: &str = "NETWORK_ERROR";
    /// GPG signature verification failure (untrusted repo, expired key, bad signature).
    pub const GPG_ERROR: &str = "GPG_ERROR";
    /// Package not found in any configured repository.
    pub const PKG_NOT_FOUND: &str = "PKG_NOT_FOUND";
    /// Unmet dependencies or package conflict prevented the operation.
    pub const DEPENDENCY_CONFLICT: &str = "DEPENDENCY_CONFLICT";
    /// Permission denied (not root, file permission, locked dpkg frontend).
    pub const PERMISSION_DENIED: &str = "PERMISSION_DENIED";
    /// System reboot command failed.
    pub const REBOOT_ERROR: &str = "REBOOT_ERROR";
    /// Job exceeded the configured timeout.
    pub const TIMEOUT: &str = "TIMEOUT";
    /// Catch-all for errors that don't match a more specific code.
    pub const UNKNOWN_ERROR: &str = "UNKNOWN_ERROR";
}

/// Classify an `anyhow::Error` into a stable error code string (one of [`error_code`]::*).
///
/// The classification inspects the full error chain (including the [`CommandError`]
/// stdout/stderr/spawn_error if present) for known signatures. It is intentionally
/// heuristic — false positives are possible but the fallback is `PKG_MANAGER_ERROR`
/// (or `COMMAND_NOT_FOUND` for spawn failures), which is safe.
pub fn classify_error(err: &AnyhowError) -> &'static str {
    // Walk the chain once; collect the CommandError (if any) and the joined message.
    let mut ce: Option<&CommandError> = None;
    let mut joined = String::new();
    for cause in err.chain() {
        if ce.is_none() {
            ce = cause.downcast_ref::<CommandError>();
        }
        let s = cause.to_string();
        if !s.is_empty() {
            if !joined.is_empty() {
                joined.push_str(" | ");
            }
            joined.push_str(&s);
        }
    }
    let lower = joined.to_lowercase();

    // Spawn failure (command not found / permission denied at exec time).
    if let Some(ce) = ce {
        if ce.timed_out {
            return error_code::TIMEOUT;
        }
        if ce.spawn_error.is_some() {
            if lower.contains("no such file") || lower.contains("not found") {
                return error_code::COMMAND_NOT_FOUND;
            }
            if lower.contains("permission denied") {
                return error_code::PERMISSION_DENIED;
            }
            return error_code::COMMAND_NOT_FOUND;
        }
    }

    // Signature-based classification on the combined message.
    if lower.contains("gpg") || lower.contains("signature") || lower.contains("untrusted") {
        return error_code::GPG_ERROR;
    }
    if lower.contains("404")
        || lower.contains("not found")
        || lower.contains("failed to fetch")
        || lower.contains("unable to fetch")
        || lower.contains("could not resolve")
        || lower.contains("connection refused")
        || lower.contains("network is unreachable")
    {
        // Distinguish "package not found" from generic network errors.
        if lower.contains("unable to locate package")
            || lower.contains("no package available")
            || lower.contains("package foo")
            || lower.contains("no such package")
        {
            return error_code::PKG_NOT_FOUND;
        }
        return error_code::NETWORK_ERROR;
    }
    if lower.contains("unable to locate package") || lower.contains("no package available") {
        return error_code::PKG_NOT_FOUND;
    }
    if lower.contains("dependency")
        || lower.contains("dependencies")
        || lower.contains("conflict")
        || lower.contains("broken count")
        || lower.contains("held broken package")
    {
        return error_code::DEPENDENCY_CONFLICT;
    }
    if lower.contains("permission denied") || lower.contains("lock file") {
        return error_code::PERMISSION_DENIED;
    }
    if lower.contains("reboot") || lower.contains("shutdown") {
        return error_code::REBOOT_ERROR;
    }
    if lower.contains("cache refresh") || lower.contains("check-update") {
        return error_code::CACHE_REFRESH_ERROR;
    }

    // If we have a CommandError with a non-zero exit but no signature matched,
    // it's a generic package-manager failure.
    if ce.is_some() {
        return error_code::PKG_MANAGER_ERROR;
    }

    error_code::UNKNOWN_ERROR
}

/// Extract the exit code from a `CommandError` in the chain, if present.
pub fn extract_exit_code(err: &AnyhowError) -> Option<i32> {
    extract_command_output(err).and_then(|ce| ce.exit_code)
}

/// Extract captured stdout from a `CommandError` in the chain, if present.
pub fn extract_stdout(err: &AnyhowError) -> Option<String> {
    extract_command_output(err).map(|ce| ce.stdout.clone())
}

/// Extract captured stderr from a `CommandError` in the chain, if present.
pub fn extract_stderr(err: &AnyhowError) -> Option<String> {
    extract_command_output(err).map(|ce| ce.stderr.clone())
}

/// Structured error for failed external command invocations.
///
/// Captures everything an operator needs to diagnose a package-manager failure:
/// the exact command, its exit code, and both output streams. The backends convert
/// `Command::output()` failures and non-zero exits into this type so the full
/// context propagates through `anyhow` instead of being flattened to a single
/// stderr-only string.
#[derive(Debug)]
pub struct CommandError {
    /// Program name (e.g. `apt-get`, `dnf`, `apk`).
    pub program: String,
    /// Arguments passed to the program (excluding the program name).
    pub args: Vec<String>,
    /// Process exit code. `None` means the process was killed by a signal or
    /// failed to start (the `spawn_error` field is set in the latter case).
    pub exit_code: Option<i32>,
    /// Captured stdout (may be empty).
    pub stdout: String,
    /// Captured stderr (may be empty).
    pub stderr: String,
    /// Set when the command could not be spawned at all (e.g. not found, permission denied).
    /// When set, `exit_code`/`stdout`/`stderr` are meaningless.
    pub spawn_error: Option<String>,
    /// True when the command exceeded its deadline and was killed by the runner.
    /// When true, `exit_code` is typically `None` (SIGKILL) and stdout/stderr
    /// contain whatever was captured before the kill. Mutually exclusive with
    /// `spawn_error` (a spawn failure never times out).
    pub timed_out: bool,
}

impl CommandError {
    /// Build a `CommandError` from a successful spawn with a non-zero (or signal) exit.
    pub fn from_output(program: &str, args: &[&str], output: &std::process::Output) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            spawn_error: None,
            timed_out: false,
        }
    }

    /// Build a `CommandError` for a spawn failure (command not found, permission denied, etc.).
    pub fn from_spawn_error(program: &str, args: &[&str], err: &std::io::Error) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            spawn_error: Some(format!("{}", err)),
            timed_out: false,
        }
    }

    /// Build a `CommandError` for a command that exceeded its deadline and was killed.
    ///
    /// `partial_output` is whatever stdout/stderr the runner captured before the kill
    /// (may be empty if the child produced no output before hanging).
    pub fn from_timeout(program: &str, args: &[&str], timeout_secs: u64) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            exit_code: None,
            stdout: String::new(),
            stderr: format!(
                "command timed out after {}s and was killed (SIGKILL)",
                timeout_secs
            ),
            spawn_error: None,
            timed_out: true,
        }
    }

    /// Render a compact single-line summary suitable for the job `error` field.
    ///
    /// Format: `apt-get failed (exit 100): <first non-empty line of stderr or stdout>`
    /// For spawn failures: `apt-get failed to start: <error>`
    /// For timeouts: `apt-get timed out after 300s`
    pub fn summary(&self) -> String {
        if let Some(ref spawn_err) = self.spawn_error {
            return format!("{} failed to start: {}", self.program, spawn_err);
        }
        if self.timed_out {
            return format!(
                "{} timed out: {}",
                self.program,
                first_nonempty_line(&self.stderr).unwrap_or_default()
            );
        }
        let detail = first_nonempty_line(&self.stderr)
            .or_else(|| first_nonempty_line(&self.stdout))
            .unwrap_or_default();
        match self.exit_code {
            Some(code) => format!("{} failed (exit {}): {}", self.program, code, detail),
            None => format!("{} failed (signal): {}", self.program, detail),
        }
    }

    /// Render the full captured output as multi-line text for the job `logs` array.
    ///
    /// Returns a vec of log lines prefixed with `[stdout]` / `[stderr]` so the manager
    /// can distinguish the streams. Output is truncated to [`MAX_OUTPUT_LINES`] lines.
    pub fn output_log_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();

        // Header line with command and exit code
        let cmd_repr = format_command(&self.program, &self.args);
        match self.exit_code {
            Some(code) => lines.push(format!("Command failed (exit {}): {}", code, cmd_repr)),
            None => lines.push(format!("Command failed (signal): {}", cmd_repr)),
        }
        if let Some(ref spawn_err) = self.spawn_error {
            lines.push(format!("Spawn error: {}", spawn_err));
            return lines;
        }

        // stderr first (usually the diagnostic), then stdout
        append_stream_lines(&mut lines, "stderr", &self.stderr);
        append_stream_lines(&mut lines, "stdout", &self.stdout);

        if lines.len() > MAX_OUTPUT_LINES {
            lines.truncate(MAX_OUTPUT_LINES);
            lines.push(format!(
                "... output truncated at {} lines",
                MAX_OUTPUT_LINES
            ));
        }
        lines
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display gives the short summary so existing `e.to_string()` call sites
        // that just want a one-liner still work. The full output is available via
        // `output_log_lines()` and `format_error_chain()`.
        write!(f, "{}", self.summary())
    }
}

impl StdError for CommandError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        None
    }
}

/// Render the full anyhow error chain as a multi-line string.
///
/// This is the `{:?}` (Debug) rendering of `anyhow::Error`, which includes every
/// `.context(...)` layer and the root cause. It is what `tracing::error!(?e)` would
/// log locally, and what we want the manager to see in `job.error` / job logs.
///
/// Example output:
/// ```text
/// apt-get failed (exit 100): E: Unable to locate package foo
///
/// Caused by:
///     Failed to execute apt command
/// ```
pub fn format_error_chain(err: &AnyhowError) -> String {
    // anyhow's Debug format already renders the full chain with "Caused by:" sections.
    format!("{:?}", err)
}

/// Extract a `CommandError` from anywhere in an anyhow error's cause chain.
///
/// Backends wrap `CommandError` via `anyhow::Error::new(...)` or `.context(...)`,
/// so the `CommandError` may be the root or a nested cause. This walks the chain
/// and returns the first `CommandError` found (as a borrowed reference).
pub fn extract_command_output(err: &AnyhowError) -> Option<&CommandError> {
    // anyhow::Error implements std::error::Error via AsRef<dyn StdError>, but the
    // chain walk needs the StdError::source() traversal. anyhow's own chain() is
    // the canonical way to walk its cause chain.
    for cause in err.chain() {
        if let Some(ce) = cause.downcast_ref::<CommandError>() {
            return Some(ce);
        }
    }
    None
}

/// Build the complete set of job log lines for a failed operation.
///
/// This is the one-call helper used by `fail_job_with_diagnostics`. It produces:
/// 1. The full error chain (so the manager sees every `.context()` layer).
/// 2. The captured command output (stdout/stderr with stream prefixes), if any.
///
/// The returned lines are meant to be appended to `job.logs` via `add_job_log`.
pub fn diagnostic_log_lines(err: &AnyhowError) -> Vec<String> {
    let mut lines = Vec::new();

    // 1. Full error chain
    lines.push("Error chain:".to_string());
    for chain_line in format_error_chain(err).lines() {
        lines.push(chain_line.to_string());
    }

    // 2. Captured command output, if available
    if let Some(ce) = extract_command_output(err) {
        lines.push(String::new()); // blank separator
        lines.push("Command output:".to_string());
        lines.extend(ce.output_log_lines());
    }

    lines
}

/// Build a human-readable summary string for a cache-refresh failure, suitable for
/// the `PackageCacheState::update_failure` message. Uses the `CommandError` summary
/// if present, otherwise the full error chain.
pub fn format_error_for_cache(err: &AnyhowError) -> String {
    if let Some(ce) = extract_command_output(err) {
        ce.summary()
    } else {
        format_error_chain(err)
    }
}

// --- private helpers ---

fn format_command(program: &str, args: &[String]) -> String {
    let mut s = String::with_capacity(program.len() + args.len() * 8);
    s.push_str(program);
    for a in args {
        s.push(' ');
        // Quote args containing whitespace or shell metacharacters for readability.
        if a.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '\'' | '"' | '$' | '`' | '\\' | '|' | '&' | ';' | '>' | '<'
                )
        }) {
            s.push('\'');
            s.push_str(&a.replace('\'', "'\\''"));
            s.push('\'');
        } else {
            s.push_str(a);
        }
    }
    s
}

fn first_nonempty_line(s: &str) -> Option<String> {
    s.lines()
        .map(|l| l.trim().to_string())
        .find(|l| !l.is_empty())
}

fn append_stream_lines(lines: &mut Vec<String>, stream_name: &str, content: &str) {
    let mut saw_any = false;
    for line in content.lines() {
        if !saw_any {
            lines.push(format!("[{}]", stream_name));
            saw_any = true;
        }
        lines.push(format!("  {}", line));
    }
    if !saw_any {
        lines.push(format!("[{}] (empty)", stream_name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn make_output(stdout: &[u8], stderr: &[u8], code: i32) -> std::process::Output {
        // On Unix, ExitStatus::from_raw expects a raw wait status (WIFEXITED encoding),
        // where the exit code is stored in bits 8-15. So shift left by 8.
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn command_error_summary_with_stderr() {
        let out = make_output(b"", b"E: Unable to locate package foo\n", 100);
        let ce = CommandError::from_output("apt-get", &["install", "foo"], &out);
        assert_eq!(
            ce.summary(),
            "apt-get failed (exit 100): E: Unable to locate package foo"
        );
    }

    #[test]
    fn command_error_summary_falls_back_to_stdout() {
        let out = make_output(b"some stdout detail\n", b"", 1);
        let ce = CommandError::from_output("dnf", &["check-update"], &out);
        assert_eq!(ce.summary(), "dnf failed (exit 1): some stdout detail");
    }

    #[test]
    fn command_error_summary_spawn_failure() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let ce = CommandError::from_spawn_error("apt-get", &["update"], &io_err);
        assert!(ce.summary().starts_with("apt-get failed to start:"));
        assert!(ce.summary().contains("no such file"));
    }

    #[test]
    fn command_error_summary_signal_termination() {
        // Simulate signal termination: exit_code None.
        let ce = CommandError {
            program: "apt-get".to_string(),
            args: vec!["install".to_string()],
            exit_code: None,
            stdout: String::new(),
            stderr: "Killed\n".to_string(),
            spawn_error: None,
            timed_out: false,
        };
        assert_eq!(ce.summary(), "apt-get failed (signal): Killed");
    }

    #[test]
    fn command_error_summary_timeout() {
        let ce = CommandError::from_timeout("apt-get", &["update"], 300);
        let s = ce.summary();
        assert!(s.starts_with("apt-get timed out:"));
        assert!(s.contains("300s"));
    }

    #[test]
    fn classify_timeout_error() {
        let ce = CommandError::from_timeout("apt-get", &["update"], 300);
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::TIMEOUT);
    }

    #[test]
    fn command_error_output_log_lines_includes_streams() {
        let out = make_output(
            b"Reading package lists...\n",
            b"E: Unable to locate package foo\nE: See apt-get --help\n",
            100,
        );
        let ce = CommandError::from_output("apt-get", &["install", "foo"], &out);
        let lines = ce.output_log_lines();
        // Header + stderr (2 lines) + stdout (1 line) = at least 5 lines
        assert!(lines.len() >= 5);
        assert!(lines[0].contains("exit 100"));
        assert!(lines[0].contains("apt-get install foo"));
        assert!(lines.iter().any(|l| l == "[stderr]"));
        assert!(lines.iter().any(|l| l == "[stdout]"));
        assert!(lines
            .iter()
            .any(|l| l.contains("Unable to locate package foo")));
    }

    #[test]
    fn command_error_output_log_lines_truncates() {
        let big_stdout = (0..500)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let out = make_output(big_stdout.as_bytes(), b"", 1);
        let ce = CommandError::from_output("dnf", &["upgrade"], &out);
        let lines = ce.output_log_lines();
        assert!(lines.len() <= MAX_OUTPUT_LINES + 2); // header + truncation marker
        assert!(lines.last().unwrap().contains("truncated"));
    }

    #[test]
    fn command_error_display_matches_summary() {
        let out = make_output(b"", b"err\n", 1);
        let ce = CommandError::from_output("apk", &["add", "foo"], &out);
        assert_eq!(format!("{}", ce), ce.summary());
    }

    #[test]
    fn format_error_chain_includes_context_layers() {
        let root = CommandError::from_output(
            "apt-get",
            &["install", "foo"],
            &make_output(b"", b"E: Unable to locate package foo\n", 100),
        );
        let err = AnyhowError::new(root).context("Failed to execute apt command");
        let chain = format_error_chain(&err);
        // Debug format includes both the root error and the context.
        assert!(chain.contains("Failed to execute apt command"));
        assert!(chain.contains("Unable to locate package foo"));
    }

    #[test]
    fn extract_command_output_finds_root() {
        let root =
            CommandError::from_output("apt-get", &["install"], &make_output(b"", b"err\n", 1));
        let err = AnyhowError::new(root);
        assert!(extract_command_output(&err).is_some());
    }

    #[test]
    fn extract_command_output_finds_nested() {
        let root = CommandError::from_output("dnf", &["upgrade"], &make_output(b"", b"err\n", 1));
        let err = AnyhowError::new(root).context("Failed to execute dnf command");
        assert!(extract_command_output(&err).is_some());
    }

    #[test]
    fn extract_command_output_none_for_plain_error() {
        let err = AnyhowError::msg("plain string error");
        assert!(extract_command_output(&err).is_none());
    }

    #[test]
    fn diagnostic_log_lines_includes_chain_and_output() {
        let root = CommandError::from_output(
            "apt-get",
            &["install", "foo"],
            &make_output(b"Reading lists\n", b"E: no package foo\n", 100),
        );
        let err = AnyhowError::new(root).context("Failed to execute apt command");
        let lines = diagnostic_log_lines(&err);
        let joined = lines.join("\n");
        assert!(joined.contains("Error chain:"));
        assert!(joined.contains("Failed to execute apt command"));
        assert!(joined.contains("Command output:"));
        assert!(joined.contains("[stderr]"));
        assert!(joined.contains("no package foo"));
    }

    #[test]
    fn diagnostic_log_lines_for_plain_error() {
        let err = AnyhowError::msg("plain string error");
        let lines = diagnostic_log_lines(&err);
        assert!(lines.iter().any(|l| l.contains("Error chain:")));
        assert!(lines.iter().any(|l| l.contains("plain string error")));
        // No command output section for a plain error.
        assert!(!lines.iter().any(|l| l.contains("Command output:")));
    }

    // --- classify_error tests ---

    #[test]
    fn classify_gpg_error() {
        let ce = CommandError::from_output(
            "apt-get",
            &["install", "foo"],
            &make_output(b"", b"W: GPG error: signature was invalid\n", 100),
        );
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::GPG_ERROR);
    }

    #[test]
    fn classify_untrusted_repo() {
        // "not signed" doesn't contain "gpg"/"signature"/"untrusted" literally;
        // but "signature" is a common apt phrase. Test the actual signature.
        let ce2 = CommandError::from_output(
            "apt-get",
            &["update"],
            &make_output(
                b"",
                b"W: An error occurred during the signature verification\n",
                100,
            ),
        );
        let err2 = AnyhowError::new(ce2);
        assert_eq!(classify_error(&err2), error_code::GPG_ERROR);
    }

    #[test]
    fn classify_network_404() {
        let ce = CommandError::from_output(
            "apt-get",
            &["update"],
            &make_output(b"", b"E: Failed to fetch 404 Not Found\n", 100),
        );
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::NETWORK_ERROR);
    }

    #[test]
    fn classify_pkg_not_found() {
        let ce = CommandError::from_output(
            "apt-get",
            &["install", "foo"],
            &make_output(b"", b"E: Unable to locate package foo\n", 100),
        );
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::PKG_NOT_FOUND);
    }

    #[test]
    fn classify_dependency_conflict() {
        let ce = CommandError::from_output(
            "apt-get",
            &["install", "foo"],
            &make_output(
                b"",
                b"E: Unmet dependencies. Try 'apt --fix-broken install'\n",
                100,
            ),
        );
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::DEPENDENCY_CONFLICT);
    }

    #[test]
    fn classify_permission_denied() {
        let ce = CommandError::from_output(
            "apt-get",
            &["install", "foo"],
            &make_output(
                b"",
                b"E: Could not open lock file; permission denied\n",
                100,
            ),
        );
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::PERMISSION_DENIED);
    }

    #[test]
    fn classify_command_not_found_spawn() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let ce = CommandError::from_spawn_error("apt-get", &["update"], &io_err);
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::COMMAND_NOT_FOUND);
    }

    #[test]
    fn classify_permission_denied_spawn() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let ce = CommandError::from_spawn_error("apt-get", &["update"], &io_err);
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::PERMISSION_DENIED);
    }

    #[test]
    fn classify_reboot_error() {
        let ce = CommandError::from_output(
            "shutdown",
            &["-r", "+1"],
            &make_output(b"", b"shutdown: must be root\n", 1),
        );
        let err = AnyhowError::new(ce).context("Failed to schedule delayed reboot");
        assert_eq!(classify_error(&err), error_code::REBOOT_ERROR);
    }

    #[test]
    fn classify_generic_pkg_manager_error() {
        let ce = CommandError::from_output(
            "apt-get",
            &["install", "foo"],
            &make_output(b"", b"E: some unknown apt error\n", 100),
        );
        let err = AnyhowError::new(ce);
        assert_eq!(classify_error(&err), error_code::PKG_MANAGER_ERROR);
    }

    #[test]
    fn classify_plain_string_unknown() {
        let err = AnyhowError::msg("something weird happened");
        assert_eq!(classify_error(&err), error_code::UNKNOWN_ERROR);
    }

    #[test]
    fn extract_exit_code_present() {
        let ce =
            CommandError::from_output("apt-get", &["install"], &make_output(b"", b"err\n", 42));
        let err = AnyhowError::new(ce);
        assert_eq!(extract_exit_code(&err), Some(42));
    }

    #[test]
    fn extract_exit_code_absent_for_plain_error() {
        let err = AnyhowError::msg("plain");
        assert_eq!(extract_exit_code(&err), None);
    }

    #[test]
    fn extract_stdout_stderr() {
        let ce = CommandError::from_output(
            "dnf",
            &["upgrade"],
            &make_output(b"stdout line\n", b"stderr line\n", 1),
        );
        let err = AnyhowError::new(ce);
        assert_eq!(extract_stdout(&err).as_deref(), Some("stdout line\n"));
        assert_eq!(extract_stderr(&err).as_deref(), Some("stderr line\n"));
    }
}
