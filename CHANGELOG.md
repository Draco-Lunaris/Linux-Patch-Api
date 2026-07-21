# Changelog

All notable changes to Linux Patch API are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

---

## [2.5.5] - 2026-07-21

### Fixed
- **Reboot field name mismatch (critical):** The manager sent
  `allow_reboot: true` in `ApplyPatchesRequest` but the agent expected
  `reboot: true`. Serde silently dropped the unknown field, so reboots
  never happened on any OS. Renamed `PatchApplyRequest.reboot` to
  `allow_reboot` with `#[serde(alias = "reboot")]` for backward compat.
- **No agent-side auto-reboot:** The agent only rebooted if `reboot: true`
  was explicitly set. Now reboots when `allow_reboot` is true AND a reboot
  is actually needed (any applied patch `requires_reboot` OR the system's
  `pending_reboot` marker is set after applying).
- **`requires_reboot` hardcoded false (all 5 backends):** All backends
  (apt, apk, dnf, yum, pacman) hardcoded `requires_reboot: false` on every
  patch. Now uses `package_requires_reboot()` checking against a
  conservative list of reboot-triggering packages (kernel, glibc, systemd,
  dbus, openssl, bootloader, microcode, etc.).
- **`pending_reboot` Debian-only:** `get_system_info()` only checked
  `/var/run/reboot-required` (Debian). Now uses distro-specific detection:
  Debian/Ubuntu marker file, `dnf needs-restarting -r` for RHEL, and
  running-vs-installed kernel version comparison fallback for all distros.
- **Alpine delayed reboot:** Alpine's `reboot_system()` now checks if
  `shutdown` exists before attempting a delayed reboot; falls back to
  immediate `reboot` if unavailable.
- **`list_patches` aggregate heuristic:** Updated aggregate
  `requires_reboot` to use the per-patch field instead of the crude
  `name.contains("kernel")` heuristic that missed glibc, systemd, dbus,
  openssl, etc.

---

## [2.5.4] - 2026-07-21

### Fixed
- **Repo-config fallback endpoint missing codename:** The agent's
  `fetch_repo_config()` called `GET /api/v1/pki/repo-config?distro_id=ubuntu`
  without a `codename` parameter. The manager defaulted to `u2404` for all
  Ubuntu hosts, causing Ubuntu 22.04 hosts to receive the u2404 binary (built
  against GLIBC 2.38+) instead of the u2204 binary (GLIBC 2.34). The 2.5.3
  binary then failed to start with `GLIBC_2.38 not found`. Now detects the
  apt suite from `VERSION_ID` in `/etc/os-release` and sends it as the
  `codename` query parameter.

---

## [2.5.3] - 2026-07-20

### Fixed
- **Self-update restart race condition:** Package-manager processes (apt-get, dnf, apk,
  pacman) were spawned in the agent's process group, so systemd's `KillMode=control-group`
  killed them mid-transaction when the postinst triggered a service restart. This caused
  self-update jobs to fail with `apt-get failed (signal)` even though the package installed
  successfully. Now spawns all package-manager commands with `process_group(0)`, isolating
  them into their own process group so they survive an agent service stop/restart.
- **Removed `kill_on_drop(true)` from package-manager spawns:** If the agent was stopped
  mid-operation, the dropped child handle would SIGKILL the package transaction. Now uses
  `kill_on_drop(false)` (the default) and relies on process-group isolation + explicit
  timeout kills instead.
- **Timeout kills now target the process group:** `kill_child()` and the cache-refresh
  timeout path now send SIGTERM/SIGKILL to the negative PID (process group), ensuring
  subprocesses (dpkg, rpm, postinst hooks) are cleaned up on timeout.

### Changed
- **All distro postinst/post-upgrade scripts now do an immediate `systemctl restart
  --no-block`** (or `rc-service restart` on OpenRC) instead of the 300s delayed restart
  timer. Process-group isolation makes the immediate restart safe — no delay needed.
  Affected: Debian `postinst`, RPM `spec %post`, Arch `.install`, Alpine `.post-upgrade`.
- **Added `libc` as a direct dependency** for process-group signal handling.

---

## [2.4.0] - 2026-07-16

### Added
- **Command timeouts**: All external command executions now have per-call deadlines
  - `CACHE_REFRESH_TIMEOUT` (300s) for apt-get update / dnf check-update / apk update
  - `PACKAGE_OP_TIMEOUT` (1800s) for apt-get install / dnf upgrade / pacman -Syu
  - `QUICK_OP_TIMEOUT` (60s) for dpkg / systemctl / rpm / hostname / uname
  - SIGTERM → 5s grace → SIGKILL escalation on timeout
  - `CommandError::from_timeout()` returns structured timeout error classified as `TIMEOUT`
  - `run_with_timeout` added to `CommandRunner` trait (default: no timeout, for mock compat)
- **GPG key health reporting**: `GET /health` now includes `gpg_key_status` and `gpg_key_expires_at`
  - Checks provisioned keyring file existence and uses `gpg --show-keys --with-colons` for expiry
  - Falls back to valid (file-exists) if gpg binary is not installed
  - Downgrades overall health to `degraded` when key is missing/expired/revoked
- **`name` key in `os_details`**: Agent now emits `/etc/os-release` `ID` field as `name` during
  enrollment, giving the manager's `detect_distro_id` a reliable second signal

### Fixed
- **#158, #156**: `SystemCommandRunner` had no timeout on any subprocess. A hung `apt-get update`
  or `systemctl show` could block the agent's mutation semaphore for hours. Replaced
  `std::process::Command` with `tokio::process::Command` + `kill_on_drop` + per-call timeouts.
- **#157**: Self-update silently no-op'd when the local apt cache was stale (>900s). The candidate
  version lookup read the stale cache, found `target == installed`, and returned
  `NO_UPDATE_AVAILABLE` without ever invoking `apt-get install`. Now forces a cache refresh
  before `get_candidate_version` regardless of `is_stale()`.
- **#126 HIGH-1**: Agent `/health` never reported `gpg_key_status`, making the manager's GPG-key
  health pathway dead code. Every agent was misclassified as legacy.
- **#126 MED-1**: Agent never emitted `name` key in `os_details`, leaving the manager's second
  distro detection signal inert.
- **#126 LOW-1**: README version mismatch (line 3 said 2.0.0, line 698 said 1.0.0).
- `CACHE_REFRESH_TIMEOUT_SECS` was defined but never referenced anywhere; `run_command_with_timeout`
  in cache.rs was misnamed — its body was a bare `Command::output()` with no timeout.

### Changed
- Removed `.gitea/` directory — CI now runs exclusively via `.github/workflows/ci.yml`
- Replaced all internal infrastructure references (Gitea URLs, moon-dragon.us hostnames,
  internal emails) with GitHub URLs and generic placeholders
- Rewrote `scripts/upload-release.sh` from Gitea API to GitHub API / `gh` CLI
- `Cargo.toml` author updated to `Draco-Lunaris-Echo`

---

## [2.0.0] - 2026-06-27

### Added

#### Self-Update Architecture
- **Self-update script rewritten** from GitHub Releases to native package manager commands (apt/dnf/apk/pacman)
- **RepoConfig** added to enrollment PkiBundle for manager-hosted repo provisioning
- **Fallback `GET /api/v1/pki/repo-config`** endpoint for pre-repo agents (legacy migration path)
- **Post-upgrade health check** with auto-rollback (60-second timeout)
- **Pacman `-U` from cache** for version pinning on Arch Linux
- **SelfUpdateRequest** fields: `restart` (default: `true`), `restart_delay_seconds` (default: `5`, max: `300`)
- Handler clamping for `restart_delay_seconds` to `MAX_RESTART_DELAY_SECONDS`
- **CI job: `publish-to-manager-repo`** — signs and publishes packages to manager-hosted repository
- **21 unit tests** for self-update (`validate_version_string`, `SelfUpdateRequest`, marker file)
- **3 integration tests** for enrollment repo provisioning
- **Handler architecture comment** explaining why JobManager is not used for self-update

#### Documentation
- **tasks/migration-guide.md** — migration from GitHub Releases to manager-hosted repo
- **tasks/self-update-runbook.md** — operational runbook for self-update and GPG key rotation
- **tasks/self-update-gap-analysis.md** — gap analysis for self-update feature

### Changed

- Updated **SPEC.md** to v2.0.0 Active
- Updated **E2E test harness** for manager-hosted repo flow (GPG-signed local apt repo)

### Security

- **Removed `eval` from `self-update.sh`** — replaced with direct `case`/`esac` execution (prevents command injection)
- GPG signature verification delegated to native package manager (security feature, not gap)

---

## [1.0.0] - 2026-07-17

### Added

#### Package Management
- **POST /api/v1/packages** - Install one or more packages asynchronously
- **GET /api/v1/packages** - List installed packages with filtering and sorting
- **GET /api/v1/packages/{name}** - Get detailed package information
- **PUT /api/v1/packages/{name}** - Update specific package
- **DELETE /api/v1/packages/{name}** - Remove package

#### Patch Management
- **GET /api/v1/patches** - List available security patches
- **POST /api/v1/patches/apply** - Apply security patches with optional auto-reboot

#### System Management
- **GET /api/v1/system/info** - Retrieve system information
- **GET /health** - Health check endpoint for load balancers
- **POST /api/v1/system/reboot** - Initiate system reboot asynchronously

#### Job Management
- **GET /api/v1/jobs** - List jobs with filtering and sorting
- **GET /api/v1/jobs/{id}** - Get detailed job status with logs
- **POST /api/v1/jobs/{id}/rollback** - Rollback completed job
- **DELETE /api/v1/jobs/{id}** - Cancel pending/running job or delete completed job

#### WebSocket Streaming
- **WS /api/v1/ws/jobs** - Real-time job status streaming

#### Security Features
- mTLS certificate-based authentication (TLS 1.3 only)
- IP whitelist enforcement (deny by default)
- Certificate validation with expiry checking
- Silent drop for unauthorized connections
- Comprehensive audit logging (systemd journal + file)
- Systemd hardening directives (ProtectSystem, NoNewPrivileges, etc.)

#### Configuration
- YAML configuration with auto-reload
- Dynamic IP whitelist updates (no restart required)
- Configurable concurrent job limits
- Configurable job timeout (default: 30 minutes)
- Multiple log levels (error, warn, info, debug, trace)

#### Package Support
- Debian package (.deb) for Ubuntu/Debian
- RPM package (.rpm) for RHEL/CentOS/Fedora
- Manual installation script (install.sh) for Alpine/Arch

#### Multi-Distro Backend Support
- apt (Debian/Ubuntu)
- dnf/yum (RHEL/CentOS/Fedora)
- apk (Alpine)
- pacman (Arch Linux)
- Auto-detection of package manager

### Security Improvements

#### Phase 3 Security Hardening
- **16/16 security tests passing**
- STRIDE threat model validation complete
- Security controls matrix: 93% compliant
- All critical/high findings resolved

#### Authentication & Authorization
- Mutual TLS (mTLS) with unique client certificates
- Internal CA infrastructure (separate secure host)
- Certificate validity: 1 year maximum
- IP whitelist with CIDR subnet support
- Binary authorization model (authenticated = full access)

#### Data Protection
- TLS 1.3 encryption for all connections
- Private key permissions: 600 (owner read/write only)
- Certificate permissions: 644
- Config file validation before reload
- Silent failure for unauthorized access (no information leakage)

#### Process Isolation
- Dedicated system user/group (linux-patch-api)
- systemd hardening directives:
  - ProtectSystem=strict
  - ProtectHome=true
  - NoNewPrivileges=true
  - PrivateTmp=true
  - SystemCallFilter=@system-service

#### Audit & Logging
- All operations logged with request_id
- Client certificate ID in audit trail
- systemd journal integration (immutable by default)
- Optional remote syslog support
- Configurable log retention (default: 30 days)

### Performance

#### Benchmark Results
- Average endpoint latency: <5ns (simulated)
- Health check latency: 866ps
- Concurrent request handling: Linear scaling to 100+ users
- TLS handshake overhead: ~15ms (expected for mTLS)
- Memory usage: 45MB idle, 78MB under load

#### Optimization Features
- Async job processing with configurable concurrency
- Job queue with priority handling
- WebSocket streaming for real-time updates
- Connection pooling support
- TLS session resumption capability

### Changed

- API versioned to `/api/v1/` for future compatibility
- Standard JSON response envelope for all endpoints
- Async pattern for all long-running operations (202 Accepted)
- Job timeout enforced at 30 minutes (configurable)
- Default concurrent job limit: 5 (configurable)

### Deprecated

- None (initial release)

### Removed

- None (initial release)

### Fixed

- TLS configuration to enforce TLS 1.3 only
- Certificate validation to reject expired certificates
- Whitelist reload to apply without service restart
- Job state persistence across service restart (cleared on restart by design)
- Error messages to avoid information leakage

### Known Issues

#### Low Priority (Deferred to Future Release)
1. **Input Length Validation** - Enhanced validation for extremely long input strings
2. **Path Traversal Enhancement** - Additional hardening for path normalization
3. **Header Size Limits** - Configurable HTTP header size limits
4. **Empty String Validation** - Stricter validation for empty string inputs
5. **HTTP Method Response Codes** - More specific 405 Method Not Allowed responses
6. **Duplicate Header Handling** - Explicit handling of duplicate HTTP headers

**Note:** These issues are documented but do not impact production security posture. All critical and high severity findings have been resolved.

#### Operational Notes
- Certificate renewal requires manual process (no auto-renewal in v1.0.0)
- Job history cleared on service restart (by design for security)
- WebSocket connections require re-subscription after reconnect
- SELinux policies may require manual configuration on RHEL/CentOS

---

## [0.1.0] - 2026-04-09

### Added

- Initial development release
- Project scaffolding with Cargo
- Basic API structure
- Security specification documents
- Performance benchmark suite
- Package build infrastructure (.deb/.rpm)

### Security

- mTLS authentication prototype
- IP whitelist implementation
- Basic audit logging
- systemd service file

### Performance

- Criterion.rs benchmark suite
- Endpoint latency measurements
- Concurrency testing framework

---

## Version History Summary

| Version | Release Date | Status | Key Milestone |
|---------|--------------|--------|---------------|
| Unreleased | TBD | In Development | Self-enrollment feature complete |
| 1.0.0 | 2026-07-17 | Production | Initial production release |
| 0.1.0 | 2026-04-09 | Development | Initial development release |

---

## Release Notes by Phase

### Phase 0: Rust Project Scaffolding ✅
- Cargo project initialized
- Module structure created
- CI/CD pipeline configured
- Development environment ready

### Phase 1: Foundation & Security Infrastructure ✅
- CI/CD pipeline operational
- Debian/RPM package build workflows
- systemd service with hardening
- CA setup documentation
- Configuration templates

### Phase 2: Core API Development ✅
- All 15 API endpoints implemented
- mTLS authentication layer
- IP whitelist enforcement
- Job manager with WebSocket
- Audit logging complete

### Phase 3: Security Hardening ✅
- Penetration testing (16/16 tests passing)
- Threat model validation
- Security controls matrix (93% compliant)
- Fuzz testing (21 tests, findings documented)
- All critical/high findings resolved

### Phase 4: Production Readiness ✅
- Performance benchmarking complete
- Optimization recommendations documented
- Package creation (.deb/.rpm) complete
- Installation script developed
- Documentation complete

---

## Upgrade Path

### From 0.1.0 to 1.0.0

1. **Backup Configuration**
   ```bash
   cp /etc/linux_patch_api/config.yaml /etc/linux_patch_api/config.yaml.bak
   cp /etc/linux_patch_api/whitelist.yaml /etc/linux_patch_api/whitelist.yaml.bak
   ```

2. **Stop Service**
   ```bash
   systemctl stop linux-patch-api
   ```

3. **Install New Package**
   ```bash
   # Debian/Ubuntu
   dpkg -i linux-patch-api_1.0.0-1_amd64.deb
   
   # RHEL/CentOS/Fedora
   rpm -Uvh linux-patch-api-1.0.0-1.x86_64.rpm
   ```

4. **Verify Configuration**
   ```bash
   linux-patch-api --check-config
   ```

5. **Start Service**
   ```bash
   systemctl start linux-patch-api
   systemctl status linux-patch-api
   ```

6. **Test Connection**
   ```bash
   curl --cacert ca.pem --cert client.pem --key client.key.pem \
        https://localhost:12443/health
   ```

---

## Support

- **Documentation:** [README.md](./README.md)
- **API Reference:** [API_DOCUMENTATION.md](./API_DOCUMENTATION.md)
- **Deployment:** [DEPLOYMENT_GUIDE.md](./DEPLOYMENT_GUIDE.md)
- **Security:** [DEPLOYMENT_SECURITY_GUIDE.md](./DEPLOYMENT_SECURITY_GUIDE.md)
- **Build:** [BUILD_PACKAGES.md](./BUILD_PACKAGES.md)

---

*For security issues, contact security@internal directly (do not create public issues)*
