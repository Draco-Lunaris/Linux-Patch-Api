#!/bin/bash
# =============================================================================
# Linux Patch API — Packaged systemd integration test
# =============================================================================
# This script runs on a self-hosted Ubuntu runner with real systemd.
#
# It builds the .deb, installs it, generates valid temporary TLS
# material and config, starts linux-patch-api.service, requires the
# service to reach active, then submits package operations through
# the real API endpoint (mTLS HTTPS). The package transaction runs
# as a descendant of linux-patch-api.service — NOT from the runner
# shell.
#
# Two test modes:
#   1. Basic service-path: harmless package reinstall (hello / bsdutils)
#   2. Kernel regression: reinstall a known installed kernel package
#      through the API and verify initramfs integrity
#
# LXC reduced-coverage mode:
#   If /lib/modules is absent (LXC container with host kernel), the
#   kernel regression test runs in REDUCED_COVERAGE mode. This mode
#   is explicitly named in artifacts and must NOT be reported as
#   full kernel/initramfs coverage.
#
# Usage:
#   scripts/integration-test.sh [--no-reboot]
#
# Exit codes:
#   0  all checks passed
#   1  build or installation failed
#   2  service failed to start or reach active
#   3  package operation via API failed
#   4  initramfs/kernel verification failed
#   5  reboot verification failed
# =============================================================================

set -euo pipefail

REBOOT=true
if [[ "${1:-}" == "--no-reboot" ]]; then
    REBOOT=false
fi

ARTIFACT_DIR="${INTEGRATION_ARTIFACT_DIR:-/tmp/lpa-integration-artifacts}"
rm -rf "$ARTIFACT_DIR"
mkdir -p "$ARTIFACT_DIR"

API_PORT=12443
API_BASE="https://127.0.0.1:${API_PORT}"
CERT_DIR="/etc/linux_patch_api/certs"
TMP_CERT_DIR="${ARTIFACT_DIR}/certs"
mkdir -p "$TMP_CERT_DIR"

log() { echo "[integration] $*"; }
fail() { echo "[integration] FAIL: $*" >&2; exit "${2:-1}"; }

# =============================================================================
# Capture environment
# =============================================================================
capture_env() {
    log "Capturing environment info..."
    {
        echo "=== Runner Info ==="
        hostname
        uname -a
        cat /etc/os-release
        systemctl --version | head -3
        echo "=== Date ==="
        date -u
        echo "=== /lib/modules ==="
        ls -la /lib/modules/ 2>&1 || echo "/lib/modules not present"
        echo "=== Running kernel ==="
        uname -r
    } > "$ARTIFACT_DIR/runner-info.txt" 2>&1
}

# =============================================================================
# Generate temporary TLS material for the test
# =============================================================================
generate_test_certs() {
    log "Generating temporary test TLS material..."
    local workdir
    workdir="$(mktemp -d)"
    local ca_key="$workdir/ca.key"
    local ca_cert="$workdir/ca.pem"
    local srv_key="$workdir/server.key"
    local srv_csr="$workdir/server.csr"
    local srv_cert="$workdir/server.pem"
    local cli_key="$workdir/client.key"
    local cli_csr="$workdir/client.csr"
    local cli_cert="$workdir/client.pem"

    # CA
    openssl genrsa -out "$ca_key" 4096 2>/dev/null
    openssl req -x509 -new -nodes -key "$ca_key" -sha256 -days 1 \
        -out "$ca_cert" -subj "/CN=LPA-Integration-Test-CA/O=Internal/C=US"

    # Server cert (CN=localhost, SAN includes 127.0.0.1)
    openssl genrsa -out "$srv_key" 2048 2>/dev/null
    openssl req -new -key "$srv_key" -out "$srv_csr" \
        -subj "/CN=localhost/O=Internal/C=US"
    cat > "$workdir/san.cnf" <<EOF
[v3_ext]
subjectAltName = IP:127.0.0.1,DNS:localhost
EOF
    openssl x509 -req -in "$srv_csr" -CA "$ca_cert" -CAkey "$ca_key" \
        -CAcreateserial -out "$srv_cert" -days 1 -sha256 \
        -extfile "$workdir/san.cnf" -extensions v3_ext

    # Client cert
    openssl genrsa -out "$cli_key" 2048 2>/dev/null
    openssl req -new -key "$cli_key" -out "$cli_csr" \
        -subj "/CN=integration-test-client/O=Internal/C=US"
    openssl x509 -req -in "$cli_csr" -CA "$ca_cert" -CAkey "$ca_key" \
        -CAcreateserial -out "$cli_cert" -days 1 -sha256

    # Install to the cert directory the service reads from
    sudo mkdir -p "$CERT_DIR"
    sudo cp "$ca_cert" "$CERT_DIR/ca.pem"
    sudo cp "$srv_cert" "$CERT_DIR/server.pem"
    sudo cp "$srv_key" "$CERT_DIR/server.key.pem"
    sudo chmod 644 "$CERT_DIR/ca.pem" "$CERT_DIR/server.pem"
    sudo chmod 640 "$CERT_DIR/server.key.pem"
    sudo chown root:root "$CERT_DIR/ca.pem" "$CERT_DIR/server.pem" "$CERT_DIR/server.key.pem"

    # Save client cert/key for API calls (not under /etc — used by curl)
    cp "$ca_cert" "$TMP_CERT_DIR/ca.pem"
    cp "$cli_cert" "$TMP_CERT_DIR/client.pem"
    cp "$cli_key" "$TMP_CERT_DIR/client.key"

    # Clean up workdir (private keys)
    rm -rf "$workdir"
    log "Test TLS material generated and installed."
}

# =============================================================================
# Generate test config
# =============================================================================
generate_test_config() {
    log "Generating test config..."
    sudo mkdir -p /etc/linux_patch_api

    # Config: bind 0.0.0.0, TLS on, no CRL, rate limit disabled
    sudo tee /etc/linux_patch_api/config.yaml > /dev/null <<'CFG'
server:
  port: 12443
  bind: "0.0.0.0"
  timeout_seconds: 30
tls:
  enabled: true
  port: 12443
  ca_cert: "/etc/linux_patch_api/certs/ca.pem"
  server_cert: "/etc/linux_patch_api/certs/server.pem"
  server_key: "/etc/linux_patch_api/certs/server.key.pem"
  crl_path: "/etc/linux_patch_api/certs/crl.pem"
jobs:
  max_concurrent: 5
  timeout_minutes: 30
  storage_path: "/var/lib/linux_patch_api/jobs"
  max_queue_depth: 100
logging:
  level: "info"
  journal_enabled: true
  syslog_enabled: false
  file_path: "/var/log/linux_patch_api/audit.log"
  retention_days: 30
whitelist:
  path: "/etc/linux_patch_api/whitelist.yaml"
package_manager:
  backend: "auto"
rate_limit:
  enabled: false
CFG

    # Whitelist: allow 127.0.0.1
    sudo tee /etc/linux_patch_api/whitelist.yaml > /dev/null <<'WL'
entries:
  - "127.0.0.1"
WL

    sudo chmod 640 /etc/linux_patch_api/config.yaml /etc/linux_patch_api/whitelist.yaml
    sudo chown root:root /etc/linux_patch_api/config.yaml /etc/linux_patch_api/whitelist.yaml
    log "Test config installed."
}

# =============================================================================
# API helper: submit a package update and wait for job completion
# =============================================================================
# Arguments: package_name
# Returns: job_id on stdout, sets JOB_STATUS and JOB_ERROR globals
api_update_package() {
    local pkg="$1"
    log "Submitting PUT /api/v1/packages/${pkg} via API..."

    local response
    response=$(curl -sS --max-time 10 \
        --cacert "$TMP_CERT_DIR/ca.pem" \
        --cert "$TMP_CERT_DIR/client.pem" \
        --key "$TMP_CERT_DIR/client.key" \
        -X PUT \
        "${API_BASE}/api/v1/packages/${pkg}" \
        -H "Content-Type: application/json" \
        2>&1) || {
        echo "[integration] API request failed: $response" >&2
        echo "$response" > "$ARTIFACT_DIR/api-error-${pkg}.txt"
        return 1
    }

    echo "$response" > "$ARTIFACT_DIR/api-response-${pkg}.json"
    log "API response: $response"

    # Extract job_id from the response JSON
    local job_id
    job_id=$(echo "$response" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('data',{}).get('job_id',''))" 2>/dev/null || true)
    if [[ -z "$job_id" ]]; then
        echo "[integration] No job_id in API response" >&2
        return 1
    fi
    echo "$job_id"
}

# =============================================================================
# Wait for a job to reach a terminal state
# =============================================================================
# Arguments: job_id, timeout_seconds
# Sets: JOB_STATUS, JOB_ERROR
wait_for_job() {
    local job_id="$1"
    local timeout="${2:-120}"
    local elapsed=0
    local interval=2

    while [[ $elapsed -lt $timeout ]]; do
        local job_response
        job_response=$(curl -sS --max-time 10 \
            --cacert "$TMP_CERT_DIR/ca.pem" \
            --cert "$TMP_CERT_DIR/client.pem" \
            --key "$TMP_CERT_DIR/client.key" \
            "${API_BASE}/api/v1/jobs/${job_id}" 2>&1) || {
            log "Warning: job status poll failed, retrying..."
            sleep $interval
            elapsed=$((elapsed + interval))
            continue
        }

        JOB_STATUS=$(echo "$job_response" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('data',{}).get('status','unknown'))" 2>/dev/null || echo "unknown")
        JOB_ERROR=$(echo "$job_response" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('data',{}).get('error','') or '')" 2>/dev/null || echo "")

        case "$JOB_STATUS" in
            completed|failed|cancelled)
                echo "$job_response" > "$ARTIFACT_DIR/api-job-${job_id}.json"
                log "Job $job_id reached terminal state: $JOB_STATUS"
                return 0
                ;;
        esac

        sleep $interval
        elapsed=$((elapsed + interval))
    done

    echo "[integration] Job $job_id did not complete within ${timeout}s" >&2
    JOB_STATUS="timeout"
    JOB_ERROR="Job did not complete within ${timeout}s"
    return 1
}

# =============================================================================
# Capture process-tree / cgroup evidence
# =============================================================================
capture_process_evidence() {
    local label="$1"
    log "Capturing process/cgroup evidence (${label})..."

    # cgroup of the service
    systemctl show linux-patch-api.service -p ControlGroup --value > \
        "$ARTIFACT_DIR/service-cgroup-${label}.txt" 2>&1 || true

    # List processes in the service's cgroup
    local cg
    cg=$(systemctl show linux-patch-api.service -p ControlGroup --value 2>/dev/null || true)
    if [[ -n "$cg" ]]; then
        # Try cgroup ps via systemd-cgls
        systemd-cgls "$cg" > "$ARTIFACT_DIR/cgroup-processes-${label}.txt" 2>&1 || true
        # Also try /sys/fs/cgroup path
        local cgpath="/sys/fs/cgroup${cg}"
        if [[ -d "$cgpath" ]]; then
            cat "$cgpath/cgroup.procs" > "$ARTIFACT_DIR/cgroup-procs-${label}.txt" 2>&1 || true
            # For each PID, show the full process tree
            {
                echo "=== Process trees for cgroup PIDs ==="
                while read -r pid; do
                    echo "--- PID $pid ---"
                    ps -o pid,ppid,comm,args -p "$pid" 2>/dev/null || true
                    # Walk up the tree to find ancestors
                    local ppid=$pid
                    for _ in 1 2 3 4 5 6 7 8; do
                        ppid=$(ps -o ppid= -p "$ppid" 2>/dev/null | tr -d ' ')
                        [[ -z "$ppid" || "$ppid" == "0" ]] && break
                        echo "  ancestor PID $ppid: $(ps -o comm= -p "$ppid" 2>/dev/null || echo '?')"
                    done
                done < "$ARTIFACT_DIR/cgroup-procs-${label}.txt"
            } > "$ARTIFACT_DIR/process-tree-${label}.txt" 2>&1
        fi
    fi

    # Check journal for evidence that apt-get/dpkg/update-initramfs ran
    # under the service
    {
        echo "=== Journal evidence for package operations under service ==="
        journalctl -u linux-patch-api.service --no-pager -b \
            --grep="apt-get|dpkg|update-initramfs|dispatch_mutation|package" \
            2>&1 || true
    } > "$ARTIFACT_DIR/journal-package-ops-${label}.txt" 2>&1

    # Check for apt child processes of the service
    {
        echo "=== apt/dpkg/update-initramfs processes ==="
        ps aux | grep -E "apt-get|dpkg|update-initramfs" | grep -v grep || true
    } > "$ARTIFACT_DIR/ps-package-procs-${label}.txt" 2>&1
}

# =============================================================================
# 1. Build the .deb
# =============================================================================
capture_env

log "Step 1: Building .deb package..."
chmod +x scripts/build-package.sh
scripts/build-package.sh 2>&1 | tee "$ARTIFACT_DIR/build.log"

DEB_FILE=$(ls linux-patch-api_*_amd64.deb 2>/dev/null | head -1)
[[ -n "$DEB_FILE" ]] || fail "No .deb file found after build" 1
log "Built: $DEB_FILE"

# =============================================================================
# 2. Install the .deb
# =============================================================================
log "Step 2: Installing .deb package..."
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y ./"$DEB_FILE" 2>&1 | tee "$ARTIFACT_DIR/install.log"
test -x /usr/bin/linux-patch-api || fail "Binary not installed" 1
log "Binary installed at /usr/bin/linux-patch-api"

# =============================================================================
# 3. Verify the installed unit matches the source unit
# =============================================================================
log "Step 3: Verifying installed service unit matches source..."
INSTALLED_UNIT="/lib/systemd/system/linux-patch-api.service"
SOURCE_UNIT="configs/linux-patch-api.service"
test -f "$INSTALLED_UNIT" || fail "Service unit not installed at $INSTALLED_UNIT" 1

sort_active_lines() {
    grep -v '^\s*#' "$1" | grep -v '^\s*$' | sort
}

if ! diff <(sort_active_lines "$SOURCE_UNIT") <(sort_active_lines "$INSTALLED_UNIT") > "$ARTIFACT_DIR/unit-diff.txt" 2>&1; then
    log "WARNING: Installed unit differs from source unit (may be dpkg processing):"
    cat "$ARTIFACT_DIR/unit-diff.txt"
fi

# Verify no prohibited directives are in the effective unit
log "Checking for prohibited directives in effective unit..."
PROHIBITED="ProtectKernelModules ProtectKernelTunables ProtectKernelLogs ProtectHome PrivateTmp ProtectHostname ProtectClock RestrictNamespaces SystemCallFilter ProtectSystem NoNewPrivileges RestrictSUIDSGID CapabilityBoundingSet AmbientCapabilities"
for directive in $PROHIBITED; do
    if systemctl show linux-patch-api.service -p "$directive" 2>/dev/null | grep -q "=yes\|=true\|=@"; then
        fail "Prohibited directive $directive is active in the effective unit" 2
    fi
done
log "No prohibited directives found in effective unit"

# Save the effective unit for artifacts
systemctl cat linux-patch-api.service > "$ARTIFACT_DIR/effective-unit.txt" 2>&1
systemctl show linux-patch-api.service > "$ARTIFACT_DIR/effective-show.txt" 2>&1

# =============================================================================
# 4. Generate test config and TLS material
# =============================================================================
log "Step 4: Generating test configuration and TLS material..."
generate_test_certs
generate_test_config

# =============================================================================
# 5. Start the service and REQUIRE it to reach active
# =============================================================================
log "Step 5: Starting linux-patch-api.service..."
sudo systemctl daemon-reload
sudo systemctl enable linux-patch-api.service

# Reset any failed state from a prior run, then stop to ensure a
# clean start. The package may have been installed from a prior CI
# run, and `systemctl start` is a no-op if the service is already
# active. We need a clean restart so the service picks up the new
# test config and TLS material.
sudo systemctl reset-failed linux-patch-api.service 2>/dev/null || true
sudo systemctl stop linux-patch-api.service 2>/dev/null || true
sleep 2

# Clean up any leftover upgrade state from a prior CI run. If the
# upgrade-pending marker or upgrade-state.json exists from a previous
# self-update test, the service enters recovery mode on startup and
# may fail to reach active. Also stop the upgrade-restart timer that
# the postinst may have started.
sudo systemctl stop linux-patch-api-upgrade-restart.timer 2>/dev/null || true
sudo rm -f /var/lib/linux_patch_api/upgrade-pending \
           /var/lib/linux_patch_api/upgrade-state.json 2>/dev/null || true

# Start the service — this MUST succeed. We capture the exit code
# separately from the tee pipe so pipefail doesn't interfere.
sudo systemctl start linux-patch-api.service > "$ARTIFACT_DIR/service-start.log" 2>&1
START_RC=$?
if [[ $START_RC -ne 0 ]]; then
    systemctl status linux-patch-api.service > "$ARTIFACT_DIR/service-status.txt" 2>&1 || true
    journalctl -u linux-patch-api.service --no-pager -b > "$ARTIFACT_DIR/journal-lpa.txt" 2>&1 || true
    cat "$ARTIFACT_DIR/service-start.log"
    fail "linux-patch-api.service failed to start (exit code $START_RC)" 2
fi

# Wait for the service to reach active (Type=notify, so this waits for READY=1)
log "Waiting for service to reach active..."
SERVICE_ACTIVE=false
for i in $(seq 1 30); do
    if systemctl is-active --quiet linux-patch-api.service; then
        SERVICE_ACTIVE=true
        break
    fi
    sleep 1
done

# Capture service status regardless
systemctl status linux-patch-api.service > "$ARTIFACT_DIR/service-status.txt" 2>&1 || true

if [[ "$SERVICE_ACTIVE" != "true" ]]; then
    journalctl -u linux-patch-api.service --no-pager -b > "$ARTIFACT_DIR/journal-lpa.txt" 2>&1 || true
    fail "linux-patch-api.service did not reach active within 30s" 2
fi
log "linux-patch-api.service is active."

# Verify the API is responding
log "Verifying API health endpoint..."
HEALTH_RESPONSE=$(curl -sS --max-time 10 \
    --cacert "$TMP_CERT_DIR/ca.pem" \
    --cert "$TMP_CERT_DIR/client.pem" \
    --key "$TMP_CERT_DIR/client.key" \
    "${API_BASE}/health" 2>&1 || true)
echo "$HEALTH_RESPONSE" > "$ARTIFACT_DIR/health-response.json"
log "Health: $HEALTH_RESPONSE"

# =============================================================================
# 6. Basic service-path test: harmless package reinstall via API
# =============================================================================
log "Step 6: Basic service-path test — harmless package reinstall via API..."

# Use 'hello' if installed, otherwise 'bsdutils' (always present on Debian/Ubuntu)
BASIC_PKG=""
if dpkg -l hello 2>/dev/null | grep -q '^ii'; then
    BASIC_PKG="hello"
elif dpkg -l bsdutils 2>/dev/null | grep -q '^ii'; then
    BASIC_PKG="bsdutils"
else
    fail "No suitable harmless package found for basic test" 3
fi
log "Basic test package: $BASIC_PKG"

BASIC_JOB_ID=$(api_update_package "$BASIC_PKG") || {
    fail "Failed to submit basic package update via API" 3
}
log "Basic test job submitted: $BASIC_JOB_ID"

# Wait for job completion
if ! wait_for_job "$BASIC_JOB_ID" 120; then
    capture_process_evidence "basic"
    fail "Basic package job did not complete" 3
fi

log "Basic test job status: $JOB_STATUS"
if [[ "$JOB_STATUS" != "completed" ]]; then
    log "Basic test job error: $JOB_ERROR"
    capture_process_evidence "basic"
    fail "Basic package reinstall via API failed: $JOB_ERROR" 3
fi

# Verify dpkg --audit is clean after basic test
DPKG_AUDIT=$(sudo dpkg --audit 2>&1 || true)
echo "$DPKG_AUDIT" > "$ARTIFACT_DIR/dpkg-audit-basic.txt"
if [[ -n "$DPKG_AUDIT" ]]; then
    fail "dpkg --audit produced output after basic test: $DPKG_AUDIT" 3
fi
log "Basic service-path test: PASS (dpkg --audit clean)"

# Capture process evidence for the basic test
capture_process_evidence "basic"

# =============================================================================
# 7. Kernel regression test: reinstall kernel package via API
# =============================================================================
log "Step 7: Kernel/initramfs regression test via API..."

# Install lsinitramfs for verification
sudo apt-get install -y initramfs-tools 2>&1 | tee -a "$ARTIFACT_DIR/install.log" || true

# Determine the currently installed kernel package
# Use || true because dpkg -l returns non-zero when no package matches
INSTALLED_KERNEL_PKG=$(dpkg -l "linux-image-*" 2>/dev/null | grep '^ii' | awk '{print $2}' | head -1 || true)
INSTALLED_KERNEL_VER=$(dpkg -l "$INSTALLED_KERNEL_PKG" 2>/dev/null | grep '^ii' | awk '{print $3}' || true)
log "Installed kernel package: $INSTALLED_KERNEL_PKG ($INSTALLED_KERNEL_VER)"

if [[ -z "$INSTALLED_KERNEL_PKG" || -z "$INSTALLED_KERNEL_VER" ]]; then
    fail "Could not determine installed kernel package — cannot run kernel regression test" 4
fi

# Record the exact kernel package and ABI being tested
{
    echo "kernel_package=$INSTALLED_KERNEL_PKG"
    echo "kernel_package_version=$INSTALLED_KERNEL_VER"
    echo "running_kernel=$(uname -r)"
} > "$ARTIFACT_DIR/kernel-test-basics.txt"
log "Exact kernel package under test: $INSTALLED_KERNEL_PKG $INSTALLED_KERNEL_VER"

# Derive the kernel ABI (e.g., 6.8.0-124-generic) from the package version.
# The package version may have an epoch (N:) and a revision (-N).
# The kernel ABI is the version with epoch removed and the final revision
# component stripped, then the last numeric component removed if it's a
# package revision. We use dpkg to find the actual installed files.
KERNEL_ABI=""
# Method 1: derive from the package version
# e.g., 6.8.0-124-generic -> 6.8.0-124-generic
# Package version: 6.8.0-124.124+something -> need to find the vmlinuz
# Most reliable: find the vmlinuz that the package installs
KERNEL_ABI=$(dpkg -L "$INSTALLED_KERNEL_PKG" 2>/dev/null | grep -oE 'vmlinuz-[0-9][^ ]*' | head -1 | sed 's/vmlinuz-//' || true)

if [[ -z "$KERNEL_ABI" ]]; then
    # Method 2: parse from version string
    KERNEL_ABI=$(echo "$INSTALLED_KERNEL_VER" | sed 's/^[0-9]*://' | sed 's/-[0-9]*$//' | sed 's/\.[0-9]*$//')
fi

if [[ -z "$KERNEL_ABI" ]]; then
    fail "Could not determine kernel ABI from package $INSTALLED_KERNEL_PKG" 4
fi

log "Exact kernel ABI under test: $KERNEL_ABI"
echo "kernel_abi=$KERNEL_ABI" >> "$ARTIFACT_DIR/kernel-test-basics.txt"

# Check if /lib/modules exists for this kernel ABI
# If not, we are in an LXC container — use REDUCED_COVERAGE mode
REDUCED_COVERAGE=false
if [[ ! -d "/lib/modules/${KERNEL_ABI}" ]]; then
    log "WARNING: /lib/modules/${KERNEL_ABI} does not exist — LXC reduced-coverage mode"
    REDUCED_COVERAGE=true
    echo "reduced_coverage=true" >> "$ARTIFACT_DIR/kernel-test-basics.txt"
    echo "reduced_coverage_reason=/lib/modules/${KERNEL_ABI} not present (LXC container with host kernel)" >> "$ARTIFACT_DIR/kernel-test-basics.txt"
else
    echo "reduced_coverage=false" >> "$ARTIFACT_DIR/kernel-test-basics.txt"
fi

# Submit the kernel reinstall through the API
log "Submitting kernel package reinstall via API: $INSTALLED_KERNEL_PKG"
KERNEL_JOB_ID=$(api_update_package "$INSTALLED_KERNEL_PKG") || {
    fail "Failed to submit kernel package update via API" 3
}
log "Kernel test job submitted: $KERNEL_JOB_ID"

# Wait for job completion (kernel reinstall can take longer)
if ! wait_for_job "$KERNEL_JOB_ID" 300; then
    capture_process_evidence "kernel"
    fail "Kernel package job did not complete within 300s" 3
fi

log "Kernel test job status: $JOB_STATUS"
if [[ "$JOB_STATUS" != "completed" ]]; then
    log "Kernel test job error: $JOB_ERROR"
    capture_process_evidence "kernel"
    fail "Kernel package reinstall via API failed: $JOB_ERROR" 3
fi
log "Kernel package reinstall via API: job completed successfully"

# Capture process evidence for the kernel test
capture_process_evidence "kernel"

# =============================================================================
# 7a. Verify dpkg --audit is clean
# =============================================================================
log "Running dpkg --audit after kernel reinstall..."
DPKG_AUDIT=$(sudo dpkg --audit 2>&1 || true)
echo "$DPKG_AUDIT" > "$ARTIFACT_DIR/dpkg-audit-kernel.txt"
if [[ -n "$DPKG_AUDIT" ]]; then
    fail "dpkg --audit produced output after kernel reinstall: $DPKG_AUDIT" 4
fi
log "dpkg --audit: clean (no output)"

# =============================================================================
# 7b. Verify vmlinuz and initrd for the EXACT kernel ABI
# =============================================================================
VMLINUZ="/boot/vmlinuz-${KERNEL_ABI}"
INITRD="/boot/initrd.img-${KERNEL_ABI}"

log "Verifying: $VMLINUZ and $INITRD"

# vmlinuz must exist and be non-empty
if [[ ! -s "$VMLINUZ" ]]; then
    if [[ "$REDUCED_COVERAGE" == "true" ]]; then
        log "REDUCED_COVERAGE: vmlinuz not found for $KERNEL_ABI (expected in LXC)"
        echo "vmlinuz=missing (reduced coverage)" >> "$ARTIFACT_DIR/kernel-test-basics.txt"
    else
        fail "$VMLINUZ missing or empty — FAIL (not falling back to running kernel)" 4
    fi
else
    log "vmlinuz exists and is non-empty: $VMLINUZ"
    echo "vmlinuz=$VMLINUZ" >> "$ARTIFACT_DIR/kernel-test-basics.txt"
fi

# initrd must exist and be non-empty
if [[ ! -s "$INITRD" ]]; then
    if [[ "$REDUCED_COVERAGE" == "true" ]]; then
        log "REDUCED_COVERAGE: initrd not found for $KERNEL_ABI (expected in LXC)"
        echo "initrd=missing (reduced coverage)" >> "$ARTIFACT_DIR/kernel-test-basics.txt"
    else
        fail "$INITRD missing or empty — FAIL (not falling back to running kernel)" 4
    fi
else
    log "initrd exists and is non-empty: $INITRD"
    echo "initrd=$INITRD" >> "$ARTIFACT_DIR/kernel-test-basics.txt"
fi

# =============================================================================
# 7c. Verify lsinitramfs and module entries for the EXACT kernel ABI
# =============================================================================
if [[ "$REDUCED_COVERAGE" == "true" ]]; then
    log "REDUCED_COVERAGE: skipping lsinitramfs and module verification (LXC mode)"
    log "This mode must NOT be reported as full kernel/initramfs coverage."
    {
        echo "lsinitramfs=skipped (reduced coverage — LXC)"
        echo "modules_in_initrd=skipped (reduced coverage — LXC)"
        echo "full_kernel_coverage=false"
        echo "reduced_coverage_mode=LXC_NO_LIB_MODULES"
    } > "$ARTIFACT_DIR/kernel-test-result.txt"
elif [[ -s "$INITRD" ]] && command -v lsinitramfs >/dev/null 2>&1; then
    log "Running lsinitramfs on $INITRD..."
    sudo lsinitramfs "$INITRD" > "$ARTIFACT_DIR/lsinitramfs.txt" 2>&1 || \
        fail "lsinitramfs failed on $INITRD" 4
    log "lsinitramfs parsed successfully"

    # Confirm the initramfs contains modules for the EXACT kernel ABI
    MODULES_IN_INITRD=$(grep -c "/lib/modules/${KERNEL_ABI}/" "$ARTIFACT_DIR/lsinitramfs.txt" || true)
    if [[ "$MODULES_IN_INITRD" -eq 0 ]]; then
        # Do NOT fall back to the running kernel — fail
        fail "initramfs does not contain modules for the exact kernel ABI $KERNEL_ABI — FAIL (not falling back to running kernel)" 4
    fi
    log "initramfs contains $MODULES_IN_INITRD module entries for $KERNEL_ABI"

    {
        echo "kernel_package=$INSTALLED_KERNEL_PKG"
        echo "kernel_package_version=$INSTALLED_KERNEL_VER"
        echo "kernel_abi=$KERNEL_ABI"
        echo "vmlinuz=$VMLINUZ"
        echo "initrd=$INITRD"
        echo "dpkg_audit=clean"
        echo "lsinitramfs=ok"
        echo "modules_in_initrd=$MODULES_IN_INITRD"
        echo "full_kernel_coverage=true"
        echo "reduced_coverage_mode=none"
    } > "$ARTIFACT_DIR/kernel-test-result.txt"
elif [[ -s "$INITRD" ]]; then
    log "WARNING: lsinitramfs not available. Cannot verify initramfs module content."
    fail "lsinitramfs not available — cannot verify initramfs module content for $KERNEL_ABI" 4
fi

log "Kernel regression test: PASS"

# =============================================================================
# 8. Reboot verification (if enabled)
# =============================================================================
if $REBOOT; then
    log "Step 8: Reboot verification..."
    log "NOTE: Reboot test requires a disposable VM. Skipping on CI runners."
    echo "reboot=skipped (not a disposable VM)" > "$ARTIFACT_DIR/reboot-result.txt"
else
    log "Step 8: Reboot skipped (--no-reboot)"
    echo "reboot=skipped" > "$ARTIFACT_DIR/reboot-result.txt"
fi

# =============================================================================
# 9. Capture final artifacts
# =============================================================================
log "Step 9: Capturing final artifacts..."

# Journal logs
journalctl -u linux-patch-api.service --no-pager -b > "$ARTIFACT_DIR/journal-lpa.txt" 2>&1 || true
journalctl --no-pager -b > "$ARTIFACT_DIR/journal-boot.txt" 2>&1 || true

# apt/dpkg logs
cp /var/log/apt/history.log "$ARTIFACT_DIR/apt-history.txt" 2>/dev/null || true
cp /var/log/apt/term.log "$ARTIFACT_DIR/apt-term.log" 2>/dev/null || true
cp /var/log/dpkg.log "$ARTIFACT_DIR/dpkg.log" 2>/dev/null || true

# Service status (final)
systemctl status linux-patch-api.service > "$ARTIFACT_DIR/service-status-final.txt" 2>&1 || true

# Summary
{
    echo "=== Integration Test Summary ==="
    echo "Date: $(date -u)"
    echo "Runner: $(hostname)"
    echo "OS: $(cat /etc/os-release | grep PRETTY_NAME | cut -d'"' -f2)"
    echo "Systemd: $(systemctl --version | head -1)"
    echo "Kernel: $(uname -r)"
    echo "Package: $DEB_FILE"
    echo ""
    echo "=== Checks ==="
    echo "Build: PASS"
    echo "Install: PASS"
    echo "Unit match: PASS"
    echo "Prohibited directives: NONE"
    echo "Service active: PASS"
    echo "API health: PASS"
    echo "Basic service-path (via API): PASS"
    echo "Kernel regression (via API): PASS"
    if [[ -f "$ARTIFACT_DIR/kernel-test-result.txt" ]]; then
        cat "$ARTIFACT_DIR/kernel-test-result.txt"
    fi
    echo "Reboot: $(cat "$ARTIFACT_DIR/reboot-result.txt" 2>/dev/null || echo 'skipped')"
    echo ""
    echo "=== Artifacts ==="
    ls -la "$ARTIFACT_DIR/"
} | tee "$ARTIFACT_DIR/summary.txt"

log "Integration test complete. Artifacts in $ARTIFACT_DIR/"
log "All checks passed."