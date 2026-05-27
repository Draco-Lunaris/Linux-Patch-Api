# Issue #2 Implementation Todo

**Spec:** tasks/issue-2-package-cache-refresh.md
**Version:** 1.1.17
**Status:** In Progress

---

## Implementation Checklist

- [ ] 1. Create `src/packages/cache.rs` - Core cache types, stale detection, state persistence, 404 retry logic
- [ ] 2. Add `mod cache;` to `src/packages/mod.rs`
- [ ] 3. Implement `refresh_package_cache()` on AptBackend
- [ ] 4. Implement `refresh_package_cache()` on DnfBackend
- [ ] 5. Implement `refresh_package_cache()` on YumBackend
- [ ] 6. Implement `refresh_package_cache()` on ApkBackend
- [ ] 7. Implement `refresh_package_cache()` on PacmanBackend
- [ ] 8. Implement `last_cache_update()` on all backends (shared state)
- [ ] 9. Add `refresh_package_cache` and `last_cache_update` to PackageManagerBackend trait
- [ ] 10. Enhance health check in `src/api/handlers/system.rs` - add cache status, trigger refresh
- [ ] 11. Update HealthData struct with `last_cache_update` and `cache_status` fields
- [ ] 12. Add pre-apply cache refresh in `src/api/handlers/patches.rs`
- [ ] 13. Bump version in `Cargo.toml` to 1.1.17
- [ ] 14. Update `ARCHITECTURE.md` with cache refresh flow
- [ ] 15. Update `REQUIREMENTS.md` with FR-007
- [ ] 16. Implement state file persistence (cache.json read/write)
- [ ] 17. Write unit tests for cache module
- [ ] 18. Build and verify compilation
- [ ] 19. Commit and push to fix/package-cache-refresh branch
- [ ] 20. Create PR and reference Issue #2

## Review

_To be filled after implementation_
