#!/bin/sh
# Build Alpine Package (.apk)
# Run on: Alpine Linux 3.18+
# Designed for native Gitea Actions runner execution

set -e

echo "=== Linux Patch API - Alpine Build Script ==="
echo ""

# Clean stale packages from previous builds
rm -f releases/linux-patch-api-*.apk 2>/dev/null || true
rm -f /home/builduser/packages/home/x86_64/linux-patch-api-*.apk 2>/dev/null || true

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

# Get version from Cargo.toml
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*=.*"\([^"]*\)".*/\1/')

# Create package directory structure
PKGDIR=$(pwd)/apk-package
rm -rf "$PKGDIR"
mkdir -p "$PKGDIR"/usr/bin
mkdir -p "$PKGDIR"/etc/linux_patch_api/certs
mkdir -p "$PKGDIR"/etc/init.d
mkdir -p "$PKGDIR"/var/lib/linux_patch_api
mkdir -p "$PKGDIR"/var/log/linux_patch_api

# Copy binary
chmod 755 target/x86_64-unknown-linux-musl/release/linux-patch-api
cp target/x86_64-unknown-linux-musl/release/linux-patch-api "$PKGDIR"/usr/bin/

# Copy OpenRC init script
cp configs/linux-patch-api-openrc "$PKGDIR"/etc/init.d/linux-patch-api
chmod 755 "$PKGDIR"/etc/init.d/linux-patch-api

# Copy example configs (as .example files - install script creates live configs)
cp configs/config.yaml.example "$PKGDIR"/etc/linux_patch_api/config.yaml.example
cp configs/whitelist.yaml.example "$PKGDIR"/etc/linux_patch_api/whitelist.yaml.example

# Prepare workspace for abuild
WORKSPACE_DIR=/home/builduser/repo
rm -rf "$WORKSPACE_DIR"
mkdir -p "$WORKSPACE_DIR"

# Copy package directory to workspace
cp -r "$PKGDIR" "$WORKSPACE_DIR"/apk-package

# Copy install scripts to workspace (must be co-located with APKBUILD)
# Alpine abuild requires SEPARATE files with valid suffixes:
#   pkgname.pre-install, pkgname.post-install, pkgname.pre-deinstall, pkgname.post-deinstall
cp configs/linux-patch-api.pre-install "$WORKSPACE_DIR"/linux-patch-api.pre-install
cp configs/linux-patch-api.post-install "$WORKSPACE_DIR"/linux-patch-api.post-install
cp configs/linux-patch-api.pre-deinstall "$WORKSPACE_DIR"/linux-patch-api.pre-deinstall
cp configs/linux-patch-api.post-deinstall "$WORKSPACE_DIR"/linux-patch-api.post-deinstall

# Create APKBUILD in workspace directory (co-located with install scripts)
echo "Creating APKBUILD..."
cat > "$WORKSPACE_DIR"/APKBUILD << EOF
pkgname=linux-patch-api
pkgver=${VERSION}
pkgrel=1
pkgdesc="Secure remote package management API for Linux systems"
url="https://gitea.moon-dragon.us/echo/linux_patch_api"
arch="x86_64"
license="MIT"
makedepends=""
depends="openrc"
install="linux-patch-api.pre-install linux-patch-api.post-install linux-patch-api.pre-deinstall linux-patch-api.post-deinstall"
subpackages=""
source=""

package() {
    install -d "\$pkgdir"/usr/bin
    install -d "\$pkgdir"/etc/linux_patch_api/certs
    install -d "\$pkgdir"/etc/init.d
    install -d "\$pkgdir"/var/lib/linux_patch_api
    install -d "\$pkgdir"/var/log/linux_patch_api

    install -Dm755 "\$startdir"/apk-package/usr/bin/linux-patch-api "\$pkgdir"/usr/bin/linux-patch-api
    install -Dm755 "\$startdir"/apk-package/etc/init.d/linux-patch-api "\$pkgdir"/etc/init.d/linux-patch-api
    install -Dm644 "\$startdir"/apk-package/etc/linux_patch_api/config.yaml.example "\$pkgdir"/etc/linux_patch_api/config.yaml.example
    install -Dm644 "\$startdir"/apk-package/etc/linux_patch_api/whitelist.yaml.example "\$pkgdir"/etc/linux_patch_api/whitelist.yaml.example
}
EOF

# Build APK package
echo "Building APK package..."

# For CI environments where we may run as root or as a build user
if [ "$(id -u)" = "0" ]; then
    echo "Running as root - creating build user for abuild..."
    adduser -D -s /bin/sh builduser 2>/dev/null || true
    addgroup builduser abuild 2>/dev/null || usermod -aG abuild builduser
    
    # Set ownership of workspace
    chown -R builduser:builduser "$WORKSPACE_DIR"
    
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
    
    # Install public key BEFORE abuild (fixes UNTRUSTED signature)
    cp /home/builduser/.abuild/*.rsa.pub /etc/apk/keys/ 2>/dev/null || true
    
    # Run abuild as builduser in workspace directory
    su - builduser -c "cd $WORKSPACE_DIR && abuild checksum && abuild -d"
    
    # Copy APK from builduser packages to releases
    mkdir -p releases
    cp /home/builduser/packages/home/x86_64/*.apk releases/ 2>/dev/null || find /home/builduser/packages -name "*.apk" -exec cp {} releases/ \; 2>/dev/null || true
else
    cd "$WORKSPACE_DIR"
    abuild checksum
    abuild -r
    cd -
    mkdir -p releases
    cp ~/packages/x86_64/*.apk releases/ 2>/dev/null || cp ~/packages/*.apk releases/ 2>/dev/null || true
fi

echo ""
echo "=== Build Complete ==="
echo "Package: releases/linux-patch-api-*.apk"
echo ""
echo "Install with:"
echo "  sudo apk add ./releases/linux-patch-api-*.apk"
