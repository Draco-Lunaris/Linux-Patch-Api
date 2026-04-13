Name:           linux-patch-api
Version:        1.0.0
Release:        1%{?dist}
Summary:        Secure remote package management API for Linux systems
License:        MIT
URL:            https://gitea.moon-dragon.us/echo/linux_patch_api
Source0:        linux-patch-api-%{version}.tar.gz
BuildArch:      x86_64

# Build requirements
# NOTE: Building in Fedora container (fedora:latest) - native RPM environment
# BuildRequires validate against Fedora's RPM database
BuildRequires:  cargo >= 1.75
BuildRequires:  rust >= 1.75
# BuildRequires:  systemd-rpm-macros  # Handling systemd manually
BuildRequires:  pkgconfig(systemd)
BuildRequires:  gcc

# Runtime requirements
Requires:       systemd
Requires:       libsystemd

# Description
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

# Build
%build
export RUSTFLAGS="-C target-cpu=native"
cargo build --release --target x86_64-unknown-linux-gnu

# Install
%install
mkdir -p %{buildroot}/usr/bin
mkdir -p %{buildroot}/etc/linux_patch_api
mkdir -p %{buildroot}/lib/systemd/system
mkdir -p %{buildroot}/var/log/linux_patch_api
mkdir -p %{buildroot}/var/lib/linux_patch_api

# Install binary
cp target/x86_64-unknown-linux-gnu/release/linux-patch-api %{buildroot}/usr/bin/
chmod 755 %{buildroot}/usr/bin/linux-patch-api

# Install systemd service
cp configs/linux-patch-api.service %{buildroot}/lib/systemd/system/
chmod 644 %{buildroot}/lib/systemd/system/linux-patch-api.service

# Install example configs
cp configs/config.yaml.example %{buildroot}/etc/linux_patch_api/config.yaml.example
cp configs/whitelist.yaml.example %{buildroot}/etc/linux_patch_api/whitelist.yaml.example
chmod 644 %{buildroot}/etc/linux_patch_api/*.example

# Pre-installation script
%pre
# Create system group
getent group linux-patch-api > /dev/null || groupadd --system linux-patch-api

# Create system user
getent passwd linux-patch-api > /dev/null || useradd --system \
    --gid linux-patch-api \
    --home-dir /var/lib/linux_patch_api \
    --no-create-home \
    --shell /usr/sbin/nologin \
    --comment "Linux Patch API Service" \
    linux-patch-api

# Create required directories
mkdir -p /etc/linux_patch_api/certs
mkdir -p /var/lib/linux_patch_api
mkdir -p /var/log/linux_patch_api

# Set proper ownership
chown -R linux-patch-api:linux-patch-api /var/lib/linux_patch_api
chown -R linux-patch-api:linux-patch-api /var/log/linux_patch_api

# Set secure permissions
chmod 750 /etc/linux_patch_api
chmod 750 /etc/linux_patch_api/certs
chmod 755 /var/lib/linux_patch_api
chmod 755 /var/log/linux_patch_api

# Post-installation script
%post
# Copy example configs if they don't exist
if [ ! -f "/etc/linux_patch_api/config.yaml" ]; then
    cp /etc/linux_patch_api/config.yaml.example /etc/linux_patch_api/config.yaml
    chmod 640 /etc/linux_patch_api/config.yaml
    chown linux-patch-api:linux-patch-api /etc/linux_patch_api/config.yaml
fi

if [ ! -f "/etc/linux_patch_api/whitelist.yaml" ]; then
    cp /etc/linux_patch_api/whitelist.yaml.example /etc/linux_patch_api/whitelist.yaml
    chmod 640 /etc/linux_patch_api/whitelist.yaml
    chown linux-patch-api:linux-patch-api /etc/linux_patch_api/whitelist.yaml
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
/usr/bin/linux-patch-api
/lib/systemd/system/linux-patch-api.service
%config(noreplace) /etc/linux_patch_api/config.yaml.example
%config(noreplace) /etc/linux_patch_api/whitelist.yaml.example
%dir /etc/linux_patch_api
%dir /etc/linux_patch_api/certs
%dir /var/lib/linux_patch_api
%dir /var/log/linux_patch_api

# Changelog
%changelog
* Thu Apr 09 2026 Echo <echo@moon-dragon.us> - 1.0.0-1
- Initial production release
- Secure mTLS-authenticated REST API for remote package management
- 15 API endpoints for package install/remove, patch application, system management
- Asynchronous job processing with WebSocket status streaming
- IP whitelist enforcement and comprehensive audit logging
- Systemd integration with security hardening
- Supports RHEL 8/9, CentOS 8/9, Fedora 38+
