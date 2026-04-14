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
# Check if running on Alpine
if ! command -v abuild &> /dev/null; then
    echo "Installing Alpine build tools..."
    apk add --no-cache alpine-sdk rust cargo openssl-dev openrc git
fi

# Generate abuild signing keys (ALWAYS generate fresh - same shell session as abuild commands)
echo "Generating abuild signing keys..."
apk add --no-cache abuild
abuild-keygen -a -n 2>&1 | tee /tmp/keygen.log
# Find the actual key file (handles missing username prefix)
KEYFILE=$(ls /root/.abuild/*.rsa 2>/dev/null | head -1)
if [ -z "$KEYFILE" ]; then
    KEYFILE=$(ls /root/.abuild/-*.rsa 2>/dev/null | head -1)
fi
echo "Found key: $KEYFILE"
# Write directly to abuild.conf (overwrite any stale config)
echo "PACKAGER_PRIVKEY=\"$KEYFILE\"" > /etc/abuild.conf
cat /etc/abuild.conf

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
source=""

package() {
    # Copy from workspace path (not srcdir - apk-package is created dynamically)
    cp -r /workspace/echo/linux_patch_api/apk-package/* "$pkgdir"/
}
EOF

# Generate checksums for APKBUILD sources
echo "Generating checksums..."

# Build APK package
echo "Building APK package..."

# For CI/container environments where we run as root, create a build user
if [ "$(id -u)" = "0" ]; then
    echo "Running as root - creating build user for abuild..."
    adduser -D -s /bin/sh builduser 2>/dev/null || true
    chown -R builduser:builduser "$(pwd)"
    chown -R builduser:builduser /root/packages 2>/dev/null || true
    # Copy abuild keys from root to builduser home
    mkdir -p /home/builduser/.abuild
    cp /root/.abuild/* /home/builduser/.abuild/
    chown -R builduser:builduser /home/builduser/.abuild
    
    # Find the actual key file
    KEYFILE=$(ls /home/builduser/.abuild/*.rsa 2>/dev/null | head -1)
    if [ -z "$KEYFILE" ]; then
        KEYFILE=$(ls /home/builduser/.abuild/-*.rsa 2>/dev/null | head -1)
    fi
    
    echo "Key file: $KEYFILE"
    echo "Key file exists: $(test -f "$KEYFILE" && echo YES || echo NO)"
    
    # CRITICAL: Write to builduser's PERSONAL abuild.conf (~/.abuild/abuild.conf)
    # abuild reads this when running as builduser - standard behavior, no shell quoting issues!
    echo "PACKAGER_PRIVKEY=\"$KEYFILE\"" > /home/builduser/.abuild/abuild.conf
    chown builduser:builduser /home/builduser/.abuild/abuild.conf
    echo "builduser abuild.conf:"
    cat /home/builduser/.abuild/abuild.conf
    
    su - builduser -c "cd $(pwd) && abuild checksum && abuild -F -r"
else
    abuild checksum
    abuild -F -r
fi

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
