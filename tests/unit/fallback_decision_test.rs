//! Deterministic tests for the systemd fallback decision script
//! (`configs/linux-patch-api-fallback-decision.sh`).
//!
//! These tests spawn the script with controlled environment
//! variables (STATE_FILE, MARKER, ACTIVE_STATE) and verify the exit
//! code matches the documented state machine.

use std::path::PathBuf;
use std::process::Command;

fn script_path() -> PathBuf {
    // Walk up from CARGO_MANIFEST_DIR/tests/unit to find configs/.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("configs/linux-patch-api-fallback-decision.sh");
    p
}

fn write_state_file(dir: &std::path::Path, json: &str) -> std::path::PathBuf {
    let p = dir.join("upgrade-state.json");
    std::fs::write(&p, json).unwrap();
    p
}

fn run_script(
    state_file: Option<&std::path::Path>,
    marker: bool,
    active_state: &str,
) -> std::process::Output {
    let tmp = std::env::temp_dir().join(format!(
        "fallback_test_{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&tmp).unwrap();

    let mut cmd = Command::new("sh");
    cmd.env("SERVICE_NAME", "linux-patch-api.service");
    cmd.env("ACTIVE_STATE", active_state);

    if let Some(sf) = state_file {
        cmd.env("STATE_FILE", sf);
    } else {
        // Point at a non-existent path inside the temp dir
        cmd.env(
            "STATE_FILE",
            tmp.join("missing-state.json").to_str().unwrap(),
        );
    }

    if marker {
        let marker_path = tmp.join("upgrade-pending");
        std::fs::write(&marker_path, "").unwrap();
        cmd.env("MARKER", marker_path.to_str().unwrap());
    } else {
        cmd.env(
            "MARKER",
            tmp.join("no-marker").to_str().unwrap(),
        );
    }

    cmd.arg(script_path());
    cmd.output().expect("failed to run fallback script")
}

fn assert_exit(output: &std::process::Output, expected: i32, case: &str) {
    let actual = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        actual,
        Some(expected),
        "{}: expected exit {}, got {:?}\nstdout: {}\nstderr: {}",
        case,
        expected,
        actual,
        stdout,
        stderr
    );
}

#[test]
fn fallback_active_state_activating_skips() {
    let tmp = std::env::temp_dir().join(format!("fs_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    write_state_file(&tmp, r#"{"state": "restart_pending"}"#);
    let out = run_script(Some(&tmp.join("upgrade-state.json")), true, "activating");
    assert_exit(&out, 55, "activating");
}

#[test]
fn fallback_active_state_active_no_marker_is_noop() {
    let out = run_script(None, false, "active");
    assert_exit(&out, 0, "active+no marker");
}

#[test]
fn fallback_active_state_active_with_marker_deadline_in_future_skips() {
    let tmp = std::env::temp_dir().join(format!("fs_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    // Deadline 1 hour in the future
    let future = chrono::Utc::now() + chrono::Duration::hours(1);
    let json = format!(
        r#"{{"state": "restart_pending", "restart_deadline": "{}"}}"#,
        future.to_rfc3339()
    );
    write_state_file(&tmp, &json);
    let out = run_script(Some(&tmp.join("upgrade-state.json")), true, "active");
    assert_exit(&out, 55, "active+marker+future deadline");
}

#[test]
fn fallback_active_state_active_with_marker_deadline_past_restarts() {
    let tmp = std::env::temp_dir().join(format!("fs_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    // Deadline 1 hour in the past
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    let json = format!(
        r#"{{"state": "restart_pending", "restart_deadline": "{}"}}"#,
        past.to_rfc3339()
    );
    write_state_file(&tmp, &json);
    let out = run_script(Some(&tmp.join("upgrade-state.json")), true, "active");
    assert_exit(&out, 0, "active+marker+past deadline");
}

#[test]
fn fallback_active_state_inactive_with_marker_valid_state_restarts() {
    let tmp = std::env::temp_dir().join(format!("fs_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    write_state_file(&tmp, r#"{"state": "restart_pending"}"#);
    let out = run_script(Some(&tmp.join("upgrade-state.json")), true, "inactive");
    assert_exit(&out, 0, "inactive+marker+valid state");
}

#[test]
fn fallback_active_state_failed_with_marker_valid_state_restarts() {
    let tmp = std::env::temp_dir().join(format!("fs_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    write_state_file(&tmp, r#"{"state": "restart_pending"}"#);
    let out = run_script(Some(&tmp.join("upgrade-state.json")), true, "failed");
    assert_exit(&out, 0, "failed+marker+valid state");
}

#[test]
fn fallback_inactive_no_marker_is_noop() {
    let out = run_script(None, false, "inactive");
    assert_exit(&out, 0, "inactive+no marker");
}

#[test]
fn fallback_inactive_with_marker_missing_state_file_fails_closed() {
    // No state file, marker present, service inactive
    let out = run_script(None, true, "inactive");
    assert_exit(&out, 55, "inactive+marker+missing state");
}

#[test]
fn fallback_active_with_marker_no_deadline_fails_closed() {
    let tmp = std::env::temp_dir().join(format!("fs_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    // State present but no restart_deadline
    write_state_file(&tmp, r#"{"state": "restart_pending"}"#);
    let out = run_script(Some(&tmp.join("upgrade-state.json")), true, "active");
    assert_exit(&out, 55, "active+marker+no deadline");
}

#[test]
fn fallback_active_with_marker_corrupt_state_fails_closed() {
    let tmp = std::env::temp_dir().join(format!("fs_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let p = tmp.join("upgrade-state.json");
    std::fs::write(&p, "this is not json").unwrap();
    let out = run_script(Some(&p), true, "active");
    assert_exit(&out, 55, "active+marker+corrupt state");
}

#[test]
fn fallback_unrecognized_active_state_fails_closed() {
    let out = run_script(None, false, "maintenance");
    assert_exit(&out, 55, "unrecognized state");
}

#[test]
fn fallback_systemctl_show_failure_fails_closed() {
    // Simulate `systemctl show` returning empty output (e.g. unit not
    // found, D-Bus failure, or systemctl not installed). The script
    // should fail-closed with exit 55, not restart.
    //
    // We create a fake `systemctl` in a temp bin dir that prints nothing
    // and exits 0. The script's `$(systemctl show ... || true)` will
    // capture empty output, then the `[ -z "$resolved_state" ]` check
    // fires and the script exits 55.
    let tmp = std::env::temp_dir().join(format!("fs_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let fake_bin = tmp.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    // Write a fake systemctl that produces no output
    let fake_systemctl = fake_bin.join("systemctl");
    std::fs::write(
        &fake_systemctl,
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_systemctl, std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    let marker_path = tmp.join("upgrade-pending");
    std::fs::write(&marker_path, "").unwrap();

    let mut cmd = Command::new("sh");
    cmd.env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()));
    // Do NOT set ACTIVE_STATE — let the script fall through to systemctl.
    cmd.env_remove("ACTIVE_STATE");
    cmd.env("STATE_FILE", tmp.join("missing-state.json").to_str().unwrap());
    cmd.env("MARKER", marker_path.to_str().unwrap());
    cmd.env("SERVICE_NAME", "linux-patch-api.service");
    cmd.arg(script_path());
    let out = cmd.output().expect("failed to run fallback script");
    assert_exit(&out, 55, "systemctl show failure (empty output)");
}