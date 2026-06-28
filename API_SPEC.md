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
    "version": "0.0.1"
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

#### POST /api/v1/system/update

**Description:** Trigger agent self-update from the manager-hosted package repository (not GitHub Releases). When `target_version` is omitted, installs the latest available version; when supplied, performs an upgrade or downgrade to that exact version. The agent writes the request to `/var/lib/linux_patch_api/self-update.request` and the update service unit invokes `self-update.sh`, which delegates to the native package manager (apt/dnf/yum/apk/pacman) and performs a post-upgrade health check with auto-rollback on failure.

**Request Body:**
```json
{
  "target_version": "1.5.6-1",
  "restart": true,
  "restart_delay_seconds": 5
}
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `target_version` | string | No | — | Specific package version to install (e.g., `"1.5.6-1"`). If omitted, installs the latest available version from the manager repo. Used for manager-initiated downgrade (rollback) by passing a previously known-good version. |
| `restart` | boolean | No | `true` | Whether to restart the agent service after a successful upgrade. If `false`, the manager must restart the agent manually. |
| `restart_delay_seconds` | integer | No | `5` | Delay in seconds before restarting the agent after upgrade. Clamped to `MAX_RESTART_DELAY_SECONDS` (300). Values above 300 are silently clamped. |

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid",
    "status": "pending",
    "operation": "self_update",
    "target_version": "1.5.6-1",
    "restart": true,
    "restart_delay_seconds": 5,
    "source": "manager_repo"
  },
  "error": null
}
```

**Error Responses:**
| Code | HTTP Status | Description |
|------|-------------|-------------|
| `INVALID_VERSION` | 400 | The specified `target_version` does not match the package version format or is not available in the manager repo |
| `UPDATE_IN_PROGRESS` | 409 | A self-update is already in progress; only one concurrent self-update is allowed |
| `UPDATE_SERVICE_START_ERROR` | 500 | The self-update systemd service unit failed to start |
| `AUTH_INVALID_CERT` | 401 | mTLS certificate validation failed |

**Notes:**
- Upgrade source is the manager-hosted apt/dnf/apk/pacman repository configured at enrollment time (not GitHub Releases)
- GPG signature verification is performed by the native package manager against the key delivered in the enrollment bundle
- A post-upgrade health check verifies the service becomes active within 60 seconds; on failure, the agent automatically rolls back to `previous_version`
- Job status is observable via `GET /api/v1/jobs/{id}` and the WebSocket stream
- The handler writes the request to `/var/lib/linux_patch_api/self-update.request` and delegates execution to the `linux-patch-api-update.service` systemd unit — JobManager is intentionally not used (see handler architecture comment in `src/api/handlers/system.rs`)

---

#### GET /api/v1/system/update/status

**Description:** Retrieve the result of the most recent self-update operation. Returns the previous version, new version, whether the agent package changed, the outcome status, and any error message. If no self-update has ever been performed, returns a 404.

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "previous_version": "1.4.3-1",
    "new_version": "1.5.6-1",
    "changed": true,
    "status": "success",
    "error": null,
    "at": "2026-06-27T14:03:12Z"
  },
  "error": null
}
```

**Response Fields (SelfUpdateStatusData):**
| Field | Type | Description |
|-------|------|-------------|
| `previous_version` | string \| null | Agent version before the update (null if first install) |
| `new_version` | string \| null | Agent version after the update (null if update failed before install) |
| `changed` | boolean | `true` if the package version changed; `false` if already at target version |
| `status` | string | Outcome: `success`, `rollback`, `failed`, `health_check_timeout` |
| `error` | string \| null | Error message if status is not `success`; otherwise `null` |
| `at` | ISO 8601 \| null | Timestamp when the self-update completed (null if never run) |

**Error Responses:**
| Code | HTTP Status | Description |
|------|-------------|-------------|
| `NO_SELF_UPDATE_RECORD` | 404 | No self-update has been performed on this agent |
| `AUTH_INVALID_CERT` | 401 | mTLS certificate validation failed |

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
      }
    ],
    "total": 1
  },
  "error": null
}
```

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
    "status": "running",
    "progress": 45,
    "message": "Downloading package...",
    "created_at": "2026-04-09T13:00:00Z",
    "completed_at": null,
    "packages": ["nginx"],
    "logs": ["Starting installation...", "Resolving dependencies..."]
  },
  "error": null
}
```

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
