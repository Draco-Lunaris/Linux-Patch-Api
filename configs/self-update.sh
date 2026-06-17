#!/bin/bash
# Linux Patch API — Self-Update Script
# Runs in its own systemd unit (linux-patch-api-update.service),
# in its own cgroup under system.slice. The agent process will be
# killed by dpkg's prerm during the upgrade; this script survives.

set -uo pipefail

MARKER_PATH="/var/lib/linux_patch_api/last_self_update.json"
REQUEST_PATH="/var/lib/linux_patch_api/self-update.request"
PKG_NAME="linux-patch-api"

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
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" "No supported package manager"
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

# --- Run upgrade (with error trap) ---
UPGRADE_OUTPUT=""
UPGRADE_RC=0
case "$PKG_MGR" in
    apt)
        if [ -n "$TARGET_VERSION" ]; then
            UPGRADE_OUTPUT=$(apt-get install -y --allow-downgrades -- "$PKG_NAME=$TARGET_VERSION" 2>&1) || UPGRADE_RC=$?
        else
            UPGRADE_OUTPUT=$(apt-get install -y --only-upgrade -- "$PKG_NAME" 2>&1) || UPGRADE_RC=$?
        fi
        ;;
    dnf)
        if [ -n "$TARGET_VERSION" ]; then
            UPGRADE_OUTPUT=$(dnf upgrade -y -- "$PKG_NAME-$TARGET_VERSION" 2>&1) || UPGRADE_RC=$?
        else
            UPGRADE_OUTPUT=$(dnf upgrade -y -- "$PKG_NAME" 2>&1) || UPGRADE_RC=$?
        fi
        ;;
    yum)
        if [ -n "$TARGET_VERSION" ]; then
            UPGRADE_OUTPUT=$(yum update -y -- "$PKG_NAME-$TARGET_VERSION" 2>&1) || UPGRADE_RC=$?
        else
            UPGRADE_OUTPUT=$(yum update -y -- "$PKG_NAME" 2>&1) || UPGRADE_RC=$?
        fi
        ;;
    apk)
        if [ -n "$TARGET_VERSION" ]; then
            UPGRADE_OUTPUT=$(apk add -- "$PKG_NAME=$TARGET_VERSION" 2>&1) || UPGRADE_RC=$?
        else
            UPGRADE_OUTPUT=$(apk upgrade -- "$PKG_NAME" 2>&1) || UPGRADE_RC=$?
        fi
        ;;
    pacman)
        UPGRADE_OUTPUT=$(pacman -S --noconfirm -- "$PKG_NAME" 2>&1) || UPGRADE_RC=$?
        ;;
esac

if [ $UPGRADE_RC -ne 0 ]; then
    echo "Upgrade failed (rc=$UPGRADE_RC): $UPGRADE_OUTPUT" >&2
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" "Package upgrade failed (rc=$UPGRADE_RC)"
    exit 1
fi

# --- Record new version ---
NEW_VERSION=$(dpkg-query -W -f='${Version}' "$PKG_NAME" 2>/dev/null || rpm -q --qf '%{VERSION}-%{RELEASE}' "$PKG_NAME" 2>/dev/null || echo "unknown")

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
