# Linux_Patch_API - API Specification Document

## API Overview

**Purpose:** Secure REST API for remote package and patch management on Linux systems

**Design Philosophy:**
- Pure REST architecture (resources as nouns, HTTP verbs for actions)
- mTLS certificate-based authentication (no sessions)
- Hybrid execution model (sync for quick ops, async for long ops)
- Real-time status via WebSocket streaming
- JSON request/response with standard envelope

**Base Path:** `/api/v1/`
**Protocol:** HTTPS (TLS 1.3 only)
**Port:** 12443
**Trailing Slashes:** Not required

---

## Authentication

### Authentication Method
- **Type:** mTLS Certificate-Based Authentication
- **CA:** Internal self-hosted Certificate Authority
- **Certificate Validity:** 1 year maximum
- **Client Identity:** Unique certificate per client
- **Session Management:** None (stateless)

### Authorization
- **Method:** IP Whitelist Enforcement
- **Default:** Deny all (block unless explicitly allowed)
- **Permissions:** Binary (whitelisted IP + valid cert = full access)
- **No Granular Permissions:** All authenticated clients have full API access

### Connection Requirements
- Valid client certificate signed by internal CA
- Client IP must be in whitelist configuration
- TLS 1.3 only
- Silent drop for non-compliant connections (no response)

---

## Standard Response Envelope

All API responses use this standard structure:

```json
{
  "success": true,
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {},
  "error": null
}
```

### Fields:
| Field | Type | Description |
|-------|------|-------------|
| `success` | boolean | true for successful requests, false for errors |
| `request_id` | UUID | Unique identifier for request tracking/auditing |
| `timestamp` | ISO 8601 | Server timestamp of response |
| `data` | object | Response payload (null on error) |
| `error` | object | Error details (null on success) |

---

## Error Response Format

```json
{
  "success": false,
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": null,
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable description",
    "details": {},
    "retryable": false
  }
}
```

### Error Codes:
| Code | HTTP Status | Description |
|------|-------------|-------------|
| `AUTH_INVALID_CERT` | 401 | Certificate validation failed |
| `AUTH_CERT_EXPIRED` | 401 | Certificate has expired |
| `AUTHZ_IP_DENIED` | 403 | IP not in whitelist |
| `PKG_NOT_FOUND` | 404 | Package not found |
| `PKG_MANAGER_ERROR` | 500 | Package manager operation failed |
| `JOB_NOT_FOUND` | 404 | Job ID not found |
| `JOB_TIMEOUT` | 408 | Job exceeded 30-minute timeout |
| `CONFIG_INVALID` | 400 | Configuration validation failed |
| `SERVICE_UNHEALTHY` | 503 | Service not ready |

---

## Endpoints

### Package Management Endpoints

#### POST /api/v1/packages

**Description:** Install one or more packages (async operation)

**Request Body:**
```json
{
  "packages": [
    {
      "name": "nginx",
      "version": "1.24.0-1"
    }
  ],
  "options": {
    "force": false,
    "no_recommends": true
  }
}
```

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid",
    "status": "pending",
    "operation": "install",
    "packages": ["nginx"]
  },
  "error": null
}
```

---

#### GET /api/v1/packages

**Description:** List installed packages with filtering and sorting

**Query Parameters:**
| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | string | Filter by package name (supports wildcard `*`) |
| `status` | string | Filter: `installed`, `upgradable`, `available` |
| `upgradable` | boolean | `true` to show only upgradable packages |
| `sort` | string | Sort field: `name`, `version`, `status` (default: `name`) |
| `order` | string | Sort order: `asc`, `desc` (default: `asc`) |

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "packages": [
      {
        "name": "nginx",
        "version": "1.24.0-1",
        "status": "installed",
        "upgradable": true,
        "latest_version": "1.25.0-1",
        "description": "High performance web server",
        "dependencies": ["libc6", "libssl3"],
        "reverse_dependencies": ["nginx-core"]
      }
    ],
    "total": 1
  },
  "error": null
}
```

---

#### GET /api/v1/packages/{name}

**Description:** Get details of a specific package

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "name": "nginx",
    "version": "1.24.0-1",
    "status": "installed",
    "upgradable": true,
    "latest_version": "1.25.0-1",
    "description": "High performance web server",
    "dependencies": ["libc6", "libssl3"],
    "reverse_dependencies": ["nginx-core"],
    "install_date": "2026-01-15T10:30:00Z",
    "size_installed": "2.5 MB"
  },
  "error": null
}
```

---

#### PUT /api/v1/packages/{name}

**Description:** Update a specific package (async operation)

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid",
    "status": "pending",
    "operation": "update",
    "package": "nginx"
  },
  "error": null
}
```

---

#### DELETE /api/v1/packages/{name}

**Description:** Remove a package (async operation)

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid",
    "status": "pending",
    "operation": "remove",
    "package": "nginx"
  },
  "error": null
}
```

---

### Patch Management Endpoints

#### GET /api/v1/patches

**Description:** List available updates/patches

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "patches": [
      {
        "name": "linux-kernel",
        "current_version": "5.15.0-91",
        "available_version": "5.15.0-92",
        "severity": "critical",
        "description": "Security update for kernel vulnerabilities",
        "cve_ids": ["CVE-2024-1234"],
        "requires_reboot": true
      }
    ],
    "total": 1,
    "security_updates": 1,
    "requires_reboot": true
  },
  "error": null
}
```

---

#### POST /api/v1/patches/apply

**Description:** Apply all or specific patches (async operation)

**Request Body:**
```json
{
  "packages": ["linux-kernel", "libc6"],  // optional, all if omitted
  "reboot": true,                         // optional, auto-reboot after patching
  "reboot_delay_seconds": 60              // optional, delay before reboot
}
```

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid",
    "status": "pending",
    "operation": "patch_apply",
    "packages_count": 2,
    "reboot_scheduled": true
  },
  "error": null
}
```

---

### System Endpoints

#### GET /api/v1/system/info

**Description:** Get system information

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "hostname": "server01",
    "os": "Ubuntu",
    "os_version": "22.04 LTS",
    "kernel": "5.15.0-91-generic",
    "architecture": "x86_64",
    "last_update_check": "2026-04-09T12:00:00Z",
    "last_update_apply": "2026-04-01T03:00:00Z",
    "pending_reboot": false
  },
  "error": null
}
```

---

#### GET /api/v1/health

**Description:** Health check endpoint

**Response (200 OK - Healthy):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "status": "healthy",
    "uptime_seconds": 12345,
    "version": "2.0.0"
  },
  "error": null
}
```

**Response (503 - Unhealthy):**
```json
{
  "success": false,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": null,
  "error": {
    "code": "SERVICE_UNHEALTHY",
    "message": "Service not ready",
    "details": {},
    "retryable": true
  }
}
```

---

#### POST /api/v1/system/reboot

**Description:** Reboot the system (async operation)

**Request Body:**
```json
{
  "delay_seconds": 0,   // optional, immediate if omitted
  "force": false        // optional, skip pending job checks
}
```

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid",
    "status": "pending",
    "operation": "reboot",
    "scheduled_at": "2026-04-09T13:04:02Z"
  },
  "error": null
}
```

---

#### POST /api/v1/system/update — DEPRECATED

**Status:** Deprecated — use `PUT /api/v1/packages/{name}` (standard package update endpoint) instead.

Self-updating the agent is a standard package update. The Manager calls `PUT /api/v1/packages/linux-patch-api` and the native package manager handles the upgrade. The prerm does NOT stop the service on upgrade; the postinst schedules a 300s delayed restart. No custom scripts, detached systemd units, or marker files are needed.

This endpoint may still exist in the codebase for backward compatibility but should be removed in a future cleanup pass.

---

#### GET /api/v1/system/update/status — DEPRECATED

**Status:** Deprecated — use `GET /api/v1/jobs/{id}` (standard job status endpoint) instead.

---

### PKI and Repository Endpoints

#### GET /api/v1/pki/repo-config

**Description:** Returns the manager-hosted package repository configuration and the GPG public key required to verify signed packages. This endpoint is intended for agents that enrolled before repo provisioning was added to the enrollment bundle (legacy migration path). On a successful response, the agent writes the GPG public key to the keyring path and installs the sources config to the distro-appropriate location.

**Authentication:** mTLS (same as all other endpoints)

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "repo_config": {
      "gpg_public_key": "-----BEGIN PGP PUBLIC KEY BLOCK-----\n...\n-----END PGP PUBLIC KEY BLOCK-----",
      "sources_config": "deb [signed-by=/etc/apt/keyrings/lpa-repo.gpg] https://manager.moon-dragon.us/apt noble main",
      "distro_id": "ubuntu",
      "keyring_path": "/etc/apt/keyrings/lpa-repo.gpg",
      "repo_base_url": "https://manager.moon-dragon.us"
    }
  },
  "error": null
}
```

**Error Responses:**
| Code | HTTP Status | Description |
|------|-------------|-------------|
| `PKI_REPO_CONFIG_UNAVAILABLE` | 404 | Manager has not provisioned a repo config for this host's distro |
| `AUTH_INVALID_CERT` | 401 | mTLS certificate validation failed |

---

### Job Management Endpoints

#### GET /api/v1/jobs

**Description:** List all jobs with optional filtering

**Query Parameters:**
| Parameter | Type | Description |
|-----------|------|-------------|
| `status` | string | Filter: `pending`, `running`, `completed`, `failed`, `cancelled` |
| `limit` | integer | Max results (default: 50, no pagination beyond limit) |

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "jobs": [
      {
        "job_id": "uuid",
        "operation": "install",
        "status": "completed",
        "created_at": "2026-04-09T13:00:00Z",
        "completed_at": "2026-04-09T13:02:00Z",
        "packages": ["nginx"]
      },
      {
        "job_id": "uuid",
        "operation": "install",
        "status": "failed",
        "created_at": "2026-04-09T13:03:00Z",
        "completed_at": "2026-04-09T13:03:30Z",
        "packages": ["nonexistent"],
        "error_code": "PKG_NOT_FOUND"
      }
    ],
    "total": 2
  },
  "error": null
}
```

**Job Summary Fields:**
| Field | Type | Description |
|-------|------|-------------|
| `job_id` | UUID | Job identifier |
| `operation` | string | `install`, `update`, `remove`, `patch_apply`, `reboot`, `self_update`, `rollback` |
| `status` | string | `pending`, `running`, `completed`, `failed`, `cancelled`, `timed_out` |
| `created_at` | ISO 8601 | Job creation timestamp |
| `completed_at` | ISO 8601 \| null | Job completion timestamp (null if not completed) |
| `packages` | string[] | Package names involved in the job |
| `error_code` | string? | Stable error code — present only for failed jobs (see [Job Error Codes](#job-error-codes)) |

---

#### GET /api/v1/jobs/{id}

**Description:** Get specific job status

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid",
    "operation": "install",
    "status": "failed",
    "progress": 0,
    "message": "Starting installation...",
    "created_at": "2026-04-09T13:00:00Z",
    "completed_at": "2026-04-09T13:00:30Z",
    "packages": ["nonexistent"],
    "logs": [
      "Job started",
      "Job failed: apt-get failed (exit 100): E: Unable to locate package nonexistent",
      "Error chain:",
      "apt-get failed (exit 100): E: Unable to locate package nonexistent",
      "",
      "Caused by:",
      "    Failed to execute apt command",
      "Command output:",
      "Command failed (exit 100): apt-get install -y -- nonexistent",
      "[stderr]",
      "  E: Unable to locate package nonexistent",
      "[stdout]",
      "  (empty)"
    ],
    "error": "apt-get failed (exit 100): E: Unable to locate package nonexistent\n\nCaused by:\n    Failed to execute apt command",
    "error_code": "PKG_NOT_FOUND",
    "exit_code": 100,
    "command_stdout": "",
    "command_stderr": "E: Unable to locate package nonexistent\n",
    "rollback_job_id": null,
    "exclusive_mode": false
  },
  "error": null
}
```

**Job Detail Fields:**
| Field | Type | Description |
|-------|------|-------------|
| `job_id` | UUID | Job identifier |
| `operation` | string | Operation type |
| `status` | string | Job status |
| `progress` | integer | 0-100 progress percentage |
| `message` | string | Current status message |
| `created_at` | ISO 8601 | Job creation timestamp |
| `completed_at` | ISO 8601 \| null | Job completion timestamp |
| `packages` | string[] | Package names involved |
| `logs` | string[] | Chronological log lines (includes error chain + captured command output on failure) |
| `error` | string \| null | Full error chain (all context layers + root cause) — null on success |
| `error_code` | string? | Stable error code — present only for failed jobs (see [Job Error Codes](#job-error-codes)) |
| `exit_code` | integer? | Exit code of the underlying package-manager command — present only when a `CommandError` was captured |
| `command_stdout` | string? | Captured stdout of the underlying command — present only when a `CommandError` was captured |
| `command_stderr` | string? | Captured stderr of the underlying command — present only when a `CommandError` was captured |
| `rollback_job_id` | UUID \| null | Rollback job ID if this job is a rollback |
| `exclusive_mode` | boolean | Whether the job runs in exclusive (rollback) mode |

---

#### Job Error Codes

Stable machine-readable strings used in `JobDetailData.error_code`, `JobSummary.error_code`, and the WebSocket `job_status` event's `error_code` field. The manager can use these to classify and route failures programmatically (e.g. retry on `NETWORK_ERROR`, alert on `GPG_ERROR`).

| Code | Description |
|------|-------------|
| `PKG_MANAGER_ERROR` | Package manager command exited non-zero (generic package operation failure) |
| `COMMAND_NOT_FOUND` | Package manager command could not be spawned (binary not found, permission denied at exec) |
| `CACHE_REFRESH_ERROR` | Cache refresh failed (apt-get update / dnf check-update / apk update) |
| `NETWORK_ERROR` | Network/fetch error during package download (404, connection refused, DNS) |
| `GPG_ERROR` | GPG signature verification failure (untrusted repo, expired key, bad signature) |
| `PKG_NOT_FOUND` | Package not found in any configured repository |
| `DEPENDENCY_CONFLICT` | Unmet dependencies or package conflict prevented the operation |
| `PERMISSION_DENIED` | Permission denied (not root, file permission, locked dpkg frontend) |
| `REBOOT_ERROR` | System reboot command failed |
| `TIMEOUT` | Job exceeded the configured timeout |
| `UNKNOWN_ERROR` | Catch-all for errors that don't match a more specific code |

**Note:** New codes may be added in the future. The manager should treat unknown codes as `UNKNOWN_ERROR`.

---

#### POST /api/v1/jobs/{id}/rollback

**Description:** Rollback a completed/failed job (async, exclusive mode)

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid-rollback",
    "status": "pending",
    "operation": "rollback",
    "original_job_id": "uuid",
    "exclusive_mode": true
  },
  "error": null
}
```

---

#### WebSocket: /api/v1/ws/jobs

**Description:** Real-time job status streaming

**Connection:** Upgrade HTTP connection with mTLS

**Client → Server (subscribe):**
```json
{
  "action": "subscribe",
  "job_id": "uuid"  // optional, omit for all jobs
}
```

**Server → Client (status update):**
```json
{
  "event": "job_status",
  "job_id": "uuid",
  "status": "running",
  "progress": 45,
  "message": "Installing package...",
  "timestamp": "2026-04-09T13:04:02Z"
}
```

**Server → Client (job complete):**
```json
{
  "event": "job_complete",
  "job_id": "uuid",
  "status": "completed",
  "progress": 100,
  "message": "Installation complete",
  "timestamp": "2026-04-09T13:04:02Z"
}
```

**Server → Client (job failed):**
```json
{
  "event": "job_status",
  "job_id": "uuid",
  "status": "failed",
  "progress": 0,
  "message": "Job failed: apt-get failed (exit 100): E: Unable to locate package foo",
  "timestamp": "2026-04-09T13:04:02Z",
  "error": "apt-get failed (exit 100): E: Unable to locate package foo",
  "error_code": "PKG_NOT_FOUND"
}
```

**WebSocket Fields:**
| Field | Type | Description |
|-------|------|-------------|
| `event` | string | Event type (`job_status`, `job_complete`, `connected`, `subscribed`, `unsubscribed`, `error`) |
| `job_id` | UUID | Job identifier (empty for connection/subscription events) |
| `status` | string | Job status or event-specific status |
| `progress` | integer | 0-100 progress percentage |
| `message` | string | Status message |
| `timestamp` | ISO 8601 | Event timestamp |
| `error` | string? | Error message — present only on failure events (new field, old clients ignore it) |
| `error_code` | string? | Stable error code — present only on failure events (see [Job Error Codes](#job-error-codes); new field, old clients ignore it) |

---

## Rate Limiting

**Not Required:** Internal network only with strict IP whitelist

---

## Versioning Strategy

- **Method:** URL Path Versioning (`/api/v1/`)
- **Backward Compatibility:** Breaking changes require new major version
- **Deprecation:** Old versions supported for 6 months after new version release
- **Current Version:** v1

---

*Following kiro spec-driven development standards*
