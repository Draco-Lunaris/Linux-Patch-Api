# Deep-Dive Gap Analysis: Self-Update Implementation

**Date:** 2026-06-26
**Baseline:** v1.5.6 master, SPEC.md, self-update-design.md, manager-hosted-repo-design.md

---

## 1. Architecture Gaps

### Gap 1.1: Enrollment Repo Provisioning — No Implementation Yet

The spec (§266-272) and design (§4.3) require the enrollment `PkiBundle` to carry `repo_config` (GPG key + sources config). The current `src/enroll/client.rs` and `src/enroll/provision.rs` have **no `RepoConfig` struct, no `repo_config` field in `PkiBundle`, and no provisioning logic**.

**Risk**: Without this, agents have no repo source configured. `self-update.sh` would fail because `apt-get update` has no repo to pull from.

**Documentation needed**: The enrollment flow must be updated in `SPEC.md` §Self-Enrollment Workflow to include repo provisioning as a Phase 3 substep.

### Gap 1.2: Fallback `GET /api/v1/pki/repo-config` Not Implemented

Spec §272 says: "If `repo_config` is absent from the bundle (older enrollment), the agent fetches it on demand from `GET /api/v1/pki/repo-config`." This endpoint doesn't exist on the manager side, and the agent has no fallback fetch logic.

**Risk**: Agents enrolled before the repo feature ships will have no repo config and no way to get one without re-enrollment.

### Gap 1.3: `self-update.sh` Still Uses GitHub Releases

The current `configs/self-update.sh` (10KB) still has `GITHUB_OWNER`, `GITHUB_REPO`, and GitHub API parsing. The design doc §6.1 provides a complete rewrite (~110 lines, native package manager only). The rewrite is documented but **not implemented**.

### Gap 1.4: Post-Upgrade Health Check + Auto-Rollback Not in Current Script

The design §6.1 includes a 60-second health check loop with automatic rollback. The current `self-update.sh` has a trap-based failure marker but **no health check and no rollback logic**.

---

## 2. Enrollment & PKI Gaps

### Gap 2.1: No `RepoConfig` Struct in Enrollment Types

Need to add `RepoConfig` struct and `repo_config: Option<RepoConfig>` field to `PkiBundle` in `src/enroll/client.rs`.

### Gap 2.2: Distro Detection for Repo Config Path Selection

The repo provisioning code needs distro detection to write sources config to the correct path. Should reuse existing `src/enroll/identity.rs` distro detection.

**Risk**: If `distro_id` from the manager doesn't match the agent's actual distro, sources config gets written to the wrong path.

### Gap 2.3: GPG Key Trust Chain Not Documented

Trust model: mTLS enrollment → GPG key → package signatures. If enrollment is compromised, package trust is compromised. This transitive trust chain is not documented in the threat model.

---

## 3. Package Manager Gaps

### Gap 3.1: Pacman Version Pinning Not Supported

The design's `self-update.sh` uses `pacman -S --noconfirm -- linux-patch-api=$TARGET_VERSION` which does not work — pacman doesn't support `=` version syntax.

**Fix**: Use `pacman -U` from a specific package file, or document Arch version pinning as unsupported.

### Gap 3.2: `eval` in self-update.sh is a Security Risk

The design's script uses `UPGRADE_OUTPUT=$(eval "$UPGRADE_CMD" 2>&1)`. The design's own Prohibited Architecture list (§8) bans "invoking `sh -c` with interpolated values." `eval` is equivalent.

**Fix**: Replace `eval` with direct command execution using `case`/`esac` branches that run commands directly.

### Gap 3.3: No `set -e` in Design Script — Silent Failures

The design script uses `set -uo pipefail` but NOT `set -e`. Version query commands that fail silently produce "unknown" and the script continues.

**Fix**: Either add `set -e` with explicit `|| true` on non-fatal commands, or document which failures are acceptable.

---

## 4. Review Bug Gaps

### Gap 4.1: CRITICAL-1 (prerm disables on upgrade) — RESOLVED ✅

Verified: `debian/prerm` now only runs `systemctl disable` on `remove`, NOT on `upgrade`. It stops the service on upgrade but preserves the enable state. Postinst restarts it.

### Gap 4.1b: RPM %post Upgrade Branch — RESOLVED ✅

Verified: RPM spec has `elif [ $1 -gt 1 ]` branch with `systemctl daemon-reload` + `systemctl restart`. RPM %preun only stops/disables on removal (`$1 -eq 0`). RPM %install includes self-update.sh and update.service.

### Gap 4.1c: Packaging Scripts — RESOLVED ✅

Verified: `scripts/build-package.sh` ships both `self-update.sh` (line 73, chmod 755) and `linux-patch-api-update.service` (line 71). All four distro post-install scripts (Debian, RPM, APK, Arch) are upgrade-aware and restart the service on upgrade.

### Gap 4.2: CRITICAL-2 (Handler bypasses JobManager) — Intentional but Undocumented

The handler uses its own concurrency guard instead of `JobManager::create_job()`. This is intentional (JobManager is in-memory, destroyed on restart), but the decision is not documented in code comments.

**Fix**: Add a comment block in `update_self` handler explaining why it doesn't use JobManager.

### Gap 4.3: Signal Handling in self-update.sh

No `trap` for SIGTERM/SIGINT/SIGHUP. If the update service is killed mid-upgrade, no failure marker is written.

---

## 5. CI/CD & Packaging Gaps

### Gap 5.1: No CI Job for Repo Publishing

The design §5.3 defines a `publish-to-manager-repo` CI job. This doesn't exist yet. Current CI only publishes to GitHub Releases.

### Gap 5.2: `scripts/build-package.sh` Doesn't Ship New Files

Need to verify whether `self-update.sh` and `linux-patch-api-update.service` are included in packages.

### Gap 5.3: GPG Key Generation and Storage Not Documented Operationally

No operational runbook covering:
- Who runs key generation
- Where the private key is stored (Vaultwarden collection?)
- How CI accesses it (which secret name?)
- Key rotation procedure

---

## 6. Testing Gaps

### Gap 6.1: No Unit Tests for New Types

Need: `validate_version_string` injection tests, `SelfUpdateRequest` deserialization, marker read/write, handler paths.

### Gap 6.2: E2E Test Uses GitHub Releases, Not Manager Repo

The existing `tests/e2e/test_self_update.sh` (45KB) was written for GitHub Releases flow. Needs update for manager-hosted repo.

### Gap 6.3: No Test for Enrollment Repo Provisioning

No test verifies enrollment writes GPG keys and sources config correctly across all four distro families.

---

## 7. Documentation Gaps

### Gap 7.1: SPEC.md Version Out of Date

SPEC.md says "Version: 1.2.0" and "Status: Draft." Should reflect current target version.

### Gap 7.2: No Migration Guide for Existing Agents

Agents already deployed need a migration path: re-enrollment, fallback fetch, or `--renew-certs`.

### Gap 7.3: No Operational Runbook

No document covering: triggering self-update, monitoring progress, diagnosing failures, manual rollback, log checking.

---

## 8. Summary: Priority-Ordered Action Items

| Priority | Gap | Action |
|----------|-----|--------|
| **P0** | self-update.sh still uses GitHub Releases | Rewrite to native package manager commands per design §6.1 |
| **P0** | No RepoConfig in enrollment PkiBundle | Add struct + provisioning logic in `src/enroll/` |
| **P0** | prerm disables on upgrade | Fix `debian/prerm` (verify current state first) |
| **P1** | No post-upgrade health check / auto-rollback | Add to rewritten self-update.sh |
| **P1** | `eval` in design script | Replace with direct command execution |
| **P1** | Pacman version pinning unsupported | Document limitation or implement `pacman -U` path |
| **P1** | No CI job for repo publishing | Add `publish-to-manager-repo` to CI workflows |
| **P2** | No fallback `GET /api/v1/pki/repo-config` | Implement agent-side fetch + manager-side endpoint |
| **P2** | No signal trap in self-update.sh | Add `trap` for SIGTERM/SIGINT/SIGHUP |
| **P2** | No unit tests for new types | Add tests per design §6.1 |
| **P2** | E2E test uses GitHub Releases | Update for manager-hosted repo flow |
| **P3** | SPEC.md version out of date | Bump version, update status |
| **P3** | No migration guide for existing agents | Document re-enrollment or fallback fetch path |
| **P3** | No operational runbook | Create runbook for self-update operations |
| **P3** | Handler bypasses JobManager undocumented | Add code comment explaining architectural decision |
