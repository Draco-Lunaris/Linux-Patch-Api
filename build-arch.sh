#!/bin/bash
# Build Arch Linux Package (.pkg.tar.zst)
# Run on: Arch Linux, Manjaro
# Or in Docker: docker run -v $(pwd):/build archlinux:latest /build/build-arch.sh

set -e

echo "=== Linux Patch API - Arch Build Script ==="
echo ""

# Check if running on Arch
if ! command -v makepkg &> /dev/null; then
    echo "Error: makepkg not found. This script must run on Arch Linux."
    echo "Or use Docker: docker run -v \$(pwd):/build archlinux:latest /build/build-arch.sh"
    exit 1
fi

# Install build dependencies
echo "Installing build dependencies..."
pacman -Syu --noconfirm rust cargo systemd git base-devel

# Build release binary
echo "Building release binary..."
cargo build --release

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

# Create PKGBUILD
echo "Creating PKGBUILD..."
cat > PKGBUILD << 'EOF'
pkgname=linux-patch-api
pkgver=1.0.0
pkgrel=1
pkgdesc="Secure remote package management API for Linux systems"
url="https://gitea.internal/linux-patch-api"
arch=('x86_64')
license=('MIT')
depends=('systemd')
source=('arch-package')

package() {
    cp -r "$srcdir"/arch-package/* "$pkgdir"/
}
EOF

# Create .SRCINFO
echo "Creating .SRCINFO..."
makepkg --printsrcinfo --allow-root > .SRCINFO

# Build package
echo "Building Arch package..."
makepkg -f --noconfirm --allow-root

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
