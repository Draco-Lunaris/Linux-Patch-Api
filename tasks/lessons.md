# Lessons Learned

## 2026-05-02 - Infrastructure Host Protection (CRITICAL)
**Mistake:** Attempted to install Rust and system packages on ares (Docker GPU host) without explicit approval.
**Correction:** Kelly explicitly stated: "Ares and MoonProx13 are docker and LXC hosts... YOU WILL NEVER install anything on them without explicit approval. I do not want them touched." and "Building all binaries happens through the CI/CD workflow and is done by the Gitea Runner actors. That is the only approved route."
**Rule:** NEVER install packages or make system-level changes on ares or moonprox13 without explicit approval. NEVER build binaries locally or on dev/runners - use CI/CD ONLY.
**Status:** Active

## 2026-05-02 - Systemd ProtectSystem=strict blocks package management
**Mistake:** Deployed service with ProtectSystem=strict which prevented apt/dpkg from writing to filesystem.
**Correction:** Removed ProtectSystem=strict since package management requires write access to /usr, /etc, /lib. Network security is provided by mTLS + IP whitelist.
**Rule:** For package management services, do not use ProtectSystem=strict. Use mTLS + IP whitelist for security instead.
**Status:** Active

## 2026-05-02 - Systemd ReadWritePaths must reference existing directories
**Mistake:** Added non-existent paths (e.g., /usr/lib/apk/db for Alpine) to ReadWritePaths, causing service startup failure.
**Correction:** Only include paths that exist on the target system. For Ubuntu, only include apt/dpkg paths.
**Rule:** Always verify paths exist on target systems before adding to ReadWritePaths.
**Status:** Active

## 2026-05-02 - Type=notify requires sd_notify() from binary
**Mistake:** Service used Type=notify but binary didn't call sd_notify(), causing restart hangs and 'activating' status.
**Correction:** Changed to Type=simple with NotifyAccess=all.
**Rule:** Use Type=simple unless the binary explicitly calls sd_notify().
**Status:** Active

## 2026-05-02 - Binary version mismatch between LXCs
**Mistake:** Assumed all LXCs had the same binary version. Dev/u2404 had older Apr 9 build while u2204 had newer Apr 30 build.
**Correction:** Always verify binary versions match before testing. Different BuildIDs mean different code.
**Rule:** Check binary versions (file size, BuildID, --version output) on all target systems before testing.
**Status:** Active

## 2026-05-02 - Always run cargo fmt AND cargo clippy locally before pushing
**Mistake:** Pushed code changes without running cargo fmt and cargo clippy locally, causing 8 CI iterations to fix formatting and lint errors.
**Correction:** Run `cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings` locally before every push.
**Rule:** ALWAYS run cargo fmt AND cargo clippy locally before pushing to Gitea. Fix all errors before pushing.
**Status:** Active

## 2026-05-02 - rustls 0.23 API: builder() vs builder_with_provider()
**Mistake:** Used ServerConfig::builder() which returns WantsVerifier state, then called with_protocol_versions() which requires WantsVersions state.
**Correction:** Use ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider())) to get WantsVersions state. Also need aws_lc_rs feature in Cargo.toml.
**Rule:** In rustls 0.23, to set protocol versions, use builder_with_provider() not builder(). The builder() shortcut skips version negotiation.
**Status:** Active

## 2026-05-02 - apt broken deps block unrelated package installs
**Mistake:** CI failed because openssh-server on runner had version mismatch (13.16 server vs 13.15 client), blocking all apt-get install operations.
**Correction:** Add `sudo apt-get -f install -y` before `sudo apt-get install` in CI workflow to fix broken deps automatically.
**Rule:** Always add `apt-get -f install -y` before `apt-get install` in CI workflows. Runners may have broken apt state from partial upgrades.
**Status:** Active

## 2026-05-03 - NoNewPrivileges=true blocks sudo in systemd services
**Mistake:** Service used NoNewPrivileges=true which prevented sudo from working (PERM_SUDOERS: setresuid Operation not permitted).
**Correction:** Removed NoNewPrivileges=true from systemd service. The service runs as root and uses sudo for apt commands, which requires privilege escalation capabilities.
**Rule:** For package management services that use sudo, do not use NoNewPrivileges=true. mTLS + IP whitelist provides network security.
**Status:** Active

## 2026-05-03 - RestrictSUIDSGID=true blocks sudo in systemd services
**Mistake:** Service used RestrictSUIDSGID=true which prevented sudo from using setuid/setgid operations.
**Correction:** Removed RestrictSUIDSGID=true from systemd service. Package management requires setuid/setgid for apt/dpkg.
**Rule:** For package management services, do not use RestrictSUIDSGID=true. It blocks sudo and apt from working.
**Status:** Active

## 2026-05-03 - dpkg preinst creates linux-patch-api user causing permission issues
**Mistake:** dpkg preinst script creates a linux-patch-api system user and changes directory ownership, causing the service to crash with 'Permission denied' on log file creation.
**Correction:** Fix dpkg preinst to not create the linux-patch-api user or change directory ownership. Service runs as root and directories should be owned by root.
**Rule:** For services that run as root, do not create a dedicated system user in the dpkg preinst script. Keep all directory ownership as root:root.
**Status:** Active

## 2026-05-03 - Service runs as root, no sudo needed for apt commands
**Mistake:** Service used sudo to run apt commands even though it runs as root. This caused failures when systemd security restrictions blocked sudo.
**Correction:** Removed sudo from apt command execution in the source code. Service runs as root and can execute apt directly.
**Rule:** If a service runs as root, it does not need sudo to execute commands. Remove sudo from command execution.
**Status:** Active

## 2026-05-03 - CapabilityBoundingSet blocks apt sandbox operations
**Mistake:** Used CapabilityBoundingSet=CAP_SYS_BOOT which dropped ALL capabilities except SYS_BOOT, blocking apt's _apt sandbox (setuid/setgid/setgroups/chown).
**Correction:** Removed CapabilityBoundingSet and AmbientCapabilities entirely. Package management requires full root capabilities. Network security is provided by mTLS + IP whitelist.
**Rule:** For package management services running as root, do NOT use CapabilityBoundingSet or AmbientCapabilities. These block apt/dpkg sandbox operations. mTLS + IP whitelist provides network security.
**Status:** Active

## 2026-05-03 - E2E test false positives on status=failed
**Mistake:** E2E test accepted status=failed as a valid outcome for install/update/remove operations, masking critical failures.
**Correction:** Fixed E2E test to properly FAIL (assert) when status=failed is returned for package operations.
**Rule:** E2E tests must assert status=completed for core operations. A failed package install is a 100% total failure of the API's core function.
**Status:** Active

## 2026-05-03 - Systemd sandbox whack-a-mole pattern
**Mistake:** Fixed systemd sandbox restrictions one at a time (ProtectSystem → NoNewPrivileges → RestrictSUIDSGID → CapabilityBoundingSet) instead of analyzing all restrictions at once.
**Correction:** Removed ALL restrictive sandbox settings at once after understanding that package management requires full system access.
**Rule:** When a service fundamentally conflicts with systemd sandboxing, analyze ALL restrictions at once rather than fixing them one at a time. Package management services need: no ProtectSystem=strict, no NoNewPrivileges, no RestrictSUIDSGID, no CapabilityBoundingSet, no AmbientCapabilities restrictions.
**Status:** Active
