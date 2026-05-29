# Auto-Enrollment Implementation Plan

## Overview
Implement auto-enrollment workflow so the agent self-heals when certs are missing or invalid, instead of crash-looping.

## Spec Updates
- [x] Update SPEC.md: Self-Enrollment section, CLI arguments, startup behavior, cert validation, exit codes
- [x] Update DEPLOYMENT_GUIDE.md: Auto-enrollment deployment method, manual enrollment, config options

## Code Changes
- [x] src/config/loader.rs: Cert validation (CertStatus enum, validate_certs function)
- [x] src/config/loader.rs: EnrollmentConfig.manager_url changed to Option<String>
- [x] src/config/loader.rs: cert_renewal_threshold_days and polling_token fields added
- [x] src/config/loader.rs: save_polling_token() and clear_polling_token() methods
- [x] src/main.rs: Auto-enrollment path when certs invalid + URL configured
- [x] src/main.rs: --enroll exits after completion (no fall-through to server startup)
- [x] src/main.rs: --renew-certs flag for manual cert renewal
- [x] src/main.rs: SO_REUSEADDR on TcpListener::bind (socket2 crate)
- [x] src/main.rs: Move "Listening on" log after actual bind
- [x] src/main.rs: Exit code strategy (0=clean, 1=error, 2=enrollment in progress)
- [x] src/enroll/client.rs: HTTP 409 (Conflict) handling for host already exists
- [x] src/enroll/mod.rs: Polling token resume from persisted config
- [x] src/enroll/mod.rs: Handle ENROLLMENT_CONFLICT gracefully
- [x] configs/linux-patch-api.service: RestartSec=10s, StartLimitBurst=5, StartLimitIntervalSec=300
- [x] debian/postinst: Check for certs and enrollment URL, print guidance

## Build & Test
- [x] cargo check passes
- [x] cargo test passes (107 unit + 7 e2e + 11 integration)

## Remaining
- [ ] Build release package
- [ ] Test auto-enrollment on a clean host
- [ ] Test --enroll exits without starting server
- [ ] Test --renew-certs flag
- [ ] Test cert validation (missing, corrupt, expired, key mismatch, untrusted)
- [ ] Test SO_REUSEADDR (restart after crash)
- [ ] Test systemd exit code behavior
- [ ] Deploy to linux-patch-manager-dev for integration testing
