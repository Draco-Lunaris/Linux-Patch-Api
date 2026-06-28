# Manager-Side Gap Analysis: Self-Update Feature

**Date:** 2026-06-27
**Manager Project:** /a0/usr/workdir/linux_patch_manager
**Agent Project:** /a0/usr/projects/linux_patch_api (COMPLETE — 19 gaps fixed, E2E 26/30 passing)

---

## What's Already Done in the Manager

| Component | Status | Details |
|-----------|--------|---------|
| pm-agent-client | ✅ DONE | SelfUpdateRequest, SelfUpdateResponse, SelfUpdateStatus types + self_update() and self_update_status() methods |
| pm-worker | ✅ DONE | execute_self_upgrade_host_job dispatch + poll_self_upgrade_host reconciliation + reconnect_confirm_self_upgrade |
| pm-web (upgrades route) | ✅ DONE | POST /api/v1/upgrades/trigger — creates self-upgrade jobs, RBAC enforced (operator+) |
| pm-core (models) | ✅ DONE | PkiBundle struct (ca_crt, ca_chain, server_crt, server_key, crl_pem) + ApprovedEntry |
| pm-ca | ✅ DONE | issue_server_cert, issue_client_cert, generate_crl functions |
| Frontend | ✅ DONE | HostDetailPage has self-update trigger UI |

## What's Missing — Manager-Side Gaps

### Gap M1: repo_config Not in PkiBundle or ApprovedEntry (P0 — BLOCKER)

**Location:** `crates/pm-core/src/models.rs` — PkiBundle struct (line 191)

Current PkiBundle:
```rust
pub struct PkiBundle {
    pub ca_crt: String,
    pub ca_chain: String,
    pub server_crt: String,
    pub server_key: String,
    pub crl_pem: String,
}
```

**Missing:** `repo_config: Option<RepoConfig>` field. The agent expects this in the enrollment approval response per SPEC §266-272.

**Fix:**
1. Add `RepoConfig` struct to pm-core models:
```rust
pub struct RepoConfig {
    pub gpg_public_key: String,
    pub sources_config: String,
    pub distro_id: String,
    pub keyring_path: String,
}
```
2. Add `#[serde(default)] pub repo_config: Option<RepoConfig>` to PkiBundle
3. Update ApprovedEntry to carry the extended PkiBundle
4. Update enrollment approval handler to include repo_config when building the PkiBundle
5. Generate/load GPG public key from manager's CA or separate GPG key

### Gap M2: GET /api/v1/pki/repo-config Endpoint Not Implemented (P1)

**Location:** `crates/pm-web/src/routes/pki.rs` — currently only has `GET /pki/crl.pem`

**Missing:** `GET /api/v1/pki/repo-config` endpoint for fallback fetch when agent enrolled before v2.0.0.

**Fix:**
1. Add route: `.route("/pki/repo-config", get(get_repo_config))`
2. Handler returns RepoConfig JSON (gpg_public_key, sources_config, distro_id, keyring_path)
3. Determine distro_id from agent's enrollment record (os_details field)
4. Generate sources_config based on distro_id (apt sources.list line, dnf repo file, etc.)
5. Read GPG public key from manager's GPG keyring

### Gap M3: ServeDir for Package Repository Not Configured (P1)

**Location:** `crates/pm-web/src/lib.rs` — build_router() function (line 236)

Current ServeDir:
```rust
.fallback_service(
    ServeDir::new(&static_dir)
        .append_index_html_on_directories(true)
        .fallback(ServeFile::new(format!("{}/index.html", static_dir))),
)
```

**Missing:** ServeDir for `/apt/`, `/dnf/`, `/apk/`, `/pacman/` package repository paths.

**Fix:**
1. Add `tower-http` `ServeDir` for repo paths:
```rust
let repo_base = std::env::var("LPA_REPO_DIR")
    .unwrap_or_else(|_| "/var/www/lpa-repo".to_string());

Router::new()
    .nest_service("/apt", ServeDir::new(format!("{repo_base}/apt")))
    .nest_service("/dnf", ServeDir::new(format!("{repo_base}/dnf")))
    .nest_service("/apk", ServeDir::new(format!("{repo_base}/apk")))
    .nest_service("/pacman", ServeDir::new(format!("{repo_base}/pacman")))
```
2. Mount alongside existing routes (public, GPG-authenticated — not mTLS)
3. Repo files are static — no dynamic API needed

### Gap M4: GPG Key Management Not Set Up (P1)

**Location:** Manager host (lpm.moon-dragon.us)

**Current state:** No LPA GPG key exists on manager host (confirmed via SSH check).

**Missing:**
1. GPG signing key generation
2. GPG key storage in Vaultwarden + CI secrets
3. GPG key distribution via enrollment bundle
4. Package signing in CI pipeline

**Fix:**
1. Generate GPG key on manager host:
```bash
gpg --batch --gen-key <<EOF
%no-protection
Key-Type: RSA
Key-Length: 4096
Key-Usage: sign
Name-Real: Linux Patch API Repo
Name-Email: lpa-repo@moon-dragon.us
Expire-Date: 2y
%commit
EOF
```
2. Export public key for distribution via enrollment
3. Export private key for CI signing (store in Vaultwarden + GitHub/Gitea secrets)
4. Add `LPA_REPO_GPG_KEY` secret to CI

### Gap M5: Repo Directory Infrastructure Not Set Up (P1)

**Location:** Manager host filesystem

**Current state:** No `/var/www/lpa-repo/` directory exists (confirmed via SSH check).

**Missing:**
1. Repo directory structure: `/var/www/lpa-repo/{apt,dnf,apk,pacman}/`
2. reprepro config for apt (`/var/www/lpa-repo/apt/conf/distributions`)
3. CI pipeline to push signed packages to repo
4. Repo metadata regeneration (createrepo_c for dnf, apk index for apk, repo-add for pacman)

**Fix:**
1. Create directory structure on manager host
2. Install reprepro, createrepo_c, abuild tools
3. Configure CI `publish-to-manager-repo` job (workflow already exists in agent repo)
4. Test with a dry-run build

### Gap M6: Enrollment Approval Handler Needs repo_config Population (P1)

**Location:** `crates/pm-web/src/routes/enrollment.rs` (file not readable via text_editor — likely in a submodule or generated)

**Missing:** When admin approves an enrollment, the handler builds a PkiBundle and stores it in ApprovedEntry. The handler needs to:
1. Detect the agent's distro from os_details in enrollment request
2. Generate distro-specific sources_config (apt sources.list, dnf repo file, etc.)
3. Read the GPG public key from manager's keyring
4. Determine keyring_path based on distro (e.g., `/etc/apt/keyrings/lpa-repo.gpg` for apt)
5. Include repo_config in the PkiBundle before storing in ApprovedEntry

### Gap M7: Frontend Self-Update UI May Need Updates (P2)

**Location:** `frontend/src/pages/HostDetailPage.tsx` (line 733 — triggerUpgrade call exists)

**Current state:** Frontend has self-update trigger UI.

**Missing:** May need updates to show:
- repo_config status (whether agent has repo configured)
- Self-update version selection from manager-hosted repo (not GitHub)
- Migration status for pre-v2.0.0 agents

### Gap M8: pm-worker Polling Reconciliation May Need Updates (P2)

**Location:** `crates/pm-worker/src/job_executor.rs` — poll_self_upgrade_host (line 971)

**Current state:** Worker polls agent health and version after self-update.

**Missing:** May need updates to:
- Handle repo_config absence (agent enrolled before v2.0.0)
- Fallback to GET /pki/repo-config if agent reports no repo
- Track migration status per host

### Gap M9: No Repo Config in Manager Config (P3)

**Location:** `config/config.example.toml`

**Missing:** Configuration for:
- `LPA_REPO_DIR` environment variable (default: `/var/www/lpa-repo`)
- GPG key path in security config
- Repo URL base for sources_config generation (e.g., `https://lpm.moon-dragon.us`)

### Gap M10: No GPG Key Rotation Procedure (P3)

**Missing:** Operational documentation for:
- GPG key generation
- Key storage in Vaultwarden
- CI secret configuration
- Key rotation procedure (2-year cycle)
- Key revocation procedure

---

## Summary: Priority-Ordered Manager-Side Action Items

| Priority | Gap | Action |
|----------|-----|--------|
| **P0** | M1: repo_config not in PkiBundle | Add RepoConfig struct + field to PkiBundle + ApprovedEntry |
| **P1** | M2: GET /pki/repo-config endpoint | Add fallback fetch endpoint in pki.rs route |
| **P1** | M3: ServeDir for repo files | Add ServeDir for /apt/, /dnf/, /apk/, /pacman/ in build_router() |
| **P1** | M4: GPG key management | Generate GPG key, store in Vaultwarden + CI secrets |
| **P1** | M5: Repo directory infrastructure | Create /var/www/lpa-repo/ with reprepro/createrepo_c |
| **P1** | M6: Enrollment approval handler | Populate repo_config when building PkiBundle on approval |
| **P2** | M7: Frontend self-update UI | Add repo_config status, version selection from repo |
| **P2** | M8: Worker polling reconciliation | Handle repo_config absence, fallback fetch |
| **P3** | M9: Manager config | Add LPA_REPO_DIR, GPG key path, repo URL base |
| **P3** | M10: GPG key rotation procedure | Document generation, storage, rotation, revocation |

## What's NOT Needed (Already Implemented)

- ✅ pm-agent-client: SelfUpdateRequest/Response/Status types + methods
- ✅ pm-worker: execute_self_upgrade_host_job + poll_self_upgrade_host + reconnect_confirm
- ✅ pm-web: POST /upgrades/trigger with RBAC
- ✅ pm-core: PkiBundle + ApprovedEntry base structs
- ✅ pm-ca: Cert issuing functions
- ✅ Frontend: Self-update trigger UI on HostDetailPage

## Estimated Effort

| Gap | Effort | Description |
|-----|--------|-------------|
| M1 | Medium | Add RepoConfig struct, extend PkiBundle, update approval handler |
| M2 | Small | Add one route + handler in pki.rs |
| M3 | Small | Add 4 ServeDir entries in build_router() (~15 lines) |
| M4 | Small | Generate GPG key, store in Vaultwarden + CI secrets |
| M5 | Medium | Create repo dirs, install reprepro/createrepo_c, configure CI |
| M6 | Medium | Update enrollment approval to detect distro + build sources_config |
| M7 | Small | Frontend updates for repo_config status display |
| M8 | Small | Worker polling updates for repo_config fallback |
| M9 | Small | Config file additions |
| M10 | Small | Documentation |

### Gap M11: Package Collection/Sync from GitHub to Manager (P1)

**The core question Kelly raised:** How do packages actually get INTO the manager repo?

The CI `publish-to-manager-repo` job pushes NEW builds going forward, but:
- Existing GitHub Releases packages need to be migrated to the manager repo
- A sync/mirror mechanism is needed for backward compatibility
- Manual upload path needed for air-gapped environments

**Missing:**
1. GitHub Releases API pull mechanism — fetch all existing release assets from the GitHub repo
2. Import pipeline — convert GitHub assets into reprepro/createrepo_c format
3. Mirror sync — keep manager repo in sync with GitHub during migration window
4. Manual upload endpoint — allow admin to upload .deb/.rpm/.apk/.pkg.tar.zst files directly

**Fix:**
1. Add a `pm-worker` background task: `package_sync_worker` that:
   - Fetches release assets from GitHub API (`GET /repos/{owner}/{repo}/releases`)
   - Downloads each asset
   - Imports into reprepro (apt), createrepo_c (dnf), apk index (apk), repo-add (pacman)
   - Signs repo metadata with GPG key
   - Runs on schedule (daily) or on-demand via API trigger
2. Add `POST /api/v1/admin/repo/sync` endpoint to trigger sync manually
3. Add `GET /api/v1/admin/repo/status` endpoint to report sync status
4. Track sync progress in database (last_sync_at, packages_synced, errors)

### Gap M12: Manager UI File Manager (P2)

**Kelly's requirement:** Admins need a web interface to manage the repo.

**Missing:** Manager web UI for:
1. Browse repo contents — list available packages and versions per distro
2. Upload packages manually — drag-and-drop or file picker for .deb/.rpm/.apk/.pkg.tar.zst
3. View repo health — GPG signing status, metadata freshness, package count
4. Trigger repo metadata regeneration — force reprepro/createrepo_c refresh
5. View sync status — last GitHub sync, packages pending, sync errors
6. Delete old versions — prune stale packages from repo

**Fix:**
1. Add `frontend/src/pages/RepoManagementPage.tsx` with:
   - Package list table (distro, version, filename, size, signed, upload date)
   - Upload form (file picker + distro selection + version input)
   - Repo health dashboard (signing status, metadata age, total packages)
   - Sync trigger button (calls POST /api/v1/admin/repo/sync)
   - Metadata regeneration button (calls POST /api/v1/admin/repo/refresh-metadata)
2. Add `GET /api/v1/admin/repo/packages` — list all packages in repo
3. Add `POST /api/v1/admin/repo/upload` — accept multipart file upload
4. Add `DELETE /api/v1/admin/repo/packages/{id}` — remove specific package version
5. Add `POST /api/v1/admin/repo/refresh-metadata` — regenerate repo metadata
6. Add route to navigation sidebar (admin-only)

### Gap M13: Package Sync Worker (P1)

**Kelly's requirement:** A background task on the manager that pulls from GitHub and populates the repo.

**Missing:** `pm-worker` background task that:
1. Polls GitHub Releases API for new releases
2. Downloads release assets matching distro patterns
3. Imports into reprepro/createrepo_c/apk index/repo-add
4. Signs repo metadata with manager's GPG key
5. Updates `available_versions` table in database
6. Runs on schedule (configurable interval, default: hourly)
7. Reports sync status to manager UI
8. Handles rate limiting (GitHub API: 60/hr unauthenticated, 5000/hr with token)

**Fix:**
1. Add `crates/pm-worker/src/package_sync_worker.rs`:
   - `async fn run_package_sync(config, db, ca) -> Result<()>`
   - Fetch releases: `GET https://api.github.com/repos/Draco-Lunaris/Linux-Patch-Api/releases`
   - Download assets matching patterns: `*_u2404_amd64.deb`, `*_debian12_amd64.deb`, `*.fc*.x86_64.rpm`, etc.
   - Import to reprepro: `reprepro -b /var/www/lpa-repo/apt includedeb {codename} {file}`
   - Import to dnf: `cp {file} /var/www/lpa-repo/dnf/{el}/Packages/ && createrepo_c --update /var/www/lpa-repo/dnf/{el}/`
   - Sign: `gpg --detach-sign --armor /var/www/lpa-repo/{distro}/{path}/repomd.xml` (or Release for apt)
   - Update DB: `INSERT INTO available_versions (version, distro, filename, size, signed, synced_at) VALUES (...)`
2. Add to pm-worker main loop alongside existing pollers
3. Add config: `[package_sync] enabled = true, interval_secs = 3600, github_token = ""`
4. Add `POST /api/v1/admin/repo/sync` to trigger on-demand sync
5. Add `GET /api/v1/admin/repo/sync-status` for UI display

---

## Updated Summary: Priority-Ordered Manager-Side Action Items

| Priority | Gap | Action |
|----------|-----|--------|
| **P0** | M1: repo_config not in PkiBundle | Add RepoConfig struct + field to PkiBundle + ApprovedEntry |
| **P1** | M2: GET /pki/repo-config endpoint | Add fallback fetch endpoint in pki.rs route |
| **P1** | M3: ServeDir for repo files | Add ServeDir for /apt/, /dnf/, /apk/, /pacman/ in build_router() |
| **P1** | M4: GPG key management | Generate GPG key, store in Vaultwarden + CI secrets |
| **P1** | M5: Repo directory infrastructure | Create /var/www/lpa-repo/ with reprepro/createrepo_c |
| **P1** | M6: Enrollment approval handler | Populate repo_config when building PkiBundle on approval |
| **P1** | M11: Package collection/sync | GitHub Releases pull mechanism + import pipeline + manual upload |
| **P1** | M13: Package sync worker | pm-worker background task to pull from GitHub, import, sign |
| **P2** | M7: Frontend self-update UI | Add repo_config status, version selection from repo |
| **P2** | M8: Worker polling reconciliation | Handle repo_config absence, fallback fetch |
| **P2** | M12: Manager UI file manager | Browse, upload, health, regenerate, sync status, delete |
| **P3** | M9: Manager config | Add LPA_REPO_DIR, GPG key path, repo URL base, sync config |
| **P3** | M10: GPG key rotation docs | Document generation, storage, rotation, revocation |

## Updated Estimated Effort

| Gap | Effort | Description |
|-----|--------|-------------|
| M1 | Medium | Add RepoConfig struct, extend PkiBundle, update approval handler |
| M2 | Small | Add one route + handler in pki.rs |
| M3 | Small | Add 4 ServeDir entries in build_router() (~15 lines) |
| M4 | Small | Generate GPG key, store in Vaultwarden + CI secrets |
| M5 | Medium | Create repo dirs, install reprepro/createrepo_c, configure CI |
| M6 | Medium | Update enrollment approval to detect distro + build sources_config |
| M7 | Small | Frontend updates for repo_config status display |
| M8 | Small | Worker polling updates for repo_config fallback |
| M9 | Small | Config file additions |
| M10 | Small | Documentation |
| M11 | Large | GitHub API pull + import pipeline + DB tracking + manual upload endpoint |
| M12 | Large | Frontend repo management page + backend CRUD endpoints + file upload handling |
| M13 | Large | pm-worker background sync task + scheduling + GPG signing + DB updates |

**Total: 13 gaps, ~7-10 days of implementation work.**

**Key insight from Kelly:** The repo infrastructure is not just 'create directories.' It needs a full package lifecycle: collection (from GitHub or manual upload) → signing (GPG) → publishing (reprepro/createrepo_c) → browsing (UI) → managing (UI CRUD). This is 3 large components (M11-M13) that I initially missed."
