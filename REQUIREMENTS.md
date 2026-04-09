# Linux_Patch_API - Requirements Document

## Functional Requirements

### FR-001: Package Management Endpoints

| ID | Endpoint | Method | Description | Sync/Async |
|----|----------|--------|-------------|------------|
| FR-001.1 | `/packages` | GET | List installed packages with filtering (name, version, status, upgradable) | Sync |
| FR-001.2 | `/packages/{name}` | GET | Get specific package details | Sync |
| FR-001.3 | `/packages` | POST | Install package(s) with optional version pinning | Async |
| FR-001.4 | `/packages/{name}` | PUT | Update specific package | Async |
| FR-001.5 | `/packages/{name}` | DELETE | Remove package | Async |

### FR-002: Patch Management Endpoints

| ID | Endpoint | Method | Description | Sync/Async |
|----|----------|--------|-------------|------------|
| FR-002.1 | `/patches` | GET | List available updates/patches | Sync |
| FR-002.2 | `/patches/apply` | POST | Apply all or specific patches | Async |

### FR-003: System Endpoints

| ID | Endpoint | Method | Description | Sync/Async |
|----|----------|--------|-------------|------------|
| FR-003.1 | `/system/info` | GET | Get system information (OS version, kernel, last update time) | Sync |
| FR-003.2 | `/health` | GET | Health check endpoint | Sync |
| FR-003.3 | `/jobs/{id}` | GET | Get specific job status | Sync |
| FR-003.4 | `/jobs` | GET | List all jobs (with optional status filter) | Sync |

### FR-004: Job Management Endpoints

| ID | Endpoint | Method | Description | Sync/Async |
|----|----------|--------|-------------|------------|
| FR-004.1 | `/jobs/{id}/rollback` | POST | Rollback a completed/failed job | Async (Exclusive) |

### FR-005: Authentication & Authorization

- mTLS certificate validation (required for all endpoints)
- IP whitelist enforcement (deny by default, allow listed only)
- No session management (stateless, cert-based auth)
- No granular permissions (whitelisted IP + valid cert = full access)

### FR-006: Audit Logging

- Log all API requests with client cert ID, endpoint, method, timestamp
- Log all package operations (package name, version, action)
- Log authentication events (success/failure, cert validation)
- Log IP whitelist denials (blocked connection attempts)
- Log configuration changes (whitelist updates, cert renewals)
- Log system changes made by the API

---
## Non-Functional Requirements

### NFR-001: Security

- **Authentication:** mTLS certificate-based (TLS 1.3 only)
- **Authorization:** IP whitelist enforcement (block all by default)
- **Certificate Validity:** 1-year maximum, unique per client
- **Subprocess Restriction:** SystemCallFilter to limit allowed syscalls
- **Error Handling:** Silent drop for non-mTLS connections, detailed errors for authenticated clients
- **Data Encryption:** All communications encrypted via TLS

### NFR-002: Performance

- **Quick Operations:** < 5 seconds response time (GET endpoints)
- **Long Operations:** Async with job tracking, max 30-minute timeout
- **Concurrent Jobs:** Configurable limit (default: 5)
- **WebSocket:** Real-time status updates for async jobs

### NFR-003: Availability

- **Service Type:** systemd service with automatic restart on failure
- **Config Reload:** Auto-reload on config file change (validated before apply)
- **Certificate Reload:** Auto-reload on cert file change (validated before apply)
- **Health Check:** GET /health endpoint for monitoring

### NFR-004: Scalability

- **Architecture:** Single instance per host (Agent Per Host model)
- **No Central Coordination:** Each host operates independently
- **Package Manager:** Pluggable backend for multi-distribution support

### NFR-005: Reliability

- **Job Persistence:** Jobs stored in memory/file, cleared on restart
- **Rollback:** Exclusive mode during rollback (no new requests accepted)
- **Batch Operations:** Best-effort (not atomic)
- **Idempotency:** Operations should be idempotent where possible

---

## User Stories

### US-001: System Administrator - Install Package
**As a** system administrator  
**I want to** install a package remotely via API  
**So that** I can manage software without SSH access  

**Acceptance Criteria:**
- POST /packages with package name returns job ID
- Job status available via GET /jobs/{id} and WebSocket
- Audit log records the installation
- Rollback available if installation fails

### US-002: System Administrator - Apply Security Patches
**As a** system administrator  
**I want to** apply all available security patches  
**So that** the system stays secure  

**Acceptance Criteria:**
- GET /patches shows available updates
- POST /patches/apply initiates patching
- Real-time status via WebSocket
- All operations logged to audit

### US-003: System Administrator - Check System Status
**As a** system administrator  
**I want to** check system health and package status  
**So that** I can monitor the system  

**Acceptance Criteria:**
- GET /health returns service status
- GET /system/info returns OS details
- GET /packages returns installed packages
- All queries require valid mTLS cert

### US-004: System Administrator - Remove Package
**As a** system administrator  
**I want to** remove an unwanted package  
**So that** I can clean up the system  

**Acceptance Criteria:**
- DELETE /packages/{name} returns job ID
- Job tracks removal progress
- Audit log records the removal
- Rollback available if needed

---

## Technical Requirements

### System Requirements

- **OS:** Linux (Debian/Ubuntu primary, RHEL/CentOS/Fedora, Alpine, Arch supported)
- **Memory:** Minimum 256MB RAM, recommended 512MB
- **Storage:** Minimum 100MB for binary and config, plus job storage
- **CPU:** Any modern x86_64 or ARM64 processor
- **Privileges:** Root access required (for package management)

### Network Requirements

- **Port:** 12443 (TCP)
- **Protocol:** TLS 1.3 only
- **Bind Address:** 0.0.0.0 (all interfaces)
- **Firewall:** Host-level firewall to restrict inbound to whitelisted IPs

### Dependencies

- **Runtime:** None (compiled Rust binary)
- **Package Managers:** apt/dpkg, dnf/yum, apk, or pacman (at least one required)
- **systemd:** For service management
- **Certificates:** Valid mTLS certificates from internal CA

---

*Following kiro spec-driven development standards*
