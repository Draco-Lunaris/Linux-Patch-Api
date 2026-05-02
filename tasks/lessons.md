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
