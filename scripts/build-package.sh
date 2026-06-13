#!/usr/bin/env bash
# =============================================================================
# Linux Patch API — Build .deb Package for Ubuntu 24.04
# =============================================================================
# Produces: linux-patch-api_<version>-1_amd64.deb
# Prerequisites:
#   - Rust toolchain (cargo, rustc >= 1.75)
#   - dpkg-deb
# =============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; exit 1; }

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
RELEASE="1"
PKG_NAME="linux-patch-api"
# Debian does not allow hyphens in Version; replace first hyphen with tilde
# (tilde sorts before release, so 1.5.0~beta.1 < 1.5.0)
DEB_VERSION=$(echo "$VERSION" | sed 's/-/~/')
DEB_NAME="${PKG_NAME}_${DEB_VERSION}-${RELEASE}_amd64.deb"
BUILD_DIR="${PROJECT_ROOT}/package-build"

info "=== Linux Patch API — Package Build ==="
info "Version: ${VERSION}-${RELEASE}"
info "Target:  Ubuntu 24.04 (noble) amd64"
echo

# ---------------------------------------------------------------------------
# 1. Build Rust binary (release mode)
# ---------------------------------------------------------------------------
info "Step 1/4: Building Rust binary (release mode)..."
cd "${PROJECT_ROOT}"
cargo build --release 2>&1 | tail -5

# Verify binary exists
[[ -f "${PROJECT_ROOT}/target/release/linux-patch-api" ]] || error "linux-patch-api not found in target/release/"
info "Rust binary built successfully."

# Strip debug symbols for smaller package
strip "${PROJECT_ROOT}/target/release/linux-patch-api" 2>/dev/null || warn "strip failed (may already be stripped)"
info "Binary stripped."

# ---------------------------------------------------------------------------
# 2. Assemble package directory structure
# ---------------------------------------------------------------------------
info "Step 2/4: Assembling package structure..."
rm -rf "${BUILD_DIR}"
mkdir -p "${BUILD_DIR}/DEBIAN"
mkdir -p "${BUILD_DIR}/usr/bin"
mkdir -p "${BUILD_DIR}/etc/linux_patch_api"
mkdir -p "${BUILD_DIR}/etc/linux_patch_api/certs"
mkdir -p "${BUILD_DIR}/lib/systemd/system"
mkdir -p "${BUILD_DIR}/var/log/linux_patch_api"
mkdir -p "${BUILD_DIR}/var/lib/linux_patch_api"

# Binary
cp "${PROJECT_ROOT}/target/release/linux-patch-api" "${BUILD_DIR}/usr/bin/linux-patch-api"
chmod 755 "${BUILD_DIR}/usr/bin/linux-patch-api"

# Systemd service
cp "${PROJECT_ROOT}/configs/linux-patch-api.service" "${BUILD_DIR}/lib/systemd/system/"

# Configuration files (live configs for admin editing)
cp "${PROJECT_ROOT}/configs/config.yaml.example" "${BUILD_DIR}/etc/linux_patch_api/config.yaml"
cp "${PROJECT_ROOT}/configs/whitelist.yaml.example" "${BUILD_DIR}/etc/linux_patch_api/whitelist.yaml"

# Example config files (referenced by postinst for first-run setup)
cp "${PROJECT_ROOT}/configs/config.yaml.example" "${BUILD_DIR}/etc/linux_patch_api/config.yaml.example"
cp "${PROJECT_ROOT}/configs/whitelist.yaml.example" "${BUILD_DIR}/etc/linux_patch_api/whitelist.yaml.example"

# Calculate installed size BEFORE generating control file
INSTALLED_SIZE=$(du -sk "${BUILD_DIR}" | cut -f1)

# Generate DEBIAN/control from scratch for dpkg-deb --build
# (debian/control uses dpkg-buildpackage substitution variables like
#  ${shlibs:Depends} that dpkg-deb cannot resolve)
cat > "${BUILD_DIR}/DEBIAN/control" <<EOF
Package: linux-patch-api
Version: ${DEB_VERSION}-${RELEASE}
Architecture: amd64
Maintainer: Echo <echo@moon-dragon.us>
Installed-Size: ${INSTALLED_SIZE}
Depends: systemd, libsystemd0
Section: admin
Priority: optional
Homepage: https://github.com/Draco-Lunaris/Linux-Patch-Api
Description: Secure remote package management API for Linux systems
 Linux Patch API provides a secure, mTLS-authenticated REST API for
 remote package management operations including package installation
 and removal, security patch application, system health monitoring,
 and job queue management with WebSocket status streaming.
EOF

# Conffiles
cat > "${BUILD_DIR}/DEBIAN/conffiles" << 'EOF'
/etc/linux_patch_api/config.yaml
/etc/linux_patch_api/whitelist.yaml
EOF

# Maintainer scripts
cp "${PROJECT_ROOT}/debian/postinst" "${BUILD_DIR}/DEBIAN/postinst"
cp "${PROJECT_ROOT}/debian/prerm" "${BUILD_DIR}/DEBIAN/prerm"
cp "${PROJECT_ROOT}/debian/postrm" "${BUILD_DIR}/DEBIAN/postrm"
chmod 755 "${BUILD_DIR}/DEBIAN/postinst" "${BUILD_DIR}/DEBIAN/prerm" "${BUILD_DIR}/DEBIAN/postrm"

info "Package structure assembled (${INSTALLED_SIZE} KB)."

# ---------------------------------------------------------------------------
# 3. Build .deb package
# ---------------------------------------------------------------------------
info "Step 3/4: Building .deb package..."
dpkg-deb --build "${BUILD_DIR}" "${PROJECT_ROOT}/${DEB_NAME}"
info ".deb package created: ${DEB_NAME}"

# ---------------------------------------------------------------------------
# 4. Verify and summarize
# ---------------------------------------------------------------------------
info "Step 4/4: Verifying package..."
dpkg-deb --info "${PROJECT_ROOT}/${DEB_NAME}"
echo
dpkg-deb --contents "${PROJECT_ROOT}/${DEB_NAME}" | head -20 || true
echo

PKG_SIZE=$(du -h "${PROJECT_ROOT}/${DEB_NAME}" | cut -f1)

info "=== Package Build Complete ==="
info "Package: ${DEB_NAME}"
info "Size:   ${PKG_SIZE}"
echo
echo -e "${CYAN}Installation instructions:${NC}"
echo "  1. Copy ${DEB_NAME} to the target Ubuntu 24.04 host"
echo "  2. Install:  sudo dpkg -i ${DEB_NAME}"
echo "  3. Or with auto-deps:  sudo apt install ./${DEB_NAME}"
echo "  4. Configure:  /etc/linux_patch_api/config.yaml"
echo "  5. Start:  systemctl enable --now linux-patch-api.service"
echo

# Cleanup build directory
rm -rf "${BUILD_DIR}"
info "Build directory cleaned up."
