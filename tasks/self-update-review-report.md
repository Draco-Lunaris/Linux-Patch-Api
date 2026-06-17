# Code Review Report: Self-Update Feature

**Branch**: `feature/self-update` vs `v1.4.3`
**Files Changed**: 14 files, +2154/-128 lines
**Reviewer**: Code Reviewer (agentic, multi-dimensional)
**Date**: 2026-06-16

---

## Executive Summary

The self-update feature implements a sound architecture — delegating the upgrade to a detached systemd unit instead of running apt-get from the agent process. However, there are **2 critical bugs** that would cause production failures, **5 high-severity issues** including missing error handling and injection vectors, and several medium-severity quality concerns. The feature **cannot ship in its current state** without fixing the critical and high-severity items.

## Severity Distribution

| Severity | Count |
|----------|-------|
| Critical | 2     |
| High     | 5     |
| Medium   | 7     |
| Low      | 4     |
| Info     | 2     |

---

## CRITICAL Findings

### [CRITICAL-1] prerm Disables Service on Upgrade — Service Won't Survive Reboot

- **Dimension**: Correctness
- **CWE**: CWE-754 (Improper Check for Unusual or Exceptional Conditions)
- **Location**: `debian/prerm:19-21`
- **Evidence**:
```bash
# prerm runs on BOTH remove and upgrade
if [ "$1" = "remove" ] || [ "$1" = "upgrade" ]; then
    systemctl stop linux-patch-api.service
    systemctl disable linux-patch-api.service  # ← DISABLES on upgrade!
fi
```
- **Impact**: After self-update, `systemctl disable` runs in prerm. The new package's postinst calls `systemctl start` but **never re-enables** the service. The agent will be running after the upgrade but will NOT start on next reboot. This breaks the self-update feature completely — every upgrade silently disables auto-start.
- **Recommendation**: Only disable on removal, not on upgrade:
```bash
if [ "$1" = "remove" ]; then
    systemctl stop linux-patch-api.service
    systemctl disable linux-patch-api.service
elif [ "$1" = "upgrade" ]; then
    systemctl stop linux-patch-api.service
    # Do NOT disable on upgrade — postinst will restart
fi
```

### [CRITICAL-2] Handler Bypasses Job System — No Concurrency Protection

- **Dimension**: Correctness, Security
- **CWE**: CWE-362 (Concurrent Execution with Improper Synchronization)
- **Location**: `src/api/handlers/system.rs:394-490`
- **Evidence**: The `update_self` handler writes a request file and starts the systemd unit directly, without going through the `JobManager`. Every other async operation (reboot, install, update, remove, patch) uses the job queue for concurrency control, capacity limits, and status tracking.
```rust
pub async fn update_self(body: web::Json<SelfUpdateRequest>, _req: HttpRequest) -> impl Responder {
    // ... validates version ...
    // ... writes request file ...
    // ... starts systemd unit directly — NO job creation
    let start_result = std::process::Command::new("systemctl")
        .args(["start", "--no-block", "linux-patch-api-update.service"])
        .status();
    // ...
}
```
- **Impact**: Multiple concurrent self-update requests can race each other, writing overlapping request files and starting the update service multiple times. No queue capacity check means unlimited self-updates can be triggered, potentially exhausting system resources. No job ID is returned for status polling.
- **Recommendation**: Either route through `JobManager::create_job(JobOperation::SelfUpdate, ...)` with proper concurrency limits, or add explicit mutex/guard logic (e.g., check for existing request file or running update service before proceeding).

---

## HIGH Findings

### [HIGH-1] No Failure Marker on Package Upgrade Failure

- **Dimension**: Correctness
- **CWE**: CWE-755 (Improper Handling of Exceptional Conditions)
- **Location**: `configs/self-update.sh:61-93`
- **Evidence**: `set -euo pipefail` on line 7 causes immediate script exit if any package manager command fails. Lines 95-117 (version comparison, marker write, cleanup) never execute.
```bash
set -euo pipefail  # line 7
# ...
apt-get install -y --allow-downgrades -- "$PKG_NAME=$TARGET_VERSION" 2>&1  # line 64
# If this fails, script exits immediately. No marker written.
```
- **Impact**: The agent's "pending" marker persists indefinitely. The status endpoint returns `"status": "pending"` forever. Operators have no visibility into the failure.
- **Recommendation**: Trap errors and write a failure marker:
```bash
cleanup() {
    local exit_code=$?
    if [ $exit_code -ne 0 ]; then
        NEW_VERSION=$(dpkg-query -W -f='${Version}' "$PKG_NAME" 2>/dev/null || echo "unknown")
        printf '{"previous_version":"%s","new_version":"%s","changed":false,"status":"failed","error":"Upgrade command exited with code %d","at":"%s"}\n' \
            "$PREV_VERSION" "$NEW_VERSION" "$exit_code" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$MARKER_PATH"
        rm -f "$REQUEST_PATH"
    fi
}
trap cleanup EXIT
```

### [HIGH-2] JSON Injection via Unescaped Shell Variables in Marker File

- **Dimension**: Security
- **CWE**: CWE-77 (Improper Neutralization of Special Elements), CWE-79 (Injection)
- **Location**: `configs/self-update.sh:105-114`
- **Evidence**:
```bash
cat > "$MARKER_PATH" << EOF
{
  "previous_version": "$PREV_VERSION",
  "new_version": "$NEW_VERSION",
  "changed": $CHANGED,
  "error": null,
  "status": "success",
  "at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
```
- **Impact**: `$PREV_VERSION` and `$NEW_VERSION` come from `dpkg-query`/`rpm -q` output. If either contains `"`, `\`, or control characters, the resulting JSON is malformed. A malicious repo could inject arbitrary JSON content into the marker file, potentially confusing the status endpoint.
- **Recommendation**: Use `python3` or `jq` for proper JSON serialization:
```bash
python3 -c "
import json, datetime
marker = {
    'previous_version': '$PREV_VERSION',  # still needs escaping
    'new_version': '$NEW_VERSION',
    'changed': $CHANGED,
    'status': 'success',
    'error': None,
    'at': datetime.datetime.utcnow().strftime('%Y-%m-%dT%H:%M:%SZ')
}
print(json.dumps(marker, indent=2))
" > "$MARKER_PATH"
```
Better: read both versions into Python and construct JSON entirely within Python, avoiding any shell interpolation.

### [HIGH-3] Silent Fallthrough on python3 Parse Failure → Wrong Version Upgrade

- **Dimension**: Correctness
- **CWE**: CWE-754 (Improper Check for Unusual or Exceptional Conditions)
- **Location**: `configs/self-update.sh:21`
- **Evidence**:
```bash
TARGET_VERSION=$(python3 -c "import json,sys; d=json.load(open('$REQUEST_PATH')); print(d.get('target_version') or '')" 2>/dev/null || '')
```
- **Impact**: If python3 is not installed, the JSON is malformed, or there's a permissions error, `TARGET_VERSION` silently becomes empty. The script then upgrades to the **latest available version** instead of the pinned version. This violates user intent and could install an unexpected major version.
- **Recommendation**: Check python3 parse result explicitly and fail:
```bash
TARGET_VERSION=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("target_version") or "")' "$REQUEST_PATH" 2>&1)
if [ $? -ne 0 ]; then
    echo "Failed to parse self-update request file" >&2
    printf '{"previous_version":"unknown","new_version":"unknown","changed":false,"status":"failed","error":"Failed to parse request file","at":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$MARKER_PATH"
    exit 1
fi
```

### [HIGH-4] Missing TimeoutStartSec in Update Service Unit

- **Dimension**: Correctness
- **CWE**: CWE-400 (Uncontrolled Resource Consumption)
- **Location**: `configs/linux-patch-api-update.service`
- **Evidence**:
```ini
[Service]
Type=oneshot
ExecStart=/usr/lib/linux-patch-api/self-update.sh
# No TimeoutStartSec — default is 900s (15 min) for oneshot
```
- **Impact**: If the package manager hangs (network failure, interactive prompt, dpkg lock), the oneshot unit will run for up to 15 minutes before systemd kills it. During this time, the agent is stopped (prerm) and no failure marker is written. The agent appears stuck in "pending" indefinitely.
- **Recommendation**: Add explicit timeout:
```ini
[Service]
Type=oneshot
ExecStart=/usr/lib/linux-patch-api/self-update.sh
TimeoutStartSec=300
```

### [HIGH-5] Inconsistent Restart Behavior Across Package Formats

- **Dimension**: Correctness
- **CWE**: CWE-754
- **Locations**: `debian/postinst:96`, `configs/linux-patch-api.install:56`, `linux-patch-api.spec:132`, `configs/linux-patch-api.post-install:9-11`
- **Evidence**:
| Format | Upgrade Path | Starts Service? | Re-enables? |
|--------|-------------|-----------------|-------------|
| Debian | postinst `$2` non-empty | ✅ Yes (`systemctl start`) | ❌ No re-enable |
| Arch | post_upgrade | ❌ No | ❌ No |
| RPM | %post `$1 > 1` | ❌ No | ❌ No |
| Alpine | post-install (upgrade detected) | ❌ No | ❌ No |
- **Impact**: Self-update works end-to-end ONLY on Debian. On Arch, RPM, and Alpine, the agent stops (prerm) and never restarts after upgrade. The self-update feature is effectively broken on 3 out of 4 supported platforms.
- **Recommendation**: All post-install/post_upgrade scripts must call `systemctl start linux-patch-api.service` on upgrade. The Debian postinst correctly does this; the others need the same logic.

---

## MEDIUM Findings

### [MEDIUM-1] SelfUpdate JobOperation Variant Is Dead Code

- **Dimension**: Quality
- **Location**: `src/jobs/manager.rs:47`
- **Evidence**: `SelfUpdate` is defined as a variant of `JobOperation` but is never used in any match arm, job creation, or dispatch logic. The handler bypasses the job system entirely.
- **Recommendation**: Either wire `SelfUpdate` into the job system (recommended for CRITICAL-2) or remove it to avoid confusion.

### [MEDIUM-2] MAX_RESTART_DELAY_SECONDS Unused Constant

- **Dimension**: Quality
- **Location**: `src/packages/mod.rs:20`
- **Evidence**: `pub const MAX_RESTART_DELAY_SECONDS: u64 = 300;` is defined but never referenced anywhere in the codebase.
- **Recommendation**: Either use it (e.g., validate a `restart_delay` field in `SelfUpdateRequest`) or remove it.

### [MEDIUM-3] Non-Atomic Write of Request File

- **Dimension**: Correctness
- **CWE**: CWE-367 (TOCTOU)
- **Location**: `src/packages/mod.rs:2996-3000`
- **Evidence**:
```rust
std::fs::write(path, serde_json::to_string_pretty(&request)?)?;
```
- **Impact**: If the agent crashes mid-write, a partial JSON file is left. The shell script's python3 parser fails, `2>/dev/null` suppresses the error, and `|| ''` causes `TARGET_VERSION` to become empty → unintended upgrade to latest.
- **Recommendation**: Write to temp file, then rename atomically:
```rust
let tmp_path = path.with_extension("request.tmp");
std::fs::write(&tmp_path, serde_json::to_string_pretty(&request)?)?;
std::fs::rename(&tmp_path, path)?;
```

### [MEDIUM-4] Shell Variable Embedded in Python String Literal

- **Dimension**: Security
- **CWE**: CWE-78 (OS Command Injection)
- **Location**: `configs/self-update.sh:21`
- **Evidence**:
```bash
TARGET_VERSION=$(python3 -c "import json,sys; d=json.load(open('$REQUEST_PATH')); print(d.get('target_version') or '')" 2>/dev/null || '')
```
- **Impact**: While `REQUEST_PATH` is currently a hardcoded safe constant, embedding shell variables in Python string literals is a latent injection vector. If the path ever contained a single quote, it would break out of the Python string.
- **Recommendation**: Pass path as a command-line argument:
```bash
TARGET_VERSION=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("target_version") or "")' "$REQUEST_PATH")
```

### [MEDIUM-5] No Signal Handling in self-update.sh

- **Dimension**: Correctness
- **CWE**: CWE-431 (Missing Handler for Exceptional Conditions)
- **Location**: `configs/self-update.sh` (entire script)
- **Impact**: If systemd sends SIGTERM (timeout or `systemctl stop`), the script exits immediately without writing a failure marker. Package manager operations can be interrupted mid-transaction.
- **Recommendation**: Add signal handlers:
```bash
cleanup_on_signal() {
    echo "Self-update interrupted by signal" >&2
    NEW_VERSION=$(dpkg-query -W -f='${Version}' "$PKG_NAME" 2>/dev/null || echo "unknown")
    printf '{"previous_version":"%s","new_version":"%s","changed":false,"status":"failed","error":"Interrupted by signal","at":"%s"}\n' \
        "$PREV_VERSION" "$NEW_VERSION" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$MARKER_PATH"
    rm -f "$REQUEST_PATH"
    exit 1
}
trap cleanup_on_signal TERM INT HUP
```

### [MEDIUM-6] Missing systemd Security Hardening for Update Service

- **Dimension**: Security
- **CWE**: CWE-250 (Execution with Unnecessary Privileges)
- **Location**: `configs/linux-patch-api-update.service`
- **Evidence**: The unit runs as root with no sandboxing directives. While root is needed for package management, some restrictions are still safe.
- **Recommendation**: Add hardening:
```ini
[Service]
Type=oneshot
ExecStart=/usr/lib/linux-patch-api/self-update.sh
TimeoutStartSec=300
ProtectHome=yes
PrivateTmp=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/linux_patch_api
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM
```

### [MEDIUM-7] World-Readable Marker and Request Files

- **Dimension**: Security
- **CWE**: CWE-200 (Information Exposure)
- **Locations**: `configs/self-update.sh:16,27,47,105`, `src/packages/mod.rs:2979,3000`
- **Impact**: Version information and update request details are readable by any local user.
- **Recommendation**: Set `chmod 600` on both files after writing, and ensure `/var/lib/linux_patch_api/` is `0700` root-owned.

---

## LOW Findings

### [LOW-1] No Length Limit on TARGET_VERSION in Shell Script

- **Dimension**: Security
- **CWE**: CWE-20 (Improper Input Validation)
- **Location**: `configs/self-update.sh:24-30`
- **Evidence**: The Rust validator enforces `MAX_NAME_LENGTH` (256 chars) but the shell script regex has no length check.
- **Recommendation**: Add `${#TARGET_VERSION} -gt 256` check before regex validation.

### [LOW-2] NEVRA Parsing Ambiguity for dnf/yum Version Pinning

- **Dimension**: Correctness
- **CWE**: CWE-20
- **Location**: `configs/self-update.sh:71,78`
- **Impact**: `dnf upgrade -y -- "$PKG_NAME-$TARGET_VERSION"` is ambiguous for versions containing hyphens.
- **Recommendation**: Document the limitation or use explicit NEVRA format.

### [LOW-3] Log Injection via Unsanitized Version in Error Output

- **Dimension**: Security
- **CWE**: CWE-117 (Log Injection)
- **Location**: `configs/self-update.sh:26`
- **Recommendation**: Sanitize before logging: `tr -cd 'a-zA-Z0-9+.:~_-'`

### [LOW-4] TOCTOU Between Request File Existence Check and Read

- **Dimension**: Security
- **CWE**: CWE-367
- **Location**: `configs/self-update.sh:14-21`
- **Recommendation**: Remove separate existence check; handle missing file in the read.

---

## INFO Observations

### [INFO-1] No Integrity Verification on Request File

- The request file at `/var/lib/linux_patch_api/self-update.request` is read and trusted without HMAC/signature verification. A local root attacker could modify the `target_version`. This is acceptable given the threat model (root required to write to `/var/lib/linux_patch_api/`), but defense-in-depth would add an HMAC.

### [INFO-2] Prohibited Pattern Compliance ✅

All five prohibited patterns are absent:
- ✅ No `file_install` or `install_url` patterns
- ✅ No `pkill` or `kill` patterns
- ✅ No staged/temp files that could be hijacked
- ✅ No self-cleanup (script deleting itself)
- ✅ No apt-get from the agent process

---

## Positive Observations

1. **Architecture is sound**: Delegating the upgrade to a detached systemd unit is the correct approach — the agent process is killed by dpkg's prerm during upgrade, and the update service survives in its own cgroup.
2. **Version validation is thorough**: The Rust `validate_version_string` function uses a strict allowlist regex that blocks all shell metacharacters, path separators, and whitespace.
3. **Marker file pattern is good**: Writing a status marker that the restarted agent can read at `/system/update/status` is a clean design for cross-process communication.
4. **Multi-package-manager support**: The script correctly handles apt, dnf, yum, apk, and pacman with appropriate flags.
5. **`--` separator used consistently**: All package manager commands use `--` to prevent option injection.
6. **Package name pinned via constant**: `SELF_PACKAGE_NAME` is derived from `CARGO_PKG_NAME`, not user input.
7. **Upgrade-aware packaging**: Debian postinst correctly differentiates fresh install vs upgrade (`$2` check).

---

## Per-File Verdicts

### 1. `src/api/handlers/system.rs` — **REQUEST CHANGES**
- CRITICAL-2: Handler bypasses job system
- No concurrency protection on self-update endpoint
- Missing `restart_delay_seconds` validation (constant defined but unused)
- Good: Version validation, error handling, proper HTTP status codes (202 Accepted)

### 2. `src/packages/mod.rs` — **REQUEST CHANGES**
- MEDIUM-3: Non-atomic write of request file
- MEDIUM-2: Unused `MAX_RESTART_DELAY_SECONDS` constant
- Good: `validate_version_string` is thorough, `SELF_PACKAGE_NAME` pinned via `CARGO_PKG_NAME`

### 3. `src/jobs/manager.rs` — **CONCERNS**
- MEDIUM-1: `SelfUpdate` variant is dead code
- No functional bugs, but the variant should either be wired up or removed

### 4. `configs/self-update.sh` — **REQUEST CHANGES**
- HIGH-1: No failure marker on upgrade failure
- HIGH-2: JSON injection in heredoc
- HIGH-3: Silent python3 fallthrough
- MEDIUM-4: Shell variable in Python string
- MEDIUM-5: No signal handling
- LOW-1: No length limit on TARGET_VERSION
- LOW-3: Log injection
- LOW-4: TOCTOU on request file

### 5. `configs/linux-patch-api-update.service` — **REQUEST CHANGES**
- HIGH-4: Missing TimeoutStartSec
- MEDIUM-6: Missing security hardening directives

### 6. `debian/postinst` — **REQUEST CHANGES**
- CRITICAL-1 (in prerm): Service disabled on upgrade, not re-enabled
- Good: Upgrade-aware logic (`$2` check), preserves CRL/certs/config

### 7. `debian/prerm` — **REQUEST CHANGES**
- CRITICAL-1: `systemctl disable` on upgrade breaks auto-start after reboot

### 8. `scripts/build-package.sh` — **APPROVE**
- Correctly ships new files (update service, self-update.sh)
- Sets correct permissions (755 for self-update.sh)

### 9. `configs/linux-patch-api.install` (Arch) — **REQUEST CHANGES**
- HIGH-5: post_upgrade does not start service
- Missing `systemctl start` on upgrade

### 10. `configs/linux-patch-api.post-install` (Alpine) — **REQUEST CHANGES**
- HIGH-5: Upgrade path does not start service
- Alpine uses OpenRC (`rc-service`), not systemd — self-update.sh uses systemctl which won't work

### 11. `linux-patch-api.spec` (RPM) — **REQUEST CHANGES**
- HIGH-5: %post upgrade does not start service
- Missing install directives for self-update.sh and update service unit

### 12. `tests/e2e/test_self_update.sh` — **APPROVE**
- Comprehensive E2E harness covering upgrade, same-version, CRL preservation
- Good: Tests marker file correctness, service restart, and crash loop detection

---

## Overall Assessment

**REQUEST CHANGES**

The architecture is sound but the implementation has 2 critical bugs and 5 high-severity issues that must be fixed before merge:

1. **Fix prerm**: Only `systemctl disable` on remove, not on upgrade
2. **Fix handler**: Route self-update through the job system or add concurrency guard
3. **Fix self-update.sh**: Add error trap (failure marker), signal handlers, proper JSON serialization
4. **Fix update.service**: Add TimeoutStartSec=300
5. **Fix cross-platform restart**: All 4 package formats must start service on upgrade
6. **Fix python3 fallthrough**: Fail loudly on parse error instead of silently upgrading to latest

After these fixes, the feature will be ready for merge with medium/low items tracked as follow-ups.
