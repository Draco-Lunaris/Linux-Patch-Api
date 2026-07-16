#!/bin/sh
# Linux Patch API — Fallback restart decision script.
#
# This script is the testable core of the systemd fallback path
# (`linux-patch-api-upgrade-restart.service`). Extracting the logic
# into a separate script enables deterministic unit tests.
#
# Inputs (read-only):
#   $STATE_FILE     path to upgrade-state.json
#   $MARKER         path to upgrade-pending marker file
#   $SERVICE_NAME   systemd unit name (default: linux-patch-api.service)
#   $ACTIVE_STATE   set externally for tests; otherwise read from
#                   `systemctl show --property=ActiveState --value`
#
# Exit codes:
#   0  restart is authorized (caller should run `systemctl restart`)
#   55 do not restart (fail-closed or no-action result)
#   1  error (neither ACTIVE_STATE nor systemctl is available)
#
# State machine (single source of truth for the fallback path):
#
#   Condition                                         Exit
#   ------------------------------------------------- ----
#   ActiveState=activating                            55
#   active, marker absent                              0
#   active, marker present, deadline in future        55
#   active, marker present, deadline expired           0
#   active, marker present, deadline missing/unparseable  55
#   inactive or failed, marker absent                  0
#   inactive or failed, marker present, valid nonempty durable state  0
#   inactive or failed, marker present, state missing/corrupt  55
#   unknown or empty ActiveState                      55
#   neither ACTIVE_STATE nor systemctl is available    1

set -u

STATE_FILE=${STATE_FILE:-/var/lib/linux_patch_api/upgrade-state.json}
MARKER=${MARKER:-/var/lib/linux_patch_api/upgrade-pending}
SERVICE_NAME=${SERVICE_NAME:-linux-patch-api.service}

# Resolve ActiveState — prefer the override, fall back to systemctl show.
if [ -n "${ACTIVE_STATE:-}" ]; then
    resolved_state="$ACTIVE_STATE"
elif command -v systemctl >/dev/null 2>&1; then
    resolved_state=$(systemctl show --property=ActiveState --value "$SERVICE_NAME" 2>/dev/null || true)
else
    echo "systemctl not available and ACTIVE_STATE not set" >&2
    exit 1
fi

if [ -z "$resolved_state" ]; then
    echo "Could not resolve ActiveState for $SERVICE_NAME — fail-closed" >&2
    exit 55
fi

# Helper: parse the JSON state file. python3 preferred, jq fallback,
# else fail-closed.
read_state_field() {
    field="$1"
    if [ ! -f "$STATE_FILE" ]; then
        echo ""
        return 1
    fi
    if command -v python3 >/dev/null 2>&1; then
        python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print(d.get(sys.argv[2], ''))" "$STATE_FILE" "$field" 2>/dev/null
    elif command -v jq >/dev/null 2>&1; then
        jq -r ".$field // \"\"" "$STATE_FILE" 2>/dev/null
    else
        echo ""
        return 1
    fi
}

case "$resolved_state" in
    activating)
        echo "ActiveState=activating — replacement is initializing, skipping"
        exit 55
        ;;
    active)
        if [ ! -f "$MARKER" ]; then
            echo "ActiveState=active, marker cleared — nothing to do"
            exit 0
        fi
        # Marker present while service is active: only restart if the
        # durable restart_deadline has passed.
        deadline=$(read_state_field restart_deadline) || {
            echo "ActiveState=active, marker present, no JSON parser — fail-closed" >&2
            exit 55
        }
        if [ -z "$deadline" ]; then
            echo "ActiveState=active, marker present, no restart_deadline in state — fail-closed" >&2
            exit 55
        fi
        if ! command -v date >/dev/null 2>&1; then
            echo "date command not available — fail-closed" >&2
            exit 55
        fi
        now_epoch=$(date -u +%s 2>/dev/null || echo 0)
        deadline_epoch=$(date -u -d "$deadline" +%s 2>/dev/null || echo 0)
        if [ "$now_epoch" -le 0 ] || [ "$deadline_epoch" -le 0 ]; then
            echo "ActiveState=active, marker present, could not parse deadline — fail-closed" >&2
            exit 55
        fi
        if [ "$now_epoch" -lt "$deadline_epoch" ]; then
            echo "ActiveState=active, deadline $deadline not yet reached (now=$now_epoch) — skipping"
            exit 55
        fi
        echo "ActiveState=active, marker present, deadline reached — restarting"
        exit 0
        ;;
    inactive|failed)
        if [ ! -f "$MARKER" ]; then
            echo "ActiveState=$resolved_state, no marker — nothing to do"
            exit 0
        fi
        # Marker present, service down: require valid state file.
        state_value=$(read_state_field state) || {
            echo "ActiveState=$resolved_state, marker present, no JSON parser — fail-closed" >&2
            exit 55
        }
        if [ -z "$state_value" ]; then
            echo "ActiveState=$resolved_state, marker present, state file missing/corrupt — fail-closed" >&2
            exit 55
        fi
        echo "ActiveState=$resolved_state, marker present, state=$state_value — fallback restart"
        exit 0
        ;;
    *)
        echo "Unrecognized ActiveState=$resolved_state — fail-closed" >&2
        exit 55
        ;;
esac