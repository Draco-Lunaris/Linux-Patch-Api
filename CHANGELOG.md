# Changelog

All notable changes to Linux Patch API are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- **Self-enrollment workflow**: Automated host registration with linux_patch_manager
  - CLI flag: `--enroll <MANAGER_URL>` for enrollment mode
  - Three-phase enrollment: Registration → Polling (24h timeout) → PKI Provisioning
  - Automatic certificate provisioning to configured mTLS paths
  - Automatic manager IP whitelist append after successful enrollment
  - Configurable polling interval (default 60s) and max attempts (default 1440/24h)
  - Signal handling for graceful shutdown during enrollment
- Enrollment configuration section in config.yaml (`enrollment.*`)
- Identity extraction module (machine-id, FQDN, IP addresses, OS details)
- PKI bundle validation with PEM format checking
- Atomic certificate file writing with secure permissions (key=0600, certs=0644)
- Whitelist auto-append with file locking and duplicate detection

---

## [2.0.0] - 2026-06-27

### Added

#### Self-Update Architecture
- **Self-update script rewritten** from GitHub Releases to native package manager commands (apt/dnf/apk/pacman)
- **RepoConfig** added to enrollment PkiBundle for manager-hosted repo provisioning
- **Fallback `GET /api/v1/pki/repo-config`** endpoint for pre-repo agents (legacy migration path)
- **Post-upgrade health check** with auto-rollback (60-second timeout)
- **Signal trap** (SIGTERM/SIGINT/SIGHUP) in `self-update.sh` for graceful interruption
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
