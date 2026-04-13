#!/bin/sh
# Build Alpine Package (.apk)
# Run on: Alpine Linux 3.18+
# Or in Docker: docker run -v $(pwd):/build alpine:latest /build/build-alpine.sh

set -e

echo "=== Linux Patch API - Alpine Build Script ==="
echo ""

# Source cargo environment (for rustup-installed toolchain in CI)
if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi

# Check if running on Alpine

# Check if running on Alpine
if ! command -v abuild &> /dev/null; then
    echo "Installing Alpine build tools..."
    apk add --no-cache alpine-sdk rust cargo openssl-dev openrc git
fi

# Setup build environment
echo "Setting up build environment..."
export CBUILDROOT=$(pwd)/.abuild
mkdir -p "$CBUILDROOT"

# Build release binary
echo "Building release binary..."
cargo build --release --target x86_64-unknown-linux-musl

# Create package directory
PKGDIR=$(pwd)/apk-package
mkdir -p "$PKGDIR"/usr/bin
mkdir -p "$PKGDIR"/etc/linux_patch_api
mkdir -p "$PKGDIR"/etc/init.d

# Copy files
cp target/x86_64-unknown-linux-musl/release/linux-patch-api "$PKGDIR"/usr/bin/
chmod 755 "$PKGDIR"/usr/bin/linux-patch-api
cp configs/linux-patch-api-openrc "$PKGDIR"/etc/init.d/linux-patch-api
chmod 755 "$PKGDIR"/etc/init.d/linux-patch-api
cp configs/whitelist.yaml.example "$PKGDIR"/etc/linux_patch_api/whitelist.yaml

# Create APKBUILD
echo "Creating APKBUILD..."
cat > APKBUILD << 'EOF'
pkgname=linux-patch-api
pkgver=1.0.0
pkgrel=1
pkgdesc="Secure remote package management API for Linux systems"
url="https://gitea.internal/linux-patch-api"
arch="x86_64"
license="MIT"
depends="openrc"
source="apk-package"

package() {
    cp -r "$srcdir"/apk-package/* "$pkgdir"/
}
EOF

# Build APK package
echo "Building APK package..."
abuild -F -r

# Copy to releases directory
echo ""
echo "Copying package to releases/..."
mkdir -p releases
cp ~/packages/x86_64/*.apk releases/ 2>/dev/null || cp /root/packages/x86_64/*.apk releases/ || find / -name "linux-patch-api-*.apk" -exec cp {} releases/ \; 2>/dev/null || true

echo ""
echo "=== Build Complete ==="
echo "Package: releases/linux-patch-api-*.apk"
echo ""
echo "Install with:"
echo "  sudo apk add --allow-unstable ./releases/linux-patch-api-*.apk"
