# Self-Enrollment Feature - Phased Development Plan

**Feature:** Automated self-enrollment workflow for linux_patch_api daemon
**Spec Reference:** SPEC.md lines 145-161
**Target Branch:** `feat/self-enrollment`
**Status:** Planning - Awaiting Kelly Approval

---

## Overview

The self-enrollment feature enables a new `linux_patch_api` instance to automatically register with the `linux_patch_manager`, request PKI credentials, and transition to mTLS-secured operation without manual certificate distribution.

### Three Phases (per SPEC)
| Phase | Description | Manager Endpoint |
|-------|-------------|------------------|
| **Phase 1: Registration** | Extract host identity → POST unauthenticated enrollment request → receive `polling_token` | `POST /api/v1/enroll` |
| **Phase 2: Polling** | Poll manager for approval status every 60s → abort on denied/not_found | `GET /api/v1/enroll/status/{token}` |
| **Phase 3: Provisioning** | Extract PKI bundle → write certs to disk → append manager IP to whitelist → transition to mTLS mode | (response body of status endpoint) |

### Manager API Schemas (verified from linux_patch_manager source)

#### `POST /api/v1/enroll`
- **Request Body:**
  ```json
  {
    "machine_id": "<string>",
    "fqdn": "<string>",
    "ip_address": "<string>",
    "os_details": { /* JSON object: distro, version, kernel, etc. */ }
  }
  ```
- **Success Response (202 Accepted):**
  ```json
  {
    "polling_token": "<64-char alphanumeric string>"
  }
  ```
- **Rate Limit:** 1 request per minute per IP (returns 429 if exceeded)
- **Auth:** None (unauthenticated - manager approval process provides security)

#### `GET /api/v1/enroll/status/{token}`
- **Response (tagged enum with `status` field):**
  ```json
  { "status": "pending" }                    // Still waiting for admin approval
  {
    "status": "approved",
    "ca_crt": "<PEM string>",
    "server_crt": "<PEM string>",
    "server_key": "<PEM string>"
  }                                           // Approved - extract PKI bundle
  { "status": "denied" }                     // Admin rejected request
  { "status": "not_found" }                  // Token expired/invalid/purged
  ```

### Design Decisions (Confirmed with Kelly)
| Decision | Value |
|----------|-------|
| **Certificate paths** | Write to existing mTLS config paths from `config.yaml` (no separate enrollment directory) |
| **Insecure enrollment** | Default - skip TLS verification on manager connection (approval process provides security) |
| **Polling timeout** | 24 hours maximum (86400 seconds, ~1440 attempts at 60s interval) |
| **Branch strategy** | Merge incrementally to `main` after each phase completes |
| **Cross-distro requirement** | All code must be functional across Debian/Ubuntu, RHEL/CentOS/Fedora, Alpine, Arch Linux |

---

## Phase 1 - Foundation & CLI Integration

**Goal:** Add enrollment CLI flag, new `enroll` module skeleton, config support for enrollment state.

### Sub-Agent Task 1.1: CLI Argument Extension
- **Profile:** developer
- **Files:** `src/main.rs`
- **Changes:**
  - Add `--enroll <MANAGER_URL>` flag to clap Args struct (required positional or named)
  - TLS verification is disabled by default on manager connection (insecure enrollment) - manager approval process provides security
  - Wire enrollment entry point into main() before server startup
- **Output Contract:** Updated main.rs with new CLI args compiled and tested across all target distros

### Sub-Agent Task 1.2: Enroll Module Skeleton
- **Profile:** developer
- **Files:** `src/enroll/mod.rs`, `src/enroll/identity.rs`, `src/enroll/client.rs`
- **Changes:**
  - Create new `enroll` module with submodules
  - `identity.rs`: Functions to extract machine-id, FQDN, IP addresses, OS details (distro, version, kernel)
  - `client.rs`: HTTP client wrapper for manager API communication (use reqwest)
  - Define Rust structs: `EnrollmentRequest`, `EnrollmentResponse`, `PollingStatus`, `PkiBundle`
- **Output Contract:** Module compiles cleanly; identity extraction functions return correct data

### Sub-Agent Task 1.3: Config State Support
- **Profile:** developer
- **Files:** `src/config/loader.rs`, `configs/config.yaml.example`
- **Changes:**
  - Add optional `enrollment` section to config schema:
    ```yaml
    enrollment:
      manager_url: ""
      polling_token: ""
      polling_interval_seconds: 60
      max_poll_attempts: 1440  # 24 hours at 60s intervals (86400 seconds)
    ```
  - Add persistence of polling token to config file during Phase 2
- **Output Contract:** Config loads with new enrollment section; backward compatible with existing configs

### Sub-Agent Task 1.4: Unit Tests for Identity Extraction
- **Profile:** developer
- **Files:** `tests/unit/enroll_identity.rs`
- **Changes:**
  - Test machine-id extraction from `/etc/machine-id`
  - Test FQDN resolution fallback chain
  - Test OS detail extraction (distro ID, version, kernel)
- **Output Contract:** All identity tests pass in CI

### Phase 1 Dependencies
- Add `reqwest` crate to Cargo.toml (HTTP client for manager API)
- No breaking changes to existing modules

---

## Phase 2 - Registration & Polling Logic

**Goal:** Implement Phase 1 and Phase 2 of the enrollment workflow.

### Sub-Agent Task 2.1: Registration Request Implementation
- **Profile:** developer
- **Files:** `src/enroll/client.rs`, `src/enroll/mod.rs`
- **Changes:**
  - Implement `POST /api/v1/enroll` request handler in client
  - Build JSON body with machine-id, FQDN, IPs, OS details
  - Parse response for `polling_token`
  - Handle error responses (400, 409 duplicate, 500)
- **Output Contract:** Registration function returns polling_token or structured error

### Sub-Agent Task 2.2: Polling Loop Implementation
- **Profile:** developer
- **Files:** `src/enroll/client.rs`, `src/enroll/mod.rs`
- **Changes:**
  - Implement polling loop with configurable interval (default 60s)
  - `GET /api/v1/enroll/status/{token}` endpoint calls
  - Handle responses per manager API enum:
    - `{status: "approved"}` → proceed to provisioning with PKI bundle
    - `{status: "denied"}` → abort with clear error message (admin rejected)
    - `{status: "not_found"}` → abort (token expired/invalid/purged)
    - `{status: "pending"}` → continue polling
  - Hard timeout: 24 hours maximum (1440 attempts at 60s interval) per Kelly's directive
  - Graceful shutdown on SIGINT/SIGTERM during polling
  - **Cross-distro note:** Use `tokio::time::sleep` (async, no platform-specific timers)
- **Output Contract:** Polling loop works correctly with all response codes

### Sub-Agent Task 2.3: Main.rs Enrollment Entry Point
- **Profile:** developer
- **Files:** `src/main.rs`
- **Changes:**
  - Wire `--enroll` flag to call enrollment flow before server startup
  - If enrollment succeeds, fall through to normal mTLS server startup
  - If enrollment fails, exit with non-zero code and clear error message
  - Logging: structured logs for each enrollment step
- **Output Contract:** `linux_patch_api --enroll https://manager.example.com` runs end-to-end (mock manager)

### Sub-Agent Task 2.4: Integration Tests
- **Profile:** developer
- **Files:** `tests/integration/enrollment_test.rs`
- **Changes:**
  - Mock manager server that simulates enrollment workflow
  - Test successful enrollment flow
  - Test denied enrollment (403 response)
  - Test expired token (404 response)
  - Test polling timeout behavior
- **Output Contract:** All integration tests pass

---

## Phase 3 - PKI Provisioning & Whitelist Integration

**Goal:** Implement Phase 3 of the enrollment workflow - cert extraction, file writing, whitelist update.

### Sub-Agent Task 3.1: PKI Bundle Extraction
- **Profile:** developer
- **Files:** `src/enroll/provision.rs`
- **Changes:**
  - Parse enrollment status response body for PKI bundle
  - Extract `ca.crt`, `server.crt`, `server.key` PEM data
  - Validate certificate chain (basic sanity: non-empty, valid PEM format)
  - Define target paths from config:
    ```rust
    // Default paths matching existing mTLS config
    /etc/linux_patch_api/certs/ca.pem
    /etc/linux_patch_api/certs/server.pem
    /etc/linux_patch_api/certs/server.key.pem
    ```
- **Output Contract:** PKI bundle extraction validated against test certificates

### Sub-Agent Task 3.2: Certificate File Writing
- **Profile:** developer
- **Files:** `src/enroll/provision.rs`
- **Changes:**
  - Write PEM files to target paths with secure permissions:
    - Certs: 0o644 (owner rw, group/others read)
    - Key: 0o600 (owner rw only)
  - Atomic write pattern: write to temp file → rename
  - Handle existing files: backup before overwrite if present
  - Verify written files are readable after creation
- **Output Contract:** Certificates written with correct permissions and content

### Sub-Agent Task 3.3: Whitelist Auto-Append
- **Profile:** developer
- **Files:** `src/auth/whitelist.rs`, `src/enroll/provision.rs`
- **Changes:**
  - Extract manager IP address from enrollment request/connection
  - Add method to WhitelistManager: `append_entry(ip: &str) -> Result<()>`
  - Append manager IP to `/etc/linux_patch_api/whitelist.yaml`
  - Log the whitelist change to audit log
  - Handle file locking for concurrent access safety
- **Output Contract:** Manager IP correctly appended to whitelist YAML

### Sub-Agent Task 3.4: mTLS Transition Logic
- **Profile:** developer
- **Files:** `src/main.rs`, `src/enroll/mod.rs`
- **Changes:**
  - After provisioning completes, update runtime config with new cert paths
  - Trigger mTLS server startup using provisioned certificates
  - No service restart required per spec
  - Log successful transition to mTLS mode
- **Output Contract:** Server transitions from enrollment mode to mTLS listening without restart

### Sub-Agent Task 3.5: Security Hardening Review
- **Profile:** hacker
- **Files:** All enroll module files
- **Changes:**
  - Review for security issues:
    - Certificate validation (don't skip TLS verification in production)
    - Secure file permissions enforcement
    - No sensitive data in logs (polling_token, cert contents)
    - Input validation on manager URL (scheme, host format)
    - Protection against MITM during enrollment (recommend `--enroll-verify` flag)
  - Document findings in security review notes
- **Output Contract:** Security review checklist completed with mitigations applied

---

## Phase 4 - Testing & Documentation

**Goal:** End-to-end testing, documentation updates, CI integration.

### Sub-Agent Task 4.1: End-to-End Test Suite
- **Profile:** developer
- **Files:** `tests/e2e/test_enrollment.py`
- **Changes:**
  - Docker-based test environment with manager mock + api instance
  - Full enrollment flow from CLI to mTLS listening
  - Verify certificate files on disk after enrollment
  - Verify whitelist contains manager IP
  - Test denial and rejection scenarios
- **Output Contract:** E2E tests pass in CI pipeline

### Sub-Agent Task 4.2: Documentation Updates
- **Profile:** developer
- **Files:** `README.md`, `DEPLOYMENT_GUIDE.md`, `API_DOCUMENTATION.md`
- **Changes:**
  - Add enrollment usage section to README
  - Update deployment guide with self-enrollment workflow
  - Document enrollment config options
  - Add troubleshooting section for common enrollment failures
- **Output Contract:** Documentation covers enrollment feature comprehensively

### Sub-Agent Task 4.3: CI Pipeline Integration
- **Profile:** developer
- **Files:** `.gitea/workflows/ci.yml`
- **Changes:**
  - Add enrollment unit tests to CI matrix
  - Add integration test stage with mock manager
  - Verify binary builds with `--enroll` flag in help output
- **Output Contract:** CI pipeline includes enrollment test stages

---

## Phase 5 - Documentation & Spec Synchronization

**Goal:** Ensure ALL project documentation and spec files accurately reflect the self-enrollment feature. This is a mandatory final stage before any code can be considered complete.

### Sub-Agent Task 5.1: SPEC.md Update
- **Profile:** developer
- **Files:** `SPEC.md`
- **Changes:**
  - Update Self-Enrollment Workflow section with finalized implementation details
  - Add enrollment-specific error codes to Error Categories section
  - Add enrollment events to Audit Logging requirements (enrollment success/failure, cert provisioning)
  - Update Certificate Management section to reflect automated option alongside manual distribution
  - Add enrollment CLI flags to any existing CLI reference section
  - Cross-reference all spec sections that touch enrollment behavior
- **Output Contract:** SPEC.md is internally consistent and fully documents the feature

### Sub-Agent Task 5.2: API_DOCUMENTATION.md Update
- **Profile:** developer
- **Files:** `API_DOCUMENTATION.md`
- **Changes:**
  - Add complete documentation for all enrollment-related endpoints:
    - `POST /api/v1/enroll` (manager-side endpoint used by api daemon)
    - `GET /api/v1/enroll/status/{token}` (manager-side status polling)
  - Document request/response JSON schemas with field types, descriptions, and examples
  - Document all HTTP status codes for each endpoint (200, 202, 400, 403, 404, 409, 500)
  - Add enrollment-specific error codes to the error reference table
  - Include curl examples for each endpoint
  - Document the complete enrollment flow sequence diagram or step-by-step walkthrough
- **Output Contract:** API documentation is complete and usable by developers integrating with the manager

### Sub-Agent Task 5.3: DEPLOYMENT_GUIDE.md Update
- **Profile:** developer
- **Files:** `DEPLOYMENT_GUIDE.md`
- **Changes:**
  - Add comprehensive "Self-Enrollment Deployment" section covering:
    - Prerequisites (manager URL, network connectivity, DNS)
    - Step-by-step enrollment procedure for new hosts
    - Configuration options (`enrollment` config section)
    - Troubleshooting common enrollment failures
    - Post-enrollment verification steps
  - Update existing mTLS setup sections to reference self-enrollment as alternative
  - Add rollback/re-enrollment procedures if enrollment fails mid-process
- **Output Contract:** Deployment guide covers both manual and automated certificate provisioning paths

### Sub-Agent Task 5.4: README.md Update
- **Profile:** developer
- **Files:** `README.md`
- **Changes:**
  - Add self-enrollment to feature list/highlights
  - Add usage examples for `--enroll` flag
  - Link to DEPLOYMENT_GUIDE.md and API_DOCUMENTATION.md for details
  - Update architecture diagram if README contains one
- **Output Contract:** README accurately represents enrollment as a first-class feature

### Sub-Agent Task 5.5: CHANGELOG.md Update
- **Profile:** developer
- **Files:** `CHANGELOG.md`
- **Changes:**
  - Add entry under current development version:
    - Feature: Self-enrollment workflow with manager registration and PKI provisioning
    - Added: `--enroll <MANAGER_URL>` CLI flag
    - Added: Automated certificate provisioning from linux_patch_manager
    - Added: Automatic whitelist entry for manager IP after enrollment
    - Added: Configurable polling interval and max attempts
- **Output Contract:** CHANGELOG accurately reflects all enrollment-related changes

### Sub-Agent Task 5.6: ROADMAP.md Update
- **Profile:** developer
- **Files:** `ROADMAP.md`
- **Changes:**
  - Move self-enrollment from planned to completed (or current milestone)
  - Update timeline and dependencies affected by enrollment feature
- **Output Contract:** Roadmap reflects current feature state accurately

### Sub-Agent Task 5.7: Config Example Files Update
- **Profile:** developer
- **Files:** `configs/config.yaml.example`, `configs/whitelist.yaml.example`
- **Changes:**
  - Add commented enrollment section to config example:
    ```yaml
    # enrollment:
    #   manager_url: "https://manager.example.com"
    #   polling_interval_seconds: 60
    #   max_poll_attempts: 0  # 0 = unlimited
    ```
  - Update comments to explain each option
- **Output Contract:** Example configs reflect all available configuration options

### Sub-Agent Task 5.8: Final Documentation Audit
- **Profile:** researcher
- **Files:** All documentation files listed above
- **Changes:**
  - Cross-reference all docs for consistency (same terminology, same field names)
  - Verify no broken internal links
  - Check that enrollment is mentioned in every doc where it's relevant
  - Verify error codes are consistent across SPEC.md, API_DOCUMENTATION.md, and code
  - Produce a documentation audit checklist with pass/fail status
- **Output Contract:** Documentation audit report confirming consistency across all files

---

## Execution Order & Parallelism

```
Phase 1: [1.1] [1.2] [1.3] → sequential (CLI → module → config)
         ↘ [1.4] parallel with 1.2-1.3

Phase 2: [2.1] → [2.2] → [2.3] → sequential (registration → polling → wiring)
         ↘ [2.4] after 2.3 complete

Phase 3: [3.1] [3.2] [3.3] → can run in parallel (PKI, certs, whitelist are independent)
         ↘ [3.4] depends on all of 3.1-3.3
         ↘ [3.5] runs after Phase 3 code complete

Phase 4: [4.1] [4.2] [4.3] → parallel (tests, docs, CI independent)

Phase 5: [5.1]-[5.6] → can run in parallel (each doc file is independent)
         ↘ [5.7] after 5.1-5.6 (config examples depend on finalized config schema)
         ↘ [5.8] final audit depends on ALL Phase 5 tasks complete
```

**Estimated Total Effort:** ~10 sub-agent cycles across 5 phases

---

## Risks & Considerations

| Risk | Mitigation |
|------|------------|
| Manager API contract mismatch | Verify exact request/response schemas with deployed manager code before Phase 2 |
| Certificate path conflicts | Use config-defined paths, not hardcoded; validate against existing mTLS config |
| File permission issues on non-Linux targets | Scope to Linux only per spec; document limitation |
| Enrollment during active API service | Enrollment runs pre-server-startup per design; no conflict |
| Token expiry during long polling | Configurable max_poll_attempts; log warnings at intervals |

---

## Pre-Development Checklist

Before kicking off sub-agents:
- [ ] Kelly approves this phased plan
- [ ] Verify manager-side enrollment API endpoint schemas (request/response JSON)
- [ ] Confirm target certificate paths match existing mTLS config structure
- [ ] Create `feat/self-enrollment` branch from main
- [ ] Add `reqwest` dependency to Cargo.toml

---


## Confirmed Design Decisions

| # | Question | Decision | Source |
|---|----------|----------|--------|
| 1 | Manager API schema | Verified from `linux_patch_manager` source at `/a0/usr/projects/linux_patch_manager/crates/pm-core/src/models.rs` lines 130-169 and `pm-web/src/routes/enrollment.rs` | Local source code |
| 2 | Certificate paths | Write to existing mTLS config paths from `config.yaml` (no separate enrollment directory) | Kelly confirmation |
| 3 | Insecure enrollment default | TLS verification disabled by default on manager connection - approval process provides security | Kelly confirmation |
| 4 | Polling timeout | Hard limit: 24 hours maximum (1440 attempts at 60s interval) | Kelly confirmation |
| 5 | Branch strategy | Merge incrementally to `main` after each phase completes | Kelly confirmation |
| 6 | Cross-distro requirement | All code must be functional across Debian/Ubuntu, RHEL/CentOS/Fedora, Alpine, Arch Linux | Kelly confirmation |
