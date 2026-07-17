# Linux Patch API — Security Findings Report

**Date:** 2026-07-16
**API Version:** 2.4.0
**Scope:** Full security posture assessment — mTLS, IP whitelist, input validation, injection prevention, CRL, rate limiting, CI/CD security scanning, committed secrets
**Supersedes:** Phase 3 report (2026-04-09) and all prior findings

---

## Executive Summary

| Metric | Value |
|--------|-------|
| **Critical Findings** | 0 (all resolved) |
| **High Findings** | 0 (all resolved) |
| **Medium Findings** | 0 (all resolved) |
| **Low Findings** | 2 (acceptable — see below) |

**Overall Security Status:** All Critical, High, and Medium findings from prior phases have been resolved and verified against the current codebase. The remaining Low findings are acceptable for internal-network deployment.

---

## Issue #17 Remediation

Issue #17 identified four inaccuracies in the prior version of this report. All four have been addressed in the codebase and are reflected below.

### 1. IP Whitelist Enforcement — RESOLVED (verified)

**Prior claim was false:** The old report claimed the whitelist was "RESOLVED" because "the auth middleware chain is now complete" after the TLS fix. This was incorrect — the middleware was never wired into the pipeline.

**Current state — actually resolved:**

- `WhitelistMiddleware` is wired into the Actix-web pipeline in `src/main.rs:454`
- Middleware order: WhitelistMiddleware → SecurityHeadersMiddleware → RateLimitMiddleware → Logger
- Deny-by-default: non-whitelisted IPs receive `403 Forbidden` (`src/auth/whitelist.rs:529`)
- Fail-closed: if `peer_addr()` is unavailable, the request is denied (`src/auth/whitelist.rs:536`)
- Fail-closed on load failure: `WhitelistManager::new_deny_all()` is used if the whitelist file cannot be loaded (`src/main.rs:357`)
- CIDR subnet support with correct bitmask matching (`src/auth/whitelist.rs:412`)
- IPv6 denied by default (whitelist supports IPv4 only) (`src/auth/whitelist.rs:284`)
- Health and system-info endpoints are exempt from whitelist checks (`src/auth/whitelist.rs:486`)
- Auto-reload via file watcher (`src/auth/whitelist.rs:376`)

**Negative test coverage (the gap called out in issue #17):**

- `tests/integration/auth_test.rs:86` — `192.168.1.101` denied when only `192.168.1.100` is whitelisted
- `tests/integration/auth_test.rs:106-107` — IPs outside `/24` CIDR denied
- `tests/integration/auth_test.rs:128-130` — Multiple-entry deny cases
- `tests/integration/auth_test.rs:161-162` — Non-whitelisted socket address denied
- `src/auth/whitelist.rs:625` — `new_deny_all()` blocks all IPs (unit test)
- `src/auth/whitelist.rs:639` — IPv6 socket denied (unit test)

### 2. Whitelist Test 2.1 — RESOLVED (negative cases added)

**Prior gap:** Test 2.1 only checked that a whitelisted IP (localhost) was allowed. It never tested denial.

**Current state:** The test suite now includes both positive and negative cases. See the test list above — every whitelist test includes both allow and deny assertions.

### 3. Injection Tests — RESOLVED (argument injection covered)

**Prior gap:** Tests 4.1–4.3 only used SQL/shell metacharacter payloads. They did not test argument/option injection (tokens beginning with `-`), which is the actual exploitable vector. `/patches/apply` was not injection-tested at all.

**Current state:**

- `validate_package_name()` in `src/packages/mod.rs:31` enforces a strict allowlist:
  - Must start with an alphanumeric character (blocks leading `-`, `/`, `.`, etc.)
  - Only `a-zA-Z0-9+._-` allowed in subsequent characters
  - Maximum 256 characters
  - Empty strings rejected
- `validate_version_string()` in `src/packages/mod.rs:64` enforces the same pattern with additional `:` and `~` for RPM epochs and Debian ordering
- `validate_service_name()` in `src/packages/mod.rs` enforces the same pattern for systemctl/rc-service targets
- Validation is applied in all handlers:
  - `GET /api/v1/packages/{name}` — `src/api/handlers/packages.rs:237`
  - `POST /api/v1/packages` — `src/api/handlers/packages.rs:284` (validates all names + versions in batch)
  - `PUT /api/v1/packages/{name}` — `src/api/handlers/packages.rs:361`
  - `DELETE /api/v1/packages/{name}` — `src/api/handlers/packages.rs:975`
  - `POST /api/v1/patches/apply` — `src/api/handlers/patches.rs:162` (validates all package names in request)

**Test coverage for argument injection:**

- `src/packages/mod.rs:3279` — `test_validate_package_name_leading_hyphen`: rejects `-evil`, `--allow-unauthenticated`
- `src/packages/mod.rs:3286` — `test_validate_package_name_shell_metacharacters`: rejects `;`, `$()`, backticks, `|`, `&`, `>`, `<`, quotes, `!`
- `src/packages/mod.rs:3301` — `test_validate_package_name_path_separators`: rejects `/usr/bin/evil`, `..\..\evil`, `../evil`
- `src/packages/mod.rs:3308` — `test_validate_package_name_whitespace`: rejects spaces, tabs, newlines
- `src/packages/mod.rs:3337` — `test_validate_version_string_leading_hyphen`: rejects `-1.0`
- `src/packages/mod.rs:3372` — `test_validate_service_name_leading_hyphen`: rejects `-evil`, `--help`
- `tests/integration/api_test.rs:452` — `test_vuln_001_package_name_length_validation`: 300-char name → 400
- `tests/integration/api_test.rs:471` — `test_vuln_003_empty_string_rejection`: empty name → 400

### 4. Committed Private Keys — RESOLVED (verified)

**Prior gap:** The old report listed CA key handling only as a Low "permissions" note and did not mention that the CA private key was committed to the repo.

**Current state:**

- No `.key` or `.key.pem` files exist in the repository (verified via filesystem search)
- `configs/certs/` contains only `README.md` — no certificate material
- `.gitignore` excludes `*.key`, `*.key.pem`, `configs/certs/*.pem`, `configs/certs/*.srl`, `tests/e2e/certs/*.key`
- `gitleaks` secret scanning runs in CI (`.github/workflows/ci.yml:68`)
- `scripts/generate-dev-certs.sh` generates test certificates at runtime

---

## Current Security Controls

### mTLS Authentication (TLS Handshake Gate)

**Status:** RESOLVED

Client certificate authentication is enforced at the TLS handshake level by rustls via `CrlAwareVerifier` (wraps `WebPkiClientVerifier`). No application-layer certificate middleware is needed.

- TLS 1.3 only — hardcoded in `build_rustls_config()` (`src/auth/mtls.rs:190`)
- Plain HTTP connections are rejected at the TLS handshake
- Self-signed certificates rejected (CA-signed only)
- `CrlAwareVerifier` checks certificate serials against CRL at handshake time (`src/auth/mtls.rs:74`)
- Fail-closed on invalid CRL signature (`src/auth/mtls.rs:110`)
- Degraded mode (WebPKI-only) on missing/expired CRL (`src/auth/mtls.rs:102`)

**Architecture Decision Record:** See `src/auth/mtls.rs` header and `src/auth/mod.rs` header. The old `MtlsMiddleware` was dead code (never wired in) and has been removed. The duplicate-header check was extracted to `SecurityHeadersMiddleware` (which IS wired in).

### IP Whitelist Enforcement

**Status:** RESOLVED

See Issue #17 section above for full details. `WhitelistMiddleware` is wired into the pipeline with deny-by-default, fail-closed, and CIDR support.

### CRL (Certificate Revocation List)

**Status:** RESOLVED

- Full CRL implementation in `src/auth/crl.rs` (928 lines)
- CRL loading from disk (PEM format) with CA signature verification
- In-memory revoked serial index (hex-encoded, lowercase)
- Manager-based CRL refresh background task
- Periodic CRL health re-evaluation (hourly disk reload)
- CRL status reported via `/health` endpoint: `valid`, `expired`, `missing`, `invalid`, `degraded`
- CRL age and nextUpdate reported via `/health`
- Fail-closed on invalid CRL signature (server refuses to start)
- Degraded mode on missing/expired CRL (WebPKI-only, health reports degraded)

### Rate Limiting

**Status:** RESOLVED

- `RateLimitMiddleware` in `src/api/rate_limit.rs` — wired into pipeline (`src/main.rs:456`)
- Per-IP, two-tier rate limiting:
  - Destructive tier (POST/PUT/DELETE): 20 req/min, burst 10
  - Read tier (GET): 120 req/min, burst 30
- Health and system-info endpoints exempt
- Configurable via `rate_limit` config section
- Returns `429 Too Many Requests` with `Retry-After` header

### Input Validation & Injection Prevention

**Status:** RESOLVED

- Package names: strict allowlist (`^[a-zA-Z0-9][a-zA-Z0-9+._-]*$`), max 256 chars, no leading hyphens
- Version strings: strict allowlist with `:` and `~` for RPM/Debian conventions, max 256 chars, no leading hyphens
- Service names: strict allowlist for systemctl/rc-service targets
- Path traversal: `validate_path_no_traversal()` blocks `..`, `//`, `\\`, URL-encoded variants (`%2e`, `%2f`, `%5c`)
- Applied to all package handlers and `/patches/apply`
- JSON schema validation via serde strict types
- Empty strings rejected for required fields

### Security Headers

**Status:** RESOLVED

- `SecurityHeadersMiddleware` in `src/auth/security_headers.rs` — wired into pipeline (`src/main.rs:455`)
- Rejects duplicate critical headers (`content-type`, `authorization`, `host`) with HTTP 400
- Prevents HTTP request smuggling and response-splitting attacks

### HTTP Protocol Hardening

**Status:** RESOLVED

- Unsupported HTTP methods return `405 Method Not Allowed` with `Allow` header (`src/api/routes.rs:21`)
- Client request timeout: 5 seconds (`src/main.rs:477`)
- Client disconnect timeout: 5 seconds (`src/main.rs:479`)
- Keep-alive: 15 seconds (`src/main.rs:491`)
- Max connection rate: 1000 (`src/main.rs:492`)
- Graceful shutdown timeout: 10 seconds (`src/main.rs:484`)

### CI/CD Security Scanning

**Status:** RESOLVED

- **cargo-audit**: Runs on every push, PR, and daily schedule (`.github/workflows/ci.yml:57`)
- **gitleaks**: Runs on every push and PR with full history (`fetch-depth: 0`) (`.github/workflows/ci.yml:68`)
- **clippy**: `-D warnings` enforced (`.github/workflows/ci.yml:44`)
- **fmt**: `--check` enforced (`.github/workflows/ci.yml:31`)
- **Fuzz tests**: 5000 iterations on every PR, 50000+10000 nightly (`.github/workflows/ci.yml:94`)

### Self-Update Security

**Status:** RESOLVED

- Manager Pull model only — agent never fetches from GitHub directly
- GPG-signed package repository hosted by manager
- GPG public key delivered via mTLS enrollment (`PkiBundle.repo_config.gpg_public_key`)
- Per-manager GPG key (never hardcoded)
- Native package manager signature verification (apt `signed-by`, dnf `gpgcheck`, etc.)
- GPG key health reported via `/health` endpoint (`gpg_key_status`, `gpg_key_expires_at`)
- Self-update uses atomic reservation with persistent state file
- Fail-closed: version mismatch on restart → recovery mode
- SIGTERM handler drains in-progress package operations before shutdown

### Systemd Hardening

**Status:** RESOLVED

- `ProtectSystem=strict`, `ProtectHome=yes`, `NoNewPrivileges=yes`
- `SystemCallFilter=@system-service`
- Runs as root (required for package management)
- `Type=notify` with sd_notify readiness protocol

---

## Remaining Low Findings

### LOW: Verbose Error Messages

Some error responses may leak internal implementation details (e.g., package manager error strings forwarded to API clients). This is acceptable for an internal-network management API where the client is always the trusted manager, but should be reviewed if the network model changes.

### LOW: Log Retention (30 days)

Logs are retained for 30 days with daily rotation and compression. Some compliance frameworks require 90+ days. This is a configuration/policy decision, not a code defect.

---

## Test Evidence Summary

| Test Suite | Location | Coverage |
|------------|----------|----------|
| Whitelist allow/deny | `tests/integration/auth_test.rs` | Positive + negative cases, CIDR, multiple entries, socket addr, IPv6 denial |
| Security headers | `tests/integration/auth_test.rs` | Duplicate content-type, authorization, host detection |
| Input validation (VULN-001/003) | `tests/integration/api_test.rs` | Length validation, empty string rejection |
| Path traversal (VULN-002) | `tests/integration/api_test.rs` | `..`, `//`, `\\`, URL-encoded variants |
| Method not allowed (VULN-005) | `tests/integration/api_test.rs` | PATCH, OPTIONS → 405 |
| Package name validation | `src/packages/mod.rs` (unit tests) | Valid names, empty, too long, leading hyphen, shell metacharacters, path separators, whitespace, leading digit |
| Version string validation | `src/packages/mod.rs` (unit tests) | Valid versions, empty, leading hyphen, shell metacharacters, path separators |
| Service name validation | `src/packages/mod.rs` (unit tests) | Valid names, empty, leading hyphen, path separators, shell metacharacters |
| CRL-aware verifier | `src/auth/mtls.rs` (unit tests) | Construction with Valid/Missing/Invalid/revoked-serial CRL states |
| Deny-all whitelist | `src/auth/whitelist.rs` (unit tests) | All IPs blocked, IPv6 blocked, entry count = 0 |
| Concurrency invariants | `tests/unit/concurrency_invariant_test.rs` | Self-update vs. job admission ordering, queue capacity, restart-pending state |
| Enrollment | `tests/e2e/test_enrollment_e2e.rs` | Full enrollment flow, whitelist append, duplicate prevention, failure rollback |
| CI fuzz | `.github/workflows/ci.yml` | 5000 iterations per PR, 50000+10000 nightly |

---

## Security Middleware Stack

Middleware order in `src/main.rs:452-459` (order matters — outermost runs first):

1. **WhitelistMiddleware** — IP-based access control (deny-by-default, fail-closed)
2. **SecurityHeadersMiddleware** — VULN-006: reject duplicate critical headers
3. **RateLimitMiddleware** — per-IP rate limiting (read + destructive tiers)
4. **Logger** — request logging (after auth decisions)

Client certificate authentication is handled at the TLS handshake level by rustls — before any middleware runs.

---

## Architecture Decision Record: rustls as Authoritative Client-Auth Gate

**Date:** 2026-06-06
**Status:** Accepted
**Context:** Issue #13

### Decision

Client certificate authentication is enforced at the TLS handshake level by rustls via `CrlAwareVerifier`, NOT by application-layer middleware.

### Context

The original `MtlsMiddleware` was never wired into the Actix-web pipeline (dead code). It contained:
1. A duplicate-header check (VULN-006) that never ran
2. A `validate_client_certificate()` stub that returned `Ok(())` unconditionally

Meanwhile, actual client certificate verification was always performed by rustls at the TLS handshake level through `CrlAwareVerifier` (which wraps `WebPkiClientVerifier`), with CRL revocation checking integrated into the same path.

### Changes Made

1. **Removed dead code:** `MtlsMiddleware`, `MtlsMiddlewareService`, `validate_client_certificate()`, and the Transform/Service impls
2. **Extracted VULN-006:** `has_duplicate_critical_headers()` moved to new `SecurityHeadersMiddleware` (wired into pipeline)
3. **Converted `build_rustls_config()`** from method on `MtlsMiddleware` to free function
4. **Preserved:** `CrlAwareVerifier`, `MtlsConfig`, `MtlsError`, `ClientCertInfo`, `build_rustls_config()`, and all CRL infrastructure

### Rationale

- rustls provides battle-tested X.509 verification at the TLS handshake level
- Enforcing auth at the TLS layer eliminates bypass vulnerabilities (middleware ordering bugs, route-specific skips)
- CRL revocation checking is integrated into the same handshake path
- Application-layer certificate validation is redundant when TLS already rejects untrusted connections

---

**Report Generated:** 2026-07-16
**Verified Against:** v2.4.0 codebase (commit `291cca1`)