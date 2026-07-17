#!/bin/sh
# Alpine Linux delayed-restart helper for linux-patch-api.
#
# State-aware restart: checks the upgrade state file before restarting.
# Only restarts when the state is "restart_pending" — if the state is
# "installing", "verifying", or "recovering", a package operation may
# still be running.
#
# Uses an atomic lock (mkdir) for deduplication instead of a PID file,
# which avoids the race conditions inherent in PID-file kill/check/write.
#
# On timeout: leaves the system in recovery state and logs the problem.
# Does NOT force a restart after a fixed retry count while state is unsafe.
#
# Installed as: /usr/lib/linux-patch-api/delayed-restart.sh
# Invoked by: configs/linux-patch-api.post-upgrade

MARKER=/var/lib/linux_patch_api/upgrade-pending
STATE_FILE=/var/lib/linux_patch_api/upgrade-state.json
LOCKDIR=/var/run/linux-patch-api-restart.lock
DELAY=10
MAX_RETRIES=30  # 30 * 10s = 300s max wait

# Atomic lock: mkdir is atomic on all filesystems. If the directory
# already exists, another restart helper is running — exit.
if ! mkdir "$LOCKDIR" 2>/dev/null; then
    echo "Another delayed-restart process is running (lock held) — exiting"
    exit 0
fi

# Clean up lock on exit
trap 'rmdir "$LOCKDIR" 2>/dev/null' EXIT INT TERM

# Parse the state file as JSON and extract the "state" field.
# Uses python3 if available, falls back to jq, then to grep as last resort.
get_state() {
    if [ ! -f "$STATE_FILE" ]; then
        echo "missing"
        return
    fi
    if command -v python3 >/dev/null 2>&1; then
        python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print(d.get('state','unknown'))" "$STATE_FILE" 2>/dev/null
    elif command -v jq >/dev/null 2>&1; then
        jq -r '.state // "unknown"' "$STATE_FILE" 2>/dev/null
    else
        # Last resort: grep for the state field (less reliable but
        # better than nothing). This is a fallback, not the primary path.
        grep -o '"state"[[:space:]]*:[[:space:]]*"[^"]*"' "$STATE_FILE" 2>/dev/null | \
            sed 's/.*:.*"\([^"]*\)"/\1/' | head -1
    fi
}

# Wait for the marker to appear (the agent creates it when it writes
# the RestartPending state) and then for the state to be safe for restart.
# The postinst no longer creates the marker — only the agent does.
RETRY=0
while [ "$RETRY" -lt "$MAX_RETRIES" ]; do
    # Check if marker exists — if not, the agent hasn't written
    # RestartPending state yet. Keep waiting.
    if [ ! -f "$MARKER" ]; then
        echo "Waiting for agent to create upgrade-pending marker (retry $RETRY/$MAX_RETRIES)"
        sleep "$DELAY"
        RETRY=$((RETRY + 1))
        continue
    fi

    STATE=$(get_state)

    if [ "$STATE" = "restart_pending" ]; then
        echo "Upgrade state is restart_pending — safe to restart"
        break
    fi

    if [ "$STATE" = "missing" ]; then
        # State file missing but marker exists — NOT safe to restart.
        # This is the fail-closed behavior: never treat "marker exists
        # but state missing" as safe to restart.
        echo "State file missing but marker exists — NOT safe to restart (retry $RETRY/$MAX_RETRIES)"
    else
        echo "Upgrade state is '$STATE' — not safe to restart (retry $RETRY/$MAX_RETRIES)"
    fi

    sleep "$DELAY"
    RETRY=$((RETRY + 1))
done

if [ "$RETRY" -ge "$MAX_RETRIES" ]; then
    echo "Max retries reached — state is still unsafe. Leaving system in recovery state."
    echo "Manual intervention required: check $STATE_FILE and restart linux-patch-api manually."
    # Do NOT force a restart. Leave the system in recovery state.
    exit 1
fi

# Verify the marker still exists before restarting
if [ ! -f "$MARKER" ]; then
    echo "Upgrade-pending marker no longer exists — aborting restart"
    exit 0
fi

# Restart the service
echo "Restarting linux-patch-api service..."
rc-service linux-patch-api restart

# Do NOT remove the marker here.
# The new process removes the marker after successful readiness.
echo "Delayed restart initiated — marker will be removed by the new process after readiness"