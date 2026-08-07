#!/usr/bin/env bash
# verify-matrix.sh — local pre-tag 9-distro build verification.
#
# SSHes from the dev box (kore) into each self-hosted build runner, checks out the
# target branch, and runs `just deps-<distro> && just pkg-<distro>` — the exact
# commands CI runs at release time. All 9 green => safe to tag.
#
#   ./scripts/verify-matrix.sh                      # default branch = master
#   ./scripts/verify-matrix.sh -b feat/just-task-runner
#   ./scripts/verify-matrix.sh -l u2404             # single distro, for iteration
#
# Runs as the echo user; SSH uses the echo key (id_ed25519_echo). Alpine builds as
# root via sudo because build-alpine.sh generates abuild signing keys into /root
# and writes /etc/abuild.conf.

set -uo pipefail

BRANCH="master"
ONLY_LABEL=""

usage() {
    cat <<EOF
Usage: $0 [-b BRANCH] [-l LABEL]
  -b BRANCH   branch to verify (default: master)
  -l LABEL    verify only this distro label
EOF
    exit "${1:-1}"
}

while getopts ":b:l:h" opt; do
    case "$opt" in
        b) BRANCH="$OPTARG" ;;
        l) ONLY_LABEL="$OPTARG" ;;
        h) usage 0 ;;
        *) usage 1 ;;
    esac
done

SSH_USER="${VERIFY_SSH_USER:-echo}"
SSH_KEY="${VERIFY_SSH_KEY:-$HOME/.ssh/id_ed25519_echo}"
SSH_OPTS="-i $SSH_KEY -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o ServerAliveInterval=30"
RUN_TIMEOUT="${VERIFY_TIMEOUT:-2400}"   # 40 min per distro (cold builds on slow links)

if [ ! -f "$SSH_KEY" ]; then
    echo "ERROR: SSH key not found: $SSH_KEY" >&2
    echo "Set VERIFY_SSH_KEY to point at the echo key." >&2
    exit 2
fi

# Fleet: ip:label:deps:pkg:mode   (mode = echo | root)
# mode=root only for alpine (build-alpine.sh needs root for abuild signing).
FLEET=(
    "192.168.2.232:u2204:deb:deb:echo"
    "192.168.3.180:u2404:deb:deb:echo"
    "192.168.3.179:u2604:deb:deb:echo"
    "192.168.0.222:debian12:deb:deb:echo"
    "192.168.3.1:debian13:deb:deb:echo"
    "192.168.1.86:fedora:rpm:rpm:echo"
    "192.168.1.114:almalinux:rpm:rpm:echo"
    "192.168.0.156:arch:arch:arch:echo"
    "192.168.1.146:alpine:alpine:alpine:root"
)

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

declare -A LOG PID START

# Build on one runner. Output goes to the caller's redirected stdout/stderr.
# The unquoted heredoc expands local vars ($BRANCH, $mode, $deps, $pkg, $SSH_USER);
# \$HOME and \$PATH are passed literally to the remote shell.
run_one() {
    local ip="$1" label="$2" deps="$3" pkg="$4" mode="$5"
    # shellcheck disable=SC2086
    timeout "$RUN_TIMEOUT" ssh $SSH_OPTS "${SSH_USER}@${ip}" 'bash -s' <<REMOTE
set -e
cd ~/Linux-Patch-Api
git fetch --quiet --all
git checkout -B "$BRANCH" "origin/$BRANCH"
git clean -fd
if [ "$mode" = root ]; then
    sudo rm -rf releases
    sudo env "HOME=/root" "PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" just deps-$deps
    sudo env "HOME=/root" "PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" just pkg-$pkg
    sudo chown -R $SSH_USER:$SSH_USER releases/ || true
else
    rm -rf releases
    export PATH="\$HOME/.cargo/bin:\$PATH"
    just deps-$deps
    just pkg-$pkg
fi
echo "VERIFY-BUILD-OK"
REMOTE
}

echo "Verifying branch: $BRANCH"
[ -n "$ONLY_LABEL" ] && echo "Single distro: $ONLY_LABEL"
echo

# Launch all matching runners in parallel. LOG/PID are set in the main shell so
# the wait loop (also main shell) can see them.
for entry in "${FLEET[@]}"; do
    IFS=: read -r ip label deps pkg mode <<< "$entry"
    if [ -n "$ONLY_LABEL" ] && [ "$label" != "$ONLY_LABEL" ]; then
        continue
    fi
    LOG[$label]="$TMPDIR/$label.log"
    run_one "$ip" "$label" "$deps" "$pkg" "$mode" >"${LOG[$label]}" 2>&1 &
    PID[$label]=$!
    START[$label]=$(date +%s)
    printf "  launched %-12s (%s)\n" "$label" "$ip"
done

if [ ${#PID[@]} -eq 0 ]; then
    echo "No matching runners (label '$ONLY_LABEL' not in fleet)." >&2
    exit 2
fi

echo
echo "Waiting for builds to finish..."
echo

# Collect results in fleet order (stable output).
fail=0
for entry in "${FLEET[@]}"; do
    IFS=: read -r ip label deps pkg mode <<< "$entry"
    [ -z "${PID[$label]:-}" ] && continue
    wait "${PID[$label]}"; rc=$?
    elapsed=$(( $(date +%s) - ${START[$label]} ))
    if [ "$rc" -eq 0 ]; then
        printf "PASS  %-12s  (%ds)\n" "$label" "$elapsed"
    else
        printf "FAIL  %-12s  (exit %d, %ds)\n" "$label" "$rc" "$elapsed"
        fail=1
        echo "    ----- last 20 lines of ${LOG[$label]} -----"
        tail -n 20 "${LOG[$label]}" | sed 's/^/      /'
        echo "    -------------------------------------------"
    fi
done

echo
if [ "$fail" -eq 0 ]; then
    echo "ALL GREEN — ready to tag v<next>."
    exit 0
else
    echo "One or more distros failed — do NOT tag. See logs above."
    exit 1
fi