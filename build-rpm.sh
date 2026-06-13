#!/bin/bash
# Build RPM Package for RHEL/CentOS/Fedora
# Run on: RHEL 8/9, CentOS 8/9, Fedora 38+
# Designed for native Gitea Actions runner execution
#
# Build pattern: Pre-build binary BEFORE creating tarball (like Alpine/Arch)
# The binary is included in the source tarball so rpmbuild's %build
# section is a no-op. This avoids PATH issues where rpmbuild can't find
# cargo installed via rustup.

set -e

echo "=== Linux Patch API - RPM Build Script ==="
echo ""

# Source cargo environment (for rustup-installed toolchain in CI)
if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi

# Check if running on RPM-based system
if ! command -v rpmbuild &> /dev/null; then
    echo "Installing RPM build tools..."
    if command -v dnf &> /dev/null; then
        dnf install -y rpm-build
    elif command -v yum &> /dev/null; then
        yum install -y rpm-build
    else
        echo "Error: Cannot install rpm-build. Please install manually."
        exit 1
    fi
fi

# Get version from Cargo.toml
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*=.*"\([^"]*\)".*/\1/')
if [ -z "$VERSION" ]; then
    echo "Error: Could not determine version from Cargo.toml"
    exit 1
fi
echo "Building version: $VERSION"

# RPM does not allow hyphens in Version field; replace first hyphen with tilde
# (tilde sorts before release in RPM, so 1.5.0~beta.1 < 1.5.0)
RPM_VERSION=$(echo "$VERSION" | sed 's/-/~/')
echo "RPM package version: $RPM_VERSION"

# Remove stale RPM artifacts to prevent uploading cached/old packages
echo "Cleaning stale RPM artifacts..."
rm -f ~/rpmbuild/RPMS/x86_64/linux-patch-api-*.rpm
rm -f releases/linux-patch-api-*.rpm

# Build release binary (skip if already built by CI)
if [ -z "$SKIP_CARGO_BUILD" ]; then
    echo "Building release binary..."
    cargo build --release
else
    echo "Skipping cargo build (SKIP_CARGO_BUILD is set)"
fi

# Verify binary exists
if [ ! -f "target/release/linux-patch-api" ]; then
    echo "Error: Pre-built binary not found at target/release/linux-patch-api"
    echo "Run 'cargo build --release' first or unset SKIP_CARGO_BUILD"
    exit 1
fi

# Setup RPM build directory structure
mkdir -p ~/rpmbuild/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

# Create source tarball with pre-built binary included
# (required by %autosetup in spec file)
echo "Creating source tarball with pre-built binary..."
TMPDIR=$(mktemp -d)
mkdir -p "$TMPDIR/linux-patch-api-${RPM_VERSION}"

# Copy files excluding unnecessary directories
cp -r . "$TMPDIR/linux-patch-api-${RPM_VERSION}/"

# Remove unnecessary directories from tarball
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/target"
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/.git"
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/releases"
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/.github"
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/debian"
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/arch-package"
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/.abuild"
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/apk-package"
rm -rf "$TMPDIR/linux-patch-api-${RPM_VERSION}/.a0proj"

# Re-create target/release with just the pre-built binary
# This is the key change: binary is in the tarball so %build is a no-op
mkdir -p "$TMPDIR/linux-patch-api-${RPM_VERSION}/target/release"
cp target/release/linux-patch-api "$TMPDIR/linux-patch-api-${RPM_VERSION}/target/release/"
chmod 755 "$TMPDIR/linux-patch-api-${RPM_VERSION}/target/release/linux-patch-api"

tar -czf ~/rpmbuild/SOURCES/linux-patch-api-${RPM_VERSION}.tar.gz -C "$TMPDIR" "linux-patch-api-${RPM_VERSION}"
rm -rf "$TMPDIR"

# Prepare spec file with dynamic version
echo "Preparing spec file..."
sed "s/VERSION_PLACEHOLDER/$RPM_VERSION/" linux-patch-api.spec > ~/rpmbuild/SPECS/linux-patch-api.spec

# Verify VERSION replacement succeeded
if grep -q 'VERSION_PLACEHOLDER' ~/rpmbuild/SPECS/linux-patch-api.spec; then
    echo "Error: VERSION_PLACEHOLDER not replaced in spec file!"
    exit 1
fi
echo "Spec file version verified: $RPM_VERSION"

# Build RPM
echo "Building RPM package..."
rpmbuild -ba ~/rpmbuild/SPECS/linux-patch-api.spec

# Verify RPM was actually built
RPM_FILE=$(ls ~/rpmbuild/RPMS/x86_64/linux-patch-api-${RPM_VERSION}-*.rpm 2>/dev/null | head -1)
if [ -z "$RPM_FILE" ]; then
    echo "Error: RPM package not found after build!"
    echo "Looking for: ~/rpmbuild/RPMS/x86_64/linux-patch-api-${RPM_VERSION}-*.rpm"
    ls -la ~/rpmbuild/RPMS/x86_64/ 2>/dev/null || echo "Directory empty or missing"
    exit 1
fi

# Verify RPM contains the correct version
RPM_QUERY_VERSION=$(rpm -qp --queryformat '%{VERSION}' "$RPM_FILE" 2>/dev/null || true)
echo "RPM built: $RPM_FILE"
echo "RPM version: $RPM_QUERY_VERSION"
if [ "$RPM_QUERY_VERSION" != "$RPM_VERSION" ]; then
    echo "Error: RPM version ($RPM_QUERY_VERSION) does not match expected version ($RPM_VERSION)!"
    exit 1
fi

# Copy to releases directory
echo ""
echo "Copying package to releases/..."
mkdir -p releases
cp ~/rpmbuild/RPMS/x86_64/*.rpm releases/

echo ""
echo "=== Build Complete ==="
echo "Package: releases/linux-patch-api-*.rpm"
echo ""
echo "Install with:"
echo "  dnf install -y ./releases/linux-patch-api-*.rpm"
echo "  # or"
echo "  yum install -y ./releases/linux-patch-api-*.rpm"
