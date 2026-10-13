#!/usr/bin/env bash
# verify-matrix.sh — local pre-tag 9-distro build verification.
#
# SSHes from the dev box into the docker host that runs the CI runners (docker3),
# starts a throwaway container per distro from the same image the self-hosted
# runner uses, clones the target branch, and runs `just deps-<distro> && just
# pkg-<distro>` — the exact commands CI runs at release time. All 9 green => safe to tag.
#
#   ./scripts/verify-matrix.sh                      # default branch = master
#   ./scripts/verify-matrix.sh -b feat/just-task-runner
#   ./scripts/verify-matrix.sh -l u2404             # single distro, for iteration
#
# Builds run as the image's non-root `runner` user (passwordless sudo, like CI).
# Alpine builds as root in alpine:3.21 — the same container CI's alpine job uses —
# because build-alpine.sh generates abuild signing keys into /root and writes
# /etc/abuild.conf. The cargo registry is cached in a docker volume shared by all
# distros; build trees are discarded with each container.

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

HOST="${VERIFY_HOST:-docker3.moon-dragon.us}"
SSH_USER="${VERIFY_SSH_USER:-echo}"
SSH_KEY="${VERIFY_SSH_KEY:-$HOME/.ssh/id_ed25519}"
SSH_OPTS="-i $SSH_KEY -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o ServerAliveInterval=30"
RUN_TIMEOUT="${VERIFY_TIMEOUT:-2400}"   # 40 min per distro (cold builds on slow links)
REPO_URL="${VERIFY_REPO_URL:-https://github.com/Draco-Lunaris/Linux-Patch-Api.git}"

if [ ! -f "$SSH_KEY" ]; then
    echo "ERROR: SSH key not found: $SSH_KEY" >&2
    echo "Set VERIFY_SSH_KEY to point at a key authorized for $SSH_USER@$HOST." >&2
    exit 2
fi

# Fleet: label:image:deps:pkg   (image = runner image on $HOST)
# alpine uses alpine:3.21 directly; the GitHub runner can't run on musl, so CI's
# alpine runner is a glibc image that runs this job in an alpine:3.21 container.
FLEET=(
    "u2204:gh-runner:u2204:deb:deb"
    "u2404:gh-runner:u2404:deb:deb"
    "u2604:gh-runner:u2604:deb:deb"
    "debian12:gh-runner:debian12:deb:deb"
    "debian13:gh-runner:debian13:deb:deb"
    "fedora:gh-runner:fedora:rpm:rpm"
    "almalinux:gh-runner:almalinux10:rpm:rpm"
    "arch:gh-runner:arch:arch:arch"
    "alpine:alpine:3.21:alpine:alpine"
)

TMPDIR="$(mktemp -d)"
LAUNCHED=()
cleanup() {
    # Interrupted or timed-out runs leave their containers behind on the host.
    if [ ${#LAUNCHED[@]} -gt 0 ]; then
        # shellcheck disable=SC2086
        ssh $SSH_OPTS "${SSH_USER}@${HOST}" "docker rm -f ${LAUNCHED[*]}" >/dev/null 2>&1 || true
    fi
    rm -rf "$TMPDIR"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

declare -A LOG=() PID=() START=()   # initialized: under set -u, ${#PID[@]} on an unset array aborts

# Build one distro in a throwaway container. Output goes to the caller's
# redirected stdout/stderr. The unquoted heredoc expands local vars ($BRANCH,
# $REPO_URL, $image, $deps, $pkg, $name); \$HOME and \$PATH are expanded in the container.
run_one() {
    local label="$1" image="$2" deps="$3" pkg="$4" name="$5"
    local build
    if [ "$label" = alpine ]; then
        build=$(cat <<INNER
apk add --no-cache bash git curl just >/dev/null
if [ ! -x "\$HOME/.cargo/bin/cargo" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
export PATH="\$HOME/.cargo/bin:\$PATH"
git clone --quiet --depth 1 --branch "$BRANCH" "$REPO_URL" /src
cd /src
just deps-$deps
just pkg-$pkg
INNER
)
    else
        build=$(cat <<INNER
# Fresh named volumes are root-owned; hand them to the runner user.
sudo chown runner:runner "\$HOME/.cargo/registry" "\$HOME/.local/bin"
# The tools volume is shared by the parallel builds: install to a private dir,
# then rename into place (atomic within the volume).
if [ ! -x "\$HOME/.local/bin/just" ]; then
    tmp="\$HOME/.local/bin/.just-\$(hostname)"
    curl -fsSL https://just.systems/install.sh | bash -s -- --to "\$tmp"
    mv -f "\$tmp/just" "\$HOME/.local/bin/just"
    rm -rf "\$tmp"
fi
export PATH="\$HOME/.local/bin:\$HOME/.cargo/bin:\$PATH"
git clone --quiet --depth 1 --branch "$BRANCH" "$REPO_URL" "\$HOME/src"
cd "\$HOME/src"
just deps-$deps
just pkg-$pkg
INNER
)
    fi

    local mounts
    if [ "$label" = alpine ]; then
        mounts="-v lpa-verify-alpine-cargo:/root/.cargo -v lpa-verify-alpine-rustup:/root/.rustup"
    else
        mounts="-v lpa-verify-cargo-registry:/home/runner/.cargo/registry -v lpa-verify-tools:/home/runner/.local/bin"
    fi

    # shellcheck disable=SC2086
    timeout "$RUN_TIMEOUT" ssh $SSH_OPTS "${SSH_USER}@${HOST}" 'bash -s' <<REMOTE
set -e
docker rm -f $name >/dev/null 2>&1 || true
docker run --rm -i --name $name --entrypoint sh $mounts $image -s <<'BUILD'
set -e
$build
echo "VERIFY-BUILD-OK"
BUILD
REMOTE
}

echo "Verifying branch: $BRANCH  (on $HOST)"
[ -n "$ONLY_LABEL" ] && echo "Single distro: $ONLY_LABEL"
echo

# Launch all matching distros in parallel. LOG/PID are set in the main shell so
# the wait loop (also main shell) can see them.
for entry in "${FLEET[@]}"; do
    IFS=: read -r label repo tag deps pkg <<< "$entry"
    if [ -n "$ONLY_LABEL" ] && [ "$label" != "$ONLY_LABEL" ]; then
        continue
    fi
    name="lpa-verify-$label"
    LOG[$label]="$TMPDIR/$label.log"
    run_one "$label" "$repo:$tag" "$deps" "$pkg" "$name" >"${LOG[$label]}" 2>&1 &
    PID[$label]=$!
    START[$label]=$(date +%s)
    LAUNCHED+=("$name")
    printf "  launched %-12s (%s)\n" "$label" "$repo:$tag"
done

if [ ${#PID[@]} -eq 0 ]; then
    echo "No matching distros (label '$ONLY_LABEL' not in fleet)." >&2
    exit 2
fi

echo
echo "Waiting for builds to finish..."
echo

# Collect results in fleet order (stable output).
fail=0
for entry in "${FLEET[@]}"; do
    IFS=: read -r label repo tag deps pkg <<< "$entry"
    [ -z "${PID[$label]:-}" ] && continue
    wait "${PID[$label]}"; rc=$?
    elapsed=$(( $(date +%s) - ${START[$label]} ))
    if [ "$rc" -eq 0 ] && grep -q VERIFY-BUILD-OK "${LOG[$label]}"; then
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
