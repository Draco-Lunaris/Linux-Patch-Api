# Issue #2 Implementation Todo

**Spec:** tasks/issue-2-package-cache-refresh.md
**Version:** 1.1.17
**Status:** Complete - PR #3 Open

---

## Implementation Checklist

- [x] 1. Create `src/packages/cache.rs` - Core cache types, stale detection, state persistence, 404 retry logic
- [x] 2. Add `mod cache;` to `src/packages/mod.rs`
- [x] 3. Implement `refresh_package_cache()` on AptBackend
- [x] 4. Implement `refresh_package_cache()` on DnfBackend
- [x] 5. Implement `refresh_package_cache()` on YumBackend
- [x] 6. Implement `refresh_package_cache()` on ApkBackend
- [x] 7. Implement `refresh_package_cache()` on PacmanBackend
- [x] 8. Implement `last_cache_update()` on all backends (shared state)
- [x] 9. Add `refresh_package_cache` and `last_cache_update` to PackageManagerBackend trait
- [x] 10. Enhance health check in `src/api/handlers/system.rs` - add cache status, trigger refresh
- [x] 11. Update HealthData struct with `last_cache_update` and `cache_status` fields
- [x] 12. Add pre-apply cache refresh in `src/api/handlers/patches.rs`
- [x] 13. Bump version in `Cargo.toml` to 1.1.17
- [x] 14. Update `ARCHITECTURE.md` with cache refresh flow
- [x] 15. Update `REQUIREMENTS.md` with FR-007
- [x] 16. Implement state file persistence (cache.json read/write)
- [x] 17. Write unit tests for cache module
- [x] 18. Build and verify compilation
- [x] 19. Commit and push to fix/package-cache-refresh branch
- [x] 20. Create PR and reference Issue #2

## Review

**PR:** https://gitea-lxc.moon-dragon.us/git-echo/linux_patch_api/pulls/3
**Branch:** fix/package-cache-refresh
**Commit:** cf3d597
**Files Changed:** 12 files, 944 insertions, 15 deletions

### Issue Resolution

All 4 requirements from Issue #2 addressed:
1. ✅ Pre-Upgrade Cache Refresh (MUST) - Mandatory cache refresh before every patch_apply
2. ✅ Regular Interval Cache Refresh (MUST) - Cache refresh triggered on health check when stale (>4h)
3. ✅ 404/Fetch Error Handling (SHOULD) - Auto-retry with cache refresh on fetch errors (1 retry)
4. ✅ Stale Cache Detection (SHOULD) - Tracks last_cache_update, reports in health response

### Known Issue
- SSH key `git_echo_id_ed25519` was rejected by Gitea on port 2222 - pushed via HTTPS + API token instead
- Root cause: Key fingerprint SHA256:W1BK9fCA53/or7iJkONbFSf3KJ6+oiAggPgisZNPhsc not registered in git-echo Gitea account
- Needs investigation: SSH key may need re-registration in Gitea
