# Linux Patch API — Security Controls Matrix

**Version:** 2.4.0
**Date:** 2026-07-16
**Document Purpose:** Map security requirements to implementations with compliance evidence

---

## Compliance Overview

| Category | Total Controls | Compliant | Partial | Not Implemented | Compliance Rate |
|----------|---------------|-----------|---------|-----------------|-----------------|
| Authentication | 5 | 5 | 0 | 0 | 100% |
| Authorization | 3 | 3 | 0 | 0 | 100% |
| Data Protection | 4 | 4 | 0 | 0 | 100% |
| API Security | 7 | 7 | 0 | 0 | 100% |
| Audit & Logging | 5 | 5 | 0 | 0 | 100% |
| System Hardening | 4 | 4 | 0 | 0 | 100% |
| CI/CD Security | 3 | 3 | 0 | 0 | 100% |
| **TOTAL** | **31** | **31** | **0** | **0** | **100%** |

---

## 1. Authentication Controls

### AUTH-001: mTLS Certificate Authentication

| Field | Value |
|-------|-------|
| **Requirement** | mTLS certificate-based authentication required for all connections |
| **Implementation** | rustls with `CrlAwareVerifier` (wraps `WebPkiClientVerifier`), enforced at TLS handshake |
| **Evidence** | `src/auth/mtls.rs:74-120`, `src/main.rs:608` |
| **Test Result** | PASS — Non-mTLS connections rejected at handshake; self-signed certs rejected |
| **Compliance Status** | COMPLIANT |

### AUTH-002: Certificate Authority

| Field | Value |
|-------|-------|
| **Requirement** | Internal self-hosted CA for certificate issuance |
| **Implementation** | OpenSSL CA infrastructure; private keys generated at runtime, never committed |
| **Evidence** | `scripts/generate-dev-certs.sh`, `.gitignore` (excludes `*.key`, `*.key.pem`) |
| **Test Result** | PASS — No private key files in repository (verified via filesystem search) |
| **Compliance Status** | COMPLIANT |

### AUTH-003: Unique Client Certificates

| Field | Value |
|-------|-------|
| **Requirement** | Unique certificate per client (no shared certs) |
| **Implementation** | Per-client certificate generation with unique CN |
| **Evidence** | `scripts/generate-dev-certs.sh` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### AUTH-004: Certificate Validity Period

| Field | Value |
|-------|-------|
| **Requirement** | 1 year standard certificate expiration |
| **Implementation** | Certificates generated with `-days 365` parameter |
| **Evidence** | `scripts/generate-dev-certs.sh` |
| **Test Result** | PASS — Expired certificates rejected at TLS handshake |
| **Compliance Status** | COMPLIANT |

### AUTH-005: TLS Version Enforcement

| Field | Value |
|-------|-------|
| **Requirement** | TLS 1.3 only, no legacy protocol support |
| **Implementation** | rustls `with_protocol_versions(&[&TLS13])` — hardcoded |
| **Evidence** | `src/auth/mtls.rs:190` |
| **Test Result** | PASS — Plain HTTP connections rejected; TLS 1.2 and below not negotiated |
| **Compliance Status** | COMPLIANT |

---

## 2. Authorization Controls

### AUTHZ-001: IP Whitelist Enforcement

| Field | Value |
|-------|-------|
| **Requirement** | IP whitelist enforcement (deny by default, allow only listed) |
| **Implementation** | `WhitelistMiddleware` wired into Actix-web pipeline; deny-by-default; fail-closed on missing peer addr; fail-closed on load failure (`new_deny_all()`) |
| **Evidence** | `src/auth/whitelist.rs:488-548`, `src/main.rs:454` |
| **Test Result** | PASS — Negative tests: non-whitelisted IPs denied (`auth_test.rs:86,106,128,161`); deny-all blocks everything (`whitelist.rs:625`); IPv6 denied (`whitelist.rs:639`) |
| **Compliance Status** | COMPLIANT |

### AUTHZ-002: Binary Authorization Model

| Field | Value |
|-------|-------|
| **Requirement** | All-or-nothing access (no RBAC complexity) |
| **Implementation** | Single permission level — authenticated clients have full API access |
| **Evidence** | `src/auth/mod.rs` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### AUTHZ-003: Silent Drop for Unauthorized

| Field | Value |
|-------|-------|
| **Requirement** | Silent drop for non-mTLS connections (no response) |
| **Implementation** | TLS handshake failure returns no HTTP response |
| **Evidence** | `src/auth/mtls.rs` (rustls handshake), `src/main.rs:651` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

---

## 3. Data Protection Controls

### DATA-001: Encryption in Transit

| Field | Value |
|-------|-------|
| **Requirement** | TLS 1.3 encryption for all API communications |
| **Implementation** | rustls TLS 1.3 on port 12443 |
| **Evidence** | `src/auth/mtls.rs:190` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### DATA-002: Certificate Key Protection

| Field | Value |
|-------|-------|
| **Requirement** | Private key permissions 600; keys never committed to repository |
| **Implementation** | `.gitignore` excludes `*.key`, `*.key.pem`, `configs/certs/*.pem`, `tests/e2e/certs/*.key`; `gitleaks` runs in CI |
| **Evidence** | `.gitignore:17-22`, `.github/workflows/ci.yml:68` |
| **Test Result** | PASS — No private key files found in repository |
| **Compliance Status** | COMPLIANT |

### DATA-003: Job Storage Isolation

| Field | Value |
|-------|-------|
| **Requirement** | Job storage isolated in `/var/lib/linux_patch_api/jobs/` |
| **Implementation** | Dedicated directory with restricted access |
| **Evidence** | `src/jobs/manager.rs` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### DATA-004: Config File Protection

| Field | Value |
|-------|-------|
| **Requirement** | Config files with appropriate permissions (644 for config, 600 for keys) |
| **Implementation** | File permissions enforced during deployment |
| **Evidence** | `DEPLOYMENT_SECURITY_GUIDE.md` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

---

## 4. API Security Controls

### API-001: Input Validation — Package Names

| Field | Value |
|-------|-------|
| **Requirement** | Package names: alphanumeric + standard package chars only; no leading hyphens; max 256 chars |
| **Implementation** | `validate_package_name()` — strict allowlist `^[a-zA-Z0-9][a-zA-Z0-9+._-]*$`, max 256, no empty, no leading hyphen |
| **Evidence** | `src/packages/mod.rs:31-58`, applied at `src/api/handlers/packages.rs:237,284,361,975` |
| **Test Result** | PASS — Unit tests: valid names, empty, too long, leading hyphen, shell metacharacters, path separators, whitespace |
| **Compliance Status** | COMPLIANT |

### API-002: Input Validation — Version Strings

| Field | Value |
|-------|-------|
| **Requirement** | Versions: strict allowlist with RPM epoch (`:`) and Debian tilde (`~`); no leading hyphens; max 256 chars |
| **Implementation** | `validate_version_string()` — `^[a-zA-Z0-9][a-zA-Z0-9+.:~_-]*$` |
| **Evidence** | `src/packages/mod.rs:64-96` |
| **Test Result** | PASS — Unit tests: valid versions, empty, leading hyphen, shell metacharacters, path separators |
| **Compliance Status** | COMPLIANT |

### API-003: Input Validation — IP Addresses

| Field | Value |
|-------|-------|
| **Requirement** | IP Addresses: IPv4 + CIDR validation for whitelist |
| **Implementation** | IP address parsing with CIDR support |
| **Evidence** | `src/auth/whitelist.rs:102-121,332-369` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### API-004: Input Validation — Path Traversal

| Field | Value |
|-------|-------|
| **Requirement** | Path traversal blocked (no `..` in paths) |
| **Implementation** | `validate_path_no_traversal()` blocks `..`, `//`, `\\`, URL-encoded variants (`%2e`, `%2f`, `%5c`) |
| **Evidence** | `src/api/handlers/system.rs:25-31` |
| **Test Result** | PASS — Tests: `../etc/passwd`, `..\windows\system32`, `path//double//slash`, `%2e%2e/etc/passwd`, `..%2fetc/passwd` all rejected |
| **Compliance Status** | COMPLIANT |

### API-005: JSON Schema Validation

| Field | Value |
|-------|-------|
| **Requirement** | Strict schema validation for all request bodies |
| **Implementation** | Serde JSON deserialization with strict types |
| **Evidence** | `src/api/handlers/packages.rs:128-133`, `src/api/handlers/patches.rs:38-46` |
| **Test Result** | PASS — Malformed JSON rejected with 400 |
| **Compliance Status** | COMPLIANT |

### API-006: Job Timeout Enforcement

| Field | Value |
|-------|-------|
| **Requirement** | Maximum 30 minutes per job |
| **Implementation** | Job manager timeout configuration |
| **Evidence** | `src/jobs/manager.rs`, `src/packages/coordinator.rs` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### API-007: Rate Limiting

| Field | Value |
|-------|-------|
| **Requirement** | Per-IP rate limiting to prevent DoS |
| **Implementation** | `RateLimitMiddleware` — per-IP, two-tier (destructive: 20/min burst 10; read: 120/min burst 30); health-exempt |
| **Evidence** | `src/api/rate_limit.rs`, `src/main.rs:456` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

---

## 5. Audit & Logging Controls

### AUDIT-001: Request Logging

| Field | Value |
|-------|-------|
| **Requirement** | All API requests logged (endpoint, method, timestamp, client cert ID) |
| **Implementation** | Actix-web Logger middleware + structured tracing |
| **Evidence** | `src/main.rs:459`, `src/logging/` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### AUDIT-002: Authentication Event Logging

| Field | Value |
|-------|-------|
| **Requirement** | Authentication events (success/failure, cert validation) logged |
| **Implementation** | rustls/CrlAwareVerifier logs revocation events; whitelist logs denials |
| **Evidence** | `src/auth/mtls.rs:91`, `src/auth/whitelist.rs:523` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### AUDIT-003: Package Operation Logging

| Field | Value |
|-------|-------|
| **Requirement** | Package operations logged (name, version, action, result) |
| **Implementation** | Package handlers log all operations via tracing |
| **Evidence** | `src/api/handlers/packages.rs`, `src/api/handlers/patches.rs` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### AUDIT-004: Log Retention

| Field | Value |
|-------|-------|
| **Requirement** | 30-day retention with daily rotation and compression |
| **Implementation** | logrotate configuration |
| **Evidence** | `DEPLOYMENT_SECURITY_GUIDE.md` |
| **Test Result** | PASS (30 days; 90+ days recommended for some compliance frameworks) |
| **Compliance Status** | COMPLIANT |

### AUDIT-005: Request ID Tracking

| Field | Value |
|-------|-------|
| **Requirement** | Request IDs required for all requests |
| **Implementation** | UUID generation per request, included in response envelope |
| **Evidence** | `src/api/handlers/packages.rs:50,161,232,279,356` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

---

## 6. System Hardening Controls

### SYS-001: Systemd Service Hardening

| Field | Value |
|-------|-------|
| **Requirement** | Run as systemd service with security hardening |
| **Implementation** | `ProtectSystem=strict`, `ProtectHome=yes`, `NoNewPrivileges=yes`, `SystemCallFilter=@system-service` |
| **Evidence** | `configs/linux-patch-api.service` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### SYS-002: Root Privilege Requirement

| Field | Value |
|-------|-------|
| **Requirement** | Must run with elevated privileges for package management |
| **Implementation** | Service runs as root user |
| **Evidence** | `configs/linux-patch-api.service` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### SYS-003: System Call Filtering

| Field | Value |
|-------|-------|
| **Requirement** | Restrict system calls to minimum required |
| **Implementation** | `SystemCallFilter=@system-service` in systemd unit |
| **Evidence** | `configs/linux-patch-api.service` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### SYS-004: Internal Network Only

| Field | Value |
|-------|-------|
| **Requirement** | Internal network only (no internet exposure) |
| **Implementation** | Firewall rules restrict access to management network |
| **Evidence** | `DEPLOYMENT_SECURITY_GUIDE.md` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

---

## 7. CI/CD Security Controls

### CICD-001: Dependency Vulnerability Scanning

| Field | Value |
|-------|-------|
| **Requirement** | Automated dependency scanning in CI |
| **Implementation** | `cargo-audit` runs on every push, PR, and daily schedule |
| **Evidence** | `.github/workflows/ci.yml:57-66` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### CICD-002: Secret Scanning

| Field | Value |
|-------|-------|
| **Requirement** | Automated secret scanning in CI with full history |
| **Implementation** | `gitleaks-action@v3` with `fetch-depth: 0` |
| **Evidence** | `.github/workflows/ci.yml:68-78` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

### CICD-003: Fuzz Testing in CI

| Field | Value |
|-------|-------|
| **Requirement** | Automated fuzz testing in CI |
| **Implementation** | 5000 iterations on every PR; 50000 random + 10000 truncated nightly |
| **Evidence** | `.github/workflows/ci.yml:94-123` |
| **Test Result** | PASS |
| **Compliance Status** | COMPLIANT |

---

## 8. Test Evidence Summary

| Test Suite | Location | Key Coverage |
|------------|----------|--------------|
| Whitelist allow/deny | `tests/integration/auth_test.rs` | Positive + negative cases, CIDR, multiple entries, socket addr, IPv6 denial |
| Security headers | `tests/integration/auth_test.rs` | Duplicate content-type, authorization, host detection |
| Input validation | `tests/integration/api_test.rs` | Length, empty string, method not allowed, path traversal |
| Package name validation | `src/packages/mod.rs` (unit) | Valid, empty, too long, leading hyphen, shell metacharacters, path separators, whitespace |
| Version string validation | `src/packages/mod.rs` (unit) | Valid, empty, leading hyphen, shell metacharacters, path separators |
| Service name validation | `src/packages/mod.rs` (unit) | Valid, empty, leading hyphen, path separators, shell metacharacters |
| CRL-aware verifier | `src/auth/mtls.rs` (unit) | Valid/Missing/Invalid/revoked-serial CRL states |
| Deny-all whitelist | `src/auth/whitelist.rs` (unit) | All IPs blocked, IPv6 blocked |
| Concurrency invariants | `tests/unit/concurrency_invariant_test.rs` | Self-update vs. job admission ordering |
| Enrollment E2E | `tests/e2e/test_enrollment_e2e.rs` | Full flow, whitelist append, duplicate prevention, failure rollback |
| CI fuzz | `.github/workflows/ci.yml` | 5000/PR, 60000/nightly |

---

## 9. Compliance Certification

**Overall Compliance:** 100% (31/31 controls fully compliant)

**Deployment Authorization:** Approved for internal network deployment

**Conditions:**
- Deploy only on isolated internal network
- Maintain certificate inventory and whitelist documentation
- Monitor audit logs for security events
- Review log retention policy if compliance framework requires 90+ days

**Certified Against:** v2.4.0 codebase (commit `291cca1`)
**Date:** 2026-07-16

---

*Document generated 2026-07-16 — supersedes all prior versions*