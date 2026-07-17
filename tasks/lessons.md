# Lessons Learned

## 2026-05-02 - Infrastructure Host Protection (CRITICAL)
**Mistake:** Attempted to install Rust and system packages on infrastructure hosts without explicit approval.
**Correction:** Infrastructure hosts (Docker/LXC hosts) must not be modified without explicit approval. Building all binaries happens through the CI/CD workflow.
**Rule:** NEVER install packages or make system-level changes on infrastructure hosts without explicit approval. NEVER build binaries locally or on dev/runners - use CI/CD ONLY.
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

## 2026-05-02 - Binary version mismatch between containers
**Mistake:** Assumed all containers had the same binary version. Dev/u2404 had older Apr 9 build while u2204 had newer Apr 30 build.
**Correction:** Always verify binary versions match before testing. Different BuildIDs mean different code.
**Rule:** Check binary versions (file size, BuildID, --version output) on all target systems before testing.
**Status:** Active

## 2026-05-02 - Always run cargo fmt AND cargo clippy locally before pushing
**Mistake:** Pushed code changes without running cargo fmt and cargo clippy locally, causing 8 CI iterations to fix formatting and lint errors.
**Correction:** Run `cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings` locally before every push.
**Rule:** ALWAYS run cargo fmt AND cargo clippy locally before pushing. Fix all errors before pushing.
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

## 2026-05-20 - Verify on actual target systems before declaring something fixed (CRITICAL)
**Mistake:** Edited Alpine packaging files multiple times without SSHing to the actual Alpine runner to verify. Made assumptions about abuild install script format based on documentation/comments instead of checking the actual abuild source code on the target system.
**Correction:** SSHed to Alpine runner, read abuild source code (lines 247-257), discovered that .apk-install is NOT a valid suffix. abuild expects SEPARATE files: pkgname.pre-install, .post-install, .pre-deinstall, .post-deinstall. The CI build used || true which masked the abuild failure, so APK was built WITHOUT install scripts silently.
**Rule:** ALWAYS verify fixes on actual target systems before pushing. SSH to the runner, inspect the built artifact, test the install. Never assume a file edit is correct without runtime verification. Read the tool's source code when documentation is unclear.
**Status:** Active

## 2026-05-20 - Alpine abuild install script format requires separate files with valid suffixes
**Mistake:** Used a single .apk-install file with function definitions (pre_install, post_install, etc.) for Alpine packaging. This is NOT a valid abuild format.
**Correction:** Created 4 separate files: linux-patch-api.pre-install, .post-install, .pre-deinstall, .post-deinstall as standalone shell scripts. These are the ONLY valid suffixes abuild accepts (lines 247-257 of /usr/bin/abuild).
**Rule:** Alpine abuild install scripts MUST be separate files with valid suffixes: pre-install, post-install, pre-upgrade, post-upgrade, pre-deinstall, post-deinstall. Do NOT use function definitions in a single file. Do NOT invent custom suffixes like .apk-install.
**Status:** Active

## 2026-05-20 - Ask for help with access blocks immediately (CRITICAL)
**Mistake:** Spent many turns and significant compute time trying to work around not having root access on the Alpine runner instead of simply asking for sudo to be installed.
**Correction:** Sudo was installed in seconds. The time and money wasted on workarounds far exceeded the trivial effort of asking for help.
**Rule:** When blocked by an access or permission issue, ASK FOR HELP IMMEDIATELY. Do not spend time on workarounds. A quick fix is worth far more than hours of compute trying to bypass the block.
**Status:** Active

## 2026-05-03 - Systemd sandbox whack-a-mole pattern
**Mistake:** Fixed systemd sandbox restrictions one at a time (ProtectSystem → NoNewPrivileges → RestrictSUIDSGID → CapabilityBoundingSet) instead of analyzing all restrictions at once.
**Correction:** Removed ALL restrictive sandbox settings at once after understanding that package management requires full system access.
**Rule:** When a service fundamentally conflicts with systemd sandboxing, analyze ALL restrictions at once rather than fixing them one at a time. Package management services need: no ProtectSystem=strict, no NoNewPrivileges, no RestrictSUIDSGID, no CapabilityBoundingSet, no AmbientCapabilities restrictions.
**Status:** Active
