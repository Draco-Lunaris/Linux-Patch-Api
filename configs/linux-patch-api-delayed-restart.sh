#!/bin/sh
# Alpine Linux delayed-restart helper for linux-patch-api.
#
# This script replaces the previous anonymous `nohup sh -c 'sleep 30 && ...'`
# approach which could not be deduplicated, canceled, or tracked. Multiple
# retried upgrades would spawn multiple sleep processes, each scheduling an
# independent restart.
#
# This script uses a PID file for deduplication:
# - If a restart is already pending (PID file exists and process is alive),
#   the old process is killed and this invocation replaces it.
# - The script sleeps for 30 seconds, then verifies the upgrade-pending
#   marker still exists before restarting the service.
# - The marker is removed after the restart, preventing duplicate restarts
#   from any stale invocations.
#
# Installed as: /usr/lib/linux-patch-api/delayed-restart.sh
# Invoked by: configs/linux-patch-api.post-upgrade

PIDFILE=/var/run/linux-patch-api-restart.pid
MARKER=/var/lib/linux_patch_api/upgrade-pending
DELAY=30

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

# Wait for the delay period
sleep "$DELAY"

# Verify the marker still exists — if the new process already started
# and removed it, a restart is unnecessary (and could be harmful).
if [ ! -f "$MARKER" ]; then
    echo "Upgrade-pending marker no longer exists — aborting restart (already completed)"
    exit 0
fi

# Restart the service
echo "Restarting linux-patch-api service..."
rc-service linux-patch-api restart

# Remove the marker after successful restart
rm -f "$MARKER"
echo "Delayed restart complete — marker removed"