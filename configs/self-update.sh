#!/bin/bash
# Linux Patch API — Self-Update Script
# Runs in its own systemd unit (linux-patch-api-update.service),
# in its own cgroup under system.slice. The agent process will be
# killed by dpkg's prerm during the upgrade; this script survives.

set -euo pipefail

MARKER_PATH="/var/lib/linux_patch_api/last_self_update.json"
REQUEST_PATH="/var/lib/linux_patch_api/self-update.request"
PKG_NAME="linux-patch-api"

# --- Read request ---
if [ ! -f "$REQUEST_PATH" ]; then
    echo "No self-update request file found" >&2
    echo "{\"previous_version\":\"unknown\",\"new_version\":\"unknown\",\"changed\":false,\"status\":\"failed\",\"error\":\"No request file\",\"at\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "$MARKER_PATH"
    exit 1
fi

# Read target_version from JSON request file
TARGET_VERSION=$(python3 -c "import json,sys; d=json.load(open('$REQUEST_PATH')); print(d.get('target_version') or '')" 2>/dev/null || '')

# --- Validate target_version (prevent shell injection) ---
if [ -n "$TARGET_VERSION" ]; then
    if ! echo "$TARGET_VERSION" | grep -qE '^[a-zA-Z0-9][a-zA-Z0-9+.:~_-]*$'; then
        echo "Invalid target version: $TARGET_VERSION" >&2
        echo "{\"previous_version\":\"unknown\",\"new_version\":\"unknown\",\"changed\":false,\"status\":\"failed\",\"error\":\"Invalid target version\",\"at\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "$MARKER_PATH"
        exit 1
    fi
fi

# --- Record previous version ---
PREV_VERSION=$(dpkg-query -W -f='${Version}' "$PKG_NAME" 2>/dev/null || rpm -q --qf '%{VERSION}-%{RELEASE}' "$PKG_NAME" 2>/dev/null || echo "unknown")

# --- Detect package manager ---
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
    echo "{\"previous_version\":\"$PREV_VERSION\",\"new_version\":\"$PREV_VERSION\",\"changed\":false,\"status\":\"failed\",\"error\":\"No supported package manager\",\"at\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "$MARKER_PATH"
    exit 1
fi

# --- Refresh package index ---
case "$PKG_MGR" in
    apt) apt-get update -qq 2>&1 || echo "WARNING: apt-get update failed" >&2 ;;
    dnf) dnf check-update -y 2>&1 || true ;;
    yum) yum check-update -y 2>&1 || true ;;
    apk) apk update 2>&1 || echo "WARNING: apk update failed" >&2 ;;
    pacman) pacman -Sy --noconfirm 2>&1 || echo "WARNING: pacman -Sy failed" >&2 ;;
esac

# --- Run upgrade ---
case "$PKG_MGR" in
    apt)
        if [ -n "$TARGET_VERSION" ]; then
            apt-get install -y --allow-downgrades -- "$PKG_NAME=$TARGET_VERSION" 2>&1
        else
            apt-get install -y --only-upgrade -- "$PKG_NAME" 2>&1
        fi
        ;;
    dnf)
        if [ -n "$TARGET_VERSION" ]; then
            dnf upgrade -y -- "$PKG_NAME-$TARGET_VERSION" 2>&1
        else
            dnf upgrade -y -- "$PKG_NAME" 2>&1
        fi
        ;;
    yum)
        if [ -n "$TARGET_VERSION" ]; then
            yum update -y -- "$PKG_NAME-$TARGET_VERSION" 2>&1
        else
            yum update -y -- "$PKG_NAME" 2>&1
        fi
        ;;
    apk)
        if [ -n "$TARGET_VERSION" ]; then
            apk add -- "$PKG_NAME=$TARGET_VERSION" 2>&1
        else
            apk upgrade -- "$PKG_NAME" 2>&1
        fi
        ;;
    pacman)
        pacman -S --noconfirm -- "$PKG_NAME" 2>&1
        ;;
esac

# --- Record new version ---
NEW_VERSION=$(dpkg-query -W -f='${Version}' "$PKG_NAME" 2>/dev/null || rpm -q --qf '%{VERSION}-%{RELEASE}' "$PKG_NAME" 2>/dev/null || echo "unknown")

# --- Determine if version changed ---
CHANGED=false
if [ "$PREV_VERSION" != "$NEW_VERSION" ]; then
    CHANGED=true
fi

# --- Write marker ---
cat > "$MARKER_PATH" << EOF
{
  "previous_version": "$PREV_VERSION",
  "new_version": "$NEW_VERSION",
  "changed": $CHANGED,
  "status": "success",
  "error": null,
  "at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF

# --- Clean up request file ---
rm -f "$REQUEST_PATH"

echo "Self-update complete: $PREV_VERSION -> $NEW_VERSION (changed=$CHANGED)"
exit 0
