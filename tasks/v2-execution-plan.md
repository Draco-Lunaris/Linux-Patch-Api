# Self-Update v2.0.0 Execution Plan

> **⚠ DEPRECATED — 2026-06-29:** This document references the rejected CI-push / Vaultwarden GPG storage / publish-to-manager-repo design model. The canonical design is the **Manager Pull model** per AGENTS.md Rules 1-2 and INTERFACE_CONTRACT.md in the Linux-Patch-Manager repo. GPG keys are stored per-manager at `/etc/patch-manager/ca/`, NEVER in Vaultwarden or CI secrets. Treat all CI-push, publish-to-manager-repo, and Vaultwarden references below as historical artifacts, not current design.

**Date:** 2026-06-27
**Status:** Active

---

## Item 1: Implementation Execution

### Goal
Verify all code compiles, all tests pass, no regressions.

### Steps

#### 1.1 Cargo Check (agent-side)
```bash
cd /a0/usr/projects/linux_patch_api
cargo check --all-targets 2>&1
```
**Expected:** zero errors, zero warnings (or only pre-existing warnings)
**On failure:** fix compilation errors before proceeding

#### 1.2 Unit Tests
```bash
cargo test --test self_update_unit -- --test-threads=1 2>&1
cargo test --test enroll_identity 2>&1
cargo test --lib 2>&1
```
**Expected:** 56 self-update tests pass, all lib tests pass
**On failure:** examine test output, fix failing tests

#### 1.3 Integration Tests
```bash
cargo test --test enrollment_test 2>&1
cargo test --test auth_test 2>&1
cargo test --test api_test 2>&1
```
**Expected:** all integration tests pass (including 3 new repo_config tests)
**On failure:** check if tests require system services (systemd, apt) — may need to skip on non-Linux

#### 1.4 Shell Script Validation
```bash
bash -n configs/self-update.sh 2>&1  # syntax check
shellcheck configs/self-update.sh 2>&1 || true  # lint (optional)
```
**Expected:** no syntax errors

#### 1.5 Clippy Lint
```bash
cargo clippy --all-targets 2>&1
```
**Expected:** no new clippy warnings from our changes

#### 1.6 Test Coverage Report
```bash
cargo tarpaulin --out Html --outputDir coverage/ 2>&1 || true
```
**Target:** 95%+ coverage on self-update code paths

### Parallel Sub-Agent Assignment
- **Sub-agent A (developer):** Run 1.1-1.3 (cargo check, unit tests, integration tests)
- **Sub-agent B (developer):** Run 1.4-1.6 (shell validation, clippy, coverage)
- Both can run in parallel since they're independent checks

---

## Item 2: Integration Testing

### Goal
Verify self-update works end-to-end with a real agent + manager setup.

### Prerequisites
- Rust toolchain (for building packages)
- A test VM or LXC container with systemd
- GPG tools (gnupg, dpkg-sig)
- The E2E test harness at `tests/e2e/test_self_update.sh`

### Steps

#### 2.1 Provision Test Environment
- Create disposable LXC/VM with Ubuntu 24.04
- Install prerequisites: dpkg-dev, gnupg, curl, python3
- Copy test certs to the test environment

#### 2.2 Build Test Packages
- Build vN package (current version)
- Bump version, build vN+1 package
- Build broken vN+2 package (binary replaced with `exit 1`)

#### 2.3 Set Up Local GPG-Signed Repo
- Generate GPG key on test host
- Create flat apt repo with vN + vN+1
- Sign Release file + InRelease
- Configure apt sources with `signed-by=`

#### 2.4 Run E2E Test Suite
```bash
./tests/e2e/test_self_update.sh <test-host>
```

**Test cases to verify:**
- (a) Upgrade vN → vN+1: version changes, service healthy, CRL/certs unchanged
- (b) Same version: changed=false, no restart
- (c) restart=false: no restart, version staged
- (d) Broken binary → health check fails → auto-rollback to vN+1
- (e) Manager downgrade: vN+1 → vN via target_version
- CRL/cert preservation via dpkg
- Update service survives agent stop (cgroup isolation)
- Validation rejection (injection attempts)
- Status endpoint
- Concurrent request handling

#### 2.5 Verify Marker File States
- Check marker file after each test case
- Verify previous_version, new_version, changed, status fields
- Verify timestamp is RFC3339

#### 2.6 Verify No Crash Loop
- Check `systemctl show -p NRestarts linux-patch-api.service`
- NRestarts should not climb excessively

### Parallel Sub-Agent Assignment
- **Sub-agent C (developer):** Steps 2.1-2.3 (provision, build, repo setup)
- **Sub-agent D (hacker):** Step 2.4-2.6 (run tests, verify results)
- D depends on C completing first

---

## Item 3: Deployment Plan

### Goal
Roll out v2.0.0 to production agents with zero downtime.

### Prerequisites
- Item 1 passes (code compiles, tests pass)
- Item 2 passes (E2E tests green)
- Manager updated to support repo_config in enrollment response
- Manager repo infrastructure set up (GPG key, reprepro, repo directories)

### Steps

#### 3.1 Manager-Side Preparation
1. Generate GPG signing key (if not exists)
2. Store private key in Vaultwarden + CI secrets
3. Set up repo directories on manager host:
   - `/var/www/lpa-repo/apt/` (with reprepro config)
   - `/var/www/lpa-repo/dnf/`
   - `/var/www/lpa-repo/apk/`
   - `/var/www/lpa-repo/pacman/`
4. Configure axum ServeDir for repo routes (per design §8)
5. Update manager enrollment response to include `repo_config` in `Approved` variant
6. Implement `GET /api/v1/pki/repo-config` endpoint for fallback
7. Test enrollment with a staging agent — verify repo_config is received

#### 3.2 CI Pipeline Activation
1. Add `LPA_REPO_GPG_KEY` secret to GitHub Actions
2. Add `LPA_REPO_GPG_KEY` secret to GitHub Actions
3. Ensure SSH access from CI runner to manager host
4. Trigger a test build — verify `publish-to-manager-repo` job runs successfully
5. Verify packages appear in repo with valid GPG signatures

#### 3.3 Canary Deployment (10% of hosts)
1. Select 1-2 canary hosts (low-risk, monitored)
2. Re-enroll canary hosts (to get repo_config)
3. Verify canary hosts have:
   - GPG key at `/etc/apt/keyrings/lpa-repo.gpg`
   - Sources config at `/etc/apt/sources.list.d/lpa.list`
   - `apt-get update` succeeds
4. Manager triggers `POST /api/v1/system/update` on canary hosts
5. Monitor for 30 minutes:
   - Health check passes
   - Version updated
   - No crash loop
   - CRL/certs unchanged
6. If any canary fails: stop rollout, investigate, fix

#### 3.4 Rolling Deployment (25% → 50% → 100%)
1. Deploy to 25% of hosts
2. Monitor for 1 hour
3. If all healthy: deploy to 50%
4. Monitor for 2 hours
5. If all healthy: deploy to 100%
6. Monitor for 24 hours

#### 3.5 Migration of Existing Agents
1. Identify agents enrolled before v2.0.0 (no repo_config)
2. Option A: Trigger re-enrollment via `--renew-certs` (certs expiring within threshold)
3. Option B: Fallback fetch via `GET /api/v1/pki/repo-config`
4. Option C: Manual configuration (for air-gapped hosts)
5. Verify each migrated agent can `apt-get update` from manager repo
6. Track migration progress in manager dashboard

#### 3.6 Rollback Plan
If v2.0.0 causes issues:
1. Manager stops triggering new self-updates
2. Affected hosts auto-rollback to previous version (health check)
3. For hosts that didn't auto-rollback: manual `POST /system/update` with target_version=previous
4. Disable `publish-to-manager-repo` CI job to stop new package publishing
5. Investigate root cause, fix, re-deploy

### Parallel Sub-Agent Assignment
- **Sub-agent E (researcher):** Step 3.1 (manager-side preparation checklist)
- **Sub-agent F (default):** Steps 3.2-3.3 (CI activation + canary deployment procedure)
- **Sub-agent G (default):** Steps 3.4-3.6 (rolling deployment + migration + rollback)
- E, F can run in parallel; G depends on F

---

## Item 4: CI/CD Pipeline Verification

### Goal
Verify the `publish-to-manager-repo` CI job works end-to-end.

### Prerequisites
- Item 1 passes
- Manager host accessible via SSH from CI runner
- GPG signing key generated and stored in CI secrets
- Repo directories created on manager

### Steps

#### 4.1 Verify CI Config Syntax
```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
```
**Expected:** both files parse as valid YAML

#### 4.2 Verify Job Dependencies
```bash
# Check that publish-to-manager-repo depends on all build jobs
grep -A5 'publish-to-manager-repo' .github/workflows/ci.yml | grep 'needs:'
grep -A5 'publish-to-manager-repo' .github/workflows/ci.yml | grep 'needs:'
```
**Expected:** all build jobs listed as dependencies

#### 4.3 Dry-Run CI Job (GitHub Actions)
1. Create a test tag (e.g., `v2.0.0-rc1`)
2. Push tag to trigger CI pipeline
3. Monitor build jobs — all should pass
4. Monitor `publish-to-manager-repo` job:
   - GPG key imported successfully
   - .deb packages signed and published via reprepro
   - .rpm packages signed and published via createrepo_c
   - .apk packages signed and published
   - Arch packages signed and published
5. Verify packages on manager host:
   ```bash
   ssh root@patch-manager.example.com "ls -la /var/www/lpa-repo/apt/pool/"
   ssh root@patch-manager.example.com "ls -la /var/www/lpa-repo/dnf/"
   ```

#### 4.4 Verify GPG Signatures
```bash
# On a test agent host
apt-get update
apt-cache policy linux-patch-api  # should show new version from lpa repo
apt-get install --dry-run linux-patch-api  # should verify GPG signature
```

#### 4.5 Verify CI Job
1. Push tag to repo
2. Monitor GitHub Actions pipeline
3. Verify same steps run successfully

#### 4.6 Cleanup Test Artifacts
```bash
# Remove test tag
git tag -d v2.0.0-rc1
git push origin :refs/tags/v2.0.0-rc1
# Remove test packages from repo (optional)
```

### Parallel Sub-Agent Assignment
- **Sub-agent H (developer):** Steps 4.1-4.2 (config syntax + dependency verification)
- **Sub-agent I (hacker):** Steps 4.3-4.4 (dry-run + GPG verification)
- **Sub-agent J (developer):** Steps 4.5-4.6 (CI verification + cleanup)
- H can run immediately; I depends on H; J depends on I

---

## Execution Order

```
Item 1 (Implementation)
  ├─ Sub-agent A: cargo check + unit + integration tests
  ├─ Sub-agent B: shell validation + clippy + coverage
  └─ Wait for both → proceed to Item 2

Item 2 (Integration Testing)
  ├─ Sub-agent C: provision + build + repo setup
  ├─ Sub-agent D: run E2E tests (depends on C)
  └─ Wait for both → proceed to Item 3

Item 3 (Deployment Plan)
  ├─ Sub-agent E: manager-side preparation
  ├─ Sub-agent F: CI activation + canary (depends on E)
  ├─ Sub-agent G: rolling deployment + migration + rollback (depends on F)
  └─ Wait for all → proceed to Item 4

Item 4 (CI/CD Verification)
  ├─ Sub-agent H: config syntax + dependency verification
  ├─ Sub-agent I: dry-run + GPG verification (depends on H)
  ├─ Sub-agent J: CI verification + cleanup (depends on I)
  └─ Wait for all → DONE
```

## Success Criteria

| Item | Criteria |
|------|----------|
| 1 | cargo check clean, 56+ tests pass, no new clippy warnings |
| 2 | All 10 E2E test cases pass, marker files correct, no crash loops |
| 3 | Canary hosts upgraded successfully, 100% rollout within 48h |
| 4 | CI job publishes signed packages to manager repo, GPG verified |
