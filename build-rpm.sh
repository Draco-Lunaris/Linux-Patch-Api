#!/bin/bash
# Build RPM Package for RHEL/CentOS/Fedora
# Run on: RHEL 8/9, CentOS 8/9, Fedora 38+

set -e

echo "=== Linux Patch API - RPM Build Script ==="
echo ""

# Check if running on RPM-based system
if ! command -v rpmbuild &> /dev/null; then
    echo "Installing RPM build tools..."
    if command -v dnf &> /dev/null; then
        sudo dnf install -y rpm-build cargo rust gcc systemd-devel
    elif command -v yum &> /dev/null; then
        sudo yum install -y rpm-build cargo rust gcc systemd-devel
    else
        echo "Error: Cannot install rpm-build. Please install manually."
        exit 1
    fi
fi

# Setup RPM build directory structure
mkdir -p ~/rpmbuild/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

# Copy source files
echo "Preparing source files..."
cp linux-patch-api.spec ~/rpmbuild/SPECS/
cp -r target/release/linux-patch-api ~/rpmbuild/SOURCES/ 2>/dev/null || cargo build --release
cp -r configs ~/rpmbuild/SOURCES/
cp -r debian ~/rpmbuild/SOURCES/

# Build RPM
echo "Building RPM package..."
rpmbuild -ba ~/rpmbuild/SPECS/linux-patch-api.spec

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
echo "  sudo dnf install -y ./releases/linux-patch-api-*.rpm"
echo "  # or"
echo "  sudo yum install -y ./releases/linux-patch-api-*.rpm"
