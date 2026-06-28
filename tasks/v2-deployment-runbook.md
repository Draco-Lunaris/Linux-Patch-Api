# v2.0.0 Deployment Runbook

**Date:** 2026-06-27
**Applies to:** Linux Patch API v2.0.0 self-update rollout
**Prerequisites:** Item 1 (code compiles, all tests pass) and Item 2 (E2E tests green) from the execution plan are complete.

---

## 1. Manager-Side Preparation Checklist

Complete all items below before activating the CI pipeline.

### 1.1 Generate GPG Signing Key

The GPG key signs all packages published to the manager-hosted repo. Generate it once on a secure host (not the CI runner).

~~~bash
# Generate a 4096-bit RSA signing key (done once)
gpg --batch --gen-key <<EOF
%no-protection
Key-Type: RSA
Key-Length: 4096
Key-Usage: sign
Name-Real: Linux Patch API Repo
Name-Email: lpa-repo@moon-dragon.us
Expire-Date: 2y
%commit
EOF
~~~

**Expected output:**
~~~
gpg: key XXXXXXXXXXXXXXXX marked as ultimately trusted
gpg: revocation certificate stored as ...
~~~

Verify the key was created:

~~~bash
gpg --list-secret-keys lpa-repo@moon-dragon.us
~~~

**Expected:**
~~~
sec   rsa4096 2026-06-27 [SC] [expires: 2028-06-27]
      XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX
uid           [ultimate] Linux Patch API Repo <lpa-repo@moon-dragon.us>
~~~

Export the key material:

~~~bash
# Public key (distributed via enrollment bundle)
gpg --armor --export lpa-repo@moon-dragon.us > lpa-repo-public-key.asc

# Private key (stored in Vaultwarden + CI secrets)
gpg --armor --export-secret-keys lpa-repo@moon-dragon.us > lpa-repo-private-key.asc
~~~

**Decision point:** If a signing key already exists from a prior deployment, skip generation. Verify it has not expired:

~~~bash
gpg --list-secret-keys lpa-repo@moon-dragon.us | grep expires
~~~

If expired, generate a new key and plan re-enrollment for all agents to receive the updated public key.

### 1.2 Store Private Key in Vaultwarden

Load the Vaultwarden secrets skill, then store the private key:

1. Retrieve the private key file content:

~~~bash
cat lpa-repo-private-key.asc
~~~

2. Store it as a secure note in Vaultwarden (use the vaultwarden-secrets skill).

3. Also store the public key for reference.

4. Verify retrieval works before proceeding.

### 1.3 Set Up Repo Directories on Manager Host

SSH to the manager host and create the directory structure:

~~~bash
ssh root@manager.moon-dragon.us <<'REMOTE'
# Base directory
mkdir -p /var/www/lpa-repo/{apt,dnf/el9/Packages,apk/v3.21,pacman/x86_64}

# reprepro configuration for apt
mkdir -p /var/www/lpa-repo/apt/conf
cat > /var/www/lpa-repo/apt/conf/distributions <<'REPO'
Origin: Linux Patch API
Label: LPA Repo
Codename: noble
Architectures: amd64
Components: main
Description: Linux Patch API package repository (Ubuntu 24.04)
SignWith: LPA-REPO-SIGNING-KEY

Origin: Linux Patch API
Label: LPA Repo
Codename: jammy
Architectures: amd64
Components: main
Description: Linux Patch API package repository (Ubuntu 22.04)

Origin: Linux Patch API
Label: LPA Repo
Codename: trixie
Architectures: amd64
Components: main
Description: Linux Patch API package repository (Debian 13)
REPO

# Ensure proper ownership
chown -R root:root /var/www/lpa-repo
REMOTE
~~~

**Verify:**

~~~bash
ssh root@manager.moon-dragon.us "ls -la /var/www/lpa-repo/ && cat /var/www/lpa-repo/apt/conf/distributions"
~~~

**Expected:** Directory listing showing `apt/`, `dnf/`, `apk/`, `pacman/` subdirectories and the distributions config with noble, jammy, and trixie codenames.

### 1.4 Configure axum ServeDir for Repo Routes

The manager's axum server must serve the repo directories over HTTPS. This is a manager-side code change (not agent-side Rust code).

**Design reference (§8 of architecture doc):** The manager serves packages at these paths:

| Path | Format | Content |
|------|--------|---------|
| `/apt/` | deb repo | reprepro output (Packages, Release, InRelease) |
| `/dnf/` | rpm repo | repodata with repomd.xml |
| `/apk/` | apk repo | APKINDEX.tar.gz + .apk files |
| `/pacman/` | pacman repo | lpa-repo.db.tar.zst + .pkg.tar.zst |

The manager axum server must mount `ServeDir::new("/var/www/lpa-repo/apt")` at `/apt/`, etc.

**Verification (once implemented):**

~~~bash
# From a test host with mTLS certs (or curl with --insecure for testing)
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://manager.moon-dragon.us/apt/dists/noble/Release | head -5

curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://manager.moon-dragon.us/dnf/repodata/repomd.xml | head -5
~~~

**Expected:** HTTP 200 with valid repo metadata content.

### 1.5 Update Manager Enrollment Response

The manager must include `repo_config` in the `EnrollmentStatusResponse::Approved` payload. This is a manager-side code change.

**Required fields in `repo_config`:**

| Field | Example Value | Purpose |
|-------|-------------|---------|
| `gpg_public_key` | (ARMOR ASCII) | GPG key for package verification |
| `distro_id` | `ubuntu-24.04` | Agent's detected distro |
| `sources_config` | `deb [signed-by=/etc/apt/keyrings/lpa-repo.gpg] https://manager.moon-dragon.us/apt/ ./` | Distro-specific sources line |
| `keyring_path` | `/etc/apt/keyrings/lpa-repo.gpg` | Where to install the GPG key |

**Distro-specific sources_config values:**

| Distro | sources_config |
|--------|---------------|
| Ubuntu/Debian (apt) | `deb [signed-by=/etc/apt/keyrings/lpa-repo.gpg] https://manager.moon-dragon.us/apt/ ./` |
| Fedora/AlmaLinux (dnf) | repo file content pointing to `https://manager.moon-dragon.us/dnf/` |
| Alpine (apk) | `https://manager.moon-dragon.us/apk/` |
| Arch (pacman) | Include file content pointing to `https://manager.moon-dragon.us/pacman/$repo` |

**Verification:** Test enrollment with a staging agent:

~~~bash
# On staging agent host
linux-patch-api --enroll https://manager.moon-dragon.us

# After approval, verify repo_config was received:
ls -la /etc/apt/keyrings/lpa-repo.gpg
cat /etc/apt/sources.list.d/lpa.list
apt-get update
~~~

**Expected:**
~~~
/etc/apt/keyrings/lpa-repo.gpg exists (644 permissions)
deb [signed-by=/etc/apt/keyrings/lpa-repo.gpg] https://manager.moon-dragon.us/apt/ ./
Hit:1 https://manager.moon-dragon.us/apt  InRelease
~~~

### 1.6 Implement Fallback Endpoint

The manager must implement `GET /api/v1/pki/repo-config` for agents that enrolled before v2.0.0.

**Response format:** Same `repo_config` structure as in enrollment, returned as JSON.

**Verification:**

~~~bash
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://manager.moon-dragon.us/api/v1/pki/repo-config | jq .
~~~

**Expected:**
~~~json
{
  "gpg_public_key": "-----BEGIN PGP PUBLIC KEY BLOCK-----\n...",
  "distro_id": "ubuntu-24.04",
  "sources_config": "deb [signed-by=...] https://manager.moon-dragon.us/apt/ ./",
  "keyring_path": "/etc/apt/keyrings/lpa-repo.gpg"
}
~~~

### Manager-Side Prep Completion Checklist

- [ ] GPG signing key generated (or existing key verified not expired)
- [ ] Private key stored in Vaultwarden and CI secrets
- [ ] Public key exported and available for enrollment bundle
- [ ] Repo directories created on manager host
- [ ] reprepro configured for noble, jammy, trixie codenames
- [ ] axum ServeDir serving `/apt/`, `/dnf/`, `/apk/`, `/pacman/`
- [ ] Enrollment response includes `repo_config` in Approved payload
- [ ] `GET /api/v1/pki/repo-config` fallback endpoint implemented
- [ ] Staging agent enrollment test passed (GPG key + sources list deployed)

---

## 2. CI Pipeline Activation Steps

### 2.1 Add `LPA_REPO_GPG_KEY` Secret to GitHub Actions

~~~bash
# On the machine with the private key
cat lpa-repo-private-key.asc | base64 -w 0
~~~

1. Go to GitHub repo settings: `https://github.com/Draco-Lunaris/Linux-Patch-Api/settings/secrets/actions`
2. Click **New repository secret**
3. Name: `LPA_REPO_GPG_KEY`
4. Value: paste the base64-encoded private key (or raw ASCII-armored key)
5. Click **Add secret**

**Verify:** The `publish-to-manager-repo` job in `.github/workflows/ci.yml` (line 325) references `${{ secrets.LPA_REPO_GPG_KEY }}`.

### 2.2 Add `LPA_REPO_GPG_KEY` Secret to Gitea Actions

1. Go to Gitea repo settings: `https://gitea-lxc.moon-dragon.us/git-echo/linux_patch_api/settings/actions/secrets`
2. Click **Add secret**
3. Name: `LPA_REPO_GPG_KEY`
4. Value: same private key content as GitHub
5. Save

**Verify:** The `publish-to-manager-repo` job in `.gitea/workflows/ci.yml` (line 433) references `${{ secrets.LPA_REPO_GPG_KEY }}`.

### 2.3 Verify SSH Access from CI Runner to Manager Host

The `publish-to-manager-repo` job uses `ssh root@manager.moon-dragon.us` and `scp` to push packages.

~~~bash
# Test SSH from the CI runner host (self-hosted runner for GitHub, ubuntu-24.04 runner for Gitea)
ssh root@manager.moon-dragon.us "echo SSH_OK && hostname"
~~~

**Expected:**
~~~
SSH_OK
manager.moon-dragon.us
~~~

If SSH fails:

~~~bash
# Generate SSH key on the CI runner (if not exists)
ssh-keygen -t ed25519 -f ~/.ssh/lpa_ci_key -N ""

# Copy public key to manager host
ssh-copy-id -i ~/.ssh/lpa_ci_key root@manager.moon-dragon.us

# Test again
ssh -i ~/.ssh/lpa_ci_key root@manager.moon-dragon.us "echo SSH_OK"
~~~

Also verify the tools used by the CI job are installed on the runner:

~~~bash
# GitHub runner (self-hosted, ubuntu-latest)
which dpkg-sig rpm-sign createrepo_c gpg jq openssh-client

# If missing:
sudo apt-get update && sudo apt-get install -y dpkg-sig rpm-sign createrepo-c gpg jq openssh-client
~~~

### 2.4 Trigger a Test Build

Create a test tag to trigger the full CI pipeline without a production release:

~~~bash
cd /a0/usr/projects/linux_patch_api
git tag v2.0.0-rc1
git push origin v2.0.0-rc1
~~~

**Decision point:** Use `-rc1` suffix (or similar) to distinguish test builds from production releases. The CI `if: startsWith(github.ref, 'refs/tags/v')` condition matches any tag starting with `v`.

Monitor the pipeline:

- **GitHub Actions:** `https://github.com/Draco-Lunaris/Linux-Patch-Api/actions`
- **Gitea Actions:** `https://gitea-lxc.moon-dragon.us/git-echo/linux_patch_api/actions`

Key jobs to watch (in order):

1. `fmt` / `clippy` / `test` / `enrollment-tests` / `audit` — quality gates
2. `prepare-release` — creates GitHub/Gitea release
3. `build-deb-u2404` / `build-deb-u2204` / `build-deb-debian13` / `build-rpm-fedora` / `build-rpm-almalinux` / `build-arch` / `build-alpine` — per-distro builds
4. `publish-to-manager-repo` — signs and pushes packages to manager host

### 2.5 Verify Packages in Repo

After `publish-to-manager-repo` completes, verify packages on the manager host:

~~~bash
# .deb packages (apt)
ssh root@manager.moon-dragon.us "reprepro -b /var/www/lpa-repo/apt list noble | grep linux-patch-api"
ssh root@manager.moon-dragon.us "reprepro -b /var/www/lpa-repo/apt list jammy | grep linux-patch-api"

# .rpm packages (dnf)
ssh root@manager.moon-dragon.us "ls -la /var/www/lpa-repo/dnf/el9/Packages/linux-patch-api-*.rpm"
ssh root@manager.moon-dragon.us "ls -la /var/www/lpa-repo/dnf/el9/repodata/repomd.xml.asc"

# .apk packages
ssh root@manager.moon-dragon.us "ls -la /var/www/lpa-repo/apk/v3.21/linux-patch-api-*.apk"
ssh root@manager.moon-dragon.us "ls -la /var/www/lpa-repo/apk/v3.21/APKINDEX.tar.gz"

# Arch packages
ssh root@manager.moon-dragon.us "ls -la /var/www/lpa-repo/pacman/x86_64/linux-patch-api-*.pkg.tar.zst"
ssh root@manager.moon-dragon.us "ls -la /var/www/lpa-repo/pacman/x86_64/lpa-repo.db"
~~~

**Expected:** Package files present with `.asc` signature files for rpm/arch packages.

Verify GPG signatures:

~~~bash
# Verify a .deb signature
ssh root@manager.moon-dragon.us "dpkg-sig --verify /var/www/lpa-repo/apt/pool/main/l/linux-patch-api/linux-patch-api_*.deb"

# Verify repomd.xml signature
ssh root@manager.moon-dragon.us "gpg --verify /var/www/lpa-repo/dnf/el9/repodata/repomd.xml.asc /var/www/lpa-repo/dnf/el9/repodata/repomd.xml"
~~~

**Expected:** `Goodsig` or `Good signature from "Linux Patch API Repo"`.

Test from an agent host that the repo is accessible:

~~~bash
# On a test agent with repo_config already provisioned
apt-get update
apt-cache policy linux-patch-api
~~~

**Expected:**
~~~
linux-patch-api:
  Installed: (none)
  Candidate: 2.0.0-1
  Version table:
     2.0.0-1 500
        500 https://manager.moon-dragon.us/apt ./ Packages
~~~

### CI Pipeline Activation Completion Checklist

- [ ] `LPA_REPO_GPG_KEY` secret added to GitHub Actions
- [ ] `LPA_REPO_GPG_KEY` secret added to Gitea Actions
- [ ] SSH access from CI runner to `manager.moon-dragon.us` verified
- [ ] `dpkg-sig`, `rpm-sign`, `createrepo_c` installed on CI runner
- [ ] Test tag build triggered and all CI jobs passed
- [ ] Packages verified present in manager repo with valid GPG signatures
- [ ] `apt-get update` from a test agent successfully fetches package metadata

---

## 3. Canary Deployment Procedure

Deploy to 1-2 low-risk, monitored hosts first. These are the canary hosts.

### 3.1 Select Canary Hosts

**Selection criteria:**
- Low-risk hosts (non-critical services, staging/test environments preferred)
- Hosts with active monitoring (health endpoint polled by manager)
- Hosts running the same distro as the primary fleet (for repo compatibility)
- Hosts where a brief service interruption is tolerable

**Document the selected hosts:**

| Host | Distro | Current Version | Role | Owner |
|------|--------|----------------|------|-------|
| canary-01 | Ubuntu 24.04 | 1.5.6-1 | staging | ops-team |
| canary-02 | Ubuntu 22.04 | 1.5.6-1 | staging | ops-team |

### 3.2 Re-enroll Canary Hosts

Canary hosts must have `repo_config` to self-update from the manager-hosted repo. If they were enrolled before v2.0.0, re-enroll:

~~~bash
# On each canary host
linux-patch-api --enroll https://manager.moon-dragon.us
~~~

**Expected:** Agent posts identity, manager returns 202, agent polls until approved, receives `PkiBundle` with `repo_config`.

If certs are expiring soon, use `--renew-certs` instead:

~~~bash
linux-patch-api --renew-certs
~~~

### 3.3 Verify Repo Configuration on Canary Hosts

~~~bash
# Check GPG key exists
ls -la /etc/apt/keyrings/lpa-repo.gpg
# Expected: -rw-r--r-- 1 root root ... /etc/apt/keyrings/lpa-repo.gpg

# Check sources config exists
cat /etc/apt/sources.list.d/lpa.list
# Expected: deb [signed-by=/etc/apt/keyrings/lpa-repo.gpg] https://manager.moon-dragon.us/apt/ ./

# Test repo is reachable and metadata is valid
apt-get update
# Expected: Hit:1 https://manager.moon-dragon.us/apt  InRelease
#         Reading package lists... Done

# Verify package is available
apt-cache policy linux-patch-api
# Expected: Candidate: 2.0.0-1 from https://manager.moon-dragon.us/apt
~~~

**Decision point:** If `apt-get update` fails with GPG errors, the GPG key may not have been provisioned correctly. Check:

~~~bash
file /etc/apt/keyrings/lpa-repo.gpg  # should be GPG keyring or ASCII armored
apt-key list | grep -i lpa  # verify key is known to apt
~~~

If the key is missing, use the fallback endpoint:

~~~bash
curl -s --cacert /etc/linux_patch_api/certs/ca.pem \
  --cert /etc/linux_patch_api/certs/server.pem \
  --key /etc/linux_patch_api/certs/server.key.pem \
  https://manager.moon-dragon.us/api/v1/pki/repo-config | jq -r '.gpg_public_key' > /tmp/lpa-key.asc
gpg --dearmor < /tmp/lpa-key.asc > /etc/apt/keyrings/lpa-repo.gpg
chmod 644 /etc/apt/keyrings/lpa-repo.gpg
apt-get update
~~~

### 3.4 Trigger Self-Update on Canary Hosts

From the manager, trigger the update on each canary host:

~~~bash
# Record current version before update
CANARY_HOST=canary-01

curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://${CANARY_HOST}:12443/api/v1/system/info | jq '.data.version'
# Expected: "1.5.6" or similar

# Trigger self-update to latest version
curl -X POST --cacert ca.pem --cert client.pem --key client.key \
  -H 'Content-Type: application/json' \
  -d '{}' \
  https://${CANARY_HOST}:12443/api/v1/system/update
~~~

**Expected response:**
~~~json
{"success":true,"data":{"status":"pending","target_version":"","message":"Self-update initiated; agent will restart with new version"}}
~~~

HTTP 202 Accepted.

### 3.5 Monitor Canary for 30 Minutes

**Monitor continuously for 30 minutes. Check every 5 minutes.**

#### Check 1: Update Status (via API)

~~~bash
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://${CANARY_HOST}:12443/api/v1/system/update/status | jq .
~~~

**Expected (success):**
~~~json
{
  "previous_version": "1.5.6",
  "new_version": "2.0.0-1",
  "changed": true,
  "status": "success",
  "error": null,
  "at": "2026-06-27T16:05:00Z"
}
~~~

**Expected (pending — first 1-2 minutes):**
~~~json
{
  "previous_version": "1.5.6",
  "new_version": "1.5.6",
  "changed": false,
  "status": "pending",
  "error": null,
  "at": "2026-06-27T16:00:00Z"
}
~~~

**Decision point:** If status is `failed`, STOP rollout. Check the `error` field and proceed to Section 6 (Rollback Plan).

#### Check 2: Health Endpoint

~~~bash
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://${CANARY_HOST}:12443/health
~~~

**Expected:** HTTP 200 with `{"status":"ok"}` or equivalent healthy response.

#### Check 3: Version Updated

~~~bash
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://${CANARY_HOST}:12443/api/v1/system/info | jq '.data.version'
~~~

**Expected:** `"2.0.0"` or `"2.0.0-1"` (matching the new package version).

#### Check 4: No Crash Loop

~~~bash
# Check NRestarts count — should be 0 or 1 (one restart for the upgrade itself)
ssh root@${CANARY_HOST} "systemctl show -p NRestarts linux-patch-api.service"
# Expected: NRestarts=0 or NRestarts=1

# Check the service is stable
ssh root@${CANARY_HOST} "systemctl is-active linux-patch-api.service"
# Expected: active
~~~

**Decision point:** If NRestarts is climbing (2+), the service is crash-looping. The `StartLimitBurst=5` and `StartLimitIntervalSec=300` in the service unit means systemd will stop restarting after 5 failures in 300 seconds. Check logs:

~~~bash
ssh root@${CANARY_HOST} "journalctl -u linux-patch-api.service --no-pager -n 50 --since '30 min ago'"
ssh root@${CANARY_HOST} "journalctl -u linux-patch-api-update.service --no-pager -n 50 --since '30 min ago'"
~~~

Proceed to Section 6 (Rollback Plan).

#### Check 5: CRL/Certs Unchanged

~~~bash
ssh root@${CANARY_HOST} "ls -la /etc/linux_patch_api/certs/ && md5sum /etc/linux_patch_api/certs/*.pem"
~~~

**Expected:** Same files and checksums as before the update. The `postinst` script explicitly preserves certs on upgrade (`$2` is non-empty).

#### Check 6: Marker File (on agent host)

~~~bash
ssh root@${CANARY_HOST} "cat /var/lib/linux_patch_api/last_self_update.json"
~~~

**Expected:** Same as API status check — `status: "success"`.

### 3.6 Canary Success Criteria

All canary hosts must meet ALL of these criteria:

- [ ] Update status is `success` (via API or marker file)
- [ ] Health endpoint returns 200 OK
- [ ] Version reports as 2.0.0 (or 2.0.0-1)
- [ ] NRestarts is 0 or 1 (no crash loop)
- [ ] CRL/cert files unchanged (checksums match pre-update)
- [ ] Service is `active` and stable for 30 minutes
- [ ] No errors in journalctl for `linux-patch-api.service` or `linux-patch-api-update.service`

**If ANY canary host fails ANY criterion:** STOP rollout. Do not proceed to rolling deployment. Investigate, fix, and re-run canary.

---

## 4. Rolling Deployment Procedure

Deploy in three phases: 25%, 50%, 100%. Each phase has a monitoring window.

### 4.1 Phase 1: 25% of Hosts

**Target:** Deploy to 25% of production fleet.

**Prerequisite:** Canary deployment passed all success criteria (Section 3.6).

#### Trigger Updates

For each host in the 25% batch:

~~~bash
# Record pre-update version
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://${HOST}:12443/api/v1/system/info | jq '.data.version'

# Trigger self-update
curl -X POST --cacert ca.pem --cert client.pem --key client.key \
  -H 'Content-Type: application/json' \
  -d '{}' \
  https://${HOST}:12443/api/v1/system/update
~~~

**Expected:** HTTP 202 Accepted for each host.

#### Monitor for 1 Hour

For each host in the batch, check every 10 minutes:

~~~bash
# Health check
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://${HOST}:12443/health

# Update status
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://${HOST}:12443/api/v1/system/update/status | jq '.data.status'

# Version
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://${HOST}:12443/api/v1/system/info | jq '.data.version'

# Crash loop check
ssh root@${HOST} "systemctl show -p NRestarts linux-patch-api.service"
~~~

#### Phase 1 Success Criteria

- [ ] All hosts report `status: "success"`
- [ ] All hosts report health OK (HTTP 200)
- [ ] All hosts report version 2.0.0
- [ ] No crash loops (NRestarts <= 1 on all hosts)
- [ ] No unexpected errors in journalctl

**If any host fails:** Hold rollout. Investigate the failed host. If the issue is systemic (affects multiple hosts), proceed to Section 6 (Rollback Plan) for all updated hosts. If isolated, fix the specific host and continue.

### 4.2 Phase 2: 50% of Hosts

**Target:** Deploy to the next 25% of hosts (cumulative 50%).

**Prerequisite:** Phase 1 passed all success criteria after 1 hour of monitoring.

#### Trigger Updates

Same commands as Phase 1, targeting the next 25% batch of hosts.

#### Monitor for 2 Hours

Same monitoring checks as Phase 1, every 15 minutes.

#### Phase 2 Success Criteria

- [ ] All Phase 2 hosts report `status: "success"`
- [ ] All Phase 1 hosts remain healthy (no regressions)
- [ ] All hosts report version 2.0.0
- [ ] No crash loops
- [ ] No unexpected errors in journalctl

**If any host fails:** Same decision logic as Phase 1.

### 4.3 Phase 3: 100% of Hosts

**Target:** Deploy to the remaining 50% of hosts (cumulative 100%).

**Prerequisite:** Phase 2 passed all success criteria after 2 hours of monitoring.

#### Trigger Updates

Same commands as Phase 1, targeting the remaining hosts.

#### Monitor for 24 Hours

Same monitoring checks, every hour for the first 4 hours, then every 4 hours for the remaining 20 hours.

#### Phase 3 Success Criteria

- [ ] All remaining hosts report `status: "success"`
- [ ] All previously deployed hosts remain healthy
- [ ] All hosts report version 2.0.0
- [ ] No crash loops across the fleet
- [ ] No unexpected errors in journalctl on any host

### Escalation Procedures

| Situation | Action |
|-----------|--------|
| Single host fails auto-rollback | Manual rollback (Section 6.3). Investigate that host's logs. |
| Multiple hosts fail with same error | Stop all pending updates. Roll back all affected hosts. File incident. |
| Manager cannot reach a host | Check network/firewall. If host is offline, mark for manual deployment when it returns. |
| Health check timeout (60s) on many hosts | The auto-rollback should handle this. Verify rollback succeeded on each host. If auto-rollback also fails, manual rollback required. |
| Package integrity failure (GPG) | Check if GPG key expired or repo metadata is stale. Fix repo, re-trigger updates. |

---

## 5. Migration Guide for Existing Agents

Agents enrolled before v2.0.0 have no `repo_config` and cannot self-update from the manager-hosted repo. They must be migrated.

### 5.1 Identify Agents Needing Migration

Agents enrolled before v2.0.0 will not have:
- `/etc/apt/keyrings/lpa-repo.gpg` (or distro equivalent)
- `/etc/apt/sources.list.d/lpa.list` (or distro equivalent)

~~~bash
# Check a host for repo_config presence
ssh root@${HOST} "test -f /etc/apt/keyrings/lpa-repo.gpg && echo HAS_REPO_CONFIG || echo NEEDS_MIGRATION"
~~~

For the full fleet, run a batch check:

~~~bash
# Example: check all hosts listed in hosts.txt
while read -r HOST; do
  STATUS=$(ssh root@${HOST} "test -f /etc/apt/keyrings/lpa-repo.gpg && echo MIGRATED || echo NEEDS_MIGRATION" 2>/dev/null || echo UNREACHABLE)
  echo "${HOST}: ${STATUS}"
done < hosts.txt
~~~

### 5.2 Migration Option A: Re-Enrollment (Recommended)

Re-running enrollment gets the full `PkiBundle` with `repo_config`.

**Prerequisite:** Manager is updated to v2.0.0+ and includes `repo_config` in the approval response.

~~~bash
# On the agent host
linux-patch-api --enroll https://manager.moon-dragon.us
~~~

Or if certs are expiring within the renewal threshold:

~~~bash
linux-patch-api --renew-certs
~~~

**What happens:**
1. Agent posts identity to `POST /api/v1/enroll`
2. Manager returns 202 with polling token
3. Agent polls `GET /api/v1/enroll/status/{token}`
4. Admin approves -> manager returns enriched `PkiBundle` with `repo_config`
5. Agent provisions:
   - **apt:** GPG key -> `/etc/apt/keyrings/lpa-repo.gpg`, sources -> `/etc/apt/sources.list.d/lpa.list`
   - **dnf/yum:** GPG key -> `/etc/pki/rpm-gpg/...`, repo -> `/etc/yum.repos.d/lpa.repo`
   - **apk:** URL -> `/etc/apk/repositories`
   - **pacman:** include file -> `/etc/pacman.d/lpa-repo`
6. Agent can now self-update from manager-hosted repo

**Pros:** Clean, gets full bundle, no partial state.
**Cons:** Requires manager approval (admin action).

### 5.3 Migration Option B: Fallback Fetch (Automatic)

If `repo_config` is absent from the enrollment bundle (older manager or pre-migration), the agent automatically fetches it:

~~~
GET /api/v1/pki/repo-config
~~~

This happens during `run_enrollment()` when `pki_bundle.repo_config` is `None`. If the fetch fails, enrollment continues — the agent will retry on next enrollment or self-update attempt.

**No manual action needed if the manager has implemented the fallback endpoint (Section 1.6).**

**Verification:**

~~~bash
# Check if repo_config was fetched
ls -la /etc/apt/keyrings/lpa-repo.gpg
cat /etc/apt/sources.list.d/lpa.list
apt-get update
~~~

**Pros:** No admin action needed, automatic.
**Cons:** Requires manager to implement the endpoint.

### 5.4 Migration Option C: Manual Configuration

For air-gapped hosts or when enrollment is not available:

**apt (Debian/Ubuntu):**

~~~bash
# 1. Copy GPG key (obtain from manager or Vaultwarden)
cp lpa-repo-public-key.asc /etc/apt/keyrings/lpa-repo.gpg
chmod 644 /etc/apt/keyrings/lpa-repo.gpg

# 2. Add sources list
echo 'deb [signed-by=/etc/apt/keyrings/lpa-repo.gpg] https://manager.moon-dragon.us/apt/ ./' > /etc/apt/sources.list.d/lpa.list

# 3. Update
apt-get update

# 4. Verify
apt-cache policy linux-patch-api
~~~

**dnf (Fedora/AlmaLinux):**

~~~bash
# 1. Copy GPG key
cp lpa-repo-public-key.asc /etc/pki/rpm-gpg/RPM-GPG-KEY-lpa-repo

# 2. Add repo file
cat > /etc/yum.repos.d/lpa.repo <<EOF
[lpa-repo]
name=Linux Patch API Repo
baseurl=https://manager.moon-dragon.us/dnf/
enabled=1
gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-lpa-repo
EOF

# 3. Update
dnf makecache
~~~

**apk (Alpine):**

~~~bash
# 1. Append repo URL
echo 'https://manager.moon-dragon.us/apk/' >> /etc/apk/repositories

# 2. Copy public key to abuild keyring
cp lpa-repo-public-key.rsa.pub /etc/apk/keys/

# 3. Update
apk update
~~~

**pacman (Arch):**

~~~bash
# 1. Create repo include file
cat > /etc/pacman.d/lpa-repo <<EOF
[lpa-repo]
Server = https://manager.moon-dragon.us/pacman/$repo
EOF

# 2. Add to pacman.conf
echo 'Include = /etc/pacman.d/lpa-repo' >> /etc/pacman.conf

# 3. Import GPG key
pacman-key --add lpa-repo-public-key.asc
pacman-key --lsign-key lpa-repo@moon-dragon.us

# 4. Update
pacman -Sy
~~~

**Pros:** No enrollment needed.
**Cons:** Manual, doesn't scale, GPG key not mTLS-authenticated.

### 5.5 Verify Migration

After migration, verify each agent has repo config:

~~~bash
# Check GPG key exists
ls -la /etc/apt/keyrings/lpa-repo.gpg  # apt
ls -la /etc/pki/rpm-gpg/RPM-GPG-KEY-lpa-repo  # dnf
ls -la /etc/apk/keys/lpa-repo*.rsa.pub  # apk
pacman-key --list-keys | grep lpa  # pacman

# Check sources config exists
cat /etc/apt/sources.list.d/lpa.list  # apt
cat /etc/yum.repos.d/lpa.repo  # dnf
grep lpa /etc/apk/repositories  # apk
cat /etc/pacman.d/lpa-repo  # pacman

# Test repo is reachable
apt-get update  # apt
dnf makecache  # dnf
apk update  # apk
pacman -Sy  # pacman

# Verify package is available
apt-cache policy linux-patch-api  # apt
dnf list linux-patch-api  # dnf
apk search linux-patch-api  # apk
pacman -Ss linux-patch-api  # pacman
~~~

### 5.6 Track Migration Progress

Maintain a tracking sheet:

| Host | Distro | Migration Method | Date | Verified | Notes |
|------|--------|-----------------|------|----------|-------|
| host-01 | Ubuntu 24.04 | Re-enroll | 2026-06-27 | Y | |
| host-02 | Ubuntu 22.04 | Fallback | 2026-06-27 | Y | |
| host-03 | Alpine 3.21 | Manual | 2026-06-27 | Y | Air-gapped |
| host-04 | Fedora 42 | Re-enroll | pending | N | |

**Migration timeline:**

1. **Phase 1 (v2.0.0 release):** Manager updated, new enrollments get `repo_config`.
2. **Phase 2 (rolling):** Existing agents re-enroll or use fallback fetch.
3. **Phase 3 (complete):** All agents migrated. GitHub Releases deprecated (read-only archive may remain).

---

## 6. Rollback Plan

### 6.1 Stop Triggers

**Immediate action if issues are detected during deployment:**

~~~bash
# Stop triggering new self-updates on ALL hosts
# Do NOT send any more POST /api/v1/system/update requests

# If using a script to batch-trigger updates, kill it immediately
# Example:
kill %1  # or kill the batch update script PID
~~~

### 6.2 Auto-Rollback Behavior

The `self-update.sh` script includes automatic health check and rollback:

1. After package install, script waits up to 60 seconds for `systemctl is-active linux-patch-api.service`
2. Polling interval: 5 seconds (12 attempts)
3. If service becomes active -> writes success marker, exits 0
4. If service does not become active within 60s -> **auto-rollback**:
   - Reinstalls previous version using the package manager's downgrade flag:
     - **apt:** `apt-get install -y --allow-downgrades -- linux-patch-api=${PREV_VERSION}`
     - **dnf/yum:** `dnf install -y -- linux-patch-api-${PREV_VERSION}`
     - **apk:** `apk add -- linux-patch-api=${PREV_VERSION}`
     - **pacman:** Searches `/var/cache/pacman/pkg/` for cached package, uses `pacman -U`
   - Writes failure marker with rollback status
   - Exits with error code 1

**If auto-rollback also fails:**
- The failure marker will contain `rollback rc=1` (or non-zero)
- Manual intervention is required (Section 6.3)
- Check:
  - Package cache: is the previous version still available?
  - Repo: is the repo reachable and GPG-signed correctly?
  - Service: does the binary segfault on startup?

### 6.3 Manual Rollback

#### Via API (Manager-Initiated Downgrade)

~~~bash
# Downgrade a specific host to the previous version
curl -X POST --cacert ca.pem --cert client.pem --key client.key \
  -H 'Content-Type: application/json' \
  -d '{"target_version":"1.5.6-1"}' \
  https://${HOST}:12443/api/v1/system/update
~~~

**Expected:** HTTP 202 Accepted. The agent will run `self-update.sh` which installs the specified version.

#### Via Package Manager (Direct on Host)

~~~bash
# Debian/Ubuntu
ssh root@${HOST} "apt-get install -y --allow-downgrades -- linux-patch-api=1.5.6-1"

# RPM
ssh root@${HOST} "dnf install -y -- linux-patch-api-1.5.6-1"

# Alpine
ssh root@${HOST} "apk add -- linux-patch-api=1.5.6-r0"

# Arch (from cache)
ssh root@${HOST} "pacman -U --noconfirm /var/cache/pacman/pkg/linux-patch-api-1.5.6-*.pkg.tar.zst"
~~~

After manual rollback:

~~~bash
ssh root@${HOST} <<'EOF'
systemctl restart linux-patch-api.service
rm -f /var/lib/linux_patch_api/self-update.request
rm -f /var/lib/linux_patch_api/last_self_update.json
systemctl is-active linux-patch-api.service
EOF
~~~

**Expected:** `active`

#### Batch Rollback Script

For rolling back multiple hosts at once:

~~~bash
PREV_VERSION="1.5.6-1"

while read -r HOST; do
  echo "=== Rolling back ${HOST} ==="
  curl -X POST --cacert ca.pem --cert client.pem --key client.key \
    -H 'Content-Type: application/json' \
    -d "{\"target_version\":\"${PREV_VERSION}\"}" \
    https://${HOST}:12443/api/v1/system/update 2>&1
  echo
done < affected-hosts.txt
~~~

### 6.4 Disable CI Job

Stop new package publishing to prevent further deployments:

**GitHub Actions:**

1. Go to `https://github.com/Draco-Lunaris/Linux-Patch-Api/actions/workflows/ci.yml`
2. Click the three-dot menu on the latest run
3. Click **Disable workflow**

Or remove/comment out the `publish-to-manager-repo` job in `.github/workflows/ci.yml`:

~~~yaml
# Comment out or remove the entire publish-to-manager-repo job
# publish-to-manager-repo:
#   name: Publish to Manager Repo
#   ...
~~~

**Gitea Actions:**

1. Go to `https://gitea-lxc.moon-dragon.us/git-echo/linux_patch_api/actions`
2. Disable the workflow

Or remove/comment out the `publish-to-manager-repo` job in `.gitea/workflows/ci.yml`.

**Alternative (temporary):** Remove the `LPA_REPO_GPG_KEY` secret from both GitHub and Gitea. The `publish-to-manager-repo` job will fail at the GPG import step, preventing new packages from being published.

### 6.5 Investigation Procedure

After rollback, investigate the root cause before re-deploying.

#### Step 1: Collect Marker Files

~~~bash
# On each affected host
ssh root@${HOST} "cat /var/lib/linux_patch_api/last_self_update.json"
~~~

Record the `error` field for each host.

#### Step 2: Collect Logs

~~~bash
# Update service logs
ssh root@${HOST} "journalctl -u linux-patch-api-update.service --no-pager -n 100 --since '2 hours ago'"

# Agent service logs
ssh root@${HOST} "journalctl -u linux-patch-api.service --no-pager -n 100 --since '2 hours ago'"

# File logs (if configured)
ssh root@${HOST} "tail -200 /var/log/linux_patch_api/agent.log"
~~~

#### Step 3: Check Package State

~~~bash
# Installed version
ssh root@${HOST} "dpkg-query -W -f='${Version}' linux-patch-api"

# Check if package is in half-configured state
ssh root@${HOST} "dpkg -l linux-patch-api | grep -v '^ii'"
~~~

If the package is in a half-configured state (not `ii`):

~~~bash
ssh root@${HOST} "dpkg --configure -a"
ssh root@${HOST} "apt-get install -f"
~~~

#### Step 4: Classify the Error

Use the error classification from `self-update.sh`:

| Error Class | Pattern in Logs | Meaning |
|-------------|-----------------|---------|
| `dependency_resolution_failed` | unmet dependencies, held broken, unresolvable | Package deps not met |
| `disk_full` | No space left, disk full | Insufficient disk space |
| `package_not_found` | Unable to locate package, not found | Package not in repo |
| `permission_denied` | Permission denied, not authorized | Access control issue |
| `package_manager_locked` | locked, another process | dpkg/apt lock held |
| `package_integrity_failure` | hash sum mismatch, checksum, signature | GPG or checksum failure |
| `upgrade_failed` | (default) | Other upgrade failure |

#### Step 5: Fix and Re-Deploy

1. Fix the root cause (code, repo config, GPG key, etc.)
2. Re-run Item 1 (tests) and Item 2 (E2E) from the execution plan
3. Re-run canary deployment (Section 3)
4. If canary passes, resume rolling deployment

### Rollback Plan Completion Checklist

- [ ] All self-update triggers stopped (no new POST /system/update requests)
- [ ] Auto-rollback verified on affected hosts (check marker files)
- [ ] Manual rollback performed on hosts where auto-rollback failed
- [ ] All affected hosts verified running previous version
- [ ] `publish-to-manager-repo` CI job disabled (GitHub + Gitea)
- [ ] Root cause investigation completed
- [ ] Fix applied and tested (Items 1 and 2 re-run)
- [ ] Canary deployment re-run successfully before resuming rollout

---

## Appendix A: Quick Reference Commands

### Trigger Self-Update (Single Host)

~~~bash
curl -X POST --cacert ca.pem --cert client.pem --key client.key \
  -H 'Content-Type: application/json' \
  -d '{}' \
  https://HOST:12443/api/v1/system/update
~~~

### Trigger Self-Update (Specific Version)

~~~bash
curl -X POST --cacert ca.pem --cert client.pem --key client.key \
  -H 'Content-Type: application/json' \
  -d '{"target_version":"1.5.6-1"}' \
  https://HOST:12443/api/v1/system/update
~~~

### Check Update Status

~~~bash
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://HOST:12443/api/v1/system/update/status | jq .
~~~

### Check Health

~~~bash
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://HOST:12443/health
~~~

### Check Version

~~~bash
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://HOST:12443/api/v1/system/info | jq '.data.version'
~~~

### Check NRestarts

~~~bash
ssh root@HOST "systemctl show -p NRestarts linux-patch-api.service"
~~~

### Check Marker File

~~~bash
ssh root@HOST "cat /var/lib/linux_patch_api/last_self_update.json"
~~~

### Check Service Status

~~~bash
ssh root@HOST "systemctl status linux-patch-api.service"
ssh root@HOST "systemctl is-active linux-patch-api.service"
~~~

### View Logs

~~~bash
ssh root@HOST "journalctl -u linux-patch-api.service --no-pager -n 50 --since '1 hour ago'"
ssh root@HOST "journalctl -u linux-patch-api-update.service --no-pager -n 50 --since '1 hour ago'"
~~~

## Appendix B: Marker File Format

~~~json
{
  "previous_version": "1.5.6",
  "new_version": "2.0.0-1",
  "changed": true,
  "status": "success",
  "error": null,
  "at": "2026-06-27T16:05:00Z"
}
~~~

**Status values:** `pending`, `success`, `failed`

**Failure marker example:**

~~~json
{
  "previous_version": "1.5.6",
  "new_version": "1.5.6",
  "changed": false,
  "status": "failed",
  "error": "Post-upgrade health check failed — rolled back to 1.5.6 (rollback rc=0)",
  "at": "2026-06-27T16:02:00Z"
}
~~~

## Appendix C: Service Unit Reference

### linux-patch-api.service (Agent)

| Property | Value |
|----------|-------|
| Type | simple |
| ExecStart | `/usr/bin/linux-patch-api --config /etc/linux_patch_api/config.yaml` |
| Restart | on-failure |
| RestartSec | 10s |
| StartLimitBurst | 5 |
| StartLimitIntervalSec | 300 |
| Port | 12443 (mTLS) |

### linux-patch-api-update.service (Update Unit)

| Property | Value |
|----------|-------|
| Type | oneshot |
| ExecStart | `/usr/lib/linux-patch-api/self-update.sh` |
| TimeoutStartSec | 300 (5 minutes) |
| RemainAfterExit | no |
| Cgroup | `system.slice/linux-patch-api-update.service` (separate from agent) |

**Critical design:** The update unit runs in its own cgroup under `system.slice`. When dpkg's `prerm` stops `linux-patch-api.service`, the update unit survives because it is in a different cgroup. This prevents the half-configured package state that caused the v1.5.0-beta failure.

## Appendix D: Distro-Specific Repo Paths

| Distro | GPG Key Path | Sources Path |
|--------|-------------|---------------|
| Ubuntu/Debian (apt) | `/etc/apt/keyrings/lpa-repo.gpg` | `/etc/apt/sources.list.d/lpa.list` |
| Fedora/AlmaLinux (dnf) | `/etc/pki/rpm-gpg/RPM-GPG-KEY-lpa-repo` | `/etc/yum.repos.d/lpa.repo` |
| Alpine (apk) | `/etc/apk/keys/lpa-repo.rsa.pub` | `/etc/apk/repositories` (appended) |
| Arch (pacman) | imported via `pacman-key` | `/etc/pacman.d/lpa-repo` |

## Appendix E: CI Pipeline Job Dependencies

### GitHub Actions

~~~
fmt ─┐
clippy ─┤
test ─┼─→ prepare-release ─→ build-deb-u2404 ─┐
enrollment-tests ─┤   build-deb-u2204 ─┤
audit ─┘   build-deb-debian13 ─┤→ publish-to-manager-repo
            build-rpm-fedora ─┤
            build-rpm-almalinux ─┤
            build-arch ─┤
            build-alpine ─┘
~~~

### Gitea Actions

~~~
fmt ─┐
clippy ─┤
test ─┼─→ build-deb ─┐
enrollment-tests ─┤   build-deb-u2204 ─┤
            build-rpm ─┤→ publish-to-manager-repo
            build-apk ─┤
            build-arch ─┘
~~~

**Trigger condition:** `if: startsWith(github.ref, 'refs/tags/v')` — only on tag pushes.

**Note:** The Gitea pipeline does not include Debian 13 or AlmaLinux builds (only Ubuntu 24.04, Ubuntu 22.04, Fedora, Alpine, Arch). The GitHub pipeline covers all distros including Debian 13 and AlmaLinux 10.
