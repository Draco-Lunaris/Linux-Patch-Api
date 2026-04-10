# Linux Patch API - Package Build Guide

This document provides comprehensive instructions for building production-ready Debian (.deb) and RPM (.rpm) packages for the Linux Patch API.

## Prerequisites

### For Debian Package Building

```bash
# Install required tools
apt-get update
apt-get install -y \
    cargo \
    rustc \
    debhelper \
    pkg-config \
    libsystemd-dev \
    dpkg-dev \
    fakeroot
```

### For RPM Package Building

```bash
# Install required tools (RHEL/CentOS/Fedora)
dnf install -y \
    cargo \
    rust \
    rpm-build \
    rpmdevtools \
    systemd-rpm-macros \
    pkgconfig \
    systemd-devel \
    gcc

# Or on Ubuntu/Debian for cross-building
apt-get install -y \
    cargo \
    rustc \
    rpm \
    rpmbuild \
    libsystemd-dev
```

## Building Debian Package (.deb)

### Quick Build

```bash
cd /a0/usr/projects/linux_patch_api

# Build release binary
cargo build --release --target x86_64-unknown-linux-gnu

# Build Debian package
dpkg-buildpackage -us -uc -b

# Package will be created in parent directory
# linux-patch-api_1.0.0-1_amd64.deb
```

### Detailed Build Process

```bash
# 1. Ensure release binary exists
cargo build --release --target x86_64-unknown-linux-gnu

# 2. Verify debian/ directory structure
ls -la debian/
# Should contain: control, rules, changelog, compat, install, conffiles, copyright
# And maintainer scripts: preinst, postinst, prerm, postrm

# 3. Build the package
dpkg-buildpackage -us -uc -b

# 4. Verify package contents
dpkg-deb --contents ../linux-patch-api_1.0.0-1_amd64.deb

# 5. Verify package info
dpkg-deb --info ../linux-patch-api_1.0.0-1_amd64.deb

# 6. Lint the package (optional but recommended)
lintian ../linux-patch-api_1.0.0-1_amd64.deb
```

### Installation Test

```bash
# Install the package
dpkg -i linux-patch-api_1.0.0-1_amd64.deb

# Verify installation
systemctl status linux-patch-api
linux-patch-api --version

# Check installed files
dpkg -L linux-patch-api

# Remove package (keeping configs)
dpkg -r linux-patch-api

# Purge package (removing all configs)
dpkg -P linux-patch-api
```

## Building RPM Package (.rpm)

### Quick Build

```bash
cd /a0/usr/projects/linux_patch_api

# Build release binary
cargo build --release --target x86_64-unknown-linux-gnu

# Build RPM package
rpmbuild -ba linux-patch-api.spec

# Package will be created in ~/rpmbuild/RPMS/
```

### Detailed Build Process

```bash
# 1. Set up RPM build environment
rpmdev-setuptree

# 2. Copy spec file to SPECS directory
cp linux-patch-api.spec ~/rpmbuild/SPECS/

# 3. Copy source tarball to SOURCES directory
# Create source tarball
tar -czvf linux-patch-api-1.0.0.tar.gz \
    --exclude=target \
    --exclude=.git \
    --exclude=debian \
    --exclude=*.deb \
    --exclude=*.rpm \
    .

mv linux-patch-api-1.0.0.tar.gz ~/rpmbuild/SOURCES/

# 4. Build the RPM
rpmbuild -ba ~/rpmbuild/SPECS/linux-patch-api.spec

# 5. Verify RPM contents
rpm -qlp ~/rpmbuild/RPMS/x86_64/linux-patch-api-1.0.0-1.el9.x86_64.rpm

# 6. Verify RPM info
rpm -qip ~/rpmbuild/RPMS/x86_64/linux-patch-api-1.0.0-1.el9.x86_64.rpm

# 7. Lint the spec file (optional but recommended)
rpmlint ~/rpmbuild/SPECS/linux-patch-api.spec
```

### Installation Test

```bash
# Install the RPM
rpm -ivh ~/rpmbuild/RPMS/x86_64/linux-patch-api-1.0.0-1.el9.x86_64.rpm

# Or using dnf/yum
dnf install ~/rpmbuild/RPMS/x86_64/linux-patch-api-1.0.0-1.el9.x86_64.rpm

# Verify installation
systemctl status linux-patch-api
linux-patch-api --version

# List installed files
rpm -ql linux-patch-api

# Remove package
rpm -e linux-patch-api
```

## Using the Interactive Installer

For manual deployment without package managers:

```bash
# Ensure binary is built
cargo build --release --target x86_64-unknown-linux-gnu

# Run installer (must be root)
sudo ./install.sh
```

The installer will:
1. Detect operating system
2. Check prerequisites (systemd, binary)
3. Create system user and group
4. Create directory structure
5. Install binary and configuration files
6. Install systemd service
7. Optionally generate self-signed certificates
8. Optionally enable and start the service

## Package Contents

### Installed Files

| Path | Description | Permissions |
|------|-------------|-------------|
| `/usr/bin/linux-patch-api` | Main binary | 755 |
| `/lib/systemd/system/linux-patch-api.service` | Systemd service unit | 644 |
| `/etc/linux_patch_api/config.yaml` | Main configuration | 640 |
| `/etc/linux_patch_api/whitelist.yaml` | IP whitelist | 640 |
| `/etc/linux_patch_api/certs/` | TLS certificates directory | 750 |
| `/var/lib/linux_patch_api/` | Data directory | 755 |
| `/var/log/linux_patch_api/` | Log directory | 755 |

### System User/Group

| Property | Value |
|----------|-------|
| User | linux-patch-api |
| Group | linux-patch-api |
| Home | /var/lib/linux_patch_api |
| Shell | /usr/sbin/nologin |
| Type | System account |

## Supported Distributions

### Debian Package (.deb)

| Distribution | Versions | Status |
|--------------|----------|--------|
| Debian | 11 (Bullseye), 12 (Bookworm) | ✅ Supported |
| Ubuntu | 20.04 LTS (Focal) | ✅ Supported |
| Ubuntu | 22.04 LTS (Jammy) | ✅ Supported |
| Ubuntu | 24.04 LTS (Noble) | ✅ Supported |

### RPM Package (.rpm)

| Distribution | Versions | Status |
|--------------|----------|--------|
| RHEL | 8, 9 | ✅ Supported |
| CentOS | 8, 9 | ✅ Supported |
| Fedora | 38+ | ✅ Supported |
| AlmaLinux | 8, 9 | ✅ Supported |
| Rocky Linux | 8, 9 | ✅ Supported |

## Troubleshooting

### Debian Package Issues

**Error: `dh_auto_install: error: ...`**
```bash
# Ensure release binary exists
ls -la target/x86_64-unknown-linux-gnu/release/linux-patch-api

# Rebuild if missing
cargo build --release --target x86_64-unknown-linux-gnu
```

**Error: `missing build-dependency`**
```bash
# Install missing dependencies
apt-get install -y libsystemd-dev pkg-config
```

### RPM Package Issues

**Error: `RPMS not found`**
```bash
# Check build output
ls -la ~/rpmbuild/RPMS/x86_64/

# Check for build errors
cat ~/rpmbuild/BUILDROOT/*/var/log/rpmbuild.log
```

**Error: `missing BuildRequires`**
```bash
# Install development packages
dnf install -y systemd-devel pkgconfig
```

### Service Issues

**Service fails to start:**
```bash
# Check service status
systemctl status linux-patch-api

# View logs
journalctl -u linux-patch-api -f

# Check configuration
linux-patch-api --config /etc/linux_patch_api/config.yaml --check

# Verify certificates
ls -la /etc/linux_patch_api/certs/
```

## CI/CD Integration

### GitHub Actions Example

```yaml
name: Build Packages

on:
  release:
    types: [published]

jobs:
  build-deb:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Install dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y cargo debhelper pkg-config libsystemd-dev
      
      - name: Build release
        run: cargo build --release
      
      - name: Build Debian package
        run: dpkg-buildpackage -us -uc -b
      
      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: linux-patch-api-deb
          path: ../linux-patch-api_*.deb

  build-rpm:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Install dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y cargo rpm rpmbuild
      
      - name: Set up RPM environment
        run: rpmdev-setuptree
      
      - name: Build release
        run: cargo build --release
      
      - name: Build RPM package
        run: rpmbuild -ba linux-patch-api.spec
      
      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: linux-patch-api-rpm
          path: ~/rpmbuild/RPMS/x86_64/*.rpm
```

## Version Management

### Updating Version for New Release

1. **Update Cargo.toml:**
   ```toml
   [package]
   version = "1.0.1"  # Increment version
   ```

2. **Update debian/changelog:**
   ```bash
   dch -v 1.0.1-1 "Release notes here"
   ```

3. **Update RPM spec:**
   ```spec
   Version:        1.0.1
   Release:        1%{?dist}
   ```

4. **Update ROADMAP.md:**
   - Mark previous version complete
   - Add new version to changelog

## Security Considerations

- Packages are signed with maintainer GPG key for production deployments
- All maintainer scripts run with `set -e` for fail-fast behavior
- Configuration files are marked as conffiles to preserve user modifications
- System user has minimal privileges (nologin shell, no home directory)
- Directory permissions follow principle of least privilege
- TLS certificates should be replaced with CA-signed certs in production

## Support

For issues or questions:
- Review logs: `journalctl -u linux-patch-api -f`
- Check documentation: `/usr/share/doc/linux-patch-api/`
- Report issues: https://gitea.moon-dragon.us/echo/linux_patch_api/issues
