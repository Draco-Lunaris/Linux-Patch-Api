# Issue #2: Package Cache Refresh - Spec Document

**Version:** 1.1.17  
**Date:** 2026-05-27  
**Status:** Approved  
**GitHub Issue:** https://github.com/Draco-Lunaris/Linux-Patch-Api/issues

---

## Problem Statement

On 2026-05-27, a managed host (Ubuntu 24.04.2 LTS, agent v1.1.16) had 11 pending patches but ALL `patch_apply` jobs failed with 404 errors. Root cause: stale `apt` package cache referencing superseded versions no longer in upstream repos.

**Impact:** 700/757 total jobs failed (92.5% failure rate) across all managed hosts.

---

## Requirements (from Issue #2)

| # | Requirement | Priority | Description |
|---|-------------|----------|-------------|
| 1 | Pre-Upgrade Cache Refresh | **MUST** | Run package index update before every `patch_apply` operation |
| 2 | Regular Interval Cache Refresh | **MUST** | Refresh package index on each health check (manager-triggered) |
| 3 | 404/Fetch Error Handling | **SHOULD** | Auto-retry with cache refresh on 404 errors, then report failure |
| 4 | Stale Cache Detection | **SHOULD** | Track `last_cache_update` timestamp; include in health response |

---

## Architecture Decisions (Kelly-Approved)

1. **New module:** `src/packages/cache.rs` for dedicated cache management
2. **Health check integration:** Cache refresh triggered by health check from manager
3. **Force cache refresh before apply:** Always on, NOT configurable
4. **Cache interval:** Controlled by manager health check frequency, not agent config
5. **Health check reports `last_cache_update`:** If cache refresh fails, health check returns degraded
6. **OS detection:** Already exists via compile-time backend selection
7. **Version bump:** 1.1.17
8. **Health check failure mode:** HTTP 200 with `status: "degraded"` (not 503)
9. **Cache refresh timeout:** 120 seconds
10. **404 retry count:** Hardcoded 1 retry (not configurable)
11. **Cache state persistence:** State file at `/var/lib/linux_patch_api/state/cache.json`; in-memory otherwise

---

## Design Specification

### 1. New Module: `src/packages/cache.rs`

#### `PackageCacheStatus` struct

```rust
pub struct PackageCacheStatus {
    pub last_update: Option<DateTime<Utc>>,
    pub last_update_success: bool,
    pub last_update_error: Option<String>,
}
```

#### `PackageCacheRefresher` trait

```rust
pub trait PackageCacheRefresher: Send + Sync {
    /// Refresh the package index (apt update, dnf check-update, etc.)
    fn refresh_cache(&self) -> Result<()>;
    
    /// Get the current cache status
    fn cache_status(&self) -> PackageCacheStatus;
    
    /// Check if cache is stale (older than threshold)
    fn is_cache_stale(&self, threshold: Duration) -> bool;
}
```

#### Per-Backend Refresh Commands

| Backend | Refresh Command | Notes |
|---------|----------------|-------|
| AptBackend | `apt-get update` | Full index refresh |
| DnfBackend | `dnf check-update --refresh` | Force metadata refresh |
| YumBackend | `yum makecache` | Rebuild metadata cache |
| ApkBackend | `apk update` | Update repository index |
| PacmanBackend | `pacman -Sy` | Sync databases (with caution note) |

### 2. Health Check Enhancement: `src/api/handlers/system.rs`

#### New `HealthData` response

```rust
pub struct HealthData {
    pub status: String,           // "healthy" or "degraded"
    pub uptime_seconds: u64,
    pub version: String,
    pub last_cache_update: Option<String>,  // RFC3339 timestamp
    pub cache_status: String,     // "fresh", "stale", "unknown", "failed"
}
```

#### Health check flow

```
GET /health or GET /api/v1/health
  │
  ├─ Read uptime (existing)
  ├─ Read version (existing)
  ├─ Call backend.cache_status() → PackageCacheStatus
  │
  ├─ If cache is stale (>4 hours) OR never refreshed:
  │   ├─ Call backend.refresh_cache()  (120s timeout)
  │   ├─ If success: last_cache_update = now, cache_status = "fresh"
  │   └─ If failure: status = "degraded", cache_status = "failed"
  │
  └─ Return HealthData (HTTP 200 always)
```

**Key rule:** If cache refresh is attempted and fails, health check returns HTTP 200 with `status: "degraded"`. The manager decides how to handle degraded status.

### 3. Pre-Apply Cache Refresh: `src/api/handlers/patches.rs`

#### Patch apply flow change

```
POST /api/v1/patches/apply
  │
  ├─ Create job (existing)
  ├─ Return 202 Accepted (existing)
  │
  └─ Background task:
      ├─ job_manager.update_job(Running, 0%, "Refreshing package index...")
      ├─ backend.refresh_package_cache()  ← NEW: Always runs before apply (120s timeout)
      │   ├─ If failure: job_manager.fail_job("Package cache refresh failed: ...")
      │   └─ If success: continue
      ├─ job_manager.update_job(Running, 10%, "Cache refreshed, applying patches...")
      ├─ backend.apply_patches(packages)  (existing)
      └─ ... (existing completion flow)
```

**Key rule:** Cache refresh before apply is MANDATORY and NOT configurable. If it fails, the patch_apply job fails immediately with a clear error message.

### 4. 404/Fetch Error Retry Logic

#### In `src/packages/cache.rs`

```rust
/// Execute a patch operation with automatic cache refresh on 404/fetch errors
/// Hardcoded 1 retry after cache refresh on fetch errors.
pub fn apply_with_cache_retry<F>(
    backend: &dyn PackageManagerBackend,
    apply_fn: F,
) -> Result<()>
where
    F: Fn() -> Result<()>,
{
    match apply_fn() {
        Ok(()) => Ok(()),
        Err(e) if is_fetch_error(&e) => {
            // Refresh cache and retry once
            backend.refresh_package_cache()?;
            apply_fn()
        }
        Err(e) => Err(e),
    }
}

/// Check if error is a fetch/404 error that warrants cache refresh retry
fn is_fetch_error(error: &anyhow::Error) -> bool {
    let msg = error.to_string().to_lowercase();
    msg.contains("404") 
        || msg.contains("not found") 
        || msg.contains("failed to fetch")
        || msg.contains("unable to fetch")
}
```

**Retry policy:** Hardcoded 1 retry after cache refresh on 404/fetch errors. If retry also fails, report failure with specific error.

### 5. Stale Cache Detection

#### In `src/packages/cache.rs`

- Track `last_cache_update: Option<DateTime<Utc>>` in a thread-safe `Arc<Mutex<PackageCacheState>>`
- `is_cache_stale(threshold)` returns `true` if:
  - `last_cache_update` is `None` (never refreshed)
  - `last_cache_update` is older than threshold (default: 4 hours)
- Used by health check to decide whether to trigger refresh
- Used by patch_apply to log warning (but still force-refresh regardless)

### 6. PackageManagerBackend Trait Extension

```rust
pub trait PackageManagerBackend: Send + Sync {
    // ... existing methods ...
    
    /// NEW: Refresh the local package index
    fn refresh_package_cache(&self) -> Result<()>;
    
    /// NEW: Get the last cache update timestamp
    fn last_cache_update(&self) -> Option<DateTime<Utc>>;
}
```

Each backend implements `refresh_package_cache()` using its OS-specific command.

### 7. Cache State Persistence

The `last_cache_update` timestamp persists to disk at `/var/lib/linux_patch_api/state/cache.json`:

```json
{
  "last_cache_update": "2026-05-27T13:00:00Z",
  "last_update_success": true
}
```

- Written after each successful or failed cache refresh
- Read on service startup to initialize in-memory state
- If file is missing or corrupt, treated as never-refreshed (triggers refresh on first health check)
- File permissions: 644 (readable by manager for diagnostics)

### 8. Configuration Changes

**No new configuration parameters.** Per Kelly's decision:
- Cache refresh before apply is always-on (not configurable)
- Cache refresh interval is controlled by manager health check frequency
- Stale threshold is hardcoded at 4 hours
- Cache refresh timeout is hardcoded at 120 seconds

---

## File Changes Summary

| File | Change |
|------|--------|
| `src/packages/cache.rs` | **NEW** - PackageCacheStatus, PackageCacheRefresher, retry logic, stale detection, state persistence |
| `src/packages/mod.rs` | Add `mod cache;`, implement `refresh_package_cache()` and `last_cache_update()` on each backend |
| `src/api/handlers/system.rs` | Enhance health_check to include cache_status and last_cache_update, trigger refresh if stale |
| `src/api/handlers/patches.rs` | Add cache refresh before apply_patches in job background task |
| `src/api/handlers/mod.rs` | Update HealthData type with new fields |
| `Cargo.toml` | Bump version to 1.1.17 |
| `ARCHITECTURE.md` | Update health check section, add cache refresh flow |
| `REQUIREMENTS.md` | Add FR-007 for package cache refresh requirements |
| `/var/lib/linux_patch_api/state/cache.json` | **NEW** - Persistent cache state file |

---

## Implementation Order

1. **`src/packages/cache.rs`** - Core cache types, stale detection, state persistence
2. **Backend implementations** - Add `refresh_package_cache()` and `last_cache_update()` to each backend in `mod.rs`
3. **Health check enhancement** - Update `system.rs` to include cache status and trigger refresh
4. **Pre-apply refresh** - Update `patches.rs` job flow to refresh before apply
5. **404 retry logic** - Add retry wrapper in `cache.rs`
6. **Version bump** - Update `Cargo.toml` to 1.1.17
7. **Documentation** - Update `ARCHITECTURE.md` and `REQUIREMENTS.md`
8. **State persistence** - Implement cache.json read/write in `cache.rs`
9. **Tests** - Unit tests for cache logic, integration tests for health check

---

## Test Plan

### Unit Tests
- `cache_status()` returns correct initial state
- `is_cache_stale()` returns true for never-refreshed and >4h old
- `is_fetch_error()` correctly identifies 404/fetch errors
- `apply_with_cache_retry()` retries once on 404 then fails on second attempt
- Each backend's `refresh_package_cache()` calls correct command
- State file read/write works correctly
- Corrupt/missing state file handled gracefully

### Integration Tests
- `GET /health` returns `last_cache_update` and `cache_status` fields
- `GET /health` triggers cache refresh when stale
- `GET /health` returns `"degraded"` when cache refresh fails (HTTP 200)
- `POST /api/v1/patches/apply` refreshes cache before applying
- `POST /api/v1/patches/apply` fails job when cache refresh fails
- 404 retry logic works end-to-end
- State persists across service restart

---

*Following kiro spec-driven development standards*
