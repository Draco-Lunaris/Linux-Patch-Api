# Fix Non-Ubuntu Package Builds

## Goal
All platform packages must produce identical install results as the Debian/Ubuntu package.

## Debian Baseline Behavior
- No system user creation (service runs as root)
- Directory ownership: root:root
- Create dirs: /etc/linux_patch_api/certs, /var/lib/linux_patch_api, /var/log/linux_patch_api
- Permissions: 750 on config dirs, 755 on data/log dirs
- Copy .example configs to live configs if not present
- Enable service (systemd or rc-update)
- Print next-steps message

## Tasks
- [x] 1. Fix Arch linux-patch-api.install - removed system user creation, root:root ownership, matches Debian
- [x] 2. Fix Arch build-arch.sh - fixed $startdir path, added source=() array
- [x] 3. Fix RPM linux-patch-api.spec - uncommented BuildRequires, added runtime deps, removed system user, root:root ownership
- [x] 4. Fix Alpine linux-patch-api.apk-install - removed system user creation, root:root ownership, matches Debian
- [x] 5. Fix Alpine build-alpine.sh - co-located install script with APKBUILD, used install -Dm commands
- [ ] 6. Verify all platforms produce consistent results (needs CI run)

## Changes Summary

### Arch Linux
- **configs/linux-patch-api.install**: Removed user/group creation, changed ownership to root:root, matches Debian preinst/postinst
- **build-arch.sh**: Fixed PKGBUILD package() to use $startdir (not $srcdir), added source=() array

### RPM (Fedora/RHEL/CentOS)
- **linux-patch-api.spec**: Uncommented BuildRequires (cargo, rust, gcc, openssl-devel, systemd-devel, pkgconfig(systemd)), added runtime Requires (openssl-libs, ca-certificates), removed system user creation from %pre, changed ownership to root:root in %pre, matches Debian behavior in %post

### Alpine Linux
- **configs/linux-patch-api.apk-install**: Removed addgroup/adduser, changed ownership to root:root, matches Debian preinst/postinst
- **build-alpine.sh**: Restructured to co-locate install script with APKBUILD in workspace directory, used install -Dm commands in package() function, fixed $startdir references
