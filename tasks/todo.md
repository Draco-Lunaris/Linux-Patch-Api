# Self-Update Feature Review Plan

## Review Scope
Feature/self-update branch vs v1.4.3 tag — 14 files changed, +2154/-128 lines

## Priority Files Reviewed
1. ✅ src/api/handlers/system.rs — update_self handler, get_self_update_status, routes
2. ✅ src/packages/mod.rs — constants, types, marker/request functions, validate_version_string
3. ✅ src/jobs/manager.rs — SelfUpdate variant
4. ✅ configs/self-update.sh — multi-pkg-mgr upgrade script
5. ✅ configs/linux-patch-api-update.service — systemd oneshot unit
6. ✅ debian/postinst — upgrade-aware packaging
7. ✅ scripts/build-package.sh — ships new files
8. ✅ tests/e2e/test_self_update.sh — E2E harness

## Supporting Files Reviewed
- ✅ configs/linux-patch-api.install (Arch Linux hooks)
- ✅ configs/linux-patch-api.post-install (Alpine hooks)
- ✅ linux-patch-api.spec (RPM spec)

## Review Dimensions
- [ ] Security (shell injection, path traversal, privilege escalation, input validation)
- [ ] Correctness (race conditions, error handling, edge cases, ordering)
- [ ] Performance (resource leaks, blocking, concurrency)
- [ ] Quality (consistency, documentation, test coverage, DRY)
- [ ] Dependencies (no new deps in this diff)

## Preliminary Findings
1. Inconsistent restart behavior across package formats
2. systemd unit missing TimeoutStartSec and hardening
3. Marker file heredoc JSON injection risk
4. MAX_RESTART_DELAY_SECONDS unused constant
5. No job tracking for self-update
6. systemctl --no-block only confirms queueing
7. Shell script python3 -c pattern fragile
8. Missing network dependency in systemd unit
