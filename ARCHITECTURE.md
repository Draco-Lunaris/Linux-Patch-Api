# Linux_Patch_API - Architecture Document

## System Overview

The Linux_Patch_API is a secure, single-host API service that enables remote package and patch management on Linux systems. Each instance runs as a system service on the managed host (systemd on most distributions, OpenRC on Alpine), providing a REST API over mTLS with strict IP whitelist enforcement.

**Architecture Type:** Agent Per Host (Option B)  
**Deployment:** One instance per managed Linux host  
**Network:** Internal network only (no internet exposure)

---

## Component Architecture

### Core Components

1. **API Layer (Actix-web/Axum)**
   - HTTP/HTTPS endpoint handling
   - mTLS termination
   - IP whitelist enforcement
   - Request routing
   - WebSocket support for real-time job status

2. **Authentication Layer**
   - Certificate validation (mTLS)
   - Client identity extraction from certificate
   - No session management (stateless, cert-based auth only)

3. **Authorization Layer**
   - IP whitelist checking (deny by default)
   - No permission validation (whitelisted IP + valid cert = full access)

4. **Job Manager**
   - Async job queue for long-running operations
   - Job status tracking with persistent storage
   - WebSocket broadcast for real-time status updates
   - 30-minute timeout enforcement
   - Job cleanup and expiration

5. **Package Manager Backend (Pluggable)**
   - apt/dpkg adapter (Debian/Ubuntu - primary)
   - dnf/yum adapter (RHEL/CentOS/Fedora)
   - apk adapter (Alpine)
   - pacman adapter (Arch)
   - Distribution detection and adapter selection

6. **Audit Logger**
   - System logging integration (primary)
     - systemd journal on systemd-based systems
     - syslog/local files on OpenRC-based systems
   - Local file fallback (`/var/log/linux_patch_api/`)
   - 30-day retention with daily rotation and gzip compression

7. **Configuration Manager**
   - YAML config file watcher (`/etc/linux_patch_api/config.yaml`)
   - Auto-reload on file change
   - Config validation before reload (prevents service downtime)
   - Runtime settings access for all components

### External Integrations

- **Package Managers:** apt, dnf, yum, apk, pacman (via system commands)
- **Init System:** Service management and logging
  - systemd (Debian, Ubuntu, RHEL, CentOS, Fedora)
  - OpenRC (Alpine Linux)
- **Internal CA:** Certificate validation against self-hosted CA

---

## Technology Stack

### Backend
- **Language:** Rust
- **Framework:** Actix-web or Axum
- **Database:** None (file-based job storage)
- **mTLS:** Rust TLS library (rustls or native-tls)

### Infrastructure
- **Service Manager:** Distribution-dependent
  - systemd (most distributions)
  - OpenRC (Alpine Linux)
- **Configuration:** YAML

### Deployment
- **Package Format:** Native Linux packages (deb, rpm, apk, pkg.tar.zst)
- **Distribution:** Via target system package manager (apt, dnf, apk, pacman)
- **Installation:** Package installs binary, init script/service, and default config structure
  - systemd unit file for systemd distributions
  - OpenRC init script for Alpine
- **Updates:** Handled through system package manager

---

## Security Architecture

### Authentication
- mTLS certificate-based authentication (required)
- Internal self-hosted CA
- Unique client certificates (1-year validity)
- Silent drop for non-mTLS connections

### Authorization
- IP whitelist enforcement (block all by default)
- No granular permissions (binary access: allowed or denied)
- Whitelisted IP + valid cert = full API access

### Process Security (Init System Hardening)
- **User:** root (required for package management)

**systemd Hardening Options:**
- NoNewPrivileges: true (prevent privilege escalation)
- ProtectSystem: strict (read-only filesystem except allowed paths)
- ProtectHome: true (no access to /home, /root, /run/user)
- PrivateTmp: true (isolated /tmp)
- SystemCallFilter: Restrict to required syscalls only (application whitelist)

**OpenRC Hardening Options:**
- Run as dedicated service user
- File permission restrictions
- chroot isolation (optional)
- Equivalent security via rc.conf and init script options
### Data Security
- All communications encrypted via TLS
- Certificates stored securely with restricted permissions
- Audit logging of all operations

### Certificate Storage (Option A: Separate Files)

```
/etc/linux_patch_api/certs/
├── ca.pem       (644) - CA certificate
├── server.pem   (644) - Server certificate
└── server.key.pem (600) - Server private key (restricted)
```

**Rationale:**
- Tighter permissions on private key only (600)
- Easier certificate rotation (replace cert without touching key)
- Standard practice for TLS deployments
- No extraction overhead
---

## File System Layout

```
/etc/linux_patch_api/
├── config.yaml          # Main configuration
├── whitelist.yaml       # IP whitelist
└── certs/
    ├── ca.pem          # CA certificate (or server.p12)
    ├── server.pem      # Server certificate
    ├── server.key.pem  # Server private key
    └── crl.pem         # Certificate Revocation List

/var/lib/linux_patch_api/
├── jobs/               # Job storage (cleared on restart)
└── state/              # Runtime state

/var/log/linux_patch_api/
└── audit.log           # Local audit log fallback

/usr/bin/linux-patch-api  # Binary location
Init scripts (distribution-dependent):
- /etc/systemd/system/linux-patch-api.service  # systemd
- /etc/init.d/linux-patch-api  # OpenRC (Alpine)
```
---

## Data Flow

### Synchronous Request Flow (Quick Operations):

```
Client → [mTLS Handshake] → [IP Whitelist Check] → [API Layer]
         ↓
    [Auth: Cert Valid?] → No → Silent Drop
         ↓ Yes
    [Authz: IP Allowed?] → No → Silent Drop
         ↓ Yes
    [Route to Handler] → [Execute Package Op] → [Log to Audit]
         ↓
    [Return JSON Response] ← Client
```

### Asynchronous Request Flow (Long Operations):

```
Client → [mTLS + IP Check] → [API Layer] → [Create Job] → [Return Job ID]
                                           ↓
                                    [Job Manager Queue]
                                           ↓
                                    [Package Manager Backend]
                                           ↓
                                    [Update Job Status] → [WebSocket Broadcast]
                                           ↓
                                    [Job Complete/Timeout]
                                           ↓
                                    [Log to Audit]
```

### Job Status Endpoint Flow:

```
Client → [mTLS + IP Check] → [API Layer] → [GET /jobs/{id}]
                                           ↓
                                    [Query Job Storage]
                                           ↓
                                    [Return Job Status JSON]
```

### Configuration Reload Flow:

```
[Config File Changed] → [File Watcher Detects]
         ↓
    [Validate New Config] → Invalid → [Log Error, Keep Old Config]
         ↓ Valid
    [Swap Config in Memory] → [Notify Components] → [Log Reload Event]
```

### Certificate Renewal Flow:

```
[Cert File Updated] → [File Watcher Detects]
         ↓
    [Validate Certificate Chain] → Invalid → [Log Error, Keep Old Certs]
         ↓ Valid
    [Reload TLS Context] → [New Connections Use New Certs] → [Log Reload Event]
```

### Rollback Execution Flow (Exclusive):

```
[Rollback Triggered] → [Set Exclusive Mode] → [Reject New Requests]
         ↓
    [Execute Rollback Operations] → [Log Each Step]
         ↓
    [Rollback Complete] → [Clear Exclusive Mode] → [Accept New Requests]
```

### Key Behaviors:

- Failed jobs are cleared on service restart (no persistence)
- Rollback execution is exclusive - no new requests accepted until complete
- Certificate renewal follows same validation pattern as config reload
- Status endpoint available (GET /jobs/{id}) in addition to WebSocket for job monitoring

---

## API Design Principles

- Pure REST (resources as nouns, HTTP verbs for actions)
- JSON request/response with standard envelope
- Hybrid execution model (sync for quick ops, async for long ops)
- WebSocket for real-time job status streaming
- GET /jobs/{id} endpoint for job status polling

---

## Network Configuration

- **Bind Address:** 0.0.0.0 (all interfaces)
- **Port:** 12443 (HTTPS/mTLS)
- **Protocol:** TLS 1.3 only
- **Firewall:** Host-level firewall should restrict inbound to whitelisted IPs only

---

## Health Checks

### Endpoint: GET /health

**Purpose:** General service status check with package cache status

**Response (200 OK - Healthy):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-05-27T14:00:00Z",
  "data": {
    "status": "healthy",
    "uptime_seconds": 12345,
    "version": "1.1.17",
    "last_cache_update": "2026-05-27T13:30:00+00:00",
    "cache_status": "fresh"
  },
  "error": null
}
```

**Response (200 OK - Degraded):**
```json
{
  "success": true,
  "request_id": "uuid",
  "timestamp": "2026-05-27T14:00:00Z",
  "data": {
    "status": "degraded",
    "uptime_seconds": 12345,
    "version": "1.1.17",
    "last_cache_update": "2026-05-27T09:00:00+00:00",
    "cache_status": "failed"
  },
  "error": null
}
```

**Health Check Criteria:**
- Service is listening on port 12443
- mTLS is configured and valid
- Config file is loaded and valid
- Package manager backend is accessible
- Package cache is fresh (refreshed within 4 hours)

**Cache Refresh on Health Check:**
- If cache is stale (>4 hours since last update), health check triggers a cache refresh
- If refresh succeeds: status="healthy", cache_status="fresh"
- If refresh fails: status="degraded", cache_status="failed"
- If cache is fresh: status="healthy", cache_status="fresh"

**Cache Status Values:**
- `fresh` - Cache was updated within the last 4 hours
- `stale` - Cache is older than 4 hours (triggers refresh)
- `unknown` - No cache update has occurred yet
- `failed` - Last cache refresh attempt failed

**NOT Required:**
- Metrics collection
- Alerting integration
- Prometheus/Grafana endpoints

---

## Package Cache Management

### Module: `src/packages/cache.rs`

The package cache module manages the local package index state, ensuring that package metadata is current before performing operations.

**Key Components:**
- `PackageCacheState` - Thread-safe in-memory cache state with Mutex protection
- `PackageCacheStatus` - Snapshot of cache state for reporting
- `CacheStateFile` - Persistent state format for serialization
- `is_fetch_error()` - Detects 404/fetch errors for automatic retry
- `apply_with_cache_retry()` - Generic retry wrapper for cache-related failures
- `run_command_with_timeout()` - Executes cache refresh commands with timeout

**State Persistence:**
- Cache state persists to `/var/lib/linux_patch_api/state/cache.json`
- State is loaded on service startup and saved after every update
- Persists `last_cache_update` timestamp and `last_update_success` flag
- Parent directory is auto-created if missing

**Stale Detection:**
- Cache is considered stale after 4 hours (`STALE_THRESHOLD_SECS = 14400`)
- Health check automatically refreshes stale cache
- Patch apply operations always refresh cache before proceeding (mandatory)

**Refresh-Before-Apply Flow:**
1. `POST /patches/apply` creates a job and spawns background task
2. Background task refreshes package cache (mandatory, not configurable)
3. If refresh fails: job fails immediately with error message
4. If refresh succeeds: job progresses to 10%, applies patches
5. If apply fails with 404/fetch error: refresh cache and retry once
6. If retry also fails: job fails with error

**Cache Refresh Timeout:** 120 seconds (`CACHE_REFRESH_TIMEOUT_SECS`)

---

*Following kiro spec-driven development standards*
