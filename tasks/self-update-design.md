# Self-Update Design Note — Linux Patch API v1.4.3 → v1.5.0

**Updated after E2E verification** — Architecture corrected from agent-runs-apt-get to
detached-systemd-unit approach after the agent-cgroup kill was identified as the root cause
of the v1.5.0-beta failure.

Baseline: `v1.4.3` tag, commit `89bc5cc8bc9abab4326910841ad82cc2d034ed60`.

Verified baseline checks (all pass):
- `JobOperation` has exactly 6 variants: `Install, Update, Remove, PatchApply, Reboot, Rollback`
- `PackageManagerBackend` trait has exactly 12 methods; no `update_self`, `schedule_self_restart`, `install_file`, or `installed_version`
- `src/api/handlers/system.rs` is 434 lines; routes are `/system/info`, `/system/reboot`, `/system/services/{name}`, `/health`
- No `restart_service`, no `/system/restart`, no `FileInstall`, no `self_upgrade`, no `install_url`
- No `src/api/handlers/{file_install,install_url,self_upgrade}.rs`
- 5 backends: `AptBackend`, `DnfBackend`, `YumBackend`, `ApkBackend`, `PacmanBackend`

---

## 1. Architecture Overview

### 1.1 Endpoint

`POST /api/v1/system/update` — mirrors the existing `POST /api/v1/system/reboot` async pattern exactly:

```
validate request → can_accept_job() guard → create_job(SelfUpdate) → tokio::spawn → 202 Accepted
```

mTLS + IP whitelist + destructive-tier rate limiting already apply globally. No new auth wiring.

### 1.2 New Types

**`JobOperation::SelfUpdate`** — single new variant. No `FileInstall` or any other variant.

**`SelfUpdateRequest`** (handler input):
```rust
fn default_true() -> bool { true }
fn default_restart_delay() -> u64 { 5 }

#[derive(Debug, Deserialize, Clone)]
pub struct SelfUpdateRequest {
    /// Pin to an exact package version. None = upgrade to latest available.
    #[serde(default)]
    pub target_version: Option<String>,
    /// Restart the service after a successful upgrade so the new binary runs.
    #[serde(default = "default_true")]
    pub restart: bool,
    /// Seconds to wait before the decoupled restart fires.
    /// Clamped to max 300 (5 minutes) in the handler.
    #[serde(default = "default_restart_delay")]
    pub restart_delay_seconds: u64,
}
```

**`SelfUpdateOutcome`** (backend result):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfUpdateOutcome {
    pub previous_version: String,
    pub new_version: String,
    /// false when already at the requested/latest version (no restart needed).
    pub changed: bool,
}
```

**`SelfUpdateStatusData`** (GET endpoint response):
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SelfUpdateStatusData {
    pub previous_version: String,
    pub new_version: String,
    pub changed: bool,
    pub status: String,        // "success" | "restart_pending" | "restart_failed"
    pub error: Option<String>,
    pub at: String,             // RFC3339 timestamp
}
```

### 1.3 Constants

```rust
pub const SELF_PACKAGE_NAME: &str = env!("CARGO_PKG_NAME"); // "linux-patch-api"
pub const SELF_SERVICE_NAME: &str = "linux-patch-api";
pub const MAX_RESTART_DELAY_SECONDS: u64 = 300;
pub const SELF_UPDATE_MARKER_PATH: &str = "/var/lib/linux_patch_api/last_self_update.json";
```

### 1.4 Backend Trait Additions

Three new required methods on `PackageManagerBackend`:

```rust
fn update_self(&self, target_version: Option<&str>) -> Result<SelfUpdateOutcome>;
fn schedule_self_restart(&self, delay_seconds: u64) -> Result<()>;
fn installed_version(&self, pkg: &str) -> Option<String>;
```

### 1.5 Package Name Pinned

The package name is always `SELF_PACKAGE_NAME` (compile-time constant from `CARGO_PKG_NAME`).
The endpoint **cannot** be used to upgrade arbitrary packages — the request body never carries
a package name.

### 1.6 Upgrade Command Map

| Backend | latest upgrade | pinned version | installed-version query |
|---------|---------------|-----------------|--------------------------|
| apt     | `apt-get install -y --only-upgrade -- linux-patch-api` | `apt-get install -y --allow-downgrades -- linux-patch-api=<v>` | `dpkg-query -W -f='${Version}' -- <pkg>` |
| dnf     | `dnf upgrade -y -- linux-patch-api` | `dnf install -y -- linux-patch-api-<v>` | `rpm -q --qf '%{VERSION}-%{RELEASE}' -- <pkg>` |
| yum     | `yum update -y -- linux-patch-api` | `yum install -y -- linux-patch-api-<v>` | `rpm -q --qf '%{VERSION}-%{RELEASE}' -- <pkg>` |
| apk     | `apk upgrade -- linux-patch-api` | `apk add -- linux-patch-api=<v>` | `apk version -- <pkg>` |
| pacman  | `pacman -S --noconfirm -- linux-patch-api` | (no native pin; use repo/archive) | `pacman -Q <pkg>` |

All commands use `--` separator and validated version strings. No shell interpolation.

### 1.7 Detached Systemd Unit — Corrected Architecture

**Root cause of the v1.5.0-beta failure:** The agent cannot run `apt-get install` in its own
cgroup because dpkg's `prerm` script runs `systemctl stop linux-patch-api`, killing the very
process that is running the upgrade. This produces a half-configured package state.

**Corrected architecture:** The agent does NOT run `apt-get` directly. Instead, it hands the
entire upgrade transaction to a separate systemd oneshot unit that runs in `system.slice`,
outside the agent's cgroup. The unit survives the agent being killed by prerm.

Flow:
1. Agent validates request → writes `/var/lib/linux_patch_api/self-update.request` + pending marker
2. Agent calls `systemctl start --no-block linux-patch-api-update.service` → returns 202
3. The update service runs `/usr/lib/linux-patch-api/self-update.sh` in its **own cgroup**
   under `system.slice`
4. dpkg's prerm stops the agent — the update service **survives** (different cgroup)
5. dpkg completes → postinst starts the new agent on the new binary
6. Script writes marker file with success/failure
7. New agent serves marker at `GET /system/update/status`

**Update service unit** (`configs/linux-patch-api-update.service`):
```ini
[Unit]
Description=Linux Patch API self-update transaction
# No coupling to linux-patch-api.service — must survive its stop.

[Service]
Type=oneshot
ExecStart=/usr/lib/linux-patch-api/self-update.sh
# Run in its own cgroup under system.slice (default for a separate unit).
```

**Self-update script** (`configs/self-update.sh`):
- Reads target version from `/var/lib/linux_patch_api/self-update.request`
- Validates version string (prevents shell injection)
- Detects package manager (apt/dnf/yum/apk/pacman)
- Refreshes package index (logs warning on failure, does not abort)
- Runs upgrade with `--` separator and validated version
- Compares before/after versions for `changed` detection
- Writes `/var/lib/linux_patch_api/last_self_update.json` marker
- Cleans up request file

**OpenRC fallback** (Alpine):
- Uses `setsid` + `sleep` + `rc-service restart` as a detached process
- KNOWN LIMITATION: `setsid` creates a new session, not a separate cgroup. Weaker
  isolation than systemd-run. Acceptable on OpenRC/Alpine.

### 1.8 Handler Logic — Delegation, Not Execution

The handler does NOT run the package manager. It delegates to the detached unit:

1. Validate `target_version` with `validate_version_string` (if present).
2. Clamp `restart_delay_seconds` to `max(1, min(delay, 300))`.
3. Log request with `request_id`.
4. Check `can_accept_job()` → 429 if full.
5. `create_job(JobOperation::SelfUpdate, vec![])` → 500 if fail.
6. Write request file to `/var/lib/linux_patch_api/self-update.request`.
7. Persist pending marker to `/var/lib/linux_patch_api/last_self_update.json`.
8. `systemctl start --no-block linux-patch-api-update.service`.
9. If systemctl fails: `fail_job` with the error.
10. Return `202 Accepted` with `{job_id, status, operation, target_version, restart, restart_delay_seconds}`.

The update script (not the agent) handles the upgrade, version comparison, restart
scheduling, and marker writing. The agent's job is validation, delegation, and response.

### 1.9 Completion Visibility Across Restart

`JobManager` is in-memory; job state is destroyed on process restart. Two-part fix:

1. **Persist marker** to `/var/lib/linux_patch_api/last_self_update.json` before the restart
   fires:
   ```json
   {
     "previous_version": "1.4.3",
     "new_version": "1.5.0",
     "changed": true,
     "status": "success",
     "error": null,
     "at": "2026-06-15T23:45:00Z"
   }
   ```

2. **Expose via API**: `GET /api/v1/system/update/status` returns the marker contents.
   Also fold `agent_version` into `GET /api/v1/system/info` (already has `version` from
   `env!("CARGO_PKG_VERSION")`).

3. **Manager workflow**: poll `GET /health` with backoff after connection drops; on reconnect,
   confirm version via `GET /system/info` or `GET /system/update/status`.

---

## 2. Handler Logic — Delegation Pattern

The handler does **not** run the package manager. It validates, delegates, and responds:

1. Validate `target_version` with `validate_version_string` (if present).
2. Clamp `restart_delay_seconds` to `max(1, min(delay, 300))`.
3. Log request with `request_id`.
4. Check `can_accept_job()` → 429 if full.
5. `create_job(JobOperation::SelfUpdate, vec![])` → 500 if fail.
6. Write request file to `/var/lib/linux_patch_api/self-update.request`.
7. Persist pending marker to `/var/lib/linux_patch_api/last_self_update.json`.
8. `systemctl start --no-block linux-patch-api-update.service`.
9. If systemctl fails: `fail_job` with the error.
10. Return `202 Accepted` with `{job_id, status, operation, target_version, restart, restart_delay_seconds}`.

The update service (not the agent) handles the upgrade, version comparison, restart
scheduling, and final marker writing. The agent's job is validation, delegation, and response.

### 2.1 New Routes

```rust
// In system::configure_routes:
.route("/update", web::post().to(update_self))
.route("/update/status", web::get().to(get_self_update_status))
```

---

## 3. CRL / Cert / Config Safety on Upgrade

PR #69's #1 failure: CRL overwritten on every package install → crash loop.

### 3.1 Root Cause

The package postinst scripts run on every install, including upgrades. The current postinst
scripts do not distinguish fresh install from upgrade. On upgrade, they should NOT touch
existing CRL, certificates, or config.

### 3.2 Current State of Each Format

**Debian (`debian/postinst`)**:
- Runs on both fresh install (`$1 = configure` with no `$2`) and upgrade (`$1 = configure` with
  `$2` = previous version).
- Currently: copies example configs only if missing (safe), but unconditionally runs
  `systemctl daemon-reload` and `systemctl enable` (acceptable on upgrade).
- **No CRL/cert regeneration exists in current postinst** — the CRL is managed at runtime
  by the `crl.rs` module which fetches from the manager. However, the postinst must be
  upgrade-aware to prevent future regressions.

**RPM (`linux-patch-api.spec %post`)**:
- Runs on both install and upgrade. `$1 = 1` for install, `$1 > 1` for upgrade.
- Currently: same pattern as Debian — copies example configs only if missing.
- No CRL/cert touch, but must be made upgrade-aware.

**APK (`configs/linux-patch-api.post-install`)**:
- Runs after every install/upgrade. No install-vs-upgrade distinction in the script itself.
- Currently: copies example configs only if missing.
- Must add upgrade guard.

**Arch (`configs/linux-patch-api.install`)**:
- Has `post_install()` and `post_upgrade()` as separate functions.
- `post_upgrade` currently only runs `systemctl daemon-reload`.
- Cleanest separation — just ensure `post_upgrade` never touches CRL/certs/config.

### 3.3 Required Changes Per Format

**Debian `debian/postinst`**:
```bash
if [ "$1" = "configure" ]; then
    if [ -z "$2" ]; then
        # Fresh install: full setup (copy configs, enable service, show enrollment messages)
        # ... existing logic ...
    else
        # Upgrade ($2 = previous version): preserve everything
        echo "Upgrading linux-patch-api from $2 ..."
        # DO NOT touch: config.yaml, whitelist.yaml, certs/, CRL
        # DO NOT restart the service (self-update owns the restart)
        systemctl daemon-reload
    fi
fi
```

**RPM `linux-patch-api.spec %post`**:
```bash
%post
if [ $1 -eq 1 ]; then
    # Fresh install: full setup
    # ... existing logic ...
elif [ $1 -gt 1 ]; then
    # Upgrade: preserve everything
    echo "Upgrading linux-patch-api ..."
    # DO NOT touch: config.yaml, whitelist.yaml, certs/, CRL
    # DO NOT restart the service
    systemctl daemon-reload
fi
```

**APK `configs/linux-patch-api.post-install`**:
```bash
# APK runs post-install on both fresh install and upgrade
# Detect upgrade by checking if the service already exists
if rc-service linux-patch-api status >/dev/null 2>&1; then
    # Upgrade: preserve everything
    echo "Upgrading linux-patch-api ..."
    # DO NOT touch: config.yaml, whitelist.yaml, certs/, CRL
    # DO NOT restart the service
else
    # Fresh install: full setup
    # ... existing logic ...
fi
```

**Arch `configs/linux-patch-api.install`**:
- `post_install()` — unchanged (fresh install only)
- `post_upgrade()` — already only runs `systemctl daemon-reload`. Verify it never touches
  CRL/certs/config. Add explicit comment: `# Upgrade: do NOT touch config, certs, or CRL`.

### 3.4 Acceptance Test for CRL/Cert Preservation

Checksum CRL and cert files before and after an upgrade. They must be byte-identical:
```bash
sha256sum /etc/linux_patch_api/certs/*.pem > /tmp/certs-before.sha256
# ... perform upgrade ...
sha256sum /etc/linux_patch_api/certs/*.pem > /tmp/certs-after.sha256
diff /tmp/certs-before.sha256 /tmp/certs-after.sha256  # must be empty
```

---

## 4. Acceptance Criteria — PR #69 Failures

| #69 Failure | Required Outcome | How Addressed |
|---|---|---|
| CRL overwritten on install → crash loop | postinst upgrade-aware; CRL/cert checksums unchanged across upgrade | §3.3: postinst scripts detect upgrade and skip CRL/cert/config |
| Install script killed by cgroup | N/A — structurally impossible | No install script; upgrade runs in detached systemd unit under `system.slice` (§1.7). The agent's cgroup is killed by prerm; the update service survives. |
| `pkill -f` matched install script | N/A — structurally impossible | No `pkill`, no script process to match |
| API deleting staged files on SIGTERM | N/A — structurally impossible | Nothing staged; no temp files to clean up |
| apt refusing same-version reinstall | `changed==false` path; no forced reinstall | §1.8: detect unchanged version, skip restart |
| self-cleanup removed debug info | N/A | No self-cleanup step; marker file persists for debugging |
| shell injection | `validate_version_string` + `--` separator in script; no shell interpolation | §1.7: self-update.sh validates version with regex before use |
| version filter offered same version | manager + agent skip when `changed==false` | §1.8 + §5: version comparison before restart |
| progress tracking broken in UI | persisted marker + manager reconnect/version check | §1.9: marker file + `/system/update/status` endpoint |

---

## 5. Manager Side — `pm-agent-client`

### 5.1 Types (`crates/pm-agent-client/src/types.rs`)

```rust
#[derive(Debug, Clone, Serialize, Default)]
pub struct SelfUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_version: Option<String>,
    pub restart: bool,
    pub restart_delay_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SelfUpdateResponse {
    pub job_id: String,
    pub status: String,
    #[serde(default)]
    pub target_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SelfUpdateStatus {
    pub previous_version: String,
    pub new_version: String,
    pub changed: bool,
    pub status: String,
    pub error: Option<String>,
    pub at: String,
}
```

### 5.2 Client Method (`crates/pm-agent-client/src/client.rs`)

```rust
pub async fn self_update(&self, req: &SelfUpdateRequest)
    -> Result<SelfUpdateResponse, AgentClientError>
{
    self.post("system/update", req).await
}

pub async fn get_self_update_status(&self)
    -> Result<SelfUpdateStatus, AgentClientError>
{
    self.get("system/update/status").await
}
```

### 5.3 Worker Flow

```
1. POST /system/update → 202 { job_id }
2. Poll GET /jobs/{job_id} until status == "completed"
   OR until requests start failing (connection refused / reset).
3. Once unreachable, poll GET /health with backoff (~3s, up to ~60s).
4. On reconnect: GET /system/update/status (or /system/info) and assert
   the version moved to target (or simply changed). Mark host updated.
5. If health never returns within the window, surface a hard error with
   the agent's last known state — do NOT mark success.
```

---

## 6. E2E Test Harness Outline

Located at `tests/e2e/test_self_update.sh` (or playbook):

1. Provision a disposable systemd target (throwaway LXC/VM).
2. Build two packages: `vN` (with self-update endpoint) and `vN+1` (trivial change).
   Serve `vN+1` from a local apt repo / file source.
3. Install `vN`, enroll, confirm healthy.
4. Record CRL + cert checksums and `systemctl show -p NRestarts`.
5. Fire `POST /api/v1/system/update`.
6. Assert ALL of:
   - job reaches "completed" (or marker shows success) before the bounce,
   - `/health` returns within the delay window at version `vN+1`,
   - `NRestarts` did not climb (no crash loop),
   - CRL + cert checksums are **unchanged**,
   - `last_self_update.json` reflects correct before/after versions.
7. Repeat with `restart=false` — assert no restart, version staged for next boot.
8. Repeat with the **same** version — assert `changed==false`, no restart.

### 6.1 Unit Tests

- `validate_version_string` injection attempts (shell metacharacters, path traversal)
- `SelfUpdateRequest` deserialize defaults and edge cases
- `schedule_service_restart` when `systemd-run` binary is absent
- Handler: queue-full, validation-failure, backend-error paths
- `SelfUpdateOutcome` serialization
- Marker file read/write

---

## 7. Safety Rails on Input

- `target_version`: validated with `validate_version_string` before use.
- `restart_delay_seconds`: **clamped to `max(1, min(delay, 300))`** in the handler.
  A misconfigured manager must not leave the old binary running indefinitely.
- Package name: always `SELF_PACKAGE_NAME`; never from the request body.

---

## 8. Prohibited Architecture (from §1 of spec)

These are permanently off-limits:

- ❌ Downloading an install script or any executable and running it
- ❌ Staging files in a temp/work directory that the running process must protect across SIGTERM
- ❌ Using `pkill`, `kill`, or process-name matching
- ❌ Embedding shell scripts or invoking `sh -c` with interpolated values
- ❌ Any self-cleanup step that deletes files/logs/debug info on failure
- ❌ A synchronous `systemctl restart` (or any restart child in the agent's cgroup)
- ❌ Regenerating, overwriting, or touching CRL, certificates, or config during upgrade

---

## 9. Stop Conditions

If a second round of hardening is needed on the restart/cgroup/process-kill/staging path,
**STOP**. That pattern is the signature of the failed design. Report:

- The exact runtime failure with evidence (`journalctl`, `NRestarts`, `ss -ltnp`, marker file)
- Hypothesis for root cause
- Request review before continuing

---

## 10. File Change Summary (corrected architecture)

| File | Change |
|------|--------|
| `src/jobs/manager.rs` | Add `SelfUpdate` variant to `JobOperation` |
| `src/packages/mod.rs` | Add `SELF_PACKAGE_NAME`, `SELF_SERVICE_NAME`, `MAX_RESTART_DELAY_SECONDS`, `SELF_UPDATE_MARKER_PATH`, `SELF_UPDATE_REQUEST_PATH` constants; `SelfUpdateOutcome`, `SelfUpdateStatusData` structs; `persist_self_update_marker`, `write_self_update_request`, `read_self_update_marker` functions; `validate_version_string` function |
| `src/api/handlers/system.rs` | Add `SelfUpdateRequest` struct; `update_self` handler (delegates to systemd unit, does NOT run apt-get); `get_self_update_status` handler; two new routes |
| `configs/linux-patch-api-update.service` | **New file** — systemd oneshot unit for detached upgrade transaction. No coupling to `linux-patch-api.service`. Runs under `system.slice`. |
| `configs/self-update.sh` | **New file** — multi-pkg-mgr upgrade script. Reads request file, validates version, detects package manager, refreshes index, runs upgrade, compares versions, writes marker. |
| `debian/postinst` | Add upgrade-aware branch (`$2` check); start service on upgrade |
| `linux-patch-api.spec` | Add upgrade-aware `%post` branch (`$1 -gt 1`) |
| `configs/linux-patch-api.post-install` | Add upgrade detection (service exists check) |
| `configs/linux-patch-api.install` | Add comment to `post_upgrade`; verify no CRL/cert/config touch |
| `scripts/build-package.sh` | Ship `linux-patch-api-update.service` and `self-update.sh` in the package |
| `tests/e2e/test_self_update.sh` | **New file** — E2E harness with 8 test cases |

**Manager-side changes** (separate repo: Linux-Patch-Manager, branch `feat/agent-upgrade-controls`):
| File | Change |
|------|--------|
| `crates/pm-agent-client/src/types.rs` | Add `SelfUpdateRequest`, `SelfUpdateResponse`, `SelfUpdateStatus` |
| `crates/pm-agent-client/src/client.rs` | Add `self_update()`, `self_update_status()` methods |
| `crates/pm-worker/src/job_executor.rs` | Add `execute_self_upgrade_host_job` dispatch and `poll_self_upgrade_host` reconciliation |
| `crates/pm-web/src/routes/upgrades.rs` | Add `POST /upgrades/trigger` endpoint |
| `frontend/src/pages/HostDetailPage.tsx` | Add self-upgrade UI |
