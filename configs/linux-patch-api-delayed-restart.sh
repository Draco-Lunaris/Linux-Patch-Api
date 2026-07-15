#!/bin/sh
# Alpine Linux delayed-restart helper for linux-patch-api.
#
# State-aware restart: checks the upgrade state file before restarting.
# Only restarts when the state is "restart_pending" — if the state is
# "installing" or "verifying", a package operation may still be running.
#
# This script replaces the previous anonymous `nohup sh -c 'sleep 30 && ...'`
# approach. It uses a PID file for deduplication and checks the state file
# in a loop, retrying every 10 seconds until the state is safe for restart.
#
# Installed as: /usr/lib/linux-patch-api/delayed-restart.sh
# Invoked by: configs/linux-patch-api.post-upgrade

PIDFILE=/var/run/linux-patch-api-restart.pid
MARKER=/var/lib/linux_patch_api/upgrade-pending
STATE_FILE=/var/lib/linux_patch_api/upgrade-state.json
DELAY=10
MAX_RETRIES=30  # 30 * 10s = 300s max wait

# Deduplicate: kill any existing delayed-restart process
if [ -f "$PIDFILE" ]; then
    OLD_PID=$(cat "$PIDFILE" 2>/dev/null)
    if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
        echo "Killing existing delayed-restart process (PID $OLD_PID)"
        kill "$OLD_PID" 2>/dev/null
    fi
    rm -f "$PIDFILE"
fi

# Write our PID for future deduplication
echo $$ > "$PIDFILE"

# Clean up PID file on exit
trap 'rm -f "$PIDFILE"' EXIT INT TERM

# Wait for the state to be safe for restart.
# The state file must say "restart_pending" — if it says "installing"
# or "verifying", a package operation is still in progress.
RETRY=0
while [ "$RETRY" -lt "$MAX_RETRIES" ]; do
    # Check if marker still exists
    if [ ! -f "$MARKER" ]; then
        echo "Upgrade-pending marker no longer exists — aborting restart (already completed)"
        exit 0
    fi

    # Check state file for restart_pending
    if [ -f "$STATE_FILE" ]; then
        if grep -q '"restart_pending"' "$STATE_FILE" 2>/dev/null; then
            echo "Upgrade state is restart_pending — safe to restart"
            break
        fi
        echo "Upgrade state is not restart_pending (retry $RETRY/$MAX_RETRIES) — waiting..."
    else
        echo "State file missing but marker exists — safe to restart (fallback)"
        break
    fi

    sleep "$DELAY"
    RETRY=$((RETRY + 1))
done

if [ "$RETRY" -ge "$MAX_RETRIES" ]; then
    echo "Max retries reached — forcing restart (state may be stuck)"
fi

# Verify the marker still exists before restarting
if [ ! -f "$MARKER" ]; then
    echo "Upgrade-pending marker no longer exists — aborting restart"
    exit 0
fi

# Restart the service
echo "Restarting linux-patch-api service..."
rc-service linux-patch-api restart

# Remove the marker after successful restart
rm -f "$MARKER"
echo "Delayed restart complete — marker removed"