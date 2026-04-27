#!/bin/sh
# Build Alpine Package (.apk)
# Run on: Alpine Linux 3.18+
# Designed for native Gitea Actions runner execution

set -e

echo "=== Linux Patch API - Alpine Build Script ==="
echo ""

# Source cargo environment (for rustup-installed toolchain in CI)
if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi

# Check if running on Alpine
if ! command -v abuild &> /dev/null; then
    echo "Installing Alpine build tools..."
    apk add --no-cache alpine-sdk rust cargo openssl-dev openrc git abuild gcc
fi

# Generate abuild signing keys
echo "Generating abuild signing keys..."
apk add --no-cache abuild
abuild-keygen -a -n 2>&1 | tee /tmp/keygen.log
KEYFILE=$(ls /root/.abuild/*.rsa 2>/dev/null | head -1)
if [ -z "$KEYFILE" ]; then
    KEYFILE=$(ls /root/.abuild/-*.rsa 2>/dev/null | head -1)
fi
echo "Found key: $KEYFILE"
echo "PACKAGER_PRIVKEY=\"$KEYFILE\"" > /etc/abuild.conf
cat /etc/abuild.conf

# Setup build environment
echo "Setting up build environment..."
export CBUILDROOT=$(pwd)/.abuild
mkdir -p "$CBUILDROOT"

# Build release binary
if [ -z "$SKIP_CARGO_BUILD" ]; then
    echo "Building release binary..."
    cargo build --release --target x86_64-unknown-linux-musl
else
    echo "Skipping cargo build (SKIP_CARGO_BUILD is set)"
fi

# Create package directory in /home/builduser (accessible by builduser)
PKGDIR=/home/builduser/apk-package
mkdir -p "$PKGDIR"/usr/bin
mkdir -p "$PKGDIR"/etc/linux_patch_api
mkdir -p "$PKGDIR"/etc/init.d

# Copy files
cp target/x86_64-unknown-linux-musl/release/linux-patch-api "$PKGDIR"/usr/bin/
chmod 755 "$PKGDIR"/usr/bin/linux-patch-api
cp configs/linux-patch-api-openrc "$PKGDIR"/etc/init.d/linux-patch-api
chmod 755 "$PKGDIR"/etc/init.d/linux-patch-api
cp configs/whitelist.yaml.example "$PKGDIR"/etc/linux_patch_api/whitelist.yaml

# Use /home/builduser as workspace for APKBUILD
WORKSPACE_DIR=/home/builduser

# Create APKBUILD
echo "Creating APKBUILD..."
cat > APKBUILD << EOF
pkgname=linux-patch-api
pkgver=1.0.0
pkgrel=1
pkgdesc="Secure remote package management API for Linux systems"
url="https://gitea.moon-dragon.us/echo/linux_patch_api"
arch="x86_64"
license="MIT"
makedepends=""
depends="openrc"
source=""

package() {
    install -d "\$pkgdir"/usr/bin
    install -d "\$pkgdir"/etc/linux_patch_api
    install -d "\$pkgdir"/etc/init.d
    cp -r ${WORKSPACE_DIR}/apk-package/usr/bin/* "\$pkgdir"/usr/bin/
    cp -r ${WORKSPACE_DIR}/apk-package/etc/linux_patch_api/* "\$pkgdir"/etc/linux_patch_api/
    cp -r ${WORKSPACE_DIR}/apk-package/etc/init.d/* "\$pkgdir"/etc/init.d/
}
EOF

# Generate checksums for APKBUILD sources
echo "Generating checksums..."

# Build APK package
echo "Building APK package..."

# For CI environments where we may run as root or as a build user
if [ "$(id -u)" = "0" ]; then
    echo "Running as root - creating build user for abuild..."
    adduser -D -s /bin/sh builduser 2>/dev/null || true
    addgroup builduser abuild 2>/dev/null || usermod -aG abuild builduser
    
    # Copy repo contents to builduser home (accessible directory)
    cp -r . /home/builduser/repo/
    chown -R builduser:builduser /home/builduser/repo/
    chown -R builduser:builduser /home/builduser/apk-package/
    
    # Set up builduser home directory for abuild
    mkdir -p /home/builduser/.abuild
    cp /root/.abuild/* /home/builduser/.abuild/ 2>/dev/null || true
    chown -R builduser:builduser /home/builduser/.abuild
    
    KEYFILE=$(ls /home/builduser/.abuild/*.rsa 2>/dev/null | head -1)
    if [ -z "$KEYFILE" ]; then
        KEYFILE=$(ls /home/builduser/.abuild/-*.rsa 2>/dev/null | head -1)
    fi
    
    echo "Key file: $KEYFILE"
    echo "PACKAGER_PRIVKEY=\"$KEYFILE\"" > /home/builduser/.abuild/abuild.conf
    chown builduser:builduser /home/builduser/.abuild/abuild.conf
    
    # Copy APKBUILD and checksums to builduser home for abuild
    cp APKBUILD /home/builduser/
    cp .checksums /home/builduser/ 2>/dev/null || true
    
    # Install public key BEFORE abuild (fixes UNTRUSTED signature)
    cp /home/builduser/.abuild/*.rsa.pub /etc/apk/keys/ 2>/dev/null || true
    
    # Run abuild as builduser in /home/builduser where APKBUILD exists
    # Use || true because index update may fail but APK is still created
    su - builduser -c "cd /home/builduser && abuild checksum && abuild -d -F" || true
    
    # Copy APK from builduser packages to releases
    mkdir -p releases
    cp /home/builduser/packages/x86_64/*.apk releases/ 2>/dev/null || cp /home/builduser/packages/*.apk releases/ 2>/dev/null || find /home/builduser/packages -name "*.apk" -exec cp {} releases/ \; 2>/dev/null || true
else
    abuild checksum
    abuild -F -r
    cp ~/packages/x86_64/*.apk releases/ 2>/dev/null || cp ~/packages/*.apk releases/ 2>/dev/null || true
fi

# Copy to releases directory (fallback for non-root builds)
echo ""
echo "Copying package to releases/..."
mkdir -p releases
cp ~/packages/x86_64/*.apk releases/ 2>/dev/null || cp ~/packages/*.apk releases/ 2>/dev/null || find ~/packages -name "*.apk" -exec cp {} releases/ \; 2>/dev/null || true

echo ""
echo "=== Build Complete ==="
echo "Package: releases/linux-patch-api-*.apk"
echo ""
echo "Install with:"
echo "  sudo apk add --allow-unstable ./releases/linux-patch-api-*.apk"
