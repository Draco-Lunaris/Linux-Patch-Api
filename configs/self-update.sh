#!/bin/bash
# Linux Patch API — Self-Update Script
# Runs in its own systemd unit (linux-patch-api-update.service),
# in its own cgroup under system.slice. The agent process will be
# killed by dpkg's prerm during the upgrade; this script survives.
#
# Downloads the correct package from GitHub Releases and installs
# it via the native package manager.

set -uo pipefail

MARKER_PATH="/var/lib/linux_patch_api/last_self_update.json"
REQUEST_PATH="/var/lib/linux_patch_api/self-update.request"
PKG_NAME="linux-patch-api"
GITHUB_OWNER="Draco-Lunaris"
GITHUB_REPO="Linux-Patch-Api"

# --- Helper: write failure marker ---
write_failure_marker() {
    local prev_ver="$1"
    local new_ver="$2"
    local error_msg="$3"
    local timestamp
    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    # Escape quotes and backslashes in error message for safe JSON
    local escaped_error
    escaped_error=$(printf '%s' "$error_msg" | sed 's/\\/\\\\/g; s/"/\\"/g')
    local escaped_prev
    escaped_prev=$(printf '%s' "$prev_ver" | sed 's/\\/\\\\/g; s/"/\\"/g')
    local escaped_new
    escaped_new=$(printf '%s' "$new_ver" | sed 's/\\/\\\\/g; s/"/\\"/g')
    printf '{\n  "previous_version": "%s",\n  "new_version": "%s",\n  "changed": false,\n  "status": "failed",\n  "error": "%s",\n  "at": "%s"\n}\n' \
        "$escaped_prev" "$escaped_new" "$escaped_error" "$timestamp" > "$MARKER_PATH"
}

# --- Helper: write success marker ---
write_success_marker() {
    local prev_ver="$1"
    local new_ver="$2"
    local changed="$3"
    local timestamp
    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    # Escape quotes and backslashes for safe JSON
    local escaped_prev
    escaped_prev=$(printf '%s' "$prev_ver" | sed 's/\\/\\\\/g; s/"/\\"/g')
    local escaped_new
    escaped_new=$(printf '%s' "$new_ver" | sed 's/\\/\\\\/g; s/"/\\"/g')
    printf '{\n  "previous_version": "%s",\n  "new_version": "%s",\n  "changed": %s,\n  "status": "success",\n  "error": null,\n  "at": "%s"\n}\n' \
        "$escaped_prev" "$escaped_new" "$changed" "$timestamp" > "$MARKER_PATH"
}

# --- Read request ---
if [ ! -f "$REQUEST_PATH" ]; then
    echo "No self-update request file found" >&2
    write_failure_marker "unknown" "unknown" "No request file"
    exit 1
fi

# Read target_version from JSON request file
# Fail loudly if python3 is unavailable or parse fails
TARGET_VERSION=$(python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print(d.get('target_version') or '')" "$REQUEST_PATH" 2>&1)
if [ $? -ne 0 ]; then
    echo "Failed to parse request file: $TARGET_VERSION" >&2
    write_failure_marker "unknown" "unknown" "Failed to parse request file"
    exit 1
fi

# --- Validate target_version (prevent shell injection) ---
if [ -n "$TARGET_VERSION" ]; then
    if ! echo "$TARGET_VERSION" | grep -qE '^[a-zA-Z0-9][a-zA-Z0-9+.:~_-]*$'; then
        echo "Invalid target version: $TARGET_VERSION" >&2
        write_failure_marker "unknown" "unknown" "Invalid target version"
        exit 1
    fi
fi

# --- Record previous version ---
PREV_VERSION=$(dpkg-query -W -f='${Version}' "$PKG_NAME" 2>/dev/null || rpm -q --qf '%{VERSION}-%{RELEASE}' "$PKG_NAME" 2>/dev/null || pacman -Q "$PKG_NAME" 2>/dev/null | awk '{print $2}' || apk info -v "$PKG_NAME" 2>/dev/null | head -1 || echo "unknown")

# --- Detect package manager and distro ---
if command -v apt-get >/dev/null 2>&1; then
    PKG_MGR="apt"
elif command -v dnf >/dev/null 2>&1; then
    PKG_MGR="dnf"
elif command -v yum >/dev/null 2>&1; then
    PKG_MGR="yum"
elif command -v apk >/dev/null 2>&1; then
    PKG_MGR="apk"
elif command -v pacman >/dev/null 2>&1; then
    PKG_MGR="pacman"
else
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" "No supported package manager"
    exit 1
fi

# --- Detect distro ID and version ---
DISTRO_ID=""
DISTRO_VERSION=""
if [ -f /etc/os-release ]; then
    . /etc/os-release
    DISTRO_ID="$ID"
    DISTRO_VERSION="${VERSION_ID:-}"
fi

# --- Determine asset pattern based on distro ---
ASSET_PATTERN=""
case "$DISTRO_ID" in
    ubuntu)
        case "$DISTRO_VERSION" in
            24.04) ASSET_PATTERN="*_u2404_amd64.deb" ;;
            22.04) ASSET_PATTERN="*_u2204_amd64.deb" ;;
            *) ASSET_PATTERN="*_u*_amd64.deb" ;;
        esac
        ;;
    debian)
        case "$DISTRO_VERSION" in
            13) ASSET_PATTERN="*_debian13_amd64.deb" ;;
            12) ASSET_PATTERN="*_debian12_amd64.deb" ;;
            *) ASSET_PATTERN="*_debian*_amd64.deb" ;;
        esac
        ;;
    fedora)
        ASSET_PATTERN="*.fc*.x86_64.rpm"
        ;;
    almalinux|rhel|centos|rocky|almalinux)
        ASSET_PATTERN="*.el*.x86_64.rpm"
        ;;
    alpine)
        ASSET_PATTERN="*_r*.apk"
        ;;
    arch|manjaro|garuda)
        ASSET_PATTERN="*.pkg.tar.zst"
        ;;
    *)
        # Fallback: infer from package manager
        case "$PKG_MGR" in
            apt) ASSET_PATTERN="*_amd64.deb" ;;
            dnf|yum) ASSET_PATTERN="*.x86_64.rpm" ;;
            apk) ASSET_PATTERN="*.apk" ;;
            pacman) ASSET_PATTERN="*.pkg.tar.zst" ;;
        esac
        ;;
esac

echo "Detected: distro=$DISTRO_ID version=$DISTRO_VERSION pkg_mgr=$PKG_MGR pattern=$ASSET_PATTERN"

# --- Determine GitHub API URL ---
if [ -n "$TARGET_VERSION" ]; then
    GH_TAG="v${TARGET_VERSION}"
    API_URL="https://api.github.com/repos/${GITHUB_OWNER}/${GITHUB_REPO}/releases/tags/${GH_TAG}"
else
    GH_TAG=""
    API_URL="https://api.github.com/repos/${GITHUB_OWNER}/${GITHUB_REPO}/releases/latest"
fi

# --- Query GitHub Releases API with retry ---
API_RESPONSE=""
API_RC=1
for attempt in 1 2 3; do
    echo "Querying GitHub API (attempt $attempt): $API_URL"
    API_RESPONSE=$(curl -sL -H "Accept: application/vnd.github+json" "$API_URL" 2>&1)
    API_RC=$?
    if [ $API_RC -eq 0 ]; then
        # Check for rate limit error
        if echo "$API_RESPONSE" | grep -q '"rate limit"' 2>/dev/null; then
            echo "GitHub API rate limited, will retry..." >&2
            if [ $attempt -lt 3 ]; then
                sleep 2
                continue
            fi
        elif echo "$API_RESPONSE" | grep -q '"assets"' 2>/dev/null; then
            break
        fi
    fi
    if [ $attempt -lt 3 ]; then
        sleep 2
    fi
done

# --- Parse asset URL from API response or fall back to direct URL ---
ASSET_URL=""
ASSET_NAME=""

if echo "$API_RESPONSE" | grep -q '"assets"' 2>/dev/null; then
    # Parse assets array with python3
    ASSET_JSON=$(python3 -c "
import json, sys, fnmatch
data = json.loads(sys.stdin.read())
pattern = sys.argv[1]
for asset in data.get('assets', []):
    name = asset.get('name', '')
    if fnmatch.fnmatch(name, pattern):
        # For Arch, skip debug packages
        if '-debug-' in name:
            continue
        print(asset.get('browser_download_url', ''))
        print(name)
        break
" "$ASSET_PATTERN" <<< "$API_RESPONSE" 2>/dev/null)
    if [ $? -eq 0 ] && [ -n "$ASSET_JSON" ]; then
        ASSET_URL=$(echo "$ASSET_JSON" | head -1)
        ASSET_NAME=$(echo "$ASSET_JSON" | tail -1)
    fi
fi

# Fall back to constructing download URL directly
if [ -z "$ASSET_URL" ]; then
    echo "Could not find asset via API, falling back to direct URL construction..." >&2
    # We need the tag for the direct URL
    if [ -z "$GH_TAG" ]; then
        # Try to get tag name from API response
        GH_TAG=$(python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('tag_name',''))" <<< "$API_RESPONSE" 2>/dev/null)
    fi
    if [ -z "$GH_TAG" ]; then
        echo "Cannot determine release tag for fallback URL" >&2
        write_failure_marker "$PREV_VERSION" "$PREV_VERSION" "Could not find release asset or tag"
        exit 1
    fi
    # Try to list assets from API response to find the matching filename
    ASSET_NAME=$(python3 -c "
import json, sys, fnmatch
data = json.loads(sys.stdin.read())
pattern = sys.argv[1]
for asset in data.get('assets', []):
    name = asset.get('name', '')
    if fnmatch.fnmatch(name, pattern):
        if '-debug-' in name:
            continue
        print(name)
        break
" "$ASSET_PATTERN" <<< "$API_RESPONSE" 2>/dev/null)
    if [ -z "$ASSET_NAME" ]; then
        echo "Could not find matching asset name in release" >&2
        write_failure_marker "$PREV_VERSION" "$PREV_VERSION" "No matching package asset found"
        exit 1
    fi
    ASSET_URL="https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}/releases/download/${GH_TAG}/${ASSET_NAME}"
fi

if [ -z "$ASSET_URL" ] || [ -z "$ASSET_NAME" ]; then
    echo "Failed to determine asset URL or name" >&2
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" "Failed to find download asset"
    exit 1
fi

echo "Downloading: $ASSET_URL"
echo "Asset: $ASSET_NAME"

# --- Download package to /tmp ---
DOWNLOAD_PATH="/tmp/${ASSET_NAME}"
rm -f "$DOWNLOAD_PATH"
curl -sL -o "$DOWNLOAD_PATH" "$ASSET_URL" 2>&1
if [ $? -ne 0 ] || [ ! -s "$DOWNLOAD_PATH" ]; then
    echo "Download failed" >&2
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" "Package download failed"
    exit 1
fi

echo "Downloaded to: $DOWNLOAD_PATH ($(stat -c%s "$DOWNLOAD_PATH" 2>/dev/null || echo 'unknown') bytes)"

# --- Install via native package manager ---
UPGRADE_OUTPUT=""
UPGRADE_RC=0
case "$PKG_MGR" in
    apt)
        UPGRADE_OUTPUT=$(dpkg -i "$DOWNLOAD_PATH" 2>&1) || UPGRADE_RC=$?
        if [ $UPGRADE_RC -ne 0 ]; then
            # Try to fix dependency issues
            DEP_OUTPUT=$(apt-get -f install -y 2>&1) || true
            UPGRADE_OUTPUT="$UPGRADE_OUTPUT\n$DEP_OUTPUT"
            # Recheck if dpkg is now configured
            dpkg-query -W -f='${Status}' "$PKG_NAME" 2>/dev/null | grep -q 'install ok installed' || UPGRADE_RC=1
        fi
        ;;
    dnf)
        UPGRADE_OUTPUT=$(dnf install -y "$DOWNLOAD_PATH" 2>&1) || UPGRADE_RC=$?
        ;;
    yum)
        UPGRADE_OUTPUT=$(yum install -y "$DOWNLOAD_PATH" 2>&1) || UPGRADE_RC=$?
        ;;
    apk)
        UPGRADE_OUTPUT=$(apk add --allow-untrusted "$DOWNLOAD_PATH" 2>&1) || UPGRADE_RC=$?
        ;;
    pacman)
        UPGRADE_OUTPUT=$(pacman -U --noconfirm "$DOWNLOAD_PATH" 2>&1) || UPGRADE_RC=$?
        ;;
esac

# --- Clean up downloaded package ---
rm -f "$DOWNLOAD_PATH"

if [ $UPGRADE_RC -ne 0 ]; then
    echo "Install failed (rc=$UPGRADE_RC): $UPGRADE_OUTPUT" >&2
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" "Package install failed (rc=$UPGRADE_RC)"
    exit 1
fi

# --- Record new version ---
NEW_VERSION=$(dpkg-query -W -f='${Version}' "$PKG_NAME" 2>/dev/null || rpm -q --qf '%{VERSION}-%{RELEASE}' "$PKG_NAME" 2>/dev/null || pacman -Q "$PKG_NAME" 2>/dev/null | awk '{print $2}' || apk info -v "$PKG_NAME" 2>/dev/null | head -1 || echo "unknown")

# --- Determine if version changed ---
CHANGED=false
if [ "$PREV_VERSION" != "$NEW_VERSION" ]; then
    CHANGED=true
fi

# --- Write success marker ---
write_success_marker "$PREV_VERSION" "$NEW_VERSION" "$CHANGED"

# --- Clean up request file ---
rm -f "$REQUEST_PATH"

echo "Self-update complete: $PREV_VERSION -> $NEW_VERSION (changed=$CHANGED)"
exit 0
