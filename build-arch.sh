#!/bin/bash
# Build Arch Linux Package (.pkg.tar.zst)
# Run on: Arch Linux / Manjaro
# Designed for native Gitea Actions runner execution

set -e

echo "=== Linux Patch API - Arch Build Script ==="
echo ""

# Check if running on Arch
if ! command -v makepkg &> /dev/null; then
    echo "Error: makepkg not found. This script must run on Arch Linux."
    exit 1
fi

# Build release binary
if [ -z "$SKIP_CARGO_BUILD" ]; then
    echo "Building release binary..."
    cargo build --release
else
    echo "Skipping cargo build (SKIP_CARGO_BUILD is set)"
fi

# Create package directory
PKGDIR=$(pwd)/arch-package
rm -rf "$PKGDIR"
mkdir -p "$PKGDIR"/usr/bin
mkdir -p "$PKGDIR"/etc/linux_patch_api/certs
mkdir -p "$PKGDIR"/usr/lib/systemd/system
mkdir -p "$PKGDIR"/var/lib/linux_patch_api
mkdir -p "$PKGDIR"/var/log/linux_patch_api

# Copy binary
chmod 755 target/release/linux-patch-api
cp target/release/linux-patch-api "$PKGDIR"/usr/bin/

# Copy systemd service
cp configs/linux-patch-api.service "$PKGDIR"/usr/lib/systemd/system/

# Copy example configs (as .example files - install script creates live configs)
cp configs/config.yaml.example "$PKGDIR"/etc/linux_patch_api/config.yaml.example
cp configs/whitelist.yaml.example "$PKGDIR"/etc/linux_patch_api/whitelist.yaml.example

# Copy install script (must match filename in PKGBUILD install= line)
cp configs/linux-patch-api.install linux-patch-api.install

# Create PKGBUILD with quoted heredoc to prevent $pkgdir expansion
# $pkgdir must be literal for makepkg to expand at runtime
echo "Creating PKGBUILD..."
cat > PKGBUILD << 'EOF'
pkgname=linux-patch-api
pkgver=VERSION_PLACEHOLDER
pkgrel=1
pkgdesc="Secure remote package management API for Linux systems"
url="https://gitea.moon-dragon.us/echo/linux_patch_api"
arch=('x86_64')
license=('MIT')
depends=('systemd')
install=linux-patch-api.install
backup=(
    'etc/linux_patch_api/config.yaml'
    'etc/linux_patch_api/whitelist.yaml'
)

package() {
    cp -r /home/builduser/repo/arch-package/* "$pkgdir"/

    # Ensure directories exist with proper structure
    mkdir -p "$pkgdir"/etc/linux_patch_api/certs
    mkdir -p "$pkgdir"/var/lib/linux_patch_api
    mkdir -p "$pkgdir"/var/log/linux_patch_api
}
EOF

# Replace version placeholder with actual version from Cargo.toml
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*=.*"\([^"]*\)".*/\1/')
sed -i "s/VERSION_PLACEHOLDER/$VERSION/" PKGBUILD

echo "PKGBUILD version: $VERSION"

# Create .SRCINFO
echo "Creating .SRCINFO..."

# Build package
echo "Building Arch package..."

# For CI environments where we may run as root
if [ "$(id -u)" = "0" ]; then
    echo "Running as root - creating build user for makepkg..."
    useradd -m builduser 2>/dev/null || true
    
    # Copy repo contents to builduser home (accessible directory)
    mkdir -p /home/builduser/repo
    cp -r . /home/builduser/repo/
    chown -R builduser:builduser /home/builduser/repo/
    
    su - builduser -c "cd /home/builduser/repo && makepkg --printsrcinfo > .SRCINFO"
    su - builduser -c "cd /home/builduser/repo && makepkg -f --noconfirm"
    
    # Copy package to releases
    mkdir -p releases
    cp /home/builduser/repo/*.pkg.tar.zst releases/
else
    makepkg --printsrcinfo > .SRCINFO
    makepkg -f --noconfirm
    mkdir -p releases
    cp *.pkg.tar.zst releases/
fi

echo ""
echo "=== Build Complete ==="
echo "Package: releases/linux-patch-api-*.pkg.tar.zst"
echo ""
echo "Install with:"
echo "  sudo pacman -U ./releases/linux-patch-api-*.pkg.tar.zst"
