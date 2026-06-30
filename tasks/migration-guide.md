# Migration Guide: Manager-Hosted Repo for Existing Agents

> **⚠ DEPRECATED — 2026-06-29:** This document references the rejected CI-push / Vaultwarden GPG storage / publish-to-manager-repo design model. The canonical design is the **Manager Pull model** per AGENTS.md Rules 1-2 and INTERFACE_CONTRACT.md in the Linux-Patch-Manager repo. GPG keys are stored per-manager at `/etc/patch-manager/ca/`, NEVER in Vaultwarden or CI secrets. Treat all CI-push, publish-to-manager-repo, and Vaultwarden references below as historical artifacts, not current design.

**Date:** 2026-06-27
**Applies to:** Agents enrolled before the manager-hosted repo feature (v2.0.0)

---

## Background

Agents enrolled before v2.0.0 received only PKI certificates (CA, server cert, server key, CRL) during enrollment. They have no package repository configured and rely on the old GitHub Releases download path in `self-update.sh`.

Starting with v2.0.0, the enrollment `PkiBundle` includes an optional `repo_config` field with a GPG public key and distro-specific sources configuration. This enables agents to self-update from a manager-hosted package repository using native package manager commands.
The old GitHub Releases download path has been **removed** from `self-update.sh`.

## Migration Options

### Option A: Re-Enrollment (Recommended)

Re-run enrollment to get the full `PkiBundle` with `repo_config`:

```bash
# Manual re-enrollment
linux-patch-api --enroll https://<manager-host>

# Or trigger auto-enrollment by expiring certs
linux-patch-api --renew-certs
```

**Steps:**
1. Manager must be updated to v2.0.0+ (adds `repo_config` to approval response)
2. Run `--enroll` or `--renew-certs` on the agent
3. Agent receives new `PkiBundle` with `repo_config`
4. Agent provisions GPG key + sources config to distro-specific paths
5. Agent can now self-update from manager-hosted repo

**Pros:** Clean, gets full bundle, no partial state
**Cons:** Requires manager approval (admin action)

### Option B: Fallback Fetch (Automatic)

If `repo_config` is absent from the enrollment bundle (older manager), the agent automatically fetches it from the manager:

```
GET /api/v1/pki/repo-config
```

This happens during enrollment in `run_enrollment()` when `pki_bundle.repo_config` is `None`. If the fetch fails, enrollment continues — the agent will retry on next enrollment or self-update attempt.

**Pros:** No admin action needed, automatic
**Cons:** Requires manager to implement the endpoint

### Option C: Manual Configuration

Manually configure the repo on each agent:

```bash
# apt (Debian/Ubuntu)
# 1. Copy GPG key
cp lpa-repo-public-key.asc /etc/apt/keyrings/lpa-repo.gpg
chmod 644 /etc/apt/keyrings/lpa-repo.gpg
# 2. Add sources list
echo 'deb [signed-by=/etc/apt/keyrings/lpa-repo.gpg] http://<manager-host>/apt/ ./' > /etc/apt/sources.list.d/lpa.list
# 3. Update
apt-get update
```

**Pros:** No enrollment needed
**Cons:** Manual, doesn't scale, GPG key not mTLS-authenticated

## Migration Timeline

1. **Phase 1 (v2.0.0 release):** Manager updated, new enrollments get `repo_config`. GitHub Releases remains as read-only archive.
2. **Phase 2 (rolling):** Existing agents re-enroll or use fallback fetch. GitHub Releases stays as fallback.
3. **Phase 3 (complete):** All agents migrated. GitHub Releases deprecated (read-only archive may remain).

## Verification

After migration, verify the agent has repo config:

```bash
# Check GPG key exists
ls -la /etc/apt/keyrings/lpa-repo.gpg  # or distro-specific path

# Check sources config exists
cat /etc/apt/sources.list.d/lpa.list  # or distro-specific path

# Test repo is reachable
apt-get update  # or dnf makecache, apk update, pacman -Sy

# Verify package is available
apt-cache policy linux-patch-api
```
