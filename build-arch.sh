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

# Clean stale packages from previous builds
rm -f releases/linux-patch-api-*.pkg.tar.zst 2>/dev/null || true
rm -f /home/builduser/repo/releases/linux-patch-api-*.pkg.tar.zst 2>/dev/null || true
rm -f /home/builduser/repo/*.pkg.tar.zst 2>/dev/null || true

# Build release binary
if [ -z "$SKIP_CARGO_BUILD" ]; then
    echo "Building release binary..."
    cargo build --release
else
    echo "Skipping cargo build (SKIP_CARGO_BUILD is set)"
fi

# Create package directory structure
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

mkdir -p "$PKGDIR"/usr/lib/linux-patch-api

# Copy example configs (as .example files - install script creates live configs)
cp configs/config.yaml.example "$PKGDIR"/etc/linux_patch_api/config.yaml.example
cp configs/whitelist.yaml.example "$PKGDIR"/etc/linux_patch_api/whitelist.yaml.example

# Copy install script to current directory (must be co-located with PKGBUILD)
cp configs/linux-patch-api.install linux-patch-api.install

# Get version from Cargo.toml
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*=.*"\([^"]*\)".*/\1/')

# Translate version for Arch: hyphens are illegal in pkgver
# e.g. 1.5.0-rc1 -> 1.5.0.rc1
ARCH_VERSION=$(echo "$VERSION" | tr '-' '.')

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
source=()
backup=(
    'etc/linux_patch_api/config.yaml'
    'etc/linux_patch_api/whitelist.yaml'
)

package() {
    # Use $startdir because arch-package is co-located with PKGBUILD, not in sources
    cp -r "$startdir"/arch-package/* "$pkgdir"/

    # Ensure directories exist with proper structure
    mkdir -p "$pkgdir"/etc/linux_patch_api/certs
    mkdir -p "$pkgdir"/var/lib/linux_patch_api
    mkdir -p "$pkgdir"/var/log/linux_patch_api
}
EOF

# Replace version placeholder with actual version
sed -i "s/VERSION_PLACEHOLDER/$ARCH_VERSION/" PKGBUILD

echo "PKGBUILD version: $ARCH_VERSION (from $VERSION)"

# Build package
# For CI environments where we may run as root
if [ "$(id -u)" = "0" ]; then
    echo "Running as root - creating build user for makepkg..."
    useradd -m builduser 2>/dev/null || true
    
    # Copy repo contents to builduser home (accessible directory)
    mkdir -p /home/builduser/repo
    cp -r . /home/builduser/repo/
    chown -R builduser:builduser /home/builduser/repo/
    
    # Create source tarball for makepkg
    # makepkg expects sources to be in $srcdir after extraction
    # We create a tarball of arch-package so %autosetup or prepare can extract it
    cd /home/builduser/repo
    
    su - builduser -c "cd /home/builduser/repo && makepkg --printsrcinfo > .SRCINFO"
    su - builduser -c "cd /home/builduser/repo && makepkg -f --noconfirm"
    
    # Copy package to releases
    mkdir -p /home/builduser/repo/releases
    cp /home/builduser/repo/*.pkg.tar.zst /home/builduser/repo/releases/ 2>/dev/null || true
    cd -
    
    # Copy releases back to original directory
    mkdir -p releases
    cp /home/builduser/repo/releases/*.pkg.tar.zst releases/ 2>/dev/null || true
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
