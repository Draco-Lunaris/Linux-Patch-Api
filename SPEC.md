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
- **Scope:** Limited distribution (small number of authorized clients)
- **Validity Period:** 1 year standard expiration
- **Client Identity:** Unique certificate per client (no shared certs)
- **Rotation:** Manual renewal process before expiration

## Self-Enrollment Workflow

The `linux_patch_api` daemon supports an automated self-enrollment workflow to securely request identity from the `linux_patch_manager` without manual PKI distribution.

- **Trigger:** Initiated via CLI flag during setup/first run (e.g., `linux_patch_api --enroll https://<manager_url>`).
- **Phase 1: Registration Request:**
  - Extracts `/etc/machine-id`, FQDN, IP Address, and OS details.
  - Submits an unauthenticated `POST /api/v1/enroll` request to the manager.
  - Receives a temporary `polling_token`.
- **Phase 2: Polling & Approval:**
  - The daemon enters a polling loop, querying `GET /api/v1/enroll/status/{token}` (e.g., every 60 seconds).
  - Aborts if HTTP 403 or 404 is returned (request denied/purged).
- **Phase 3: Provisioning:**
  - Upon HTTP 200, extracts the provided PKI bundle (`ca.crt`, `server.crt`, `server.key`).
  - Writes certificates to the configured mTLS storage paths.
  - Automatically appends the manager's IP address to `/etc/linux_patch_api/whitelist.yaml`.
  - Transitions to standard mTLS listening mode without requiring a service restart.

## Audit Logging

- **Log Content (All Required):**
  - Every API request (endpoint, method, timestamp, client cert ID)
  - Package operations (package name, version, action: install/remove/update)
  - Authentication events (success/failure, cert validation)
  - IP whitelist denials (blocked connection attempts)
  - System changes made by the API
  - Configuration changes (whitelist updates, cert renewals)

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
