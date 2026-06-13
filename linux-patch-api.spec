%global debug_package %{nil}

Name:           linux-patch-api
Version:        VERSION_PLACEHOLDER
Release:        1%{?dist}
Summary:        Secure remote package management API for Linux systems
License:        MIT
URL:            https://gitea.moon-dragon.us/echo/linux_patch_api
Source0:        linux-patch-api-%{version}.tar.gz
BuildArch:      x86_64

# Build requirements
# NOTE: CI uses rustup to install cargo/rust, so they are NOT available as RPM packages.
# Only uncomment BuildRequires for native RPM build environments where cargo/rust
# are installed via dnf/yum package manager.
# BuildRequires:  cargo >= 1.75
# BuildRequires:  rust >= 1.75
# BuildRequires:  gcc
# BuildRequires:  openssl-devel
# BuildRequires:  systemd-devel
# BuildRequires:  pkgconfig(systemd)

# Runtime requirements
Requires:       systemd-libs
Requires:       openssl-libs
Requires:       ca-certificates

%description
Linux Patch API provides a secure, mTLS-authenticated REST API for
remote package management operations including:
- Package installation and removal
- Security patch application
- System health monitoring
- Job queue management with WebSocket status streaming

Features:
- Mutual TLS (mTLS) authentication
- IP whitelist enforcement
- Asynchronous job processing
- Comprehensive audit logging
- Systemd integration with security hardening

# Preparation
%prep
%autosetup -n linux-patch-api-%{version}

# Build - no-op, binary is pre-built and included in source tarball
# The binary is built by build-rpm.sh BEFORE creating the tarball,
# so cargo does not need to be in rpmbuild's PATH.
%build
# Binary already built - nothing to do

# Install
%install
mkdir -p %{buildroot}/usr/bin
mkdir -p %{buildroot}/etc/linux_patch_api
mkdir -p %{buildroot}/etc/linux_patch_api/certs
mkdir -p %{buildroot}/lib/systemd/system
mkdir -p %{buildroot}/var/log/linux_patch_api
mkdir -p %{buildroot}/var/lib/linux_patch_api

# Install binary (pre-built, included in tarball at target/release/)
cp target/release/linux-patch-api %{buildroot}/usr/bin/
chmod 755 %{buildroot}/usr/bin/linux-patch-api

# Install systemd service
cp configs/linux-patch-api.service %{buildroot}/lib/systemd/system/
chmod 644 %{buildroot}/lib/systemd/system/linux-patch-api.service

# Install example configs
cp configs/config.yaml.example %{buildroot}/etc/linux_patch_api/config.yaml.example
cp configs/whitelist.yaml.example %{buildroot}/etc/linux_patch_api/whitelist.yaml.example
chmod 644 %{buildroot}/etc/linux_patch_api/*.example

# Pre-installation script - create directories (matches Debian preinst)
%pre
# Create required directories
mkdir -p /etc/linux_patch_api/certs
mkdir -p /var/lib/linux_patch_api
mkdir -p /var/log/linux_patch_api

# Set proper ownership (service runs as root)
chown -R root:root /var/lib/linux_patch_api
chown -R root:root /var/log/linux_patch_api

# Set secure permissions
chmod 750 /etc/linux_patch_api
chmod 750 /etc/linux_patch_api/certs
chmod 755 /var/lib/linux_patch_api
chmod 755 /var/log/linux_patch_api

# Post-installation script - copy configs, enable service (matches Debian postinst)
%post
# Copy example configs if they don't exist
if [ ! -f "/etc/linux_patch_api/config.yaml" ]; then
    cp /etc/linux_patch_api/config.yaml.example /etc/linux_patch_api/config.yaml
    chmod 640 /etc/linux_patch_api/config.yaml
    chown root:root /etc/linux_patch_api/config.yaml
fi

if [ ! -f "/etc/linux_patch_api/whitelist.yaml" ]; then
    cp /etc/linux_patch_api/whitelist.yaml.example /etc/linux_patch_api/whitelist.yaml
    chmod 640 /etc/linux_patch_api/whitelist.yaml
    chown root:root /etc/linux_patch_api/whitelist.yaml
fi

# Reload systemd daemon
systemctl daemon-reload

# Enable the service (but don't start automatically)
systemctl enable linux-patch-api.service

echo ""
echo "linux-patch-api installed successfully!"
echo ""
echo "Next steps:"
echo "  1. Configure /etc/linux_patch_api/config.yaml with your settings"
echo "  2. Place TLS certificates in /etc/linux_patch_api/certs/"
echo "  3. Configure IP whitelist in /etc/linux_patch_api/whitelist.yaml"
echo "  4. Start the service: systemctl start linux-patch-api"
echo "  5. Check status: systemctl status linux-patch-api"
echo ""

# Pre-uninstallation script
%preun
if [ $1 -eq 0 ]; then
    # Package removal (not upgrade)
    if systemctl is-active --quiet linux-patch-api.service; then
        systemctl stop linux-patch-api.service
    fi
    if systemctl is-enabled --quiet linux-patch-api.service 2>/dev/null; then
        systemctl disable linux-patch-api.service
    fi
fi

# Post-uninstallation script
%postun
systemctl daemon-reload 2>/dev/null || true

if [ $1 -eq 0 ]; then
    # Package removal (not upgrade) - configs preserved
    :
fi

if [ $1 -ge 1 ]; then
    # Package upgrade
    :
fi

# Files
%files
%defattr(-,root,root,-)
/usr/bin/linux-patch-api
/lib/systemd/system/linux-patch-api.service
%config(noreplace) /etc/linux_patch_api/config.yaml.example
%config(noreplace) /etc/linux_patch_api/whitelist.yaml.example
%ghost %config(noreplace) /etc/linux_patch_api/config.yaml
%ghost %config(noreplace) /etc/linux_patch_api/whitelist.yaml
%dir /etc/linux_patch_api
%dir /etc/linux_patch_api/certs
%dir /var/lib/linux_patch_api
%dir /var/log/linux_patch_api

# Changelog
%changelog
* Thu Jun 12 2026 Echo <echo@moon-dragon.us> - 1.2.0-1
- Add file install endpoint: POST /api/v1/packages/install-file (multipart upload)
- Add system restart endpoint: POST /api/v1/system/restart (graceful self-restart)
- Add file_install.enabled config option (default: false)
- Add file_install.staging_dir config option (default: /tmp)
- File validation: extension allowlist (.deb, .rpm, .apk, .tar.zst), 1GB size limit
- Rate limiting on file upload endpoint

* Tue May 27 2026 Echo <echo@moon-dragon.us> - 1.1.17-1
- Add mandatory package cache refresh before patch_apply
- Add health check cache refresh when stale (>4h)
- Add cache status fields to health response
- Add 404/fetch error retry with cache refresh
- Add degraded health status on cache failure
- New src/packages/cache.rs module

* Tue May 20 2026 Echo <echo@moon-dragon.us> - 1.1.16-1
- Add Pacman package manager backend for Arch Linux
- Fix: Pacman backend not yet implemented error on Arch systems
- Support pacman -Q for package listing, pacman -Qi for package details
- Support pacman -Qu for patch/update detection
- Fix Arch CI: add stale package cleanup and version verification

* Tue May 20 2026 Echo <echo@moon-dragon.us> - 1.1.15-1
- Add DNF package manager backend for Fedora/RHEL/CentOS 8+
- Add YUM package manager backend for RHEL/CentOS 7
- Fix: DNF backend not yet implemented error on Fedora systems
- Support rpm -qa for package listing, rpm -qi for package details
- Support dnf check-update (exit code 100) for patch detection
- Support yum check-update (exit code 100) for patch detection

* Tue May 20 2026 Echo <echo@moon-dragon.us> - 1.1.14-1
- Fix RPM packaging: pre-build binary before tarball (like Alpine/Arch pattern)
- Fix rpmbuild can't find cargo in PATH - binary now included in source tarball
- Fix config file ownership: add %defattr(-,root,root,-) in %files section
- Fix Requires: libsystemd -> systemd-libs for Fedora compatibility
- Remove Requires: systemd (not needed, may not exist in containers)
- Add stale RPM cleanup and version verification to build-rpm.sh
- Support SKIP_CARGO_BUILD=1 like Alpine/Arch builds

* Wed May 20 2026 Echo <echo@moon-dragon.us> - 1.1.12-1
- Add APK (Alpine Linux) package manager backend
- Add machine-id generation to Alpine pre-install script
- Fix OpenRC init script ownership (root:root)


* Wed May 20 2026 Echo <echo@moon-dragon.us> - 1.1.10-1
- Fix Alpine install scripts: use separate files with valid abuild suffixes
- Root cause: .apk-install is not a valid abuild suffix (abuild silently fails)
- Correct format: pkgname.pre-install, .post-install, .pre-deinstall, .post-deinstall
- Verified on actual Alpine runner: install script suffixes now pass abuild validation

* Tue May 19 2026 Echo <echo@moon-dragon.us> - 1.1.9-1
- Fix non-Ubuntu packages: align Arch, RPM, Alpine with Debian baseline
- Remove system user creation (service runs as root)
- Fix ownership to root:root across all platforms
- Fix Alpine: co-locate install script with APKBUILD
- Fix Arch: correct $startdir path in PKGBUILD
- Fix RPM: add runtime deps, comment BuildRequires for CI
- Add comprehensive installation docs for all platforms

* Tue May 19 2026 Echo <echo@moon-dragon.us> - 1.1.8-1
- Fix RPM packaging: runtime deps, match Debian install behavior, comment BuildRequires for CI
- Remove system user creation (service runs as root per systemd unit)
- Fix ownership to root:root matching Debian package
- Add openssl-libs and ca-certificates runtime dependencies

* Mon May 18 2026 Echo <echo@moon-dragon.us> - 1.1.8-1
- Fix FQDN resolution: prioritize hostname -f over /etc/hostname
- Fix display_name blank: add hostname field to enrollment request
- Fix Arch/Alpine/RPM packaging: install scripts, user creation, directory creation

* Thu Apr 09 2026 Echo <echo@moon-dragon.us> - 1.1.7-1
- Initial production release
- Secure mTLS-authenticated REST API for remote package management
- 15 API endpoints for package install/remove, patch application, system management
