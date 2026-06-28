#!/bin/bash
# Linux Patch API — Self-Update Script (v2: Manager-Hosted Repo)
#
# Runs in its own systemd unit (linux-patch-api-update.service),
# in its own cgroup under system.slice. The agent process will be
# killed by dpkg's prerm during the upgrade; this script survives.
#
# Uses native package manager commands against the manager-hosted repo.
# No GitHub Releases, no curl downloads, no API parsing.
# Package signatures are verified by the native package manager using
# the GPG key provisioned during enrollment.
#
# Security: No eval, no sh -c with interpolated values.
# Version queries use direct commands. Upgrade commands use case/esac
# branches that execute directly — no string interpolation into shell.

set -uo pipefail

MARKER_PATH="/var/lib/linux_patch_api/last_self_update.json"
REQUEST_PATH="/var/lib/linux_patch_api/self-update.request"
PKG_NAME="linux-patch-api"
SERVICE_NAME="linux-patch-api"
HEALTH_CHECK_TIMEOUT=60  # seconds
HEALTH_CHECK_INTERVAL=5   # seconds

# --- Signal handling: write failure marker on kill ---
cleanup_on_signal() {
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" \
        "Self-update interrupted by signal during upgrade"
    rm -f "$REQUEST_PATH"
    exit 1
}
trap cleanup_on_signal TERM INT HUP

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

# --- Helper: get installed version ---
get_installed_version() {
    local ver
    ver=$(dpkg-query -W -f='${Version}' "$PKG_NAME" 2>/dev/null) && echo "$ver" && return
    ver=$(rpm -q --qf '%{VERSION}-%{RELEASE}' "$PKG_NAME" 2>/dev/null) && echo "$ver" && return
    ver=$(pacman -Q "$PKG_NAME" 2>/dev/null | awk '{print $2}') && echo "$ver" && return
    ver=$(apk info -v "$PKG_NAME" 2>/dev/null | head -1) && echo "$ver" && return
    echo "unknown"
}

# --- Read request ---
PREV_VERSION="unknown"

if [ ! -f "$REQUEST_PATH" ]; then
    echo "No self-update request file found" >&2
    write_failure_marker "unknown" "unknown" "No request file"
    exit 1
fi

# Read target_version from JSON request file
TARGET_VERSION=$(python3 -c \
    'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("target_version") or "")' \
    "$REQUEST_PATH" 2>&1)
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
PREV_VERSION=$(get_installed_version)

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

echo "Detected: pkg_mgr=$PKG_MGR prev_version=$PREV_VERSION target=$TARGET_VERSION"

# --- Refresh repo metadata (non-fatal: log warning on failure) ---
case "$PKG_MGR" in
    apt)
        apt-get update -qq 2>&1 || echo "WARNING: apt-get update failed — continuing with cached metadata"
        ;;
    dnf|yum)
        $PKG_MGR makecache 2>&1 || echo "WARNING: $PKG_MGR makecache failed — continuing with cached metadata"
        ;;
    apk)
        apk update 2>&1 || echo "WARNING: apk update failed — continuing with cached metadata"
        ;;
    pacman)
        pacman -Sy 2>&1 || echo "WARNING: pacman -Sy failed — continuing with cached metadata"
        ;;
esac

# --- Execute upgrade (direct commands, NO eval) ---
UPGRADE_RC=0
case "$PKG_MGR" in
    apt)
        if [ -n "$TARGET_VERSION" ]; then
            apt-get install -y --allow-downgrades -- "${PKG_NAME}=${TARGET_VERSION}" 2>&1 || UPGRADE_RC=$?
        else
            apt-get install -y --only-upgrade -- "$PKG_NAME" 2>&1 || UPGRADE_RC=$?
        fi
        ;;
    dnf|yum)
        if [ -n "$TARGET_VERSION" ]; then
            $PKG_MGR install -y -- "${PKG_NAME}-${TARGET_VERSION}" 2>&1 || UPGRADE_RC=$?
        else
            $PKG_MGR upgrade -y -- "$PKG_NAME" 2>&1 || UPGRADE_RC=$?
        fi
        ;;
    apk)
        if [ -n "$TARGET_VERSION" ]; then
            apk add -- "${PKG_NAME}=${TARGET_VERSION}" 2>&1 || UPGRADE_RC=$?
        else
            apk upgrade -- "$PKG_NAME" 2>&1 || UPGRADE_RC=$?
        fi
        ;;
    pacman)
        if [ -n "$TARGET_VERSION" ]; then
            # Pacman does not support = syntax for version pinning.
            # Try to find the specific version in cache or repo.
            # If not available, this will fail gracefully.
            CACHED_PKG=$(find /var/cache/pacman/pkg/ -name "${PKG_NAME}-${TARGET_VERSION}-"*.pkg.tar.zst 2>/dev/null | head -1)
            if [ -n "$CACHED_PKG" ]; then
                pacman -U --noconfirm -- "$CACHED_PKG" 2>&1 || UPGRADE_RC=$?
            else
                echo "WARNING: Pacman version pinning requires the package in cache. Attempting repo install..." >&2
                pacman -S --noconfirm -- "$PKG_NAME" 2>&1 || UPGRADE_RC=$?
            fi
        else
            pacman -Su --noconfirm -- "$PKG_NAME" 2>&1 || UPGRADE_RC=$?
        fi
        ;;
esac

if [ $UPGRADE_RC -ne 0 ]; then
    echo "Package upgrade failed (rc=$UPGRADE_RC)" >&2
    # Classify the failure for actionable error messages
    ERROR_CLASS="upgrade_failed"
    if echo "$UPGRADE_OUTPUT" | grep -qiE "unmet dependencies|held broken|unresolvable|depends.*but it is not"; then
        ERROR_CLASS="dependency_resolution_failed"
    elif echo "$UPGRADE_OUTPUT" | grep -qiE "No space left|disk full|out of space"; then
        ERROR_CLASS="disk_full"
    elif echo "$UPGRADE_OUTPUT" | grep -qiE "Unable to locate package|not found|no package"; then
        ERROR_CLASS="package_not_found"
    elif echo "$UPGRADE_OUTPUT" | grep -qiE "Permission denied|not authorized"; then
        ERROR_CLASS="permission_denied"
    elif echo "$UPGRADE_OUTPUT" | grep -qiE "locked|lock|another process"; then
        ERROR_CLASS="package_manager_locked"
    elif echo "$UPGRADE_OUTPUT" | grep -qiE "hash sum mismatch|checksum|signature"; then
        ERROR_CLASS="package_integrity_failure"
    fi
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" \
        "Package upgrade failed (rc=$UPGRADE_RC, class=$ERROR_CLASS)"
    exit 1
fi

# --- Post-upgrade health check ---
# Wait for the service to become active (up to 60 seconds).
# The package postinst may have already started it, or the systemd
# unit may need a moment to initialize.
HEALTHY=false
for i in $(seq 1 $((HEALTH_CHECK_TIMEOUT / HEALTH_CHECK_INTERVAL))); do
    if systemctl is-active --quiet "$SERVICE_NAME.service" 2>/dev/null \
       || rc-service "$SERVICE_NAME" status >/dev/null 2>&1; then
        HEALTHY=true
        break
    fi
    sleep $HEALTH_CHECK_INTERVAL
done

if [ "$HEALTHY" = false ]; then
    echo "Service failed to start within ${HEALTH_CHECK_TIMEOUT}s — rolling back to $PREV_VERSION" >&2
    # --- Auto-rollback to previous version ---
    ROLLBACK_RC=0
    case "$PKG_MGR" in
        apt)
            apt-get install -y --allow-downgrades -- "${PKG_NAME}=${PREV_VERSION}" 2>&1 || ROLLBACK_RC=$?
            ;;
        dnf|yum)
            $PKG_MGR install -y -- "${PKG_NAME}-${PREV_VERSION}" 2>&1 || ROLLBACK_RC=$?
            ;;
        apk)
            apk add -- "${PKG_NAME}=${PREV_VERSION}" 2>&1 || ROLLBACK_RC=$?
            ;;
        pacman)
            CACHED_PKG=$(find /var/cache/pacman/pkg/ -name "${PKG_NAME}-${PREV_VERSION}-"*.pkg.tar.zst 2>/dev/null | head -1)
            if [ -n "$CACHED_PKG" ]; then
                pacman -U --noconfirm -- "$CACHED_PKG" 2>&1 || ROLLBACK_RC=$?
            else
                echo "WARNING: Cannot rollback — previous version not in pacman cache" >&2
                ROLLBACK_RC=1
            fi
            ;;
    esac
    if [ $ROLLBACK_RC -ne 0 ]; then
        echo "WARNING: Rollback failed (rc=$ROLLBACK_RC) — manual intervention required" >&2
    fi
    write_failure_marker "$PREV_VERSION" "$PREV_VERSION" \
        "Post-upgrade health check failed — rolled back to $PREV_VERSION (rollback rc=$ROLLBACK_RC)"
    exit 1
fi

# --- Record new version ---
NEW_VERSION=$(get_installed_version)

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
