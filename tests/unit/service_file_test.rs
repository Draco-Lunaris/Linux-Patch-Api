//! Static test: verify the systemd service file does not contain
//! sandboxing directives that are incompatible with package management.
//!
//! The agent executes OS package-manager commands (apt-get, dnf, apk,
//! pacman) and arbitrary distribution maintainer scripts as child
//! processes. Those child processes must operate against the real host
//! filesystem, mount namespace, hostname namespace, clock namespace,
//! kernel-module tree, and syscall environment.
//!
//! Any systemd sandboxing directive that creates a private namespace or
//! restricts kernel/filesystem access will cause maintainer scripts to
//! silently fail or produce corrupted host state. The
//! `ProtectKernelModules=true` failure on a production host
//! demonstrated this: update-initramfs generated an initramfs without
//! kernel modules because `/usr/lib/modules` was masked, and the next
//! boot failed.
//!
//! This test inspects the source service file at `configs/linux-patch-api.service`
//! — the same file installed by `scripts/build-package.sh` (line 78) and
//! `debian/rules` (line 24). If any of the forbidden directives appear as
//! active (non-comment) lines, the test fails.

use std::fs;

/// Directives that are incompatible with package management.
///
/// Each entry is matched as a line that starts with the directive name
/// (after optional whitespace). Comment lines (starting with `#`) are
/// ignored, so documenting *why* a directive was removed is safe.
const FORBIDDEN_DIRECTIVES: &[&str] = &[
    "ProtectKernelModules",
    "ProtectKernelTunables",
    "ProtectKernelLogs",
    "ProtectHome",
    "PrivateTmp",
    "ProtectHostname",
    "ProtectClock",
    "RestrictNamespaces",
    "SystemCallFilter",
    "ProtectSystem",
    "NoNewPrivileges",
    "RestrictSUIDSGID",
    "CapabilityBoundingSet",
    "AmbientCapabilities",
];

/// Path to the source service file used by package builds.
const SERVICE_FILE_PATH: &str = "configs/linux-patch-api.service";

#[test]
fn service_file_has_no_incompatible_sandboxing_directives() {
    let content = fs::read_to_string(SERVICE_FILE_PATH).unwrap_or_else(|e| {
        panic!(
            "Failed to read service file at {}: {}",
            SERVICE_FILE_PATH, e
        )
    });

    let violations: Vec<(usize, &str)> = content
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                return None;
            }
            for directive in FORBIDDEN_DIRECTIVES {
                if trimmed.starts_with(directive) {
                    return Some((i + 1, line));
                }
            }
            None
        })
        .collect();

    if !violations.is_empty() {
        let mut msg = String::from(
            "Service file contains sandboxing directives that are \
             incompatible with package management:\n",
        );
        for (lineno, line) in &violations {
            msg.push_str(&format!("  line {}: {}\n", lineno, line));
        }
        msg.push_str(
            "\nPackage-manager commands and package maintainer scripts \
             must execute against the real host filesystem, mount \
             namespace, hostname namespace, clock namespace, \
             kernel-module tree, and syscall environment.\n\
             See the comment block in configs/linux-patch-api.service \
             for the rationale behind each removal.",
        );
        panic!("{}", msg);
    }
}

#[test]
fn service_file_exists_and_is_readable() {
    let content =
        fs::read_to_string(SERVICE_FILE_PATH).expect("service file must exist and be readable");
    assert!(
        content.contains("[Service]"),
        "service file does not contain [Service] section"
    );
    assert!(
        content.contains("ExecStart=/usr/bin/linux-patch-api"),
        "service file does not contain expected ExecStart"
    );
}

#[test]
fn debian_rules_installs_same_service_file() {
    let rules =
        fs::read_to_string("debian/rules").expect("debian/rules must exist and be readable");
    assert!(
        rules.contains("configs/linux-patch-api.service"),
        "debian/rules does not reference configs/linux-patch-api.service — \
         the package build must install the same service file that this \
         test inspects"
    );
}

#[test]
fn build_script_installs_same_service_file() {
    let build_script = fs::read_to_string("scripts/build-package.sh")
        .expect("scripts/build-package.sh must exist and be readable");
    assert!(
        build_script.contains("configs/linux-patch-api.service"),
        "scripts/build-package.sh does not reference \
         configs/linux-patch-api.service — the package build must install \
         the same service file that this test inspects"
    );
}
