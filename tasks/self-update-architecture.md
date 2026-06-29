# Self-Update Architecture — Agent-Side Reference

**Version:** 2.0.0
**Date:** 2026-06-29
**Status:** Active
**Supersedes:** `tasks/self-update-design.md` (implementation details)

---

## 1. Overview

The self-update system enables the Linux Patch API agent to upgrade itself from a manager-hosted package repository using native package manager commands. The architecture is designed around one critical constraint: **the agent cannot run `apt-get install` in its own process** because dpkg's prerm script kills the agent, leaving a half-configured package state.

### Key Design Principles
1. **Detached execution** — upgrade runs in a separate systemd unit with its own cgroup
2. **Native package manager** — no GitHub Releases, no curl downloads, no API parsing
3. **GPG trust chain** — packages signed by the manager's own GPG key, delivered via mTLS enrollment
4. **Auto-rollback** — health check after upgrade; if service fails, reinstall previous version
5. **Cross-restart visibility** — marker file on disk survives agent process restart

### Manager Pull Model

The manager pulls packages from GitHub Releases via standard HTTP, signs them with its own unique GPG key, and hosts them in a local package repository. The agent receives the GPG public key and repo configuration during enrollment. This model requires no CI push, no embedded credentials, and no shared secrets — each manager is self-contained.

---

## 2. Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          MANAGER HOST                                    │
│                                                                          │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────────────┐   │
│  │ Patch Manager │  │ Repo Server  │  │ GPG Key Management           │   │
│  │ (Rust API)    │  │ (axum        │  │                              │   │
│  │               │  │  ServeDir)   │  │ Key generation: manager init │   │
│  │ Enrollments   │  │              │  │ Key storage: alongside CA    │   │
│  │ CRL issuance  │  │ /apt/        │  │ Key distribution: enrollment │   │
│  │ Upgrade API   │  │ /dnf/        │  │                              │   │
│  │ Health polls  │  │ /apk/        │  │ Repo signing: manager        │   │
│  │               │  │ /pacman/     │  │                              │   │
│  └──────────────┘  └──────────────┘  └──────────────────────────────┘   │
│         │                  ▲                                              │
│         │                  │                                              │
│  ┌──────┴───────┐    ┌─────┴──────┐                                      │
│  │ Enrollment   │    │ Package    │                                      │
│  │ Response     │    │ Sync       │                                      │
│  │ + repo config│    │ Worker     │                                      │
│  │ + GPG key    │    │ (pulls from│                                      │
│  └──────────────┘    │  GitHub)   │                                      │
│         │            └────────────┘                                      │
└─────────┼──────────────────────────────────────────────────────────────────┘
          │ mTLS enrollment
          ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                          AGENT HOST                                       │
│                                                                          │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐    │
│  │ sources.list.d   │  │ GPG keyring      │  │ self-update.sh       │    │
│  │ /lpa.list         │  │ /etc/apt/        │  │ (simplified)          │    │
│  │                   │  │ trusted.gpg.d    │  │                      │    │
│  │ deb http://       │  │ lpa-repo.gpg     │  │ apt-get update       │    │
│  │ manager...        │  │                  │  │ apt-get install      │    │
│  │                   │  │ (dnf: /etc/pki/  │  │   --only-upgrade     │    │
│  │                   │  │  rpm-gpg/...)    │  │   linux-patch-api    │    │
│  └──────────────────┘  └──────────────────┘  └──────────────────────┘    │
│                                                                          │
│  ┌──────────────────────────────────────────────────────────────────┐    │
│  │                      systemd cgroup layout                        │    │
│  │                                                                   │    │
│  │  system.slice                                                     │    │
│  │  ├─ linux-patch-api.service          (agent, Type=simple)         │    │
│  │  │   cgroup: /system.slice/linux-patch-api.service               │    │
│  │  │                                                                │    │
│  │  └─ linux-patch-api-update.service   (upgrade, Type=oneshot)     │    │
│  │      cgroup: /system.slice/linux-patch-api-update.service        │    │
│  │      (SEPARATE cgroup — survives agent being killed by prerm)     │    │
│  └──────────────────────────────────────────────────────────────────┘    │
│                                                                          │
│  ┌──────────────────────────────────────────────────────────────────┐    │
│  │                      Marker File                                   │    │
│  │  /var/lib/linux_patch_api/last_self_update.json                   │    │
│  │  {                                                                │    │
│  │    "previous_version": "1.5.5-1",                                 │    │
│  │    "new_version": "1.5.6-1",                                      │    │
│  │    "changed": true,                                               │    │
│  │    "status": "success",                                           │    │
│  │    "error": null,                                                 │    │
│  │    "at": "2026-06-27T14:00:00Z"                                   │    │
│  │  }                                                                │    │
│  └──────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Phase Breakdown

### Phase 1: Enrollment (Agent ↔ Manager)

**Who:** Agent (first boot or `--enroll`)
**Channel:** mTLS-disabled initial connection → manager approval workflow

1. Agent posts identity to `POST /api/v1/enroll` (machine-id, FQDN, IPs, OS)
2. Manager returns HTTP 202 with polling token
3. Agent polls `GET /api/v1/enroll/status/{token}`
4. Admin approves → manager returns enriched `PkiBundle`:
   - CA cert + chain, server cert + key, CRL (existing)
   - **`repo_config`** (new): GPG public key, distro-specific sources config, distro_id, keyring path
5. Agent writes PKI files (atomic write+rename), provisions repo:
   - **apt**: GPG key → `/etc/apt/keyrings/lpa-repo.gpg`, sources → `/etc/apt/sources.list.d/lpa.list`
   - **dnf/yum**: GPG key → `/etc/pki/rpm-gpg/...`, repo → `/etc/yum.repos.d/lpa.repo`
   - **apk**: append URL → `/etc/apk/repositories`
   - **pacman**: include file → `/etc/pacman.d/lpa-repo`
6. Fallback: if `repo_config` absent → agent fetches `GET /api/v1/pki/repo-config` on demand

### Phase 2: Normal Operation (Steady State)

**Who:** Agent daemon
**Listens on:** mTLS port 12443

- Agent serves normal API (`/packages`, `/patches`, `/system/info`, `/health`)
- Package manager uses configured repo for routine upgrades
- `systemd` unit `linux-patch-api.service` runs the agent
- Separate unit `linux-patch-api-update.service` is **disabled by default**, only started on demand

### Phase 3: Self-Update Trigger (Manager → Agent)

**Endpoint:** `POST /api/v1/system/update`

Request body:
```json
{"target_version": "1.5.6-1", "restart": true, "restart_delay_seconds": 5}
```

Handler logic (`update_self`):
1. Validate `target_version` (regex injection check)
2. Clamp `restart_delay_seconds` to `max(1, min(delay, 300))`
3. Check concurrency guards (NOT JobManager — see §4.2):
   - `systemctl is-active linux-patch-api-update.service`
   - Existence of `/var/lib/linux_patch_api/self-update.request`
4. Write request file: `{"target_version": "..."}`
5. Write pending marker to `/var/lib/linux_patch_api/last_self_update.json`
6. Start detached unit: `systemctl start --no-block linux-patch-api-update.service`
7. Return HTTP 202

### Phase 4: Upgrade Execution (Detached Unit)

**Critical design:** The upgrade runs in `linux-patch-api-update.service`, which is in its **own cgroup under `system.slice`** — **not** the agent's cgroup.

Flow:
1. Agent has started the unit and returned 202
2. **dpkg prerm** runs `systemctl stop linux-patch-api.service` → agent dies
3. **Update service survives** (different cgroup)
4. dpkg completes install → postinst starts new agent on new binary
5. **No half-configured state** (the root cause of v1.5.0-beta failure)

`self-update.sh` logic:
1. Read target version from request file
2. Validate with regex (defense in depth — handler already validated)
3. Detect package manager (`apt`/`dnf`/`yum`/`apk`/`pacman`)
4. Refresh repo metadata (non-fatal on failure)
5. Run upgrade directly via `case/esac` branches — **no eval, no shell interpolation**
6. Pacman version pinning: search `/var/cache/pacman/pkg/` for cached `.pkg.tar.zst`, use `pacman -U`

### Phase 5: Health Check + Auto-Rollback

**60-second timeout, 5-second polling interval**

After package install:
1. Loop: check `systemctl is-active linux-patch-api.service`
2. If active within 60s → write **success marker** with `previous_version`, `new_version`, `changed: true`
3. If not active → **auto-rollback**:
   - Reinstall previous version with package manager's downgrade flag
   - Write **failure marker** with rollback status
4. **Signal trap** (`TERM`/`INT`/`HUP`): if script is killed mid-upgrade, write failure marker before exit

### Phase 6: Completion + Visibility

**Marker file:** `/var/lib/linux_patch_api/last_self_update.json`

**Two visibility surfaces:**
- `GET /api/v1/system/update/status` — serves marker via mTLS API
- Marker file on disk — survives agent restart (the agent process was killed mid-upgrade)

**Manager reconciliation:**
- Manager polls `GET /health` with backoff after connection drops
- On reconnect: confirm version via `GET /system/info` or `GET /system/update/status`

---

## 4. Cgroup Isolation (Critical Architecture)

### Why the Detached Systemd Unit Works

| Property | Explanation |
|----------|-------------|
| **Separate cgroup** | `linux-patch-api-update.service` runs in its own cgroup (`system.slice/linux-patch-api-update.service`), NOT nested under the agent's cgroup |
| **systemd kill scope is per-cgroup** | When prerm runs `systemctl stop linux-patch-api.service`, systemd only kills processes in that unit's cgroup. The update service is in a different cgroup → unaffected |
| **No coupling in unit file** | The update service unit has no `BindsTo=`, `Requires=`, or `After=` for `linux-patch-api.service`. It's a fully independent oneshot |
| **system.slice default** | Both units default to `system.slice` because no `Slice=` is specified. They share the same parent slice but different child cgroups |

### The Critical systemd Unit Config

```ini
# configs/linux-patch-api-update.service
[Unit]
Description=Linux Patch API self-update transaction
# NO After=, Requires=, BindsTo=, PartOf= referencing linux-patch-api.service
# This is what makes it survive the agent being stopped.

[Service]
Type=oneshot
ExecStart=/usr/lib/linux-patch-api/self-update.sh
# No Slice= override → defaults to system.slice/linux-patch-api-update.service
# Different from system.slice/linux-patch-api.service (agent's cgroup)
TimeoutStartSec=300
RemainAfterExit=no
```

### Forbidden Patterns (Stop Conditions)

| Pattern | Why It's Forbidden |
|---------|-------------------|
| `Requires=linux-patch-api.service` | systemd would stop the update when agent stops |
| `BindsTo=linux-patch-api.service` | Same as above, stricter |
| `PartOf=linux-patch-api.service` | Same as above |
| Running script from agent process | Agent IS the process that gets killed |
| `pkill -f self-update` | Could match update.sh if pattern is wrong |
| `sh -c "apt-get install ..."` with interpolated values | Shell injection if target_version is attacker-controlled |
| `eval` with interpolated values | Same as above |

If a future change tries to re-introduce coupling between the two units, **STOP**. That pattern is the signature of the failed v1.5.0-beta design. Report the runtime failure with evidence (`journalctl`, `NRestarts`, `ss -ltnp`, marker file) and request review.

---

## 5. Manager-Side Worker Flow

### Sequence

```
Manager (pm-worker)                    Agent (linux-patch-api)           Update Service
─────────────────────                  ──────────────────────           ──────────────

1. POST /api/v1/system/update
   {"target_version":"1.5.6-1"}
   ─────────────────────────────────►
                                        handler.update_self()
                                        ├─ validate target_version
                                        ├─ check concurrency guards
                                        ├─ write self-update.request
                                        ├─ write pending marker
                                        └─ systemctl start --no-block
                                           linux-patch-api-update.service
   ◄─────────────────────────────────
   HTTP 202 {"status":"pending",
             "target_version":"1.5.6-1"}

2. Poll job until connection drops
   GET /api/v1/jobs/{job_id}
   ─────────────────────────────────►
   ◄─── CONNECTION REFUSED ────
   (agent is restarting)

3. Health poll with exponential backoff
   GET /health (3s, 6s, 12s, 24s, 48s, 60s)
   ─────────────────────────────────►
   ◄─── CONNECTION REFUSED ──── (attempts 1-3)
   ◄─────────────────────────────────
   HTTP 200 {"status":"healthy",
             "version":"1.5.6-1"}

4. Reconcile version
   GET /api/v1/system/update/status
   ─────────────────────────────────►
   ◄─────────────────────────────────
   HTTP 200 {"previous_version":"1.5.5-1",
             "new_version":"1.5.6-1",
             "changed":true,
             "status":"success"}

5. Mark host as updated in database
```

### Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **Poll job until connection drops** | JobManager is in-memory; "completed" only means the handler returned 202. Real state is in the marker file. |
| **Exponential backoff on health poll** | 3s → 6s → 12s → 24s → 48s → 60s (capped). Avoids hammering a restarting agent. |
| **120s hard timeout** | If agent doesn't return within 2 minutes, surface hard error. Don't wait forever. |
| **Reconcile via `/system/update/status`** | Marker file is the authoritative source. Don't trust the in-memory job state. |
| **Don't mark success on timeout** | If health never returns, the upgrade may have bricked the agent. Operator must intervene. |

---

## 6. Trust Chain (End-to-End)

```
Manager generates own GPG key (alongside CA)
        ↓
  GPG public key delivered via mTLS enrollment (PkiBundle.repo_config)
        ↓
  Agent provisions GPG key to native package manager keyring
        ↓
  Manager pulls packages from GitHub Releases via HTTP
        ↓
  Manager signs packages with its own GPG key
        ↓
  Manager hosts signed packages in local repo (HTTP, port 80)
        ↓
  Agent's native package manager (apt/dnf/apk/pacman) verifies GPG signature before install
```

If enrollment is compromised → package trust is compromised. This transitive chain is documented in `THREAT_MODEL_VALIDATION.md` §7.

---

## 7. Threat Model Summary

| Trust Boundary | Key Threat | Mitigation |
|----------------|-----------|------------|
| GitHub Releases → Manager | Package tampering in transit | GPG signing by manager after download; GitHub TLS |
| Manager → Agent (enrollment) | MITM during enrollment | TLS encryption, manager approval workflow, one-time short window |
| Agent → Package Repo | Repo server compromised | GPG-signed packages, key delivered via separate mTLS channel |
| Agent → Self-Update Execution | Shell injection via target_version | `validate_version_string` regex (Rust + bash), no eval, no shell interpolation |
| Agent → Self-Update Execution | Cgroup escape | Separate cgroup, no unit coupling, kernel-enforced isolation |

Full threat model: `THREAT_MODEL_VALIDATION.md` §7

---

## 8. Failure Modes and Responses

| Failure | Detection | Response |
|---------|-----------|----------|
| Bad package binary | 60s health check timeout | Auto-rollback to previous version |
| Manager repo unreachable | `apt-get update` fails (non-fatal) | Continue with cached metadata, warn |
| Script killed mid-upgrade | SIGTERM/SIGINT/HUP trap | Write failure marker, exit 1 |
| Pacman version not in cache | `pacman -U` finds nothing | Fall back to `pacman -S` (no pin) |
| Concurrent self-update requests | Handler checks service active + request file | HTTP 409 UPDATE_IN_PROGRESS |
| GPG key compromised | 2-year expiry + rotation procedure | Re-enroll distributes new key |
| CRL/cert overwritten by upgrade | postinst upgrade-aware (`$2` check) | Skip CRL/cert on upgrade path |
| Auto-rollback fails (prev version not in repo) | Rollback command returns non-zero | Write failure marker, manual intervention required |
| Agent never returns after upgrade | Health poll timeout (120s) | Manager surfaces hard error, alerts operator |

---

## 9. File Manifest

### Agent-Side Files

| File | Purpose |
|------|---------|
| `configs/self-update.sh` | Multi-pkg-mgr upgrade script (255 lines, v2) |
| `configs/linux-patch-api-update.service` | systemd oneshot unit for detached upgrade |
| `src/api/handlers/system.rs` | `update_self` handler + `get_self_update_status` handler |
| `src/packages/mod.rs` | `SelfUpdateOutcome`, `SelfUpdateStatusData`, marker/request functions, `validate_version_string` |
| `src/jobs/manager.rs` | `JobOperation::SelfUpdate` variant |
| `src/enroll/client.rs` | `RepoConfig` struct, `PkiBundle.repo_config`, `fetch_repo_config` |
| `src/enroll/provision.rs` | `provision_repo_config` function |
| `src/enroll/mod.rs` | `run_enrollment` calls repo provisioning |
| `debian/prerm` | Only disables on remove, not upgrade |
| `debian/postinst` | Upgrade-aware, restarts service on upgrade |
| `linux-patch-api.spec` | RPM %post upgrade branch |
| `configs/linux-patch-api.post-install` | Alpine upgrade detection |
| `configs/linux-patch-api.install` | Arch post_upgrade |
| `scripts/build-package.sh` | Ships self-update.sh + update.service |

### Test Files

| File | Purpose |
|------|---------|
| `tests/unit/self_update_unit.rs` | 21 unit tests (validate_version_string, SelfUpdateRequest, marker file) |
| `tests/integration/enrollment_test.rs` | 3 integration tests (repo provisioning) |
| `tests/e2e/test_self_update.sh` | E2E harness (1161 lines, GPG-signed local repo) |

### Documentation Files

| File | Purpose |
|------|---------|
| `SPEC.md` | v2.0.0 Active — self-update specification (§260-304) |
| `API_SPEC.md` | Endpoint documentation (POST /system/update, GET /system/update/status, GET /pki/repo-config) |
| `THREAT_MODEL_VALIDATION.md` | §7: GPG trust chain for manager-hosted repo |
| `CHANGELOG.md` | v2.0.0 entry documenting all self-update changes |
| `tasks/self-update-architecture.md` | This document — agent-side authoritative reference |
| `tasks/self-update-design.md` | Implementation design details (502 lines) |
| `tasks/migration-guide.md` | Migration guide for existing agents (101 lines) |
| `tasks/self-update-runbook.md` | Operational runbook (230 lines) |

---

## 10. References

- **Specification:** `SPEC.md` §260-304 (Self-Update via Manager-Hosted Package Repository)
- **Implementation Design:** `tasks/self-update-design.md`
- **Threat Model:** `THREAT_MODEL_VALIDATION.md` §7
- **API Documentation:** `API_SPEC.md`
- **Operational Runbook:** `tasks/self-update-runbook.md`
- **Migration Guide:** `tasks/migration-guide.md`
