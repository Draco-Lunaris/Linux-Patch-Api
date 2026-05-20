# Alpine Packaging Root Cause Analysis

**Date:** 2026-05-20
**Author:** Echo
**Status:** Fixed in v1.1.10

## Problem Statement

Alpine APK packages for linux-patch-api did not create required files on `apk add`:
- No `/etc/linux_patch_api/config.yaml` (from config.yaml.example)
- No `/etc/linux_patch_api/config.yaml.example`
- No directories created
- No service enabled
- No post-install messages

## Root Cause

**The install script format was completely wrong for Alpine's `abuild` package builder.**

### Technical Details

Alpine's `abuild` (lines 247-257 of `/usr/bin/abuild`) validates install script suffixes against a whitelist:

```shell
for i in $install; do
    pre-install|post-install|pre-upgrade|post-upgrade|pre-deinstall|post-deinstall);;
    *) die "$i: unknown install script suffix"
```

**Valid suffixes:** `pre-install`, `post-install`, `pre-upgrade`, `post-upgrade`, `pre-deinstall`, `post-deinstall`

**Invalid suffix used:** `.apk-install` — this caused `abuild` to die with `"unknown install script suffix"`

**Why it wasn't caught:** The CI build script (`build-alpine.sh`) used `|| true` after `abuild`, which **silently masked the failure**. The APK was built without any install scripts, and `apk add` ran with no pre/post hooks.

### Original (Broken) Format

Single file `configs/linux-patch-api.apk-install` containing function definitions:
```sh
pre_install() { ... }
post_install() { ... }
pre_deinstall() { ... }
post_deinstall() { ... }
```

APKBUILD referenced it as:
```
install="linux-patch-api.apk-install"
```

**Two fatal errors:**
1. `.apk-install` is not a valid abuild suffix (should be `.pre-install`, `.post-install`, etc.)
2. Function definitions (`pre_install()`) are NOT how abuild install scripts work — each must be a standalone shell script

### Correct Format

Four separate files, each a standalone shell script:
- `linux-patch-api.pre-install` — runs before package files are laid down
- `linux-patch-api.post-install` — runs after package files are laid down
- `linux-patch-api.pre-deinstall` — runs before package removal
- `linux-patch-api.post-deinstall` — runs after package removal

APKBUILD references them as:
```
install="linux-patch-api.pre-install linux-patch-api.post-install linux-patch-api.pre-deinstall linux-patch-api.post-deinstall"
```

## Fix

### Files Changed
1. **Deleted** `configs/linux-patch-api.apk-install` (invalid format)
2. **Created** `configs/linux-patch-api.pre-install` (create dirs, set permissions)
3. **Created** `configs/linux-patch-api.post-install` (copy example configs, enable service)
4. **Created** `configs/linux-patch-api.pre-deinstall` (stop and disable service)
5. **Created** `configs/linux-patch-api.post-deinstall` (clean up empty dirs)
6. **Updated** `build-alpine.sh` — copy 4 install scripts to workspace, update `install=` line in APKBUILD

### Verification on Alpine Runner

Inspected v1.1.10 APK contents:
```
.SIGN.RSA.root-69eeaa18.rsa.pub
.PKGINFO
.pre-install
.post-install
.pre-deinstall
.post-deinstall
etc/init.d/linux-patch-api
etc/linux_patch_api/config.yaml.example
etc/linux_patch_api/whitelist.yaml.example
usr/bin/linux-patch-api
var/lib/linux_patch_api/
var/log/linux_patch_api/
```

All install scripts and example configs are now properly embedded in the APK.

### abuild Validation

Ran `abuild verify` on the Alpine runner with the new format:
```
>>> linux-patch-api: Checking install script suffixes...
>>> linux-patch-api: Checking if install script names match pkgname...
```

Both checks PASSED. The old `.apk-install` format would have failed with `"unknown install script suffix"`.

## Prevention

1. **Always verify on actual target systems before pushing.** SSH to the runner, inspect the built artifact, test the install.
2. **Read the tool's source code when documentation is unclear.** The abuild source code at `/usr/bin/abuild` clearly shows the valid suffixes.
3. **Never use `|| true` to mask build failures.** The CI build script masked the abuild failure, hiding the root cause.
4. **Never assume a file edit is correct without runtime verification.** Multiple edits to .apk-install were made without testing on Alpine.

## Commit

- Commit: `e033cb8` — Fix Alpine install scripts: use separate files with valid abuild suffixes
- Tag: `v1.1.10`
