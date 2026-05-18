# Linux_Patch_API - Specification Document

## Project Overview
**Title:** Linux_Patch_API  
**Description:** API service for secure remote management of patching processes and software add/removal  
**Version:** 0.0.1  
**Status:** Draft  

## Scope

**Primary Focus:** Debian/Ubuntu (apt/dpkg)  
**Secondary Support:** RHEL/CentOS/Fedora (dnf/yum), Alpine (apk), Arch (pacman)  

**In Scope:**
- Remote package installation, removal, and updates
- System patch management (security and general updates)
- Multi-distribution support via pluggable package manager backend
- Secure authentication and authorization for remote operations
- Audit logging of all package/patch operations
- RESTful API design with JSON request/response

**Supported Operations:**
- **Core Package:** GET /packages (with filtering), GET /packages/{name}, POST /packages (install), PUT /packages/{name} (update), DELETE /packages/{name} (remove)
- **Patch Management:** GET /patches (list available), POST /patches/apply (apply all or specific)
- **System Info:** GET /system/info (OS version, kernel, last update time)

**Operation Features:**
- Version pinning support (e.g., package=1.2.3)
- Rollback capability for failed operations
- Batch operations: best-effort (not atomic)
- GET filtering: by name, version, status, upgradable
- No pagination (return all results)

**Out of Scope (for now):**
- GUI/frontend interface (API-only)
- Automatic scheduled patching (manual trigger only)
- Cross-distribution package compatibility

## Objectives

**Primary Objective:** Provide secure API for remote patch/package management on individual Linux hosts

**Key Goals:**
- Run as a system service on each managed machine (Option B: Agent Per Host)
  - systemd for Debian/Ubuntu, RHEL/CentOS/Fedora
  - OpenRC for Alpine Linux
- Internal network access only (no internet exposure)
- Support Debian/Ubuntu first, then expand to other distributions
- Maintain audit trail of all operations
- Minimal resource footprint
- mTLS certificate-based authentication
- IP whitelist enforcement (deny by default)

## Constraints

**Deployment:**
- One API instance per host
- Internal network only (LAN/private network)
- No public internet exposure
- Must run as a system service (init system determined by distribution)
  - systemd: Debian, Ubuntu, RHEL, CentOS, Fedora
  - OpenRC: Alpine Linux

**Technical:**
- Must run with elevated privileges for package management (root/sudo)
- Must support multiple Linux distributions
- API-only (no GUI required)
- mTLS required for all client connections
- IP whitelist enforcement required (block all by default, allow only listed)
- Technology: Rust with Actix-web or Axum framework
- Default API port: 12443
- API Style: Pure REST (resources as nouns, HTTP verbs for actions)
- Data Format: JSON for all requests and responses
- Response Envelope: Standard envelope with success, request_id, timestamp, data, error fields
- Request IDs: Required for all requests (tracking and auditing)
- Execution Model: Hybrid (sync for quick ops, async with job ID for long ops)
- Real-time Updates: WebSocket support for job status streaming
- Job Timeout: Maximum 30 minutes per operation

**Security:**
- Certificate-based authentication (mTLS)
- Network-level access control via IP/subnet whitelist
- Silent drop for non-mTLS connections (no response)
- Detailed error messages for authenticated clients only

## Error Handling

- **HTTP Status Codes:** Standard HTTP status codes (200, 400, 401, 403, 404, 500, etc.)

- **Error Response Format** (inside envelope's `error` field):
```json
{
  "code": "ERROR_CODE",
  "message": "Human-readable description",
  "details": {},
  "retryable": false
}
```

- **Error Categories:**
  - Authentication failures (invalid/expired cert)
  - Authorization failures (valid cert but not whitelisted IP)
  - Package not found
  - Package manager errors (dpkg/apt failures)
  - Permission denied
  - System resource errors
  - Configuration errors
  - Enrollment failures:
    - `ENROLLMENT_DENIED`: Admin rejected enrollment request on linux_patch_manager
    - `ENROLLMENT_EXPIRED`: Polling token expired or purged (HTTP 404 from manager)
    - `ENROLLMENT_TIMEOUT`: 24-hour polling limit exceeded (1440 attempts exhausted)
    - `ENROLLMENT_RATE_LIMITED`: Request rate limit exceeded (1/minute per IP, HTTP 429)
    - `PKI_PROVISION_FAILED`: Certificate write or PEM validation failed during provisioning

- **Error Message Policy:**
  - mTLS confirmed clients: Detailed error messages with debugging info
  - Non-mTLS connections: Silent drop (no response sent)
  - DEBUG mode: Include additional diagnostic information

- **Idempotency:** Operations should be idempotent where possible (safe to retry)

## Assumptions

- Host machines have network connectivity to internal clients
- API clients are trusted internal systems
- Host OS has Rust toolchain available (or can be installed)
- Package manager (apt/dnf/apk/pacman) is functional on target hosts

## Dependencies

- Linux OS with package manager support
- Init system for service management (distribution-dependent)
  - systemd (most distributions)
  - OpenRC (Alpine Linux)
- Network access for API communication
- mTLS certificate infrastructure (CA, client certs)
- IP whitelist configuration
- Rust toolchain (rustc, cargo)
- Actix-web or Axum framework
- Internal CA for certificate issuance (self-hosted)

## Certificate Management

- **CA Type:** Internal self-hosted Certificate Authority
- **Distribution:** Manual certificate distribution OR automated Self-Enrollment
  - Self-Enrollment provides automatic PKI provisioning after admin approval on linux_patch_manager
  - Eliminates manual certificate copy/permission management for new hosts
- **Scope:** Limited distribution (small number of authorized clients)
- **Validity Period:** 1 year standard expiration
- **Client Identity:** Unique certificate per client (no shared certs)
- **Rotation:** Manual renewal process before expiration

## Self-Enrollment Workflow

The `linux_patch_api` daemon supports an automated self-enrollment workflow to securely request identity from the `linux_patch_manager` without manual PKI distribution.

### CLI Invocation
```
linux-patch-api --enroll https://<manager_url>
```
The enrollment flow runs before mTLS server startup. On success, the daemon proceeds to normal server initialization with the newly provisioned certificates.

### Security Model
- Initial connection uses TLS with verification disabled (`danger_accept_invalid_certs`)
- Manager approval workflow provides authorization; transport encryption is secondary during enrollment
- URL scheme validation prevents SSRF/path traversal (only `http` and `https` permitted)
- Host component required in manager URL

### Phase 1: Registration Request
- **Identity Extraction:**
  - `/etc/machine-id` (fallback: `/var/lib/dbus/machine-id`)
  - FQDN from `hostname -f` (validated contains `.`) → `hostname` + `hostname -d` → `/etc/hostname` → `hostname` → `localhost`
  - Non-loopback IPv4 addresses via network interface enumeration
  - OS details from `/etc/os-release` (distro, version, id_like, codename) + kernel version (`uname -r`)
- **Submission:** Unauthenticated `POST /api/v1/enroll` to manager with identity payload
- **Response:** HTTP 202 with temporary `polling_token` (bearer credential — never logged)
- **Rate Limiting:** Manager enforces 1 request/minute per IP (HTTP 429 on violation)

### Phase 2: Polling & Approval
- **Polling Loop:** `GET /api/v1/enroll/status/{token}` with configurable interval and max attempts
- **Default Interval:** 60 seconds (configurable via `enrollment.polling_interval_seconds`)
- **Hard Timeout:** 24 hours maximum (1440 attempts; values >1440 clamped to 1440)
- **Status States:**
  - `pending`: Continue polling
  - `approved`: Proceed to Phase 3 with PKI bundle
  - `denied`: Abort enrollment (`ENROLLMENT_DENIED`)
  - `not_found`: Token expired/purged — abort (`ENROLLMENT_EXPIRED`)
- **Signal Handling:** SIGINT (Ctrl+C) and SIGTERM interrupt polling gracefully
- **Transient Errors:** Network failures and HTTP 5xx retried with backoff; HTTP 404/429 terminate immediately
- **Log Throttling:** Status logged every 10 attempts or after 5 minutes elapsed

### Phase 3: PKI Provisioning
- **Certificate Validation:** PEM format verification for CA cert, server cert, and server key (supports PKCS#8, PKCS#1 RSA, EC keys)
- **Atomic Writes:** Temp file → set permissions → atomic rename pattern prevents partial writes
- **File Permissions:** Keys at `0600`, certificates at `0644`, directories at `0755`
- **Backup Strategy:** Existing certificate files renamed to `.bak` before overwrite
- **Target Paths:** Configured via TLS settings or defaults (`/etc/linux_patch_api/certs/{ca,server,server.key}.pem`)
- **Whitelist Auto-Append:** Manager IP resolved (hostname → DNS or direct IP) and appended to `/etc/linux_patch_api/whitelist.yaml`
- **Completion:** Daemon transitions to standard mTLS listening mode without requiring service restart

## Audit Logging

- **Log Content (All Required):**
  - Every API request (endpoint, method, timestamp, client cert ID)
  - Package operations (package name, version, action: install/remove/update)
  - Authentication events (success/failure, cert validation)
  - IP whitelist denials (blocked connection attempts)
  - System changes made by the API
  - Configuration changes (whitelist updates, cert renewals)

- **Enrollment Events:**
  - Registration request submitted (machine-id, FQDN, manager URL — polling token never logged)
  - Polling status changes (`pending` → `approved`/`denied`/`not_found`)
  - PKI bundle provisioning success/failure with target file paths
  - Whitelist auto-append during enrollment (manager IP added)
  - Enrollment timeout or denial with reason
  - Signal interruption (SIGINT/SIGTERM) during polling

- **Log Storage:**
  - Primary: Distribution-appropriate logging
    - systemd journal (journalctl) on systemd systems
    - syslog/local files on OpenRC systems
  - Secondary: Optional remote syslog server (universal)
  - Local file logs as fallback (`/var/log/linux_patch_api/`)

- **Log Retention:**
  - Retention period: 30 days
  - Rotation: Daily
  - Compression: Enabled (gzip)

- **Log Levels:** Configurable at runtime (DEBUG, INFO, WARN, ERROR)

## IP Whitelist Configuration

- **Config File:** `/etc/linux_patch_api/whitelist.yaml`
- **Format:** YAML
- **Management:** Static config file (edit file to change)
- **Apply Method:** Instant apply on file change (no restart required)
- **Logging:** All whitelist changes logged to audit log

- **Supported Entries:**
  - Individual IPv4 addresses (e.g., `192.168.1.100`)
  - CIDR subnets (e.g., `192.168.1.0/24`)
  - Hostnames (resolved at config load)
  - IPv6: Not supported (explicitly out of scope)

- **Default Behavior:** Block all connections not in whitelist

## API Configuration Management

- **Config File:** `/etc/linux_patch_api/config.yaml`
- **Format:** YAML
- **Reload Method:** Config file watch with auto-reload on change (no restart required)

- **Configurable Settings:**
  - **Server:** port, bind address, timeout settings
  - **mTLS:** CA cert path, server cert path, server key path
  - **Logging:** log level, log retention, remote syslog server (optional)
  - **Security:** job timeout, max concurrent jobs, rate limiting

- **Hard-Coded Paths (not configurable):**
  - Whitelist file: `/etc/linux_patch_api/whitelist.yaml`
  - Data directory: `/var/lib/linux_patch_api/`
  - Job storage: `/var/lib/linux_patch_api/jobs/`
- Hard-Coded Paths (not configurable):
  - Whitelist file: `/etc/linux_patch_api/whitelist.yaml`
  - Data directory: `/var/lib/linux_patch_api/`
  - Job storage: `/var/lib/linux_patch_api/jobs/`
  - Log directory: `/var/log/linux_patch_api/`

## Testing Requirements

- **Unit Test Coverage:** Minimum 95%
- **Integration Tests:** API endpoint testing with mock package manager
- **Security Tests:** mTLS validation, IP whitelist enforcement, authentication failures
- **End-to-End Tests:** Full workflow testing on actual Ubuntu systems

- **Test Environments:**
  - Primary: Ubuntu (latest LTS)
  - CI/CD Pipeline: Required for automated testing
  - Penetration Testing: Required before release

## CLI Arguments

| Flag | Description |
|------|-------------|
| `--config <PATH>` or `-c` | Path to configuration file (default: `/etc/linux_patch_api/config.yaml`) |
| `--verbose` or `-v` | Enable verbose (DEBUG-level) logging |
| `--enroll <MANAGER_URL>` | Run self-enrollment flow with manager at URL, then start mTLS server |
| `--version` or `-V` | Print version information and exit |
| `--help` or `-h` | Display help information and exit |

### Enrollment Mode Behavior
- When `--enroll` is specified, the daemon executes the self-enrollment flow before starting the mTLS server
- On enrollment success: proceeds to normal server startup with provisioned certificates
- On enrollment failure: exits immediately with error code (no server started)
- TLS verification disabled on initial manager connection (manager approval workflow provides security)

- **Phase 1 Acceptance Criteria:**
  - All endpoints functional with mTLS authentication
  - IP whitelist enforced correctly
  - Audit logging working (journalctl + file)
  - Config auto-reload working
  - WebSocket status streaming functional
  - Rollback mechanism tested

- **Security Audit:** No formal audit planned at this time

---
*Following kiro spec-driven development standards*
