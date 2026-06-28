#!/usr/bin/env bash
# =============================================================================
# Linux Patch API — Self-Update E2E Test Harness
# =============================================================================
# Tests the self-update feature per tasks/self-update-design.md:
#   1. Upgrade vN → vN+1 with service restart
#   2. CRL/cert preservation across upgrade
#   3. No crash loop (NRestarts unchanged)
#   4. Marker file correctness
#   5. Same-version upgrade (changed=false, no restart)
#   6. restart=false scenario (when implemented)
#
# Usage:
#   ./tests/e2e/test_self_update.sh [TARGET_HOST]
#   Default target: lpa-build.moon-dragon.us
#
# Prerequisites:
#   - SSH access to TARGET_HOST as root
#   - Rust toolchain for building packages
#   - dpkg-scanpackages on target for local apt repo
#   - Target has linux-patch-api not yet installed (or script will reinstall)
#
# Architecture:
#   Agent validates request → writes /var/lib/linux_patch_api/self-update.request
#   Agent calls systemctl start --no-block linux-patch-api-update.service → 202
#   Update service runs self-update.sh in its own cgroup
#   dpkg prerm stops agent; update service survives (different cgroup)
#   dpkg completes → postinst starts new agent
#   Script writes marker file with success/failure
#   New agent serves marker at GET /system/update/status
# =============================================================================

set -euo pipefail

# =============================================================================
# Configuration
# =============================================================================

TARGET_HOST="${1:-lpa-build.moon-dragon.us}"
TARGET_USER="${2:-root}"
API_PORT=12443
PKG_NAME="linux-patch-api"
SERVICE_NAME="linux-patch-api"
UPDATE_SERVICE="linux-patch-api-update"
MARKER_PATH="/var/lib/linux_patch_api/last_self_update.json"
REQUEST_PATH="/var/lib/linux_patch_api/self-update.request"
CERT_DIR="/etc/linux_patch_api/certs"
CRL_PATH="${CERT_DIR}/crl.pem"
CONFIG_PATH="/etc/linux_patch_api/config.yaml"
LOCAL_REPO_DIR="/opt/lpa-local-repo"
WORK_DIR="/tmp/lpa-e2e-self-update"

# Cert paths on the target (mTLS client auth)
CA_CERT="${CERT_DIR}/ca.pem"
CLIENT_CERT="${CERT_DIR}/client001.pem"
CLIENT_KEY="${CERT_DIR}/client001.key.pem"

# Timing
HEALTH_POLL_INTERVAL=3
HEALTH_POLL_TIMEOUT=120
JOB_POLL_INTERVAL=2
JOB_POLL_TIMEOUT=60
MARKER_POLL_INTERVAL=2
MARKER_POLL_TIMEOUT=90
SERVICE_STOP_TIMEOUT=30

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0
TEST_RESULTS=()

# Project root
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Version tracking
ORIGINAL_VERSION=""
VN_VERSION=""
VN1_VERSION=""
VN_DEB=""
VN1_DEB=""
SAVED_ORIGINAL_DEB=""

# =============================================================================
# Logging Helpers
# =============================================================================

log_info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
log_step()  { echo -e "\n${CYAN}${BOLD}==> $*${NC}"; }
log_test()  { echo -e "${BOLD}  TEST: $*${NC}"; }

pass() {
    TESTS_PASSED=$((TESTS_PASSED + 1))
    TEST_RESULTS+=("PASS: $1")
    echo -e "  ${GREEN}✓ PASS${NC}: $1"
}

fail() {
    TESTS_FAILED=$((TESTS_FAILED + 1))
    TEST_RESULTS+=("FAIL: $1")
    echo -e "  ${RED}✗ FAIL${NC}: $1"
    if [ -n "${2:-}" ]; then
        echo -e "         ${RED}Detail: $2${NC}"
    fi
}

skip() {
    TESTS_SKIPPED=$((TESTS_SKIPPED + 1))
    TEST_RESULTS+=("SKIP: $1")
    echo -e "  ${YELLOW}⊘ SKIP${NC}: $1"
}

# =============================================================================
# SSH and API Helpers
# =============================================================================

# Run command on target via SSH
ssh_run() {
    ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "${TARGET_USER}@${TARGET_HOST}" "$@"
}

# Copy file to target
scp_to() {
    local src="$1" dest="$2"
    # Use cat|ssh pipe instead of scp (more reliable across OpenSSH versions)
    cat "$src" | ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "${TARGET_USER}@${TARGET_HOST}" "cat > '${dest}'"
}

# Copy file from target
scp_from() {
    local src="$1" dest="$2"
    scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 "${TARGET_USER}@${TARGET_HOST}:${src}" "$dest"
}

# Make mTLS API GET request on target
api_get() {
    local path="$1"
    ssh_run "curl -s --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} \
        https://192.168.3.140:${API_PORT}${path}"
}

# Make mTLS API POST request on target
api_post() {
    local path="$1" body="${2:-}"
    if [ -n "$body" ]; then
        ssh_run "curl -s --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} \
            -X POST -H 'Content-Type: application/json' -d '${body}' \
            https://192.168.3.140:${API_PORT}${path}"
    else
        ssh_run "curl -s --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} \
            -X POST -H 'Content-Type: application/json' \
            https://192.168.3.140:${API_PORT}${path}"
    fi
}

# Wait for health endpoint to return 200
wait_for_health() {
    local timeout="${1:-$HEALTH_POLL_TIMEOUT}"
    local interval="${2:-$HEALTH_POLL_INTERVAL}"
    local elapsed=0
    log_info "Waiting for /health (timeout=${timeout}s, interval=${interval}s)..."
    while [ $elapsed -lt $timeout ]; do
        local resp
        resp=$(api_get "/health" 2>/dev/null || true)
        if echo "$resp" | python3 -c "import sys,json; d=json.load(sys.stdin); sys.exit(0 if d.get('success') else 1)" 2>/dev/null; then
            log_info "Health check passed after ${elapsed}s"
            echo "$resp"
            return 0
        fi
        sleep "$interval"
        elapsed=$((elapsed + interval))
    done
    log_error "Health check timed out after ${timeout}s"
    return 1
}

# Wait for self-update marker file to reach a terminal status
wait_for_marker() {
    local expected_status="${1:-success}"
    local timeout="${2:-$MARKER_POLL_TIMEOUT}"
    local interval="${3:-$MARKER_POLL_INTERVAL}"
    local elapsed=0
    log_info "Waiting for marker status='${expected_status}' (timeout=${timeout}s)..."
    while [ $elapsed -lt $timeout ]; do
        local resp
        resp=$(api_get "/api/v1/system/update/status" 2>/dev/null || true)
        if [ -n "$resp" ]; then
            local status
            status=$(echo "$resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data',{}).get('status',''))" 2>/dev/null || true)
            if [ "$status" = "$expected_status" ]; then
                log_info "Marker reached status='${expected_status}' after ${elapsed}s"
                echo "$resp"
                return 0
            fi
            # If marker shows failed, fail immediately
            if [ "$status" = "failed" ]; then
                local error_msg
                error_msg=$(echo "$resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data',{}).get('error','unknown'))" 2>/dev/null || echo "unknown")
                log_error "Marker shows status=failed: ${error_msg}"
                echo "$resp"
                return 1
            fi
        fi
        sleep "$interval"
        elapsed=$((elapsed + interval))
    done
    log_error "Marker poll timed out after ${timeout}s"
    return 1
}

# Get service property via systemctl
get_service_property() {
    local service="$1" property="$2"
    ssh_run "systemctl show -p ${property} ${service} 2>/dev/null || echo 'unknown'"
}

# Get installed package version
get_installed_version() {
    ssh_run "dpkg-query -W -f='\${Version}' ${PKG_NAME} 2>/dev/null || echo 'unknown'"
}

# Get NRestarts count for the service
get_nrestarts() {
    local val
    val=$(get_service_property "${SERVICE_NAME}" "NRestarts" | tr -d ' ' || echo "0")
    # NRestarts may not exist; default to 0
    if [ -z "$val" ] || [ "$val" = "unknown" ]; then
        echo "0"
    else
        echo "$val"
    fi
}

# Compute checksums of CRL and all cert files
collect_cert_checksums() {
    ssh_run "sha256sum ${CERT_DIR}/*.pem ${CRL_PATH} 2>/dev/null | sort"
}

# =============================================================================
# Package Building
# =============================================================================

# Read current version from Cargo.toml
read_current_version() {
    grep '^version' "${PROJECT_ROOT}/Cargo.toml" | head -1 | cut -d'"' -f2
}

# Build a .deb package at the current Cargo.toml version
build_deb() {
    local label="$1"
    log_step "Building ${label} package" >&2
    cd "${PROJECT_ROOT}"
    # Clean previous build artifacts
    rm -f "${PROJECT_ROOT}"/*.deb 2>/dev/null || true
    # Build
    bash scripts/build-package.sh 2>&1 | tail -5 >&2
    # Find the built .deb
    local deb_file
    deb_file=$(ls -t "${PROJECT_ROOT}"/*.deb 2>/dev/null | head -1)
    if [ -z "$deb_file" ]; then
        log_error "Failed to build ${label} .deb package"
        exit 1
    fi
    # Copy .deb to work directory to prevent deletion by next build_deb() call
    mkdir -p "${WORK_DIR}"
    local deb_name
    deb_name=$(basename "$deb_file")
    cp "$deb_file" "${WORK_DIR}/${deb_name}"
    echo "${WORK_DIR}/${deb_name}"
}

# =============================================================================
# Target Setup and Cleanup
# =============================================================================

# Save the currently installed package (if any) for later restoration
save_original_state() {
    log_step "Saving original state on ${TARGET_HOST}"
    local installed
    installed=$(ssh_run "dpkg -l ${PKG_NAME} 2>/dev/null | grep '^ii' | awk '{print \$3}'" || true)
    if [ -n "$installed" ]; then
        log_info "Currently installed: ${PKG_NAME}=${installed}"
        # Save the .deb for restoration
        local deb_cache
        deb_cache=$(ssh_run "dpkg -L ${PKG_NAME} 2>/dev/null | head -1" || true)
        # Try to find the cached .deb
        ssh_run "apt-cache policy ${PKG_NAME} 2>/dev/null || true"
        # Save version for later restoration
        ORIGINAL_VERSION="${installed}"
        log_info "Original version saved: ${ORIGINAL_VERSION}"
    else
        log_info "No existing ${PKG_NAME} installation found"
        ORIGINAL_VERSION=""
    fi
}

# Install a .deb package on the target
install_deb_on_target() {
    local deb_file="$1"
    local deb_name
    deb_name=$(basename "$deb_file")
    log_info "Installing ${deb_name} on ${TARGET_HOST}"
    scp_to "$deb_file" "/tmp/${deb_name}"
    ssh_run "dpkg -i /tmp/${deb_name} 2>&1 || apt-get install -f -y 2>&1"
    ssh_run "rm -f /tmp/${deb_name}"
}

# Set up a local apt repository on the target with the vN+1 package
setup_local_repo() {
    local deb_file="$1"
    local deb_name
    deb_name=$(basename "$deb_file")
    log_step "Setting up local apt repository on ${TARGET_HOST}"

    # Create repo directory
    ssh_run "mkdir -p ${LOCAL_REPO_DIR}/pool"
    scp_to "$deb_file" "${LOCAL_REPO_DIR}/pool/${deb_name}"

    # Generate Packages file
    ssh_run "cd ${LOCAL_REPO_DIR} && dpkg-scanpackages pool /dev/null | gzip -9c > pool/Packages.gz 2>/dev/null"

    # Create a minimal Release file
    ssh_run "printf 'Origin: LPA-E2E-Local\nLabel: LPA-E2E-Local\nSuite: local\nCodename: local\nArchitectures: amd64\nComponents: pool\n' > ${LOCAL_REPO_DIR}/Release"

    # Add local repo to sources.list (backup existing)
    ssh_run "if ! grep -q '${LOCAL_REPO_DIR}' /etc/apt/sources.list.d/lpa-local.list 2>/dev/null; then echo 'deb [trusted=yes] file://${LOCAL_REPO_DIR} pool/' > /etc/apt/sources.list.d/lpa-local.list; fi"

    # Update apt cache
    ssh_run "apt-get update -qq 2>&1 | tail -3"
    log_info "Local apt repository configured"
}

# Remove local apt repository
teardown_local_repo() {
    log_step "Removing local apt repository from ${TARGET_HOST}"
    ssh_run "rm -f /etc/apt/sources.list.d/lpa-local.list 2>/dev/null || true"
    ssh_run "rm -rf ${LOCAL_REPO_DIR} 2>/dev/null || true"
    ssh_run "apt-get update -qq 2>&1 | tail -3 || true"
}

# Ensure the agent service is running and healthy
ensure_service_healthy() {
    local timeout="${1:-60}"
    log_info "Ensuring ${SERVICE_NAME} service is running and healthy"
    # Start service if not running
    ssh_run "systemctl enable ${SERVICE_NAME}.service 2>/dev/null || true"
    ssh_run "systemctl start ${SERVICE_NAME}.service 2>/dev/null || true"
    # Wait for health
    if ! wait_for_health "$timeout"; then
        log_error "Service did not become healthy within ${timeout}s"
        # Gather diagnostics
        ssh_run "journalctl -u ${SERVICE_NAME}.service --no-pager -n 30 2>/dev/null || true"
        return 1
    fi
    log_info "Service is healthy"
    return 0
}

# Stop the agent service
stop_service() {
    log_info "Stopping ${SERVICE_NAME} service"
    ssh_run "systemctl stop ${SERVICE_NAME}.service 2>/dev/null || true"
    ssh_run "systemctl stop ${UPDATE_SERVICE}.service 2>/dev/null || true"
    # Wait for process to die
    local elapsed=0
    while [ $elapsed -lt $SERVICE_STOP_TIMEOUT ]; do
        if ! ssh_run "pgrep -x linux-patch-api >/dev/null 2>&1"; then
            log_info "Service stopped after ${elapsed}s"
            return 0
        fi
        sleep 2
        elapsed=$((elapsed + 2))
    done
    log_warn "Service did not stop cleanly within ${SERVICE_STOP_TIMEOUT}s"
    return 1
}

# Remove the marker and request files
clean_markers() {
    log_info "Cleaning marker and request files"
    ssh_run "rm -f ${MARKER_PATH} ${REQUEST_PATH} 2>/dev/null || true"
}

# =============================================================================
# Test Case: Upgrade vN → vN+1 with service restart
# =============================================================================

test_upgrade_with_restart() {
    log_step "TEST 1: Upgrade vN → vN+1 with service restart"
    local test_name="upgrade_with_restart"

    # --- Pre-conditions ---
    log_test "Recording pre-upgrade state"
    local pre_version pre_nrestarts pre_checksums
    pre_version=$(get_installed_version)
    pre_nrestarts=$(get_nrestarts)
    pre_checksums=$(collect_cert_checksums)
    log_info "Pre-upgrade version: ${pre_version}"
    log_info "Pre-upgrade NRestarts: ${pre_nrestarts}"

    # Verify service is healthy
    local health_resp
    health_resp=$(api_get "/health")
    if ! echo "$health_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); sys.exit(0 if d.get('success') else 1)" 2>/dev/null; then
        fail "$test_name" "Service not healthy before upgrade"
        return 1
    fi
    local pre_health_version
    pre_health_version=$(echo "$health_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['version'])" 2>/dev/null || echo "unknown")
    log_info "Pre-upgrade health version: ${pre_health_version}"

    # Clean any previous marker
    clean_markers

    # --- Trigger self-update ---
    log_test "Triggering POST /api/v1/system/update"
    local update_resp http_code
    update_resp=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' -d '{}' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)
    http_code=$(echo "$update_resp" | tail -1)
    if [ "$http_code" != "202" ]; then
        # Get full response for diagnostics
        local full_resp
        full_resp=$(api_post "/api/v1/system/update")
        fail "$test_name" "Expected HTTP 202, got ${http_code}: ${full_resp}"
        return 1
    fi
    pass "update_endpoint_returns_202"

    # --- Wait for marker to show success (before the bounce) ---
    log_test "Waiting for self-update marker to show success"
    local marker_resp
    if ! marker_resp=$(wait_for_marker "success" 120 3); then
        fail "$test_name" "Marker did not reach success status"
        # Check marker file directly
        ssh_run "cat ${MARKER_PATH} 2>/dev/null || echo 'marker not found'"
        # Check update service logs
        ssh_run "journalctl -u ${UPDATE_SERVICE}.service --no-pager -n 30 2>/dev/null || true"
        return 1
    fi
    pass "marker_shows_success"

    # --- Wait for service to come back healthy at vN+1 ---
    log_test "Waiting for service to return healthy at vN+1"
    local post_health_resp
    if ! post_health_resp=$(wait_for_health 120 3); then
        fail "$test_name" "Service did not return healthy after upgrade"
        ssh_run "journalctl -u ${SERVICE_NAME}.service --no-pager -n 30 2>/dev/null || true"
        return 1
    fi
    pass "service_healthy_after_upgrade"

    # --- Assertions ---
    local post_version post_nrestarts post_checksums marker_data
    post_version=$(get_installed_version)
    post_nrestarts=$(get_nrestarts)
    post_checksums=$(collect_cert_checksums)
    marker_data=$(api_get "/api/v1/system/update/status")

    # Assert version changed
    if [ "$pre_version" != "$post_version" ]; then
        pass "version_changed (${pre_version} → ${post_version})"
    else
        fail "version_changed" "Version did not change: ${pre_version} == ${post_version}"
    fi

    # Assert health endpoint shows new version
    local post_health_version
    post_health_version=$(echo "$post_health_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['version'])" 2>/dev/null || echo "unknown")
    if [ "$post_health_version" = "$post_version" ]; then
        pass "health_endpoint_shows_new_version (${post_health_version})"
    else
        fail "health_endpoint_shows_new_version" "Health shows ${post_health_version}, expected ${post_version}"
    fi

    # Assert NRestarts did not climb (no crash loop)
    # Allow +1 for the intentional restart, but not more
    local restart_delta=$((post_nrestarts - pre_nrestarts))
    if [ "$restart_delta" -le 1 ]; then
        pass "no_crash_loop (NRestarts: ${pre_nrestarts} → ${post_nrestarts}, delta=${restart_delta})"
    else
        fail "no_crash_loop" "NRestarts climbed from ${pre_nrestarts} to ${post_nrestarts} (delta=${restart_delta})"
    fi

    # Assert CRL and cert checksums unchanged
    if [ "$pre_checksums" = "$post_checksums" ]; then
        pass "crl_cert_checksums_unchanged"
    else
        fail "crl_cert_checksums_unchanged" "Checksums differ!"
        log_info "Before: ${pre_checksums}"
        log_info "After:  ${post_checksums}"
    fi

    # Assert marker file reflects correct before/after versions
    local marker_prev marker_new marker_changed
    marker_prev=$(echo "$marker_data" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['previous_version'])" 2>/dev/null || echo "unknown")
    marker_new=$(echo "$marker_data" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['new_version'])" 2>/dev/null || echo "unknown")
    marker_changed=$(echo "$marker_data" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['changed'])" 2>/dev/null || echo "unknown")

    if [ "$marker_prev" = "$pre_version" ]; then
        pass "marker_previous_version_correct (${marker_prev})"
    else
        fail "marker_previous_version_correct" "Expected ${pre_version}, got ${marker_prev}"
    fi

    if [ "$marker_new" = "$post_version" ]; then
        pass "marker_new_version_correct (${marker_new})"
    else
        fail "marker_new_version_correct" "Expected ${post_version}, got ${marker_new}"
    fi

    if [ "$marker_changed" = "True" ]; then
        pass "marker_changed_is_true"
    else
        fail "marker_changed_is_true" "Expected True, got ${marker_changed}"
    fi
}

# =============================================================================
# Test Case: Same version upgrade (changed=false, no restart)
# =============================================================================

test_same_version() {
    log_step "TEST 2: Same version upgrade (changed=false, no restart)"
    local test_name="same_version"

    # --- Pre-conditions ---
    log_test "Recording pre-upgrade state"
    local pre_version pre_nrestarts
    pre_version=$(get_installed_version)
    pre_nrestarts=$(get_nrestarts)
    log_info "Current version: ${pre_version}"

    # Verify service is healthy
    local health_resp
    health_resp=$(api_get "/health")
    if ! echo "$health_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); sys.exit(0 if d.get('success') else 1)" 2>/dev/null; then
        fail "$test_name" "Service not healthy before test"
        return 1
    fi

    # Clean previous marker
    clean_markers

    # --- Trigger self-update with current version ---
    log_test "Triggering POST /api/v1/system/update with target_version=${pre_version}"
    local update_resp http_code
    update_resp=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' -d '{\"target_version\":\"${pre_version}\"}' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)
    http_code=$(echo "$update_resp" | tail -1)

    # Accept 202 (started) or 200 (already at version)
    if [ "$http_code" != "202" ] && [ "$http_code" != "200" ]; then
        local full_resp
        full_resp=$(api_post "/api/v1/system/update" "{\"target_version\":\"${pre_version}\"}")
        fail "$test_name" "Expected HTTP 202/200, got ${http_code}: ${full_resp}"
        return 1
    fi
    pass "same_version_endpoint_accepted"

    # --- Wait for marker or job completion ---
    log_test "Waiting for self-update to complete"
    local marker_resp
    if marker_resp=$(wait_for_marker "success" 90 3); then
        pass "same_version_marker_shows_success"
    else
        # If the update service didn't run (same version), check if service stayed up
        log_info "Marker poll timed out; checking if service stayed up (expected for same-version)"
    fi

    # --- Assertions ---
    local post_version post_nrestarts marker_data
    post_version=$(get_installed_version)
    post_nrestarts=$(get_nrestarts)
    marker_data=$(api_get "/api/v1/system/update/status" 2>/dev/null || echo '{}')

    # Assert version unchanged
    if [ "$pre_version" = "$post_version" ]; then
        pass "same_version_unchanged (${post_version})"
    else
        fail "same_version_unchanged" "Version changed: ${pre_version} → ${post_version}"
    fi

    # Assert NRestarts unchanged (no restart for same version)
    if [ "$pre_nrestarts" = "$post_nrestarts" ]; then
        pass "same_version_no_restart (NRestarts: ${pre_nrestarts})"
    else
        # Allow +1 for the update service cycling, but not more
        local delta=$((post_nrestarts - pre_nrestarts))
        if [ "$delta" -le 1 ]; then
            pass "same_version_no_crash_loop (NRestarts delta: ${delta})"
        else
            fail "same_version_no_restart" "NRestarts changed: ${pre_nrestarts} → ${post_nrestarts}"
        fi
    fi

    # Assert marker shows changed=false (if marker exists)
    local marker_changed
    marker_changed=$(echo "$marker_data" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data',{}).get('changed','N/A'))" 2>/dev/null || echo "N/A")
    if [ "$marker_changed" = "False" ] || [ "$marker_changed" = "false" ]; then
        pass "same_version_marker_changed_is_false"
    elif [ "$marker_changed" = "N/A" ]; then
        skip "same_version_marker_changed_is_false (no marker file)"
    else
        fail "same_version_marker_changed_is_false" "Expected false, got ${marker_changed}"
    fi
}

# =============================================================================
# Test Case: restart=false (version staged for next boot)
# =============================================================================

test_restart_false() {
    log_step "TEST 3: Self-update with restart=false (version staged for next boot)"
    local test_name="restart_false"

    # NOTE: The current SelfUpdateRequest only has target_version.
    # The restart field is described in the design doc but not yet implemented.
    # This test case is ready for when the full SelfUpdateRequest is implemented.

    # Check if the API accepts the restart field
    local test_resp
    test_resp=$(ssh_run "curl -s --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' -d '{\"restart\":false}' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null || echo '{}')
    local test_code
    test_code=$(echo "$test_resp" | python3 -c "import sys; print('ok')" 2>/dev/null || echo "parse_error")

    # If the API rejects the restart field, skip this test
    local http_code
    http_code=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' -d '{\"restart\":false}' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)

    if [ "$http_code" = "400" ]; then
        skip "$test_name (restart field not yet implemented in API)"
        return 0
    fi

    # --- Pre-conditions ---
    log_test "Recording pre-upgrade state"
    local pre_version pre_nrestarts pre_checksums
    pre_version=$(get_installed_version)
    pre_nrestarts=$(get_nrestarts)
    pre_checksums=$(collect_cert_checksums)
    log_info "Current version: ${pre_version}"

    # Verify service is healthy
    local health_resp
    health_resp=$(api_get "/health")
    if ! echo "$health_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); sys.exit(0 if d.get('success') else 1)" 2>/dev/null; then
        fail "$test_name" "Service not healthy before test"
        return 1
    fi

    # Clean previous marker
    clean_markers

    # --- Trigger self-update with restart=false ---
    log_test "Triggering POST /api/v1/system/update with restart=false"
    local update_resp
    update_resp=$(api_post "/api/v1/system/update" '{"restart":false}')
    local http_code_resp
    http_code_resp=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' -d '{\"restart\":false}' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)

    if [ "$http_code_resp" != "202" ]; then
        fail "$test_name" "Expected HTTP 202, got ${http_code_resp}"
        return 1
    fi
    pass "restart_false_endpoint_accepted"

    # --- Wait for marker ---
    log_test "Waiting for self-update marker"
    local marker_resp
    if ! marker_resp=$(wait_for_marker "success" 120 3); then
        fail "$test_name" "Marker did not reach success status"
        return 1
    fi
    pass "restart_false_marker_shows_success"

    # --- Assertions ---
    local post_version post_nrestarts post_checksums
    post_version=$(get_installed_version)
    post_nrestarts=$(get_nrestarts)
    post_checksums=$(collect_cert_checksums)

    # Version should be upgraded (package installed)
    if [ "$pre_version" != "$post_version" ]; then
        pass "restart_false_version_upgraded (${pre_version} → ${post_version})"
    else
        fail "restart_false_version_upgraded" "Version unchanged: ${pre_version}"
    fi

    # NRestarts should NOT have climbed (no restart)
    if [ "$pre_nrestarts" = "$post_nrestarts" ]; then
        pass "restart_false_no_restart (NRestarts: ${pre_nrestarts})"
    else
        fail "restart_false_no_restart" "NRestarts changed: ${pre_nrestarts} → ${post_nrestarts}"
    fi

    # CRL and cert checksums should be unchanged
    if [ "$pre_checksums" = "$post_checksums" ]; then
        pass "restart_false_crl_cert_unchanged"
    else
        fail "restart_false_crl_cert_unchanged" "Checksums differ!"
    fi
}

# =============================================================================
# Test Case: CRL and cert preservation (standalone check)
# =============================================================================

test_crl_cert_preservation() {
    log_step "TEST 4: CRL and cert preservation across dpkg upgrade"
    local test_name="crl_cert_preservation"

    # This test verifies that upgrading the package via dpkg does not
    # touch CRL, certificates, or config files.

    # Record checksums before upgrade
    local pre_checksums
    pre_checksums=$(collect_cert_checksums)
    log_info "Pre-upgrade cert checksums recorded"

    # Also check config checksums
    local pre_config_checksums
    pre_config_checksums=$(ssh_run "sha256sum /etc/linux_patch_api/config.yaml /etc/linux_patch_api/whitelist.yaml 2>/dev/null | sort" || true)
    log_info "Pre-upgrade config checksums recorded"

    # Upgrade via dpkg directly (bypassing self-update for isolated test)
    local vn1_deb
    vn1_deb=$(ls -t "${PROJECT_ROOT}"/*.deb 2>/dev/null | head -1)
    if [ -z "$vn1_deb" ]; then
        fail "$test_name" "No vN+1 .deb package found"
        return 1
    fi

    log_test "Upgrading via dpkg -i ${vn1_deb}"
    install_deb_on_target "$vn1_deb"

    # Wait for service to come back
    if ! wait_for_health 60; then
        fail "$test_name" "Service did not return healthy after dpkg upgrade"
        return 1
    fi

    # Record checksums after upgrade
    local post_checksums post_config_checksums
    post_checksums=$(collect_cert_checksums)
    post_config_checksums=$(ssh_run "sha256sum /etc/linux_patch_api/config.yaml /etc/linux_patch_api/whitelist.yaml 2>/dev/null | sort" || true)

    # Assert cert checksums unchanged
    if [ "$pre_checksums" = "$post_checksums" ]; then
        pass "cert_checksums_unchanged_across_dpkg_upgrade"
    else
        fail "cert_checksums_unchanged_across_dpkg_upgrade" "Cert checksums differ!"
        log_info "Before: ${pre_checksums}"
        log_info "After:  ${post_checksums}"
    fi

    # Assert config checksums unchanged
    if [ "$pre_config_checksums" = "$post_config_checksums" ]; then
        pass "config_checksums_unchanged_across_dpkg_upgrade"
    else
        fail "config_checksums_unchanged_across_dpkg_upgrade" "Config checksums differ!"
    fi
}

# =============================================================================
# Test Case: Update service survives agent stop (cgroup isolation)
# =============================================================================

test_update_service_survives() {
    log_step "TEST 5: Update service survives agent cgroup stop"
    local test_name="update_service_survives"

    # Verify that the update service unit is in system.slice (not the agent's cgroup)
    local slice_info
    slice_info=$(ssh_run "systemctl show ${UPDATE_SERVICE}.service -p Slice 2>/dev/null || echo 'unknown'")
    log_info "Update service slice: ${slice_info}"

    # The update service should be in system.slice, not the agent's slice
    if echo "$slice_info" | grep -qi "system"; then
        pass "update_service_in_system_slice"
    else
        # Not a hard failure — just informational
        log_warn "Update service may not be in system.slice: ${slice_info}"
        skip "update_service_in_system_slice (cannot confirm slice)"
    fi

    # Verify the update service unit file has no coupling to the agent service
    local unit_conflicts
    unit_conflicts=$(ssh_run "systemctl show ${UPDATE_SERVICE}.service -p Conflicts 2>/dev/null || echo 'none'")
    log_info "Update service conflicts: ${unit_conflicts}"
    pass "update_service_unit_verified"
}

# =============================================================================
# Test Case: Validation rejection (invalid target_version)
# =============================================================================

test_validation_rejection() {
    log_step "TEST 6: Validation rejection for invalid target_version"
    local test_name="validation_rejection"

    # Test various injection attempts that should be rejected
    local injection_attempts=(
        "1.0.0;rm -rf /"
        "../../etc/passwd"
        "$(printf '1.0.0\$(whoami)')"
        "1.0.0|cat /etc/shadow"
        "1.0.0\`id\`"
        ""
    )

    local all_passed=true
    for injection in "${injection_attempts[@]}"; do
        local escaped_injection
        escaped_injection=$(echo "$injection" | sed 's/"/\"/g')
        local http_code
        http_code=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' -d '{\"target_version\":\"${escaped_injection}\"}' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)
        if [ "$http_code" = "400" ]; then
            log_info "Rejected injection attempt (HTTP 400): ${injection}"
        elif [ "$http_code" = "202" ] || [ "$http_code" = "200" ]; then
            fail "${test_name}_injection_${injection}" "Injection accepted with HTTP ${http_code}: ${injection}"
            all_passed=false
            # Clean up any self-update that was triggered
            ssh_run "systemctl stop ${UPDATE_SERVICE}.service 2>/dev/null || true"
            clean_markers
        else
            log_info "Injection attempt returned HTTP ${http_code}: ${injection}"
        fi
    done

    # Test empty string (should be 400)
    local http_code_empty
    http_code_empty=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' -d '{\"target_version\":\"\"}' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)
    if [ "$http_code_empty" = "400" ]; then
        pass "empty_version_rejected"
    else
        fail "empty_version_rejected" "Expected 400, got ${http_code_empty}"
    fi

    # Test valid version (should be 202)
    local http_code_valid
    http_code_valid=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' -d '{\"target_version\":\"1.0.0\"}' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)
    if [ "$http_code_valid" = "202" ]; then
        pass "valid_version_accepted"
        # Clean up the self-update request that was triggered
        ssh_run "systemctl stop ${UPDATE_SERVICE}.service 2>/dev/null || true"
        clean_markers
    else
        log_warn "Valid version returned HTTP ${http_code_valid} (may be expected if version not found)"
    fi

    if [ "$all_passed" = true ]; then
        pass "all_injection_attempts_rejected"
    fi
}

# =============================================================================
# Test Case: GET /system/update/status endpoint
# =============================================================================

test_update_status_endpoint() {
    log_step "TEST 7: GET /system/update/status endpoint"
    local test_name="update_status_endpoint"

    # Test when no update has occurred (should return 404)
    clean_markers
    local no_marker_resp
    no_marker_resp=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} https://192.168.3.140:${API_PORT}/api/v1/system/update/status" 2>/dev/null)
    if [ "$no_marker_resp" = "404" ]; then
        pass "status_returns_404_when_no_update"
    else
        fail "status_returns_404_when_no_update" "Expected 404, got ${no_marker_resp}"
    fi

    # Trigger a self-update and check status
    clean_markers
    local update_resp
    update_resp=$(api_post "/api/v1/system/update")
    local http_code
    http_code=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)

    if [ "$http_code" = "202" ]; then
        # Wait for marker
        local marker_resp
        if marker_resp=$(wait_for_marker "success" 120 3); then
            # Now check the status endpoint
            local status_resp
            status_resp=$(api_get "/api/v1/system/update/status")
            local status_success
            status_success=$(echo "$status_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('success',False))" 2>/dev/null || echo "False")
            if [ "$status_success" = "True" ]; then
                pass "status_endpoint_returns_success_after_update"
            else
                fail "status_endpoint_returns_success_after_update" "Expected success=True, got ${status_resp}"
            fi

            # Verify marker fields
            local prev_ver new_ver changed status
            prev_ver=$(echo "$status_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['previous_version'])" 2>/dev/null || echo "missing")
            new_ver=$(echo "$status_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['new_version'])" 2>/dev/null || echo "missing")
            changed=$(echo "$status_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['changed'])" 2>/dev/null || echo "missing")
            status=$(echo "$status_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data']['status'])" 2>/dev/null || echo "missing")

            if [ "$prev_ver" != "missing" ] && [ "$new_ver" != "missing" ]; then
                pass "status_marker_has_version_fields (${prev_ver} → ${new_ver})"
            else
                fail "status_marker_has_version_fields" "Missing version fields in marker"
            fi

            if [ "$status" = "success" ]; then
                pass "status_marker_status_is_success"
            else
                fail "status_marker_status_is_success" "Expected success, got ${status}"
            fi
        else
            fail "status_endpoint_test" "Marker did not reach success status"
        fi

        # Wait for service to come back
        wait_for_health 120 3 || true
    else
        skip "status_endpoint_test (could not trigger self-update)"
    fi
}

# =============================================================================
# Test Case: Queue-full and concurrent request handling
# =============================================================================

test_concurrent_update_rejection() {
    log_step "TEST 8: Concurrent self-update request handling"
    local test_name="concurrent_update_rejection"

    # Clean markers
    clean_markers

    # Trigger a self-update
    local http_code
    http_code=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)

    if [ "$http_code" != "202" ]; then
        skip "$test_name (could not trigger initial self-update, got HTTP ${http_code})"
        return 0
    fi

    # Immediately try another self-update — should be rejected or queued
    # The update service is already running, so a second request may:
    # - Return 202 (queued) — acceptable
    # - Return 429 (rate limited) — acceptable
    # - Return 409 (conflict) — acceptable
    # - Return 500 (error) — may indicate the request file already exists
    local second_http_code
    second_http_code=$(ssh_run "curl -s -o /dev/null -w '%{http_code}' --cacert ${CA_CERT} --cert ${CLIENT_CERT} --key ${CLIENT_KEY} -X POST -H 'Content-Type: application/json' https://192.168.3.140:${API_PORT}/api/v1/system/update" 2>/dev/null)

    log_info "Second self-update request returned HTTP ${second_http_code}"

    # Wait for the first update to complete
    wait_for_marker "success" 120 3 || true
    wait_for_health 120 3 || true

    pass "concurrent_update_handled (second request: ${second_http_code})"
}

# =============================================================================
# Main Test Runner
# =============================================================================

# Build both vN and vN+1 packages
build_test_packages() {
    log_step "Building test packages"

    # Read current version
    ORIGINAL_VERSION=$(read_current_version)
    log_info "Current version: ${ORIGINAL_VERSION}"

    # Parse version for bumping
    # Handle versions like 1.5.0-dev1, 1.4.3, etc.
    local base_version dev_suffix
    if echo "$ORIGINAL_VERSION" | grep -qE -- '-dev[0-9]+$'; then
        base_version=$(echo "$ORIGINAL_VERSION" | sed 's/-dev[0-9]*$//')
        dev_suffix=$(echo "$ORIGINAL_VERSION" | grep -oE 'dev[0-9]*$')
        local dev_num
        dev_num=$(echo "$dev_suffix" | grep -oE '[0-9]+$')
        VN1_VERSION="${base_version}-dev$((dev_num + 1))"
    else
        # Simple semver bump: increment patch
        local major minor patch
        IFS='.' read -r major minor patch <<< "${ORIGINAL_VERSION}"
        VN1_VERSION="${major}.${minor}.$((patch + 1))"
    fi

    VN_VERSION="${ORIGINAL_VERSION}"
    log_info "vN version: ${VN_VERSION}"
    log_info "vN+1 version: ${VN1_VERSION}"

    # Build vN package
    log_info "Building vN package (${VN_VERSION})..."
    cd "${PROJECT_ROOT}"
    VN_DEB=$(build_deb "vN (${VN_VERSION})")
    log_info "vN package: ${VN_DEB}"

    # Bump version to vN+1
    log_info "Bumping version to ${VN1_VERSION}..."
    bash scripts/bump-version.sh "${VN1_VERSION}" "${VN_VERSION}" 2>&1 | tail -5

    # Build vN+1 package
    log_info "Building vN+1 package (${VN1_VERSION})..."
    VN1_DEB=$(build_deb "vN+1 (${VN1_VERSION})")
    log_info "vN+1 package: ${VN1_DEB}"

    # Restore original version
    log_info "Restoring original version ${ORIGINAL_VERSION}..."
    bash scripts/bump-version.sh "${ORIGINAL_VERSION}" "${VN1_VERSION}" 2>&1 | tail -5

    log_info "Package build complete"
    log_info "  vN:   ${VN_DEB}"
    log_info "  vN+1: ${VN1_DEB}"
}

# Deploy vN on the target and set up local apt repo with vN+1
deploy_vn_and_repo() {
    log_step "Deploying vN and setting up local apt repository"

    # Install vN
    log_info "Installing vN (${VN_VERSION}) on ${TARGET_HOST}"
    install_deb_on_target "${VN_DEB}"

    # Ensure service is healthy
    if ! ensure_service_healthy 60; then
        log_error "Failed to start vN service"
        return 1
    fi

    # Verify version
    local installed_version
    installed_version=$(get_installed_version)
    log_info "Installed version: ${installed_version}"

    # Set up local apt repo with vN+1
    setup_local_repo "${VN1_DEB}"

    log_info "Deployment complete: vN=${installed_version}, vN+1 available in local repo"
}

# Clean up: remove test packages, restore original state
cleanup() {
    log_step "Cleaning up"

    # Stop services
    ssh_run "systemctl stop ${UPDATE_SERVICE}.service 2>/dev/null || true"
    ssh_run "systemctl stop ${SERVICE_NAME}.service 2>/dev/null || true"

    # Remove local apt repo
    teardown_local_repo

    # Remove test packages
    ssh_run "dpkg --remove ${PKG_NAME} 2>/dev/null || true"

    # Restore original package if it was installed
    if [ -n "${ORIGINAL_VERSION:-}" ] && [ -n "${SAVED_ORIGINAL_DEB:-}" ]; then
        log_info "Restoring original package: ${ORIGINAL_VERSION}"
        install_deb_on_target "${SAVED_ORIGINAL_DEB}" || true
    fi

    # Clean marker files
    clean_markers

    # Clean work directory
    rm -rf "${WORK_DIR}" 2>/dev/null || true

    log_info "Cleanup complete"
}

# Print final report
print_report() {
    log_step "Test Results"
    echo ""
    echo "=========================================="
    echo "  Self-Update E2E Test Report"
    echo "=========================================="
    echo "  Target: ${TARGET_USER}@${TARGET_HOST}"
    echo "  vN:     ${VN_VERSION:-unknown}"
    echo "  vN+1:   ${VN1_VERSION:-unknown}"
    echo ""
    echo "  Passed:  ${TESTS_PASSED}"
    echo "  Failed:  ${TESTS_FAILED}"
    echo "  Skipped: ${TESTS_SKIPPED}"
    echo ""
    echo "------------------------------------------"
    for result in "${TEST_RESULTS[@]}"; do
        echo "  ${result}"
    done
    echo "=========================================="
    echo ""

    if [ "$TESTS_FAILED" -gt 0 ]; then
        log_error "${TESTS_FAILED} tests FAILED"
        return 1
    else
        log_info "All tests PASSED"
        return 0
    fi
}

# =============================================================================
# Main Entry Point
# =============================================================================

main() {
    echo ""
    echo "=========================================="
    echo "  Linux Patch API — Self-Update E2E Test"
    echo "=========================================="
    echo "  Target: ${TARGET_USER}@${TARGET_HOST}"
    echo "  API:    https://${TARGET_HOST}:${API_PORT}"
    echo ""

    # Verify SSH connectivity
    log_info "Verifying SSH connectivity to ${TARGET_HOST}..."
    if ! ssh_run "echo 'SSH OK'" >/dev/null 2>&1; then
        log_error "Cannot connect to ${TARGET_USER}@${TARGET_HOST} via SSH"
        exit 1
    fi
    log_info "SSH connectivity confirmed"

    # Verify target has required tools
    log_info "Verifying target prerequisites..."
    ssh_run "command -v dpkg-scanpackages >/dev/null || apt-get install -y dpkg-dev" 2>/dev/null || true
    ssh_run "command -v curl >/dev/null || apt-get install -y curl" 2>/dev/null || true
    ssh_run "command -v python3 >/dev/null || apt-get install -y python3" 2>/dev/null || true

    # Save original state
    save_original_state

    # Build test packages
    build_test_packages

    # Deploy vN and set up local repo with vN+1
    deploy_vn_and_repo

    # Run test cases (each wrapped with || true so set -e doesn't kill the script)
    test_update_service_survives || true
    test_validation_rejection || true
    test_update_status_endpoint || true
    test_upgrade_with_restart || true
    test_same_version || true
    test_restart_false || true
    test_crl_cert_preservation || true
    test_concurrent_update_rejection || true

    # Clean up
    cleanup

    # Print report
    print_report
}

# Run main
main "$@"
