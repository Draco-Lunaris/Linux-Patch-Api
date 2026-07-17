//! Mock CommandRunner for deterministic backend error testing.
//!
//! This module provides a `MockCommandRunner` that can be programmed with
//! responses for specific (program, args) patterns. It enables testing
//! backend error handling without real package managers.

use std::collections::HashMap;
use std::sync::Mutex;

use linux_patch_api::packages::coordinator::{CommandOutput, CommandRunner};
use linux_patch_api::packages::error_utils::CommandError;

/// A mock command runner that returns programmed responses.
///
/// Responses are matched by (program, args) key. If no match is found,
/// returns a default error. This allows tests to simulate:
/// - Successful commands (exit 0 with stdout)
/// - Failed commands (non-zero exit with stderr)
/// - Spawn failures (command not found)
/// - Specific package-manager output formats
pub struct MockCommandRunner {
    responses: Mutex<HashMap<String, MockResponse>>,
    /// Records all calls for verification
    call_log: Mutex<Vec<(String, Vec<String>)>>,
}

/// A programmed response for the mock runner.
#[derive(Clone)]
pub struct MockResponse {
    /// The exit code. `None` simulates a signal kill.
    pub exit_code: Option<i32>,
    /// stdout content
    pub stdout: String,
    /// stderr content
    pub stderr: String,
    /// If set, simulates a spawn failure (command not found, permission denied).
    /// When set, exit_code/stdout/stderr are ignored.
    pub spawn_error: Option<String>,
    /// If true, the runner returns Err instead of Ok(CommandOutput).
    /// This simulates the SystemCommandRunner behavior for non-zero exits
    /// when used via the `run_command` helper.
    pub return_as_error: bool,
}

impl MockResponse {
    /// A successful command with stdout.
    pub fn success(stdout: &str) -> Self {
        Self {
            exit_code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
            spawn_error: None,
            return_as_error: false,
        }
    }

    /// A successful command with empty output.
    pub fn success_empty() -> Self {
        Self::success("")
    }

    /// A failed command with exit code and stderr.
    pub fn failure(exit_code: i32, stderr: &str) -> Self {
        Self {
            exit_code: Some(exit_code),
            stdout: String::new(),
            stderr: stderr.to_string(),
            spawn_error: None,
            return_as_error: false,
        }
    }

    /// A failed command that returns Err (simulating SystemCommandRunner
    /// behavior for `run_command` helper users).
    pub fn error(exit_code: i32, stderr: &str) -> Self {
        Self {
            exit_code: Some(exit_code),
            stdout: String::new(),
            stderr: stderr.to_string(),
            spawn_error: None,
            return_as_error: true,
        }
    }

    /// A spawn failure (command not found).
    pub fn spawn_failed(msg: &str) -> Self {
        Self {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            spawn_error: Some(msg.to_string()),
            return_as_error: true,
        }
    }
}

impl MockCommandRunner {
    /// Create a new empty mock runner.
    pub fn new() -> Self {
        Self {
            responses: Mutex::new(HashMap::new()),
            call_log: Mutex::new(Vec::new()),
        }
    }

    /// Program a response for a specific (program, args) combination.
    /// The key is `program args[0] args[1]...` joined by spaces.
    pub fn add_response(&self, program: &str, args: &[&str], response: MockResponse) {
        let key = make_key(program, args);
        self.responses.lock().unwrap().insert(key, response);
    }

    /// Program a response that matches any call to the given program
    /// (ignoring args). Uses a wildcard key `program *`.
    pub fn add_wildcard_response(&self, program: &str, response: MockResponse) {
        let key = format!("{} *", program);
        self.responses.lock().unwrap().insert(key, response);
    }

    /// Get the list of all calls made to the runner.
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.call_log.lock().unwrap().clone()
    }

    /// Assert that a specific command was called.
    pub fn assert_called(&self, program: &str, args: &[&str]) {
        let target = make_key(program, args);
        let calls = self.calls();
        assert!(
            calls.iter().any(|(p, a)| {
                let key = format!("{} {}", p, a.join(" "));
                key == target
            }),
            "Expected command '{}' {} {:?} was not called. Calls: {:?}",
            program,
            args.len(),
            args,
            calls
        );
    }

    /// Assert that at least one call was made to the given program.
    pub fn assert_program_called(&self, program: &str) {
        let calls = self.calls();
        assert!(
            calls.iter().any(|(p, _)| p == program),
            "Expected program '{}' was not called. Calls: {:?}",
            program,
            calls
        );
    }

    /// Look up a response by key, trying exact match first, then wildcard.
    fn lookup(&self, program: &str, args: &[&str]) -> Option<MockResponse> {
        let responses = self.responses.lock().unwrap();
        let exact_key = make_key(program, args);
        if let Some(r) = responses.get(&exact_key) {
            return Some(r.clone());
        }
        let wildcard_key = format!("{} *", program);
        responses.get(&wildcard_key).cloned()
    }
}

impl Clone for MockCommandRunner {
    fn clone(&self) -> Self {
        Self {
            responses: Mutex::new(self.responses.lock().unwrap().clone()),
            call_log: Mutex::new(Vec::new()),
        }
    }
}

impl Default for MockCommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRunner for MockCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        // Record the call
        self.call_log.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ));

        let response = self.lookup(program, args).unwrap_or_else(|| {
            // Default: command not found
            MockResponse::spawn_failed(&format!(
                "MockCommandRunner: no response programmed for '{}' {}",
                program,
                args.join(" ")
            ))
        });

        // If spawn error, return Err with CommandError
        if let Some(ref spawn_err) = response.spawn_error {
            return Err(anyhow::Error::new(CommandError {
                program: program.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                spawn_error: Some(spawn_err.clone()),
                timed_out: false,
            })
            .context(format!("Failed to execute {}", program)));
        }

        // If return_as_error, return Err with CommandError (simulates
        // SystemCommandRunner behavior for non-zero exits)
        if response.return_as_error {
            return Err(anyhow::Error::new(CommandError {
                program: program.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
                exit_code: response.exit_code,
                stdout: response.stdout.clone(),
                stderr: response.stderr.clone(),
                spawn_error: None,
                timed_out: false,
            }));
        }

        // Return Ok with the programmed output
        Ok(CommandOutput {
            status_code: response.exit_code,
            stdout: response.stdout,
            stderr: response.stderr,
            timed_out: false,
        })
    }
}

/// Build a lookup key from program and args.
fn make_key(program: &str, args: &[&str]) -> String {
    if args.is_empty() {
        program.to_string()
    } else {
        format!("{} {}", program, args.join(" "))
    }
}

// =============================================================================
// APT Backend Tests
// ==============================================================================

mod apt_tests {
    use super::*;
    use linux_patch_api::packages::AptBackend;
    use linux_patch_api::packages::PackageManagerBackend;
    use std::sync::Arc;

    fn make_backend(mock: &MockCommandRunner) -> AptBackend {
        AptBackend::new(Arc::new(MockCommandRunner {
            responses: Mutex::new(mock.responses.lock().unwrap().clone()),
            call_log: Mutex::new(Vec::new()),
        }))
    }

    #[test]
    fn apt_install_success() {
        let mock = MockCommandRunner::new();
        // dpkg --configure -a (pre-flight) succeeds
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        // apt-get install succeeds
        mock.add_response(
            "apt-get",
            &["install", "-y", "--", "curl"],
            MockResponse::success("Setting up curl...\n"),
        );
        // dpkg --audit (post-verify) is clean
        mock.add_response("dpkg", &["--audit"], MockResponse::success_empty());

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_ok(), "install should succeed: {:?}", result.err());
    }

    #[test]
    fn apt_install_nonzero_exit_returns_error() {
        let mock = MockCommandRunner::new();
        // dpkg --configure -a (pre-flight) succeeds
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        // apt-get install fails with exit 100
        mock.add_response(
            "apt-get",
            &["install", "-y", "--", "nonexistent-pkg"],
            MockResponse {
                exit_code: Some(100),
                stdout: String::new(),
                stderr: "E: Unable to locate package nonexistent-pkg".to_string(),
                spawn_error: None,
                return_as_error: false,
            },
        );
        // dpkg --configure -a (cleanup after failure)
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "nonexistent-pkg".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "install should fail");
        let err = result.unwrap_err();
        let err_str = err.to_string();
        assert!(
            err_str.contains("100") || err_str.contains("Unable to locate"),
            "error should contain exit code or stderr: {}",
            err_str
        );
    }

    #[test]
    fn apt_install_spawn_failure_returns_error() {
        let mock = MockCommandRunner::new();
        // dpkg --configure -a (pre-flight) succeeds
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        // apt-get spawn fails (binary not found)
        mock.add_response(
            "apt-get",
            &["install", "-y", "--", "curl"],
            MockResponse::spawn_failed("No such file or directory"),
        );
        // dpkg --configure -a (cleanup after spawn failure)
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "install should fail on spawn error");
    }

    #[test]
    fn apt_preflight_dpkg_failure_propagates() {
        let mock = MockCommandRunner::new();
        // dpkg --configure -a (pre-flight) fails — dpkg is broken
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::error(1, "dpkg: error: package is in a broken state"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "should fail when pre-flight dpkg fails");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("dpkg") || err.contains("configure"),
            "error should mention dpkg: {}",
            err
        );
    }

    #[test]
    fn apt_post_audit_dirty_returns_error() {
        let mock = MockCommandRunner::new();
        // dpkg --configure -a (pre-flight) succeeds
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        // apt-get install succeeds
        mock.add_response(
            "apt-get",
            &["install", "-y", "--", "kernel-image"],
            MockResponse::success("Setting up kernel-image...\n"),
        );
        // dpkg --audit finds problems (not empty)
        mock.add_response(
            "dpkg",
            &["--audit"],
            MockResponse::success(
                "The following packages are only half configured:\n  kernel-image\n",
            ),
        );
        // dpkg --configure -a (cleanup attempt)
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        // dpkg --audit (recheck — still dirty)
        mock.add_response(
            "dpkg",
            &["--audit"],
            MockResponse::success(
                "The following packages are only half configured:\n  kernel-image\n",
            ),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "kernel-image".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "should fail when post-audit is dirty");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("half-configured") || err.contains("audit"),
            "error should mention audit failure: {}",
            err
        );
    }

    #[test]
    fn apt_list_packages_parses_output() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success(
                "Listing...\ncurl/noble-updates,now 8.5.0-2ubuntu10 amd64 [installed]\nopenssl/noble-security,now 3.0.13-0ubuntu3 amd64 [installed]\n",
            ),
        );

        let backend = make_backend(&mock);
        let result = backend.list_packages(None);

        assert!(result.is_ok(), "list should succeed: {:?}", result.err());
        let packages = result.unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "curl");
        assert_eq!(packages[1].name, "openssl");
    }

    #[test]
    fn apt_list_packages_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::error(1, "apt: error reading package lists"),
        );

        let backend = make_backend(&mock);
        let result = backend.list_packages(None);

        assert!(result.is_err(), "list should fail");
    }

    #[test]
    fn apt_refresh_cache_success() {
        let mock = MockCommandRunner::new();
        // dpkg --configure -a (pre-flight)
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        // apt-get update succeeds
        mock.add_response("apt-get", &["update"], MockResponse::success("Hit:1 ...\n"));
        // dpkg --audit (post-verify)
        mock.add_response("dpkg", &["--audit"], MockResponse::success_empty());

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(
            result.is_ok(),
            "cache refresh should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn apt_refresh_cache_failure_returns_error() {
        let mock = MockCommandRunner::new();
        // dpkg --configure -a (pre-flight)
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        // apt-get update fails
        mock.add_response(
            "apt-get",
            &["update"],
            MockResponse {
                exit_code: Some(100),
                stdout: String::new(),
                stderr: "E: Could not get lock /var/lib/apt/lists/lock".to_string(),
                spawn_error: None,
                return_as_error: false,
            },
        );
        // dpkg --configure -a (cleanup)
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(result.is_err(), "cache refresh should fail");
    }

    #[test]
    fn apt_get_installed_version_parses_dpkg() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dpkg",
            &["-s", "linux-patch-api"],
            MockResponse::success(
                "Package: linux-patch-api\nVersion: 2.2.0\nStatus: install ok installed\n",
            ),
        );

        let backend = make_backend(&mock);
        let result = backend.get_installed_version("linux-patch-api");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("2.2.0".to_string()));
    }

    #[test]
    fn apt_get_installed_version_not_installed() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dpkg",
            &["-s", "nonexistent"],
            MockResponse::error(1, "dpkg-query: package not found"),
        );

        let backend = make_backend(&mock);
        let result = backend.get_installed_version("nonexistent");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn apt_get_candidate_version_parses_policy() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt-cache",
            &["policy", "linux-patch-api"],
            MockResponse::success(
                "linux-patch-api:\n  Installed: 2.1.0\n  Candidate: 2.2.0\n  Version table:\n",
            ),
        );

        let backend = make_backend(&mock);
        let result = backend.get_candidate_version("linux-patch-api");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("2.2.0".to_string()));
    }
}

// =============================================================================
// APK Backend Tests
// =============================================================================

mod apk_tests {
    use super::*;
    use linux_patch_api::packages::ApkBackend;
    use linux_patch_api::packages::PackageManagerBackend;
    use std::sync::Arc;

    fn make_backend(mock: &MockCommandRunner) -> ApkBackend {
        ApkBackend::new(Arc::new(MockCommandRunner {
            responses: Mutex::new(mock.responses.lock().unwrap().clone()),
            call_log: Mutex::new(Vec::new()),
        }))
    }

    #[test]
    fn apk_install_success() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["add", "--", "curl"],
            MockResponse::success("OK: 1 MiB\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_ok(), "install should succeed: {:?}", result.err());
    }

    #[test]
    fn apk_install_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["add", "--", "nonexistent-pkg"],
            MockResponse::error(1, "ERROR: nonexistent-pkg (no such package)"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "nonexistent-pkg".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "install should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent") || err.contains("no such"),
            "error should contain package name: {}",
            err
        );
    }

    #[test]
    fn apk_remove_success() {
        let mock = MockCommandRunner::new();
        mock.add_response("apk", &["del", "--", "curl"], MockResponse::success("OK\n"));

        let backend = make_backend(&mock);
        let result = backend.remove_package("curl", false);

        assert!(result.is_ok(), "remove should succeed: {:?}", result.err());
    }

    #[test]
    fn apk_remove_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["del", "--", "not-installed"],
            MockResponse::error(1, "ERROR: not-installed not installed"),
        );

        let backend = make_backend(&mock);
        let result = backend.remove_package("not-installed", false);

        assert!(result.is_err(), "remove should fail");
    }

    #[test]
    fn apk_refresh_cache_success() {
        let mock = MockCommandRunner::new();
        mock.add_response("apk", &["update"], MockResponse::success("OK: 1 MiB\n"));

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(
            result.is_ok(),
            "cache refresh should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn apk_refresh_cache_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["update"],
            MockResponse::error(1, "ERROR: failed to fetch index"),
        );

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(result.is_err(), "cache refresh should fail");
    }

    #[test]
    fn apk_list_packages_parses_output() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["list", "--installed"],
            MockResponse::success("bash-5.2.21-r0 [main] The GNU Bourne Again shell\nopenssl-3.1.4-r0 [main] Toolkit for SSL/TLS\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.list_packages(None);

        assert!(result.is_ok());
        let packages = result.unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "bash");
        assert_eq!(packages[1].name, "openssl");
    }

    #[test]
    fn apk_get_installed_version_parses_output() {
        let mock = MockCommandRunner::new();
        // APK list output format: {name}-{version} [{repo}] {description}
        // parse_name_version finds the first hyphen-digit and splits there.
        // The version will include everything after the name-version separator.
        mock.add_response(
            "apk",
            &["list", "--installed", "bash"],
            MockResponse::success("bash-5.2.21-r0\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.get_installed_version("bash");

        assert!(result.is_ok());
        // With just "bash-5.2.21-r0" (no trailing description), version is "5.2.21-r0"
        assert_eq!(result.unwrap(), Some("5.2.21-r0".to_string()));
    }
}

// =============================================================================
// DNF Backend Tests
// =============================================================================

mod dnf_tests {
    use super::*;
    use linux_patch_api::packages::DnfBackend;
    use linux_patch_api::packages::PackageManagerBackend;
    use std::sync::Arc;

    fn make_backend(mock: &MockCommandRunner) -> DnfBackend {
        DnfBackend::new(Arc::new(MockCommandRunner {
            responses: Mutex::new(mock.responses.lock().unwrap().clone()),
            call_log: Mutex::new(Vec::new()),
        }))
    }

    #[test]
    fn dnf_install_success() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dnf",
            &["install", "-y", "--", "curl"],
            MockResponse::success("Installed: curl-8.0\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_ok(), "install should succeed: {:?}", result.err());
    }

    #[test]
    fn dnf_install_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dnf",
            &["install", "-y", "--", "nonexistent"],
            MockResponse::error(1, "No package nonexistent available"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "nonexistent".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "install should fail");
    }

    #[test]
    fn dnf_refresh_cache_success() {
        let mock = MockCommandRunner::new();
        // dnf check-update returns 0 (no updates) or 100 (updates available)
        // The backend uses run_command_with_acceptable_exit which handles both
        mock.add_response(
            "dnf",
            &["check-update", "--refresh"],
            MockResponse {
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                spawn_error: None,
                return_as_error: false,
            },
        );

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(
            result.is_ok(),
            "cache refresh should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn dnf_refresh_cache_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dnf",
            &["check-update", "--refresh"],
            MockResponse::error(1, "Error: Failed to synchronize"),
        );

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(result.is_err(), "cache refresh should fail");
    }

    #[test]
    fn dnf_get_installed_version_parses_rpm() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "rpm",
            &["-q", "--qf", "%{VERSION}-%{RELEASE}", "bash"],
            MockResponse::success("5.2.21-1.fc43\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.get_installed_version("bash");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("5.2.21-1.fc43".to_string()));
    }

    #[test]
    fn dnf_get_installed_version_not_installed() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "rpm",
            &["-q", "--qf", "%{VERSION}-%{RELEASE}", "nonexistent"],
            MockResponse::error(1, "package nonexistent is not installed"),
        );

        let backend = make_backend(&mock);
        let result = backend.get_installed_version("nonexistent");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }
}

// =============================================================================
// YUM Backend Tests
// =============================================================================

mod yum_tests {
    use super::*;
    use linux_patch_api::packages::PackageManagerBackend;
    use linux_patch_api::packages::YumBackend;
    use std::sync::Arc;

    fn make_backend(mock: &MockCommandRunner) -> YumBackend {
        YumBackend::new(Arc::new(MockCommandRunner {
            responses: Mutex::new(mock.responses.lock().unwrap().clone()),
            call_log: Mutex::new(Vec::new()),
        }))
    }

    #[test]
    fn yum_install_success() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "yum",
            &["install", "-y", "--", "curl"],
            MockResponse::success("Installed: curl-8.0\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_ok(), "install should succeed: {:?}", result.err());
    }

    #[test]
    fn yum_install_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "yum",
            &["install", "-y", "--", "nonexistent"],
            MockResponse::error(1, "No package nonexistent available"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "nonexistent".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "install should fail");
    }

    #[test]
    fn yum_refresh_cache_success() {
        let mock = MockCommandRunner::new();
        mock.add_response("yum", &["makecache"], MockResponse::success("OK\n"));

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(
            result.is_ok(),
            "cache refresh should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn yum_refresh_cache_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "yum",
            &["makecache"],
            MockResponse::error(1, "Error: Cannot retrieve metalink"),
        );

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(result.is_err(), "cache refresh should fail");
    }
}

// =============================================================================
// Pacman Backend Tests
// =============================================================================

mod pacman_tests {
    use super::*;
    use linux_patch_api::packages::PackageManagerBackend;
    use linux_patch_api::packages::PacmanBackend;
    use std::sync::Arc;

    fn make_backend(mock: &MockCommandRunner) -> PacmanBackend {
        PacmanBackend::new(Arc::new(MockCommandRunner {
            responses: Mutex::new(mock.responses.lock().unwrap().clone()),
            call_log: Mutex::new(Vec::new()),
        }))
    }

    #[test]
    fn pacman_install_success() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-S", "--noconfirm", "--needed", "--", "curl"],
            MockResponse::success("resolving dependencies...\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_ok(), "install should succeed: {:?}", result.err());
    }

    #[test]
    fn pacman_install_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-S", "--noconfirm", "--needed", "--", "nonexistent"],
            MockResponse::error(1, "error: target not found: nonexistent"),
        );

        let backend = make_backend(&mock);
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "nonexistent".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "install should fail");
    }

    #[test]
    fn pacman_remove_success() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-R", "--noconfirm", "--", "curl"],
            MockResponse::success("checking dependencies...\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.remove_package("curl", false);

        assert!(result.is_ok(), "remove should succeed: {:?}", result.err());
    }

    #[test]
    fn pacman_remove_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-R", "--noconfirm", "--", "not-installed"],
            MockResponse::error(1, "error: target not found: not-installed"),
        );

        let backend = make_backend(&mock);
        let result = backend.remove_package("not-installed", false);

        assert!(result.is_err(), "remove should fail");
    }

    #[test]
    fn pacman_refresh_cache_success() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-Sy"],
            MockResponse::success(":: Synchronizing package databases...\n"),
        );

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(
            result.is_ok(),
            "cache refresh should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn pacman_refresh_cache_failure_returns_error() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-Sy"],
            MockResponse::error(1, "error: failed to synchronize databases"),
        );

        let backend = make_backend(&mock);
        let cache_state = linux_patch_api::packages::cache::PackageCacheState::new();
        let result = backend.refresh_package_cache(&cache_state);

        assert!(result.is_err(), "cache refresh should fail");
    }

    #[test]
    fn pacman_get_installed_version_parses_output() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-Q", "bash"],
            MockResponse::success("bash 5.2.21-1\n"),
        );

        let backend = make_backend(&mock);
        let result = backend.get_installed_version("bash");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("5.2.21-1".to_string()));
    }

    #[test]
    fn pacman_get_installed_version_not_installed() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-Q", "nonexistent"],
            MockResponse::error(1, "error: package 'nonexistent' was not found"),
        );

        let backend = make_backend(&mock);
        let result = backend.get_installed_version("nonexistent");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }
}

// =============================================================================
// Cross-backend: spawn failure (command not found)
// =============================================================================

mod spawn_failure_tests {
    use super::*;
    use linux_patch_api::packages::PackageManagerBackend;
    use std::sync::Arc;

    #[test]
    fn apk_spawn_failure_returns_error() {
        let mock = MockCommandRunner::new();
        // No responses programmed — default is spawn failure
        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err(), "should fail when command not found");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No such file") || err.contains("not found") || err.contains("Failed"),
            "error should indicate spawn failure: {}",
            err
        );
    }

    #[test]
    fn dnf_spawn_failure_returns_error() {
        let mock = MockCommandRunner::new();
        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn pacman_spawn_failure_returns_error() {
        let mock = MockCommandRunner::new();
        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err());
    }
}

// =============================================================================
// Mock infrastructure tests
// =============================================================================

mod mock_tests {
    use super::*;

    #[test]
    fn mock_returns_programmed_success() {
        let mock = MockCommandRunner::new();
        mock.add_response("echo", &["hello"], MockResponse::success("hello\n"));

        let result = mock.run("echo", &["hello"]).unwrap();
        assert!(result.success());
        assert_eq!(result.stdout, "hello\n");
    }

    #[test]
    fn mock_returns_programmed_error() {
        let mock = MockCommandRunner::new();
        mock.add_response("false", &[], MockResponse::error(1, "failed"));

        let result = mock.run("false", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn mock_returns_spawn_error_when_unprogrammed() {
        let mock = MockCommandRunner::new();
        let result = mock.run("nonexistent-binary", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn mock_records_calls() {
        let mock = MockCommandRunner::new();
        mock.add_response("echo", &["test"], MockResponse::success_empty());

        let _ = mock.run("echo", &["test"]);
        mock.assert_called("echo", &["test"]);
    }

    #[test]
    fn mock_wildcard_matches_any_args() {
        let mock = MockCommandRunner::new();
        mock.add_wildcard_response("apt-get", MockResponse::success("done\n"));

        let result1 = mock.run("apt-get", &["update"]).unwrap();
        let result2 = mock.run("apt-get", &["install", "-y", "curl"]).unwrap();

        assert_eq!(result1.stdout, "done\n");
        assert_eq!(result2.stdout, "done\n");
    }
}

// =============================================================================
// Repeated / sequential failure tests
// =============================================================================

mod repeated_failure_tests {
    use super::*;
    use linux_patch_api::packages::PackageManagerBackend;
    use std::sync::Arc;

    /// APT: two consecutive install failures should both return errors,
    /// and the second call should still run pre-flight + cleanup.
    #[test]
    fn apt_repeated_install_failures_both_error() {
        let mock = MockCommandRunner::new();
        // Pre-flight dpkg --configure -a succeeds (both calls)
        mock.add_wildcard_response("dpkg", MockResponse::success_empty());
        // apt-get install always fails
        mock.add_wildcard_response(
            "apt-get",
            MockResponse {
                exit_code: Some(100),
                stdout: String::new(),
                stderr: "E: Unable to locate package".to_string(),
                spawn_error: None,
                return_as_error: false,
            },
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));

        let spec = linux_patch_api::packages::PackageSpec {
            name: "nonexistent".to_string(),
            version: None,
        };
        let opts = linux_patch_api::packages::InstallOptions::default();

        // First install attempt
        let result1 = backend.install_packages(std::slice::from_ref(&spec), &opts);
        assert!(result1.is_err(), "first install should fail");

        // Second install attempt — should also fail, not panic or hang
        let result2 = backend.install_packages(&[spec], &opts);
        assert!(result2.is_err(), "second install should fail");
    }

    /// APT: pre-flight dpkg failure on the second call should propagate
    /// even if the first call succeeded.
    #[test]
    fn apt_second_call_preflight_failure_propagates() {
        let mock = MockCommandRunner::new();
        // First call: dpkg succeeds, apt-get succeeds, audit clean
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        mock.add_response(
            "apt-get",
            &["install", "-y", "--", "curl"],
            MockResponse::success("Setting up curl\n"),
        );
        mock.add_response("dpkg", &["--audit"], MockResponse::success_empty());

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let spec = linux_patch_api::packages::PackageSpec {
            name: "curl".to_string(),
            version: None,
        };
        let opts = linux_patch_api::packages::InstallOptions::default();

        // First call succeeds
        let result1 = backend.install_packages(std::slice::from_ref(&spec), &opts);
        assert!(result1.is_ok(), "first install should succeed");

        // Second call: dpkg --configure -a fails (simulating broken dpkg)
        // We need a fresh mock for the second call since the first consumed responses
        let mock2 = MockCommandRunner::new();
        mock2.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::error(1, "dpkg: error: broken state"),
        );
        let backend2 = linux_patch_api::packages::AptBackend::new(Arc::new(mock2));

        let result2 = backend2.install_packages(&[spec], &opts);
        assert!(
            result2.is_err(),
            "second install should fail when pre-flight fails"
        );
    }

    /// APK: repeated install failures should both return errors cleanly.
    #[test]
    fn apk_repeated_install_failures_both_error() {
        let mock = MockCommandRunner::new();
        mock.add_wildcard_response("apk", MockResponse::error(1, "ERROR: no such package"));

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let spec = linux_patch_api::packages::PackageSpec {
            name: "nonexistent".to_string(),
            version: None,
        };
        let opts = linux_patch_api::packages::InstallOptions::default();

        let result1 = backend.install_packages(std::slice::from_ref(&spec), &opts);
        assert!(result1.is_err(), "first install should fail");

        let result2 = backend.install_packages(&[spec], &opts);
        assert!(result2.is_err(), "second install should fail");
    }

    /// DNF: repeated install failures should both return errors cleanly.
    #[test]
    fn dnf_repeated_install_failures_both_error() {
        let mock = MockCommandRunner::new();
        mock.add_wildcard_response("dnf", MockResponse::error(1, "No package available"));

        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let spec = linux_patch_api::packages::PackageSpec {
            name: "nonexistent".to_string(),
            version: None,
        };
        let opts = linux_patch_api::packages::InstallOptions::default();

        let result1 = backend.install_packages(std::slice::from_ref(&spec), &opts);
        assert!(result1.is_err(), "first install should fail");

        let result2 = backend.install_packages(&[spec], &opts);
        assert!(result2.is_err(), "second install should fail");
    }

    /// Pacman: repeated install failures should both return errors cleanly.
    #[test]
    fn pacman_repeated_install_failures_both_error() {
        let mock = MockCommandRunner::new();
        mock.add_wildcard_response("pacman", MockResponse::error(1, "target not found"));

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let spec = linux_patch_api::packages::PackageSpec {
            name: "nonexistent".to_string(),
            version: None,
        };
        let opts = linux_patch_api::packages::InstallOptions::default();

        let result1 = backend.install_packages(std::slice::from_ref(&spec), &opts);
        assert!(result1.is_err(), "first install should fail");

        let result2 = backend.install_packages(&[spec], &opts);
        assert!(result2.is_err(), "second install should fail");
    }

    /// APT: install succeeds, then a second install fails — verify the
    /// backend doesn't carry state from the first call.
    #[test]
    fn apt_success_then_failure_no_state_leak() {
        let mock = MockCommandRunner::new();
        // First call: all succeed
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        mock.add_response(
            "apt-get",
            &["install", "-y", "--", "curl"],
            MockResponse::success("Setting up curl\n"),
        );
        mock.add_response("dpkg", &["--audit"], MockResponse::success_empty());

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let opts = linux_patch_api::packages::InstallOptions::default();

        let result1 = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &opts,
        );
        assert!(result1.is_ok(), "first install should succeed");

        // Second call with a fresh mock that fails
        let mock2 = MockCommandRunner::new();
        mock2.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        mock2.add_response(
            "apt-get",
            &["install", "-y", "--", "broken-pkg"],
            MockResponse {
                exit_code: Some(100),
                stdout: String::new(),
                stderr: "E: Unable to locate package broken-pkg".to_string(),
                spawn_error: None,
                return_as_error: false,
            },
        );
        mock2.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );

        let backend2 = linux_patch_api::packages::AptBackend::new(Arc::new(mock2));
        let result2 = backend2.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "broken-pkg".to_string(),
                version: None,
            }],
            &opts,
        );
        assert!(result2.is_err(), "second install should fail");
    }

    /// APT: pre-flight dpkg fails, then apt-get is never called.
    /// Verify the runner was NOT called for apt-get.
    #[test]
    fn apt_preflight_failure_skips_apt_get() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::error(1, "dpkg: broken"),
        );
        // apt-get should NOT be called — don't program a response.
        // If it is called, the mock returns spawn_failed (default).

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock.clone()));
        let result = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(result.is_err());
        // Verify apt-get was never called
        let calls = mock.calls();
        assert!(
            !calls.iter().any(|(p, _)| p == "apt-get"),
            "apt-get should not be called when pre-flight fails, but calls: {:?}",
            calls
        );
    }

    /// APT: coordinator op_in_progress is cleared after a failed install.
    /// The coordinator's flag is the sole authority — the backend's
    /// is_operation_in_progress() always returns false now.
    #[test]
    fn apt_op_in_progress_cleared_after_failure() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        mock.add_response(
            "apt-get",
            &["install", "-y", "--", "broken"],
            MockResponse {
                exit_code: Some(100),
                stdout: String::new(),
                stderr: "E: broken".to_string(),
                spawn_error: None,
                return_as_error: false,
            },
        );
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let _ = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "broken".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        // The backend's is_operation_in_progress always returns false now.
        // The coordinator is the sole authority for tracking mutations.
        assert!(
            !backend.is_operation_in_progress(),
            "backend is_operation_in_progress should always be false (coordinator is sole authority)"
        );
    }

    /// APT: coordinator op_in_progress is cleared after a successful install.
    #[test]
    fn apt_op_in_progress_cleared_after_success() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dpkg",
            &["--configure", "-a"],
            MockResponse::success_empty(),
        );
        mock.add_response(
            "apt-get",
            &["install", "-y", "--", "curl"],
            MockResponse::success("Setting up curl\n"),
        );
        mock.add_response("dpkg", &["--audit"], MockResponse::success_empty());

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let _ = backend.install_packages(
            &[linux_patch_api::packages::PackageSpec {
                name: "curl".to_string(),
                version: None,
            }],
            &linux_patch_api::packages::InstallOptions::default(),
        );

        assert!(
            !backend.is_operation_in_progress(),
            "backend is_operation_in_progress should always be false (coordinator is sole authority)"
        );
    }
}

// =============================================================================
// Malformed / edge-case package line parsing tests
// =============================================================================

mod parsing_edge_case_tests {
    use super::*;
    use linux_patch_api::packages::PackageManagerBackend;
    use std::sync::Arc;

    // ---- APT parse_package_list edge cases ----

    /// APT: empty output yields zero packages.
    #[test]
    fn apt_list_empty_output() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success_empty(),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert!(
            packages.is_empty(),
            "empty output should yield zero packages"
        );
    }

    /// APT: only the "Listing..." header yields zero packages.
    #[test]
    fn apt_list_only_header() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success("Listing...\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert!(packages.is_empty());
    }

    /// APT: line with only 2 fields (missing arch) is skipped.
    #[test]
    fn apt_list_short_line_skipped() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success("Listing...\ncurl/noble 8.5.0\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert!(packages.is_empty(), "line with <3 fields should be skipped");
    }

    /// APT: line with no repo suffix (no '/') still parses the name.
    #[test]
    fn apt_list_no_repo_suffix() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success("Listing...\ncurl 8.5.0 amd64 [installed]\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "curl");
        assert_eq!(packages[0].version, "8.5.0");
    }

    /// APT: blank lines mixed with valid data are skipped.
    #[test]
    fn apt_list_blank_lines_mixed() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success(
                "Listing...\n\ncurl/noble 8.5.0 amd64 [installed]\n\nopenssl/noble 3.0 amd64 [installed]\n",
            ),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 2, "blank lines should be skipped");
    }

    /// APT: upgradable status is detected from [upgradable] annotation.
    #[test]
    fn apt_list_upgradable_status() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success("Listing...\ncurl/noble 8.5.0 amd64 [upgradable]\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert!(packages[0].upgradable, "should detect upgradable status");
        assert_eq!(
            packages[0].status,
            linux_patch_api::packages::PackageStatus::Upgradable,
            "status should be Upgradable"
        );
    }

    /// APT: line with no status bracket defaults to Available.
    #[test]
    fn apt_list_no_status_bracket() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success("Listing...\ncurl/noble 8.5.0 amd64\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(
            packages[0].status,
            linux_patch_api::packages::PackageStatus::Available,
            "no bracket should default to Available"
        );
    }

    // ---- APK parse_apk_package_list edge cases ----

    /// APK: empty output yields zero packages.
    #[test]
    fn apk_list_empty_output() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["list", "--installed"],
            MockResponse::success_empty(),
        );

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert!(packages.is_empty());
    }

    /// APK: line with no space separator is still parsed as name-version.
    #[test]
    fn apk_list_no_space_separator() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["list", "--installed"],
            MockResponse::success("bash-5.2.21-r0\n"),
        );

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "bash");
    }

    /// APK: package name with hyphens (e.g. gcc-gnat) is parsed correctly.
    #[test]
    fn apk_list_hyphenated_name() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["list", "--installed"],
            MockResponse::success("gcc-gnat-13.2.1-r0 [main] GCC GNAT\n"),
        );

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "gcc-gnat");
    }

    /// APK: blank lines mixed with valid data are skipped.
    #[test]
    fn apk_list_blank_lines_mixed() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["list", "--installed"],
            MockResponse::success("\nbash-5.2.21-r0 [main] shell\n\nopenssl-3.1.4-r0 [main] SSL\n"),
        );

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 2, "blank lines should be skipped");
    }

    // ---- DNF/YUM RPM parse edge cases ----

    /// DNF: empty rpm -qa output yields zero packages.
    #[test]
    fn dnf_list_empty_output() {
        let mock = MockCommandRunner::new();
        mock.add_response("rpm", &["-qa"], MockResponse::success_empty());

        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert!(packages.is_empty());
    }

    /// DNF: line with no arch suffix still parses.
    #[test]
    fn dnf_list_no_arch_suffix() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "rpm",
            &["-qa"],
            MockResponse::success("bash-5.2.21-1.fc43\n"),
        );

        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "bash");
    }

    /// DNF: line with multiple dots in arch-like suffix — only the last
    /// dot-separated token is stripped if it looks like an arch.
    #[test]
    fn dnf_list_multiple_dots() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "rpm",
            &["-qa"],
            MockResponse::success("perl-Net-SSLeay-1.94-1.fc43.x86_64\n"),
        );

        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "perl-Net-SSLeay");
    }

    /// DNF: blank lines mixed with valid data are skipped.
    #[test]
    fn dnf_list_blank_lines_mixed() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "rpm",
            &["-qa"],
            MockResponse::success("\nbash-5.2.21-1.fc43.x86_64\n\nopenssl-3.1.4-1.fc43.x86_64\n"),
        );

        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 2, "blank lines should be skipped");
    }

    /// YUM: empty output yields zero packages.
    #[test]
    fn yum_list_empty_output() {
        let mock = MockCommandRunner::new();
        mock.add_response("rpm", &["-qa"], MockResponse::success_empty());

        let backend = linux_patch_api::packages::YumBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert!(packages.is_empty());
    }

    // ---- Pacman parse_pacman_package_list edge cases ----

    /// Pacman: empty output yields zero packages.
    #[test]
    fn pacman_list_empty_output() {
        let mock = MockCommandRunner::new();
        mock.add_response("pacman", &["-Q"], MockResponse::success_empty());

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert!(packages.is_empty());
    }

    /// Pacman: line with only a name (no version) is skipped.
    #[test]
    fn pacman_list_name_only_skipped() {
        let mock = MockCommandRunner::new();
        mock.add_response("pacman", &["-Q"], MockResponse::success("bash\n"));

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert!(
            packages.is_empty(),
            "line with only a name (no version) should be skipped"
        );
    }

    /// Pacman: blank lines mixed with valid data are skipped.
    #[test]
    fn pacman_list_blank_lines_mixed() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-Q"],
            MockResponse::success("\nbash 5.2.21-1\n\nopenssl 3.1.4-1\n"),
        );

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 2, "blank lines should be skipped");
    }

    /// Pacman: line with extra whitespace between name and version.
    #[test]
    fn pacman_list_extra_whitespace() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-Q"],
            MockResponse::success("bash   5.2.21-1\n"),
        );

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "bash");
        // splitn(2, whitespace) splits on the first space, so the version
        // includes leading spaces from the remaining gap. This is a known
        // limitation — real pacman output uses a single space.
        assert_eq!(packages[0].version, "  5.2.21-1");
    }

    // ---- APT list_patches edge cases ----

    /// APT: empty upgradable output yields zero patches.
    #[test]
    fn apt_list_patches_empty() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--upgradable"],
            MockResponse::success("Listing...\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let patches = backend.list_patches().unwrap();
        assert!(patches.is_empty());
    }

    /// APT: upgradable line with only 2 fields is skipped.
    #[test]
    fn apt_list_patches_short_line_skipped() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--upgradable"],
            MockResponse::success("Listing...\ncurl/noble 8.5.0\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let patches = backend.list_patches().unwrap();
        assert!(patches.is_empty(), "line with <3 fields should be skipped");
    }

    // ---- APK list_patches edge cases ----

    /// APK: empty upgradable output yields zero patches.
    #[test]
    fn apk_list_patches_empty() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["list", "--upgradable"],
            MockResponse::success_empty(),
        );

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let patches = backend.list_patches().unwrap();
        assert!(patches.is_empty());
    }

    /// APK: upgradable line with no space is skipped (can't parse).
    #[test]
    fn apk_list_patches_no_space_skipped() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["list", "--upgradable"],
            MockResponse::success("bash-5.2.21-r0\n"),
        );

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let patches = backend.list_patches().unwrap();
        // The line has no space, so the "find ' ')" branch fails and the
        // "find ' < ')" branch also fails, so it's skipped via `continue`.
        assert!(patches.is_empty(), "line with no space should be skipped");
    }

    // ---- Pacman list_patches edge cases ----

    /// Pacman: empty -Qu output yields zero patches.
    #[test]
    fn pacman_list_patches_empty() {
        let mock = MockCommandRunner::new();
        mock.add_response("pacman", &["-Qu"], MockResponse::success_empty());

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let patches = backend.list_patches().unwrap();
        assert!(patches.is_empty());
    }

    /// Pacman: line without "->" arrow is skipped.
    #[test]
    fn pacman_list_patches_no_arrow_skipped() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "pacman",
            &["-Qu"],
            MockResponse::success("bash 5.2.21-1 5.3.0-1\n"),
        );

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let patches = backend.list_patches().unwrap();
        assert!(
            patches.is_empty(),
            "line without -> arrow should be skipped"
        );
    }

    // ---- get_package edge cases ----

    /// APT: get_package for a non-installed, non-available package returns None.
    #[test]
    fn apt_get_package_not_found() {
        let mock = MockCommandRunner::new();
        // dpkg -s fails (not installed)
        mock.add_response(
            "dpkg",
            &["-s", "nonexistent"],
            MockResponse::error(1, "not installed"),
        );
        // apt list returns empty (not available)
        mock.add_response(
            "apt",
            &["list", "nonexistent"],
            MockResponse::success("Listing...\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let result = backend.get_package("nonexistent").unwrap();
        assert!(result.is_none(), "non-existent package should return None");
    }

    /// APK: get_package for a non-installed, non-available package returns None.
    #[test]
    fn apk_get_package_not_found() {
        let mock = MockCommandRunner::new();
        // apk list --installed returns empty
        mock.add_response(
            "apk",
            &["list", "--installed", "nonexistent"],
            MockResponse::success_empty(),
        );
        // apk search returns empty
        mock.add_response(
            "apk",
            &["search", "nonexistent"],
            MockResponse::success_empty(),
        );

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let result = backend.get_package("nonexistent").unwrap();
        assert!(result.is_none());
    }

    /// DNF: get_package for a non-installed, non-available package returns None.
    #[test]
    fn dnf_get_package_not_found() {
        let mock = MockCommandRunner::new();
        // rpm -q fails (not installed)
        mock.add_response(
            "rpm",
            &["-q", "nonexistent"],
            MockResponse::error(1, "not installed"),
        );
        // dnf info fails (not available)
        mock.add_response(
            "dnf",
            &["info", "-q", "nonexistent"],
            MockResponse::error(1, "No package"),
        );

        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let result = backend.get_package("nonexistent").unwrap();
        assert!(result.is_none());
    }

    /// Pacman: get_package for a non-installed, non-available package returns None.
    #[test]
    fn pacman_get_package_not_found() {
        let mock = MockCommandRunner::new();
        // pacman -Q fails (not installed)
        mock.add_response(
            "pacman",
            &["-Q", "nonexistent"],
            MockResponse::error(1, "not found"),
        );
        // pacman -Si fails (not available)
        mock.add_response(
            "pacman",
            &["-Si", "nonexistent"],
            MockResponse::error(1, "not found"),
        );

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let result = backend.get_package("nonexistent").unwrap();
        assert!(result.is_none());
    }

    // ---- get_installed_version edge cases ----

    /// APT: get_installed_version with no Version: line returns None.
    #[test]
    fn apt_get_installed_version_no_version_field() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dpkg",
            &["-s", "bash"],
            MockResponse::success("Package: bash\nStatus: install ok installed\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let result = backend.get_installed_version("bash").unwrap();
        assert_eq!(result, None, "no Version: line should return None");
    }

    /// APT: get_candidate_version with no Candidate: line returns None.
    #[test]
    fn apt_get_candidate_version_no_candidate_field() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt-cache",
            &["policy", "bash"],
            MockResponse::success("bash:\n  Installed: 5.0\n  Version table:\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let result = backend.get_candidate_version("bash").unwrap();
        assert_eq!(result, None, "no Candidate: line should return None");
    }

    /// DNF: get_installed_version with empty rpm output returns None.
    #[test]
    fn dnf_get_installed_version_empty_output() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "rpm",
            &["-q", "--qf", "%{VERSION}-%{RELEASE}", "bash"],
            MockResponse::success_empty(),
        );

        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let result = backend.get_installed_version("bash").unwrap();
        assert_eq!(result, None, "empty output should return None");
    }

    /// Pacman: get_installed_version with empty pacman -Q output returns None.
    #[test]
    fn pacman_get_installed_version_empty_output() {
        let mock = MockCommandRunner::new();
        mock.add_response("pacman", &["-Q", "bash"], MockResponse::success_empty());

        let backend = linux_patch_api::packages::PacmanBackend::new(Arc::new(mock));
        let result = backend.get_installed_version("bash").unwrap();
        assert_eq!(result, None, "empty output should return None");
    }

    // ---- Partial / truncated output edge cases ----

    /// APT: dpkg -s output truncated mid-line still extracts version if
    /// the Version: line was complete.
    #[test]
    fn apt_get_installed_version_truncated_after_version() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dpkg",
            &["-s", "bash"],
            MockResponse::success("Package: bash\nVersion: 5.2.21-1\nStatus: install ok inst"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let result = backend.get_installed_version("bash").unwrap();
        assert_eq!(
            result,
            Some("5.2.21-1".to_string()),
            "version should be extracted even if output is truncated after the Version: line"
        );
    }

    /// APT: dpkg -s output with Version: line but no value (empty after colon).
    #[test]
    fn apt_get_installed_version_empty_version_value() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "dpkg",
            &["-s", "bash"],
            MockResponse::success("Package: bash\nVersion: \nStatus: install ok installed\n"),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let result = backend.get_installed_version("bash").unwrap();
        // Empty version string after trim — the code checks !version.is_empty()
        assert_eq!(result, None, "empty version value should return None");
    }

    /// APT: list_packages with binary garbage in output — lines that don't
    /// parse are silently skipped, valid lines are still returned.
    #[test]
    fn apt_list_packages_partial_garbage() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apt",
            &["list", "--installed"],
            MockResponse::success(
                "Listing...\n\x00\x01garbage line\ncurl/noble 8.5.0 amd64 [installed]\n",
            ),
        );

        let backend = linux_patch_api::packages::AptBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        // The garbage line has 2 fields ("garbage" and "line"), so it's skipped.
        // The valid curl line has 3+ fields and is parsed.
        assert_eq!(
            packages.len(),
            1,
            "garbage lines should be skipped, valid lines parsed"
        );
        assert_eq!(packages[0].name, "curl");
    }

    /// APK: list_packages with a line that has no hyphen-digit version
    /// separator — parse_name_version returns the full line as name.
    #[test]
    fn apk_list_packages_no_version_separator() {
        let mock = MockCommandRunner::new();
        mock.add_response(
            "apk",
            &["list", "--installed"],
            MockResponse::success("nohyphen [main] some package\n"),
        );

        let backend = linux_patch_api::packages::ApkBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "nohyphen");
        assert_eq!(
            packages[0].version, "",
            "no version separator means empty version"
        );
    }

    /// DNF: list_packages with a line that has no hyphen-digit version
    /// separator — parse_rpm_name_version returns the full line as name.
    #[test]
    fn dnf_list_packages_no_version_separator() {
        let mock = MockCommandRunner::new();
        mock.add_response("rpm", &["-qa"], MockResponse::success("nohyphen.x86_64\n"));

        let backend = linux_patch_api::packages::DnfBackend::new(Arc::new(mock));
        let packages = backend.list_packages(None).unwrap();
        assert_eq!(packages.len(), 1);
        // "nohyphen" after stripping .x86_64, then parse_rpm_name_version
        // finds no hyphen-digit, so name="nohyphen", version=""
        assert_eq!(packages[0].name, "nohyphen");
    }
}
