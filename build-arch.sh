#!/bin/bash
# Build Arch Linux Package (.pkg.tar.zst)
# Run on: Arch Linux / Manjaro
# Designed for native Gitea Actions runner execution

set -e

# Store working directory at script start (before any su commands)
REPO_DIR=$(pwd)

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
mkdir -p "$PKGDIR"/usr/bin
mkdir -p "$PKGDIR"/etc/linux_patch_api
mkdir -p "$PKGDIR"/usr/lib/systemd/system

# Copy files
cp target/release/linux-patch-api "$PKGDIR"/usr/bin/
chmod 755 "$PKGDIR"/usr/bin/linux-patch-api
cp configs/linux-patch-api.service "$PKGDIR"/usr/lib/systemd/system/
cp configs/config.yaml.example "$PKGDIR"/etc/linux_patch_api/config.yaml
cp configs/whitelist.yaml.example "$PKGDIR"/etc/linux_patch_api/whitelist.yaml

# Determine workspace path for PKGBUILD
WORKSPACE_DIR=$(pwd)

# Create PKGBUILD
echo "Creating PKGBUILD..."
cat > PKGBUILD << EOF
pkgname=linux-patch-api
pkgver=1.0.0
pkgrel=1
pkgdesc="Secure remote package management API for Linux systems"
url="https://gitea.moon-dragon.us/echo/linux_patch_api"
arch=('x86_64')
license=('MIT')
depends=('systemd')

package() {
    cp -r ${WORKSPACE_DIR}/arch-package/* "$pkgdir"/
}
EOF

# Create .SRCINFO
echo "Creating .SRCINFO..."

# Build package
echo "Building Arch package..."

# For CI environments where we may run as root
# For CI environments where we may run as root
if [ "$(id -u)" = "0" ]; then
    echo "Running as root - creating build user for makepkg..."
    useradd -m builduser 2>/dev/null || true
    chown -R builduser:builduser "$REPO_DIR"
    su - builduser -c "cd $REPO_DIR && makepkg --printsrcinfo > .SRCINFO"
    su - builduser -c "cd $REPO_DIR && makepkg -f --noconfirm"
else
    makepkg --printsrcinfo > .SRCINFO
    makepkg -f --noconfirm
fi

# Copy to releases directory
echo ""
echo "Copying package to releases/..."
mkdir -p releases
cp *.pkg.tar.zst releases/

echo ""
echo "=== Build Complete ==="
echo "Package: releases/linux-patch-api-*.pkg.tar.zst"
echo ""
echo "Install with:"
echo "  sudo pacman -U ./releases/linux-patch-api-*.pkg.tar.zst"
