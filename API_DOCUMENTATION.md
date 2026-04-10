# Linux Patch API - API Documentation

**Version:** 1.0.0  
**Base Path:** `/api/v1/`  
**Protocol:** HTTPS (TLS 1.3 only)  
**Port:** 12443  

Complete API reference for the Linux Patch API service.

---

## Table of Contents

- [Overview](#overview)
- [Authentication](#authentication)
- [Standard Response Format](#standard-response-format)
- [Error Handling](#error-handling)
- [Package Management Endpoints](#package-management-endpoints)
- [Patch Management Endpoints](#patch-management-endpoints)
- [System Management Endpoints](#system-management-endpoints)
- [Job Management Endpoints](#job-management-endpoints)
- [WebSocket Streaming](#websocket-streaming)
- [Async Job Handling Guide](#async-job-handling-guide)

---

## Overview

The Linux Patch API provides a secure REST interface for remote package and patch management. All operations require mTLS authentication and IP whitelist validation.

**Design Principles:**
- Pure REST architecture (resources as nouns, HTTP verbs for actions)
- Stateless authentication (no sessions)
- Async operations for long-running tasks
- Real-time status via WebSocket streaming
- Standard JSON request/response envelope

---

## Authentication

### Requirements

All API requests must include:

1. **Valid Client Certificate**
   - Signed by internal CA
   - Not expired (max 1-year validity)
   - Unique per client (no shared certificates)

2. **IP Whitelist Validation**
   - Source IP must be in `/etc/linux_patch_api/whitelist.yaml`
   - Default: Deny all (block unless explicitly allowed)
   - Changes applied automatically (no restart required)

### Connection Example

```bash
curl --cacert /etc/linux_patch_api/certs/ca.pem \
     --cert /etc/linux_patch_api/certs/client.pem \
     --key /etc/linux_patch_api/certs/client.key.pem \
     https://localhost:12443/api/v1/health
```

### Authentication Failures

| Condition | Response |
|-----------|----------|
| No certificate | Silent drop (no response) |
| Invalid certificate | Silent drop (no response) |
| Expired certificate | Silent drop (no response) |
| IP not whitelisted | Silent drop (no response) |

**Note:** Failed authentication results in silent drop for security (no information leakage).

---

## Standard Response Format

All API responses use this standard JSON envelope:

```json
{
  "success": true,
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {},
  "error": null
}
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `success` | boolean | `true` for successful requests, `false` for errors |
| `request_id` | UUID | Unique identifier for request tracking and auditing |
| `timestamp` | ISO 8601 | Server timestamp of response (UTC) |
| `data` | object | Response payload (null on error) |
| `error` | object | Error details (null on success) |

---

## Error Handling

### Error Response Format

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

### Error Codes Reference

| Code | HTTP Status | Description | Retryable |
|------|-------------|-------------|----------|
| `AUTH_INVALID_CERT` | 401 | Certificate validation failed | No |
| `AUTH_CERT_EXPIRED` | 401 | Certificate has expired | No |
| `AUTHZ_IP_DENIED` | 403 | IP not in whitelist | No |
| `PKG_NOT_FOUND` | 404 | Package not found | No |
| `PKG_MANAGER_ERROR` | 500 | Package manager operation failed | Yes |
| `PKG_DEPENDENCY_ERROR` | 400 | Package dependency conflict | No |
| `PKG_VERSION_CONFLICT` | 400 | Requested version not available | No |
| `PATCH_NOT_FOUND` | 404 | Patch not found | No |
| `PATCH_APPLY_ERROR` | 500 | Patch application failed | Yes |
| `JOB_NOT_FOUND` | 404 | Job ID not found | No |
| `JOB_TIMEOUT` | 408 | Job exceeded 30-minute timeout | Yes |
| `JOB_CANCELLED` | 400 | Job was cancelled | No |
| `CONFIG_INVALID` | 400 | Configuration validation failed | No |
| `CONFIG_RELOAD_ERROR` | 500 | Configuration reload failed | Yes |
| `SERVICE_UNHEALTHY` | 503 | Service not ready | Yes |
| `SYSTEM_REBOOT_ERROR` | 500 | Reboot operation failed | Yes |
| `INVALID_REQUEST` | 400 | Request body validation failed | No |
| `RATE_LIMIT_EXCEEDED` | 429 | Too many requests | Yes |

---

## Package Management Endpoints

### POST /api/v1/packages

**Description:** Install one or more packages (async operation)

**Request:**
```json
{
  "packages": [
    {
      "name": "nginx",
      "version": "1.24.0-1"
    },
    {
      "name": "openssl",
      "version": null
    }
  ],
  "options": {
    "force": false,
    "no_recommends": true,
    "allow_downgrade": false
  }
}
```

**Request Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `packages` | array | Yes | List of packages to install |
| `packages[].name` | string | Yes | Package name |
| `packages[].version` | string | No | Specific version (null for latest) |
| `options` | object | No | Installation options |
| `options.force` | boolean | No | Force reinstall (default: false) |
| `options.no_recommends` | boolean | No | Skip recommended packages (default: false) |
| `options.allow_downgrade` | boolean | No | Allow version downgrade (default: false) |

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
    "status": "pending",
    "operation": "install",
    "packages": ["nginx", "openssl"],
    "created_at": "2026-04-09T13:04:02Z"
  },
  "error": null
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `job_id` | UUID | Job identifier for status tracking |
| `status` | string | Initial status: `pending` |
| `operation` | string | Operation type: `install` |
| `packages` | array | List of package names |
| `created_at` | ISO 8601 | Job creation timestamp |

---

### GET /api/v1/packages

**Description:** List installed packages with filtering and sorting

**Query Parameters:**

| Parameter | Type | Description | Example |
|-----------|------|-------------|---------|
| `name` | string | Filter by package name (supports `*` wildcard) | `nginx*` |
| `status` | string | Filter by status: `installed`, `upgradable`, `all` | `upgradable` |
| `limit` | integer | Maximum results (default: 100, max: 1000) | `50` |
| `offset` | integer | Pagination offset | `100` |
| `sort` | string | Sort field: `name`, `version`, `size` | `name` |
| `order` | string | Sort order: `asc`, `desc` | `asc` |

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
        "description": "High-performance web server",
        "size_bytes": 2458624,
        "installed_at": "2026-04-01T10:00:00Z",
        "upgradable": true,
        "available_version": "1.24.0-2",
        "dependencies": ["openssl", "libpcre3"],
        "maintainer": "nginx-team@example.com"
      }
    ],
    "total": 245,
    "limit": 100,
    "offset": 0
  },
  "error": null
}
```

---

### GET /api/v1/packages/{name}

**Description:** Get details for a specific installed package

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | string | Package name (URL-encoded) |

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
    "description": "High-performance web server",
    "size_bytes": 2458624,
    "installed_at": "2026-04-01T10:00:00Z",
    "upgradable": true,
    "available_version": "1.24.0-2",
    "dependencies": [
      {"name": "openssl", "version": ">=1.1.1", "required": true},
      {"name": "libpcre3", "version": ">=8.0", "required": true}
    ],
    "reverse_dependencies": ["nginx-module-vts"],
    "maintainer": "nginx-team@example.com",
    "homepage": "https://nginx.org",
    "license": "BSD-2-Clause",
    "files": [
      "/usr/sbin/nginx",
      "/etc/nginx/nginx.conf",
      "/var/log/nginx/access.log"
    ]
  },
  "error": null
}
```

---

### PUT /api/v1/packages/{name}

**Description:** Update a specific package (async operation)

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | string | Package name |

**Request:**
```json
{
  "version": "1.24.0-2",
  "options": {
    "force": false,
    "no_recommends": false
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
    "operation": "update",
    "package": "nginx",
    "target_version": "1.24.0-2"
  },
  "error": null
}
```

---

### DELETE /api/v1/packages/{name}

**Description:** Remove a package (async operation)

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | string | Package name |

**Query Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `purge` | boolean | Remove config files (default: false) |
| `force` | boolean | Force removal despite dependencies (default: false) |

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
    "package": "nginx",
    "purge": false
  },
  "error": null
}
```

---

## Patch Management Endpoints

### GET /api/v1/patches

**Description:** List available security patches

**Query Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `severity` | string | Filter: `critical`, `high`, `medium`, `low`, `all` |
| `status` | string | Filter: `available`, `applied`, `pending`, `all` |
| `limit` | integer | Maximum results (default: 100) |
| `offset` | integer | Pagination offset |
| `sort` | string | Sort: `severity`, `published_date`, `name` |
| `order` | string | Order: `asc`, `desc` |

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "patches": [
      {
        "id": "USN-6000-1",
        "name": "linux-security-update",
        "severity": "critical",
        "status": "available",
        "published_date": "2026-04-08T00:00:00Z",
        "description": "Security update for Linux kernel",
        "cve_ids": ["CVE-2026-1234", "CVE-2026-5678"],
        "affected_packages": ["linux-image-generic", "linux-headers-generic"],
        "requires_reboot": true
      }
    ],
    "total": 15,
    "limit": 100,
    "offset": 0
  },
  "error": null
}
```

---

### POST /api/v1/patches/apply

**Description:** Apply security patches (async operation)

**Request:**
```json
{
  "patches": ["USN-6000-1"],
  "options": {
    "reboot": false,
    "reboot_delay_minutes": 0,
    "exclude_packages": []
  }
}
```

**Request Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `patches` | array | No | Specific patch IDs (empty = all available) |
| `options` | object | No | Application options |
| `options.reboot` | boolean | No | Auto-reboot if required (default: false) |
| `options.reboot_delay_minutes` | integer | No | Delay before reboot (default: 0) |
| `options.exclude_packages` | array | No | Packages to exclude from update |

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
    "patches_count": 1,
    "requires_reboot": true,
    "auto_reboot": false
  },
  "error": null
}
```

---

## System Management Endpoints

### GET /api/v1/system/info

**Description:** Get system information

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "hostname": "patch-server-01",
    "os": {
      "name": "Ubuntu",
      "version": "22.04 LTS",
      "codename": "jammy",
      "architecture": "x86_64"
    },
    "kernel": {
      "version": "5.15.0-100-generic",
      "architecture": "x86_64"
    },
    "uptime_seconds": 864000,
    "last_boot": "2026-04-01T00:00:00Z",
    "package_manager": "apt",
    "api_version": "1.0.0",
    "service_status": "running"
  },
  "error": null
}
```

---

### GET /health

**Description:** Health check endpoint (no authentication required for monitoring systems)

**Note:** This endpoint may be configured to allow unauthenticated access for load balancer health checks.

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "status": "healthy",
    "version": "1.0.0",
    "uptime_seconds": 864000,
    "checks": {
      "config": "ok",
      "certificates": "ok",
      "package_manager": "ok",
      "job_queue": "ok"
    }
  },
  "error": null
}
```

---

### POST /api/v1/system/reboot

**Description:** Initiate system reboot (async operation)

**Request:**
```json
{
  "delay_seconds": 60,
  "force": false,
  "reason": "Scheduled maintenance"
}
```

**Request Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `delay_seconds` | integer | No | Delay before reboot (default: 0) |
| `force` | boolean | No | Force reboot despite active jobs (default: false) |
| `reason` | string | No | Reason for reboot (logged for audit) |

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
    "scheduled_at": "2026-04-09T13:05:02Z",
    "reason": "Scheduled maintenance"
  },
  "error": null
}
```

---

## Job Management Endpoints

### GET /api/v1/jobs

**Description:** List jobs with filtering and sorting

**Query Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `status` | string | Filter: `pending`, `running`, `completed`, `failed`, `cancelled` |
| `operation` | string | Filter by operation type |
| `limit` | integer | Maximum results (default: 100) |
| `offset` | integer | Pagination offset |
| `sort` | string | Sort: `created_at`, `updated_at`, `status` |
| `order` | string | Order: `asc`, `desc` |

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "jobs": [
      {
        "job_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
        "operation": "install",
        "status": "completed",
        "progress_percent": 100,
        "created_at": "2026-04-09T13:00:00Z",
        "updated_at": "2026-04-09T13:02:00Z",
        "completed_at": "2026-04-09T13:02:00Z"
      }
    ],
    "total": 50,
    "limit": 100,
    "offset": 0
  },
  "error": null
}
```

---

### GET /api/v1/jobs/{id}

**Description:** Get detailed job status

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `id` | UUID | Job identifier |

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
    "operation": "install",
    "status": "completed",
    "progress_percent": 100,
    "created_at": "2026-04-09T13:00:00Z",
    "updated_at": "2026-04-09T13:02:00Z",
    "completed_at": "2026-04-09T13:02:00Z",
    "packages": ["nginx"],
    "result": {
      "success": true,
      "packages_installed": ["nginx"],
      "packages_failed": []
    },
    "logs": [
      {"timestamp": "2026-04-09T13:00:01Z", "level": "info", "message": "Starting package installation"},
      {"timestamp": "2026-04-09T13:01:00Z", "level": "info", "message": "Downloading nginx 1.24.0-1"},
      {"timestamp": "2026-04-09T13:02:00Z", "level": "info", "message": "Installation complete"}
    ]
  },
  "error": null
}
```

**Job Status Values:**

| Status | Description |
|--------|-------------|
| `pending` | Job queued, waiting for execution |
| `running` | Job currently executing |
| `completed` | Job finished successfully |
| `failed` | Job finished with errors |
| `cancelled` | Job was cancelled by user |
| `timeout` | Job exceeded 30-minute limit |

---

### POST /api/v1/jobs/{id}/rollback

**Description:** Rollback a completed job (async operation)

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `id` | UUID | Job identifier |

**Response (202 Accepted):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "uuid",
    "status": "pending",
    "operation": "rollback",
    "original_job_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
  },
  "error": null
}
```

---

### DELETE /api/v1/jobs/{id}

**Description:** Cancel a pending/running job or delete a completed job

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `id` | UUID | Job identifier |

**Response (200 OK):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-04-09T13:04:02Z",
  "data": {
    "job_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
    "previous_status": "running",
    "current_status": "cancelled",
    "action": "cancelled"
  },
  "error": null
}
```

---

## WebSocket Streaming

### WS /api/v1/ws/jobs

**Description:** Real-time job status streaming

**Connection:**
```javascript
const ws = new WebSocket('wss://localhost:12443/api/v1/ws/jobs', {
  cert: clientCert,
  key: clientKey,
  ca: caCert
});
```

**Client Messages:**

| Type | Payload | Description |
|------|---------|-------------|
| `subscribe` | `{"job_id": "uuid"}` | Subscribe to specific job |
| `unsubscribe` | `{"job_id": "uuid"}` | Unsubscribe from job |
| `subscribe_all` | `{}` | Subscribe to all jobs |
| `ping` | `{}` | Keep-alive ping |

**Server Messages:**

| Type | Payload | Description |
|------|---------|-------------|
| `job_status` | Job status object | Job status update |
| `job_complete` | Job result object | Job completion notification |
| `pong` | `{}` | Ping response |
| `error` | Error object | WebSocket error |

**Example Flow:**
```javascript
ws.onopen = () => {
  // Subscribe to job updates
  ws.send(JSON.stringify({
    type: 'subscribe',
    job_id: '6ba7b810-9dad-11d1-80b4-00c04fd430c8'
  }));
};

ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  
  if (data.type === 'job_status') {
    console.log('Job progress:', data.payload.progress_percent, '%');
  } else if (data.type === 'job_complete') {
    console.log('Job completed:', data.payload.result);
  }
};
```

**Server Message Format:**
```json
{
  "type": "job_status",
  "payload": {
    "job_id": "uuid",
    "status": "running",
    "progress_percent": 45,
    "updated_at": "2026-04-09T13:01:30Z"
  }
}
```

---

## Async Job Handling Guide

### Understanding Async Operations

Long-running operations return immediately with a `202 Accepted` status and a `job_id`. Clients must poll or use WebSocket to track completion.

### Operations Using Async Pattern

| Operation | Endpoint | Typical Duration |
|-----------|----------|------------------|
| Package Install | POST /api/v1/packages | 10s - 5min |
| Package Update | PUT /api/v1/packages/{name} | 10s - 3min |
| Package Remove | DELETE /api/v1/packages/{name} | 5s - 2min |
| Patch Apply | POST /api/v1/patches/apply | 1min - 30min |
| System Reboot | POST /api/v1/system/reboot | 1min + reboot time |
| Job Rollback | POST /api/v1/jobs/{id}/rollback | 5s - 5min |

### Polling Strategy

```python
import time
import requests

def wait_for_job(job_id, base_url, certs, poll_interval=2):
    """Poll job status until completion."""
    while True:
        response = requests.get(
            f"{base_url}/api/v1/jobs/{job_id}",
            cert=certs,
            verify=ca_cert
        )
        data = response.json()['data']
        
        if data['status'] in ['completed', 'failed', 'cancelled', 'timeout']:
            return data
        
        time.sleep(poll_interval)
```

### Job Timeout

- **Default Timeout:** 30 minutes
- **Timeout Behavior:** Job marked as `timeout`, partial changes may exist
- **Recovery:** Use rollback endpoint to revert changes

### Concurrent Job Limits

- **Default:** 5 concurrent jobs
- **Configuration:** `jobs.max_concurrent` in config.yaml
- **Behavior:** Additional jobs queued until slot available

---

## Rate Limiting

| Endpoint Category | Limit | Window |
|-------------------|-------|--------|
| Health Check | 60 requests | 1 minute |
| Package List | 30 requests | 1 minute |
| Package Operations | 10 requests | 1 minute |
| Patch Operations | 5 requests | 1 minute |
| Job Operations | 60 requests | 1 minute |
| System Operations | 5 requests | 1 minute |

**Response on Limit Exceeded:**
```json
{
  "success": false,
  "error": {
    "code": "RATE_LIMIT_EXCEEDED",
    "message": "Too many requests",
    "retryable": true,
    "details": {
      "retry_after_seconds": 30
    }
  }
}
```

---

## Support

- **Documentation:** [README.md](./README.md)
- **Deployment:** [DEPLOYMENT_GUIDE.md](./DEPLOYMENT_GUIDE.md)
- **Security:** [DEPLOYMENT_SECURITY_GUIDE.md](./DEPLOYMENT_SECURITY_GUIDE.md)
