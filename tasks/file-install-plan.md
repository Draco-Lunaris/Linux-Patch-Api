# File Install & Self-Upgrade Feature Plan

## Overview
Add file-upload package installation to the API, enabling deployment of `.deb`/`.rpm`/`.apk`/`.tar.zst` packages from arbitrary sources. Primary use case: the Linux Patch Manager deploying linux-patch-api upgrades to all managed clients.

## Resolved Decisions

| # | Question | Decision |
|---|----------|----------|
| 1 | Restart approach | Explicit `POST /api/v1/system/restart` endpoint. Manager orchestrates automatically (install → poll → restart → reconnect). No manual user step. |
| 2 | File size limit | 1 GB |
| 3 | Staging directory | Configurable in config.yaml, defaults to `/tmp`. Falls back naturally if /tmp is tmpfs and too small (admin overrides in config). |
| 4 | Build LXC resources | 4 cores / 4GB RAM / 100GB disk |

## Architecture

### File Install Endpoint
- **Endpoint:** `POST /api/v1/packages/install-file` (multipart upload)
- **Flow:** Upload → Stage → Validate → Install → Cleanup → Return job result
- **Staging dir:** `/tmp` by default, configurable via `file_staging_dir` in config.yaml
- **File validation:** Extension allowlist (`.deb`, `.rpm`, `.apk`, `.tar.zst`), 1GB size limit
- **Config gate:** `allow_file_install: true` in config.yaml (default: false for security)

### Self-Upgrade Design
Manager controls the full sequence automatically — no manual user step:
1. `POST /api/v1/packages/install-file` → upload and install the new .deb
2. Poll job until success
3. `POST /api/v1/system/restart` → daemon drains connections, then `systemctl restart linux-patch-api`
4. Manager expects connection loss, reconnects with backoff
5. Daemon comes back up on new version

postinst stays as-is — does NOT auto-restart on upgrade. The restart is a separate, explicit API call the manager controls.

### Backend Commands

| Backend | Command | Dep Resolution |
|---------|---------|---------------|
| APT | `apt install -y /path/to/file.deb` | ✅ Automatic |
| DNF | `dnf install -y /path/to/file.rpm` | ✅ Automatic |
| YUM | `yum install -y /path/to/file.rpm` | ✅ Automatic |
| APK | `apk add --allow-untrusted /path/to/file.apk` | ✅ Automatic |
| Pacman | `pacman -U --noconfirm /path/to/file.tar.zst` | ✅ Automatic |

### Build LXC
- **Hostname:** `lpa-build.moon-dragon.us`
- **OS:** Ubuntu 24.04 (matches primary target)
- **Resources:** 4 cores, 4096 MB RAM, 100 GB disk
- **Purpose:** Local builds, package testing, integration testing
- **Software:** Rust toolchain, dpkg-deb, rpm tools, dev scripts

## Implementation Plan

### Phase 1: Infrastructure & Spec ✅
- [x] Create feature branch `feature/file-install` from master
- [x] Update SPEC.md with file install endpoint design (request/response format, config gate, validation rules)
- [x] Update SPEC.md with system restart endpoint design
- [x] Update SPEC.md with `file_install.enabled` and `file_install.staging_dir` config options
- [x] Update THREAT_MODEL_VALIDATION.md with file install security considerations
- [x] Update DEPLOYMENT_GUIDE.md with file install configuration
- [x] Create build LXC `lpa-build` (4 cores / 4GB / 100GB / Ubuntu 24.04) — VMID 218, IP 192.168.3.140
- [x] Verify Rust build works in the LXC — Rust 1.96.0, cargo check passes
- [x] Set up SSH access and git clone in LXC — repo cloned, feature branch checked out

### Phase 2: Core File Install ✅
- [x] Add `file_install.enabled` and `file_install.staging_dir` config options to `src/config/loader.rs`
- [x] Add file staging logic (save to staging dir, validate extension, enforce size limit)
- [x] Add `install_file` method to `PackageManagerBackend` trait
- [x] Implement `install_file` for all 5 backends (apt, apk, dnf, yum, pacman)
- [x] Add `POST /api/v1/packages/install-file` endpoint (multipart upload)
- [x] Add route in `src/api/routes.rs`
- [x] Add file cleanup on success/failure

### Phase 3: Self-Upgrade Support ✅
- [x] Add `POST /api/v1/system/restart` endpoint
- [x] Implement graceful restart: 2s delay via tokio::spawn, then `systemctl restart linux-patch-api`
- [x] Verify postinst does NOT auto-restart on upgrade (current behavior preserved)

### Phase 4: Testing
- [ ] Unit tests for file validation (extension allowlist, size limits)
- [ ] Unit tests for staging dir config
- [ ] Integration test: file upload → install → verify package present
- [ ] Integration test: self-upgrade (install new .deb → restart → verify new version)
- [ ] Test on build LXC with real .deb packages
- [ ] Test error paths: invalid extension, oversized file, missing deps, config gate disabled

### Phase 5: Final Documentation
- [ ] Verify SPEC.md matches implemented behavior
- [ ] Verify DEPLOYMENT_GUIDE.md matches implemented config options
- [ ] Verify THREAT_MODEL_VALIDATION.md covers all security considerations
- [ ] Update BUILD_PACKAGES.md if build process changes

## Security Considerations
- File installs bypass repo GPG signing — must be explicitly enabled via `allow_file_install: true`
- Upload endpoint needs rate limiting (reuse existing rate limiter)
- File extension allowlist prevents arbitrary file upload
- 1GB size limit prevents disk exhaustion
- Staging dir should be root-owned, mode 0700
- Self-restart endpoint requires mTLS auth (already enforced)
- Manager must handle connection loss gracefully during restart
- `/tmp` staging: if tmpfs-backed, large files may fail — admin can override via config
