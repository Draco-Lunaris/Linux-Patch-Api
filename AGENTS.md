# AGENTS.md — Linux Patch API (Agent)

**Repository:** `Draco-Lunaris/Linux-Patch-Api`
**Purpose:** Agent that runs on managed Linux hosts for patch management
**Language:** Rust
**Default Branch:** `master`

---

## Project Overview

The Linux Patch API is the agent-side component of the Linux Patch Management system. It runs on managed hosts, communicates with the manager via mTLS, executes package operations, and performs self-updates from a manager-hosted GPG-signed package repository.

### Key Components

| Module | Purpose |
|--------|---------|
| `src/api/handlers/` | API endpoints (packages, patches, system, health, self-update) |
| `src/auth/` | mTLS authentication, CRL verification, IP whitelist |
| `src/enroll/` | Enrollment client (request enrollment, receive PkiBundle, provision repo config) |
| `src/jobs/` | Async job manager for package operations |
| `src/packages/` | Package manager abstraction (apt, dnf, apk, pacman) |
| `configs/` | systemd units, self-update.sh, packaging scripts |

---

## Build & Test Commands

```bash
# Check compilation
cargo check

# Format code
cargo fmt --all

# Lint
cargo clippy --all-targets --all-features

# Run tests
cargo test --workspace --all-features --lib --bins --tests

# Security audit
cargo audit

# Build release
cargo build --release

# Build package
./scripts/build-package.sh
```

---

## Critical Architectural Rules

### 1. Manager Pull Model ONLY

The ONLY valid package delivery mechanism for self-updates is the **Manager Pull model**. The manager pulls packages from GitHub Releases via HTTP, signs them with its own GPG key, and hosts them in a local repo. The agent receives the GPG public key and repo config during enrollment.

**NEVER implement or reference a CI push model.** CI push is logistically impossible and was never the intended design.

### 2. Per-Manager GPG Key

Each manager has its own unique GPG signing key. The agent receives the public key via enrollment (`PkiBundle.repo_config.gpg_public_key`) and provisions it to the native package manager's keyring. The agent trusts ONLY the key delivered via mTLS enrollment.

**NEVER hardcode or embed GPG keys.** The key is per-manager and delivered at enrollment time.

### 3. No Embedded Credentials

This is an open-source project. Agents may number in the thousands. **NEVER embed credentials, tokens, or secrets in code or configuration.** The agent uses mTLS certificates received during enrollment for all manager communication.

### 4. Self-Update via Native Package Manager

The agent self-updates using the host's native package manager (apt, dnf, apk, pacman). The update runs in a detached systemd unit (`linux-patch-api-update.service`) with its own cgroup to survive the agent being killed by dpkg prerm.

**NEVER run `apt-get install` in the agent's own process.** Always use the detached systemd unit.

### 5. Enrollment Protocol

- Agent posts identity to `POST /api/v1/enroll` (machine-id, FQDN, IPs, OS details)
- Polls `GET /api/v1/enroll/status/{token}` until approved
- Receives `PkiBundle` with CA cert, server cert/key, CRL, and optional `repo_config`
- Provisions PKI files and repo config to distro-specific paths
- Fallback: if `repo_config` absent, fetches `GET /api/v1/pki/repo-config`

### 6. Health Reporting

Agent health endpoint (`GET /health`) reports:
- `version`: agent version
- `crl_status`: CRL validity (valid/expired/missing/invalid)
- `crl_age_seconds`: time since last CRL refresh
- `crl_next_update`: when CRL expires
- `gpg_key_status`: GPG key validity (valid/expired/missing/revoked)
- `gpg_key_expires_at`: when GPG key expires

---

## Git Conventions

- **Branch naming:** `feat/`, `fix/`, `docs/`, `chore/`, `release/` prefixes
- **Commit format:** Conventional commits (`feat:`, `fix:`, `docs:`, `chore:`)
- **PR required for master:** Branch protection is enabled
- **Tag format:** `vX.Y.Z` for releases, `vX.Y.Z-N` for hotfix revisions

---

## Related Repositories

- **Manager:** `Draco-Lunaris/Linux-Patch-Manager` — The server that manages hosts and hosts the package repo
- **Shared Spec:** `SPEC.md` in the manager repo defines the manager-agent contract

---

## Lessons Learned

1. **CI push hallucination:** Design docs described a CI push model that referenced non-existent servers. Removed and replaced with Manager Pull model.
2. **Self-update cgroup isolation:** The detached systemd unit MUST have no coupling to the agent service (no `Requires=`, `BindsTo=`, `PartOf=`). This is what allows the update to survive the agent being killed by dpkg prerm.
3. **Shell injection prevention:** `self-update.sh` uses `case/esac` branches with no `eval` and no shell interpolation of `target_version`. All version strings are validated with regex before use.
4. **Marker file is authoritative:** After self-update, the marker file (`/var/lib/linux_patch_api/last_self_update.json`) is the source of truth, not the in-memory job state.
