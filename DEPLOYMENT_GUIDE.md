# Linux Patch API - Deployment Guide

**Version:** 1.0.0  
**Status:** Production Ready  
**Last Updated:** 2026-04-09  

Complete guide for deploying Linux Patch API to production environments.

---

## Table of Contents

- [Prerequisites](#prerequisites)
- [Deployment Methods](#deployment-methods)
- [Debian/Ubuntu Deployment](#debianubuntu-deployment)
- [RHEL/CentOS/Fedora Deployment](#rhelcentosfedora-deployment)
- [Manual Deployment](#manual-deployment)
- [Certificate Deployment](#certificate-deployment)
- [Self-Enrollment Deployment](#self-enrollment-deployment)
- [Configuration](#configuration)
  - [File Install Configuration](#file-install-configuration)
- [systemd Service Management](#systemd-service-management)
- [Monitoring and Logging](#monitoring-and-logging)
- [Troubleshooting](#troubleshooting)
- [Post-Deployment Checklist](#post-deployment-checklist)

---

## Prerequisites

### Hardware Requirements

| Component | Minimum | Recommended |
|-----------|---------|-------------|
| CPU | 2 cores | 4 cores |
| RAM | 512 MB | 2 GB |
| Disk Space | 100 MB | 500 MB |
| Network | 1 Gbps | 1 Gbps |

### Software Requirements

| Component | Version | Notes |
|-----------|---------|-------|
| Linux Kernel | 4.15+ | systemd required |
| systemd | 237+ | For service management |
| Package Manager | apt/dnf/yum/apk/pacman | Auto-detected |

### Supported Distributions

| Distribution | Versions | Package Format |
|--------------|----------|----------------|
| Ubuntu | 20.04, 22.04, 24.04 | .deb |
| Debian | 11, 12 | .deb |
| RHEL | 8, 9 | .rpm |
| CentOS | 8, 9 | .rpm |
| Fedora | 38+ | .rpm |
| Alpine | 3.18+ | Manual |
| Arch Linux | Rolling | Manual |

### Network Requirements

| Requirement | Details |
|-------------|---------|
| Port | 12443/TCP (HTTPS) |
| Protocol | TLS 1.3 only |
| Firewall | Allow only whitelisted IPs |
| Internal Network | Recommended (not exposed to internet) |

### Certificate Requirements

- Internal Certificate Authority (CA)
- Server certificate signed by internal CA
- Unique client certificate per client
- Certificate validity: 1 year maximum

---

## Deployment Methods

### Method Comparison

| Method | Best For | Complexity | Auto-Updates |
|--------|----------|------------|--------------|
| .deb Package | Debian/Ubuntu | Low | Yes (apt) |
| .rpm Package | RHEL/CentOS/Fedora | Low | Yes (dnf/yum) |
| Manual Script | Alpine/Arch/Other | Medium | No |
| Source Build | Development/Custom | High | No |

### Recommended Approach

- **Production:** Use official packages (.deb/.rpm) when available
- **Unsupported Distros:** Use install.sh manual installer
- **Development:** Build from source for custom configurations

---

## Debian/Ubuntu Deployment

### Step 1: Install Package

```bash
# Download package
wget https://git.local/linux-patch-api/releases/v1.0.0/linux-patch-api_1.0.0-1_amd64.deb

# Install package
dpkg -i linux-patch-api_1.0.0-1_amd64.deb

# Fix any dependency issues
apt-get install -f -y
```

### Step 2: Verify Installation

```bash
# Check package installation
dpkg -l | grep linux-patch-api

# Verify binary
linux-patch-api --version

# Check service status
systemctl status linux-patch-api
```

### Step 3: Deploy Certificates

```bash
# Create certificate directory
mkdir -p /etc/linux_patch_api/certs

# Copy CA certificate
cp /path/to/ca.pem /etc/linux_patch_api/certs/
chmod 644 /etc/linux_patch_api/certs/ca.pem

# Copy server certificate
cp /path/to/server.pem /etc/linux_patch_api/certs/
chmod 644 /etc/linux_patch_api/certs/server.pem

# Copy server private key
cp /path/to/server.key.pem /etc/linux_patch_api/certs/
chmod 600 /etc/linux_patch_api/certs/server.key.pem
chown root:root /etc/linux_patch_api/certs/server.key.pem
```

### Step 4: Configure IP Whitelist

```bash
# Copy example whitelist
cp /etc/linux_patch_api/whitelist.yaml.example /etc/linux_patch_api/whitelist.yaml

# Edit whitelist
vi /etc/linux_patch_api/whitelist.yaml
```

Example whitelist configuration:
```yaml
entries:
  - "192.168.1.0/24"      # Management network
  - "10.0.0.50"           # Primary admin workstation
  - "10.0.0.51"           # Secondary admin workstation
```

### Step 5: Configure Service

```bash
# Copy example config
cp /etc/linux_patch_api/config.yaml.example /etc/linux_patch_api/config.yaml

# Edit configuration
vi /etc/linux_patch_api/config.yaml
```

Key configuration options:
```yaml
server:
  port: 12443
  bind: "0.0.0.0"
  timeout_seconds: 30

tls:
  enabled: true
  ca_cert: "/etc/linux_patch_api/certs/ca.pem"
  server_cert: "/etc/linux_patch_api/certs/server.pem"
  server_key: "/etc/linux_patch_api/certs/server.key"
  # TLS 1.3 is the only supported version (hardcoded, not configurable)

jobs:
  max_concurrent: 5
  timeout_minutes: 30

logging:
  level: "info"
  journal_enabled: true
  file_path: "/var/log/linux_patch_api/audit.log"
  retention_days: 30

file_install:
  enabled: false              # Must be true to allow file installs (default: false)
  staging_dir: "/tmp"         # Directory for staging uploaded files before install
```

### Step 6: Start Service

```bash
# Enable service (start on boot)
systemctl enable linux-patch-api

# Start service
systemctl start linux-patch-api

# Check status
systemctl status linux-patch-api
```

### Step 7: Test Connection

```bash
# Test health endpoint
curl --cacert /etc/linux_patch_api/certs/ca.pem \
     --cert /path/to/client.pem \
     --key /path/to/client.key.pem \
     https://localhost:12443/health

# Expected response:
# {"success":true,"data":{"status":"healthy",...}}
```

---

## RHEL/CentOS/Fedora Deployment

### Step 1: Install Package

```bash
# Download package
wget https://git.local/linux-patch-api/releases/v1.0.0/linux-patch-api-1.0.0-1.x86_64.rpm

# Install package (RHEL/CentOS 8/9)
dnf install -y ./linux-patch-api-1.0.0-1.x86_64.rpm

# Or on older systems (CentOS 7)
yum install -y ./linux-patch-api-1.0.0-1.x86_64.rpm
```

### Step 2: Verify Installation

```bash
# Check package installation
rpm -qa | grep linux-patch-api

# Verify binary
linux-patch-api --version

# Check service status
systemctl status linux-patch-api
```

### Step 3: Deploy Certificates

```bash
# Create certificate directory
mkdir -p /etc/linux_patch_api/certs

# Copy CA certificate
cp /path/to/ca.pem /etc/linux_patch_api/certs/
chmod 644 /etc/linux_patch_api/certs/ca.pem

# Copy server certificate
cp /path/to/server.pem /etc/linux_patch_api/certs/
chmod 644 /etc/linux_patch_api/certs/server.pem

# Copy server private key
cp /path/to/server.key.pem /etc/linux_patch_api/certs/
chmod 600 /etc/linux_patch_api/certs/server.key.pem
chown root:root /etc/linux_patch_api/certs/server.key.pem
```

### Step 4: Configure IP Whitelist

```bash
# Copy example whitelist
cp /etc/linux_patch_api/whitelist.yaml.example /etc/linux_patch_api/whitelist.yaml

# Edit whitelist
vi /etc/linux_patch_api/whitelist.yaml
```

### Step 5: Configure Service

```bash
# Copy example config
cp /etc/linux_patch_api/config.yaml.example /etc/linux_patch_api/config.yaml

# Edit configuration
vi /etc/linux_patch_api/config.yaml
```

### Step 6: SELinux Configuration (if enabled)

```bash
# Check SELinux status
getenforce

# If enforcing, allow port 12443
semanage port -a -t http_port_t -p tcp 12443

# Or create custom policy
ausearch -c 'linux-patch-api' --raw | audit2allow -M my-linux-patch-api
semodule -i my-linux-patch-api.pp
```

### Step 7: Start Service

```bash
# Enable service
systemctl enable linux-patch-api

# Start service
systemctl start linux-patch-api

# Check status
systemctl status linux-patch-api
```

### Step 8: Test Connection

```bash
curl --cacert /etc/linux_patch_api/certs/ca.pem \
     --cert /path/to/client.pem \
     --key /path/to/client.key.pem \
     https://localhost:12443/health
```

---

## Manual Deployment

For distributions without package support (Alpine, Arch, etc.)

### Step 1: Run Installer

```bash
# Download installer
wget https://git.local/linux-patch-api/releases/v1.0.0/install.sh
chmod +x install.sh

# Run installer (requires root)
./install.sh
```

### Step 2: Follow Interactive Prompts

The installer will:
1. Detect operating system
2. Check prerequisites (systemd, binary)
3. Create system user and group
4. Set up directory structure
5. Install binary and configuration
6. Configure systemd service

### Step 3: Deploy Certificates

```bash
mkdir -p /etc/linux_patch_api/certs
cp /path/to/ca.pem /etc/linux_patch_api/certs/
cp /path/to/server.pem /etc/linux_patch_api/certs/
cp /path/to/server.key.pem /etc/linux_patch_api/certs/
chmod 644 /etc/linux_patch_api/certs/ca.pem
chmod 644 /etc/linux_patch_api/certs/server.pem
chmod 600 /etc/linux_patch_api/certs/server.key.pem
```

### Step 4: Configure and Start

```bash
# Configure whitelist
vi /etc/linux_patch_api/whitelist.yaml

# Configure service
vi /etc/linux_patch_api/config.yaml

# Start service
systemctl enable linux-patch-api
systemctl start linux-patch-api
```

---

## Certificate Deployment

### Certificate Authority Setup

The API requires an internal CA for mTLS authentication.

```bash
# CA should be on separate secure host
# CA private key: /etc/linux_patch_api/ca/ca.key.pem (permissions: 600)
# CA certificate: /etc/linux_patch_api/ca/ca.pem (permissions: 644)
```

### Server Certificate Generation

```bash
# Generate server key and CSR
openssl req -new -newkey rsa:4096 -keyout /etc/linux_patch_api/certs/server.key.pem \
  -out /etc/linux_patch_api/certs/server.csr.pem -nodes \
  -subj "/CN=linux-patch-api.internal"

# Sign with internal CA
openssl x509 -req -in /etc/linux_patch_api/certs/server.csr.pem \
  -CA /etc/linux_patch_api/ca/ca.pem \
  -CAkey /etc/linux_patch_api/ca/ca.key.pem \
  -CAcreateserial -out /etc/linux_patch_api/certs/server.pem -days 365

# Set permissions
chmod 600 /etc/linux_patch_api/certs/server.key.pem
chmod 644 /etc/linux_patch_api/certs/server.pem
```

### Client Certificate Generation (Per Client)

```bash
# Generate client key and CSR
openssl req -new -newkey rsa:4096 -keyout /tmp/client001.key.pem \
  -out /tmp/client001.csr.pem -nodes \
  -subj "/CN=client001"

# Sign with internal CA
openssl x509 -req -in /tmp/client001.csr.pem \
  -CA /etc/linux_patch_api/ca/ca.pem \
  -CAkey /etc/linux_patch_api/ca/ca.key.pem \
  -CAcreateserial -out /tmp/client001.pem -days 365

# Distribute securely to client
scp /tmp/client001.pem /tmp/client001.key.pem client001:/etc/linux_patch_api/certs/

# Clean up local copies
shred -u /tmp/client001.key.pem
```

### Certificate Validation Checklist

- [ ] Server certificate CN matches API hostname
- [ ] Client certificates unique per client (no shared certs)
- [ ] All certificates signed by internal CA
- [ ] Certificate validity: 1 year maximum
- [ ] Private key permissions: 600
- [ ] Certificate permissions: 644
- [ ] CA private key stored on separate secure host
- [ ] Certificate inventory maintained

---

## Self-Enrollment Deployment

Self-enrollment allows a new host to automatically request and receive mTLS certificates from the `linux_patch_manager` without manual PKI distribution. The daemon supports two enrollment modes:

1. **Auto-enrollment (recommended):** When `enrollment.manager_url` is configured in `config.yaml`, the daemon automatically enrolls on startup when certificates are missing or invalid. After provisioning, it continues to normal mTLS server startup.

2. **Manual enrollment:** Run `linux-patch-api --enroll <manager_url>` explicitly. The process provisions certificates and **exits** — it does NOT start the server. Start the service separately after enrollment completes.

### How It Works

The enrollment workflow operates in three phases:

1. **Registration:** Extracts `/etc/machine-id`, FQDN, IP address, and OS details. Submits an unauthenticated `POST /api/v1/enroll` request to the manager. Receives a temporary `polling_token`.
2. **Polling & Approval:** Enters a polling loop querying `GET /api/v1/enroll/status/{token}` (default: every 60 seconds, up to 1440 attempts = 24 hours). Aborts on HTTP 403/404 (denied/purged). The polling token is persisted to `config.yaml` for resume after service restart.
3. **Provisioning:** On HTTP 200, downloads the PKI bundle (`ca.crt`, `server.crt`, `server.key`), writes certificates to configured mTLS paths, appends manager IP to whitelist. For auto-enrollment, transitions to standard mTLS listening mode. For `--enroll`, exits with code 0.

### Certificate Validation

On startup, the daemon validates all configured TLS certificates before attempting to bind the listening port:

1. **Existence:** All three cert files must exist at configured paths
2. **Parse:** Each file must be valid PEM (X.509 for certs, PKCS#8/PKCS#1 for keys)
3. **Expiry:** Certs must not be expired. Certs expiring within `cert_renewal_threshold_days` (default 7) trigger a warning
4. **Key match:** Server cert public key must correspond to server key private key
5. **CA trust:** Server cert must be signed by the CA cert

Validation results determine startup behavior:

| Result | Action |
|--------|--------|
| Valid | Start normally with mTLS |
| ExpiringSoon | Log warning, start normally, schedule re-enrollment |
| Missing/Corrupt/Expired/KeyMismatch/Untrusted | Auto-enroll if `enrollment.manager_url` configured, otherwise exit with guidance |

### Prerequisites

| Requirement | Details |
|-------------|---------|
| Manager URL | Must be accessible from the host (HTTPS) |
| Network Connectivity | Outbound HTTPS to manager endpoint |
| DNS Resolution | Manager hostname must resolve correctly |
| systemd | Version 237+ for service management |
| Root Access | Required for certificate file writes |

**Verification before enrollment:**
```bash
# Verify network connectivity to manager
curl -I https://manager.example.com

# Verify DNS resolution
nslookup manager.example.com

# Verify outbound HTTPS works
curl -ks https://manager.example.com/api/v1/health
```

### Deployment Method 1: Auto-Enrollment (Recommended)

The simplest deployment. Just install the package, configure the manager URL, and start the service. The daemon handles the rest.

#### Step 1: Install Package

```bash
# Debian/Ubuntu
dpkg -i linux-patch-api_1.2.0-1_amd64.deb

# RHEL/CentOS/Fedora
rpm -ivh linux-patch-api-1.2.0-1.x86_64.rpm
```

#### Step 2: Configure Enrollment URL

```bash
# Edit the config to add manager URL
cat >> /etc/linux_patch_api/config.yaml <<EOF

enrollment:
  manager_url: "https://lpm.local"
  polling_interval_seconds: 60
  max_poll_attempts: 1440
  cert_renewal_threshold_days: 7
EOF
```

#### Step 3: Start Service

```bash
# Enable and start
systemctl enable linux-patch-api
systemctl start linux-patch-api

# Watch auto-enrollment progress
journalctl -u linux-patch-api -f
```

The daemon will:
1. Validate certificates → find them missing
2. Read `enrollment.manager_url` → begin auto-enrollment
3. Register with manager, poll for approval
4. Provision certificates after admin approval
5. Continue to normal mTLS server startup

**No manual `--enroll` command needed.** The service self-heals on restart if certificates are missing or invalid.

#### Step 4: Admin Approval (Manager Side)

On the `linux_patch_manager` dashboard:
1. Navigate to Pending Enrollments
2. Review host details (machine-id, FQDN, IP, OS)
3. Approve the enrollment request
4. Manager provisions PKI bundle and signals approval

#### Step 5: Verify Successful Enrollment

```bash
# Check service is running
systemctl status linux-patch-api

# Verify certificates exist
ls -la /etc/linux_patch_api/certs/

# Test mTLS connection
curl --cacert /etc/linux_patch_api/certs/ca.pem \
     --cert /path/to/client.pem \
     --key /path/to/client.key.pem \
     https://localhost:12443/health
```

### Deployment Method 2: Manual Enrollment

For environments where auto-enrollment is not desired, or for initial setup before the service is enabled.

#### Step 1: Install Package

```bash
# Debian/Ubuntu
dpkg -i linux-patch-api_1.2.0-1_amd64.deb

# RHEL/CentOS/Fedora
rpm -ivh linux-patch-api-1.2.0-1.x86_64.rpm
```

#### Step 2: Run Enrollment Command

```bash
# Basic enrollment with manager URL
sudo linux-patch-api --enroll https://lpm.local

# With verbose logging for troubleshooting
sudo linux-patch-api --enroll https://lpm.local --verbose
```

**Important:** The `--enroll` command provisions certificates and **exits**. It does NOT start the server. This prevents port conflicts with the systemd service.

The enrollment process will:
- Extract machine identity from `/etc/machine-id` and system properties
- Submit registration request to manager
- Enter polling loop (logs progress every 60 seconds)
- Await admin approval on the manager side
- Download and install certificates automatically
- Update IP whitelist with manager address
- Print: "Enrollment complete. Start service: systemctl start linux-patch-api"
- Exit with code 0

#### Step 3: Start Service

```bash
systemctl enable linux-patch-api
systemctl start linux-patch-api
systemctl status linux-patch-api
```

### Certificate Renewal

Certificates can be renewed manually or automatically:

```bash
# Manual renewal
sudo linux-patch-api --renew-certs

# Auto-renewal: certs expiring within cert_renewal_threshold_days trigger re-enrollment on startup
```

### Configuration Options

Enrollment behavior can be tuned via the `enrollment` section in `/etc/linux_patch_api/config.yaml`:

```yaml
# Enrollment Configuration
enrollment:
  manager_url: "https://lpm.local"
  polling_interval_seconds: 60    # Time between approval polls (default: 60)
  max_poll_attempts: 1440         # Maximum poll attempts (default: 1440 = 24 hours)
  polling_token: ""               # Auto-populated during enrollment (do not edit)
  cert_renewal_threshold_days: 7  # Days before expiry to trigger re-enrollment
```

**Parameter Reference:**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `manager_url` | (none) | Manager URL for auto-enrollment. Required for auto-enrollment on startup. |
| `polling_interval_seconds` | 60 | Seconds between approval status polls. Minimum: 10 |
| `max_poll_attempts` | 1440 | Maximum polling attempts before timeout. Effective timeout = interval × attempts |
| `polling_token` | (empty) | Auto-populated during enrollment for resume after restart. Do not edit manually. |
| `cert_renewal_threshold_days` | 7 | Days before cert expiry to trigger automatic re-enrollment |

**Effective Timeout Calculation:**
- Default: 60s × 1440 = 86,400 seconds (24 hours)
- Custom example: 30s × 720 = 21,600 seconds (6 hours)

### Troubleshooting

| Symptom | Cause | Resolution |
|---------|-------|------------|
| `Enrollment failed: connection refused` | Manager not reachable | Verify manager URL, check firewall rules |
| `Enrollment failed: DNS resolution error` | Hostname cannot resolve | Check `/etc/resolv.conf`, verify DNS |
| `HTTP 403 - Enrollment denied` | Admin rejected request | Contact manager admin to approve enrollment |
| `HTTP 404 - Token not found` | Token expired/purged | Re-run enrollment command with `--enroll` flag |
| `Polling timeout after N attempts` | Max attempts exceeded | Increase `max_poll_attempts` in config, re-enroll |
| `Rate limited: 429 Too Many Requests` | Polling too frequently | Ensure `polling_interval_seconds >= 10` |
| `Permission denied writing certificates` | Insufficient privileges | Run with `sudo` or as root user |
| `Whitelist update failed` | File permission issue | Verify `/etc/linux_patch_api/` is writable by service user |

**Diagnostic Commands:**
```bash
# Check enrollment logs
journalctl -u linux-patch-api --since "1 hour ago"

# Test manager connectivity
curl -v https://manager.example.com/api/v1/enroll

# Verify DNS resolution
dig manager.example.com
nslookup manager.example.com

# Check certificate paths are writable
ls -la /etc/linux_patch_api/certs/
sudo touch /etc/linux_patch_api/certs/test && sudo rm /etc/linux_patch_api/certs/test
```

### Post-Enrollment Verification

After successful enrollment, verify the following:

1. **Certificate Files Exist:**
   ```bash
   ls -la /etc/linux_patch_api/certs/
   # Expected: ca.pem (644), server.pem (644), server.key (600)
   ```

2. **Certificate Validity:**
   ```bash
   openssl x509 -in /etc/linux_patch_api/certs/server.pem -text -noout | grep -A2 "Validity"
   openssl x509 -in /etc/linux_patch_api/certs/ca.pem -text -noout | grep -A2 "Validity"
   ```

3. **Whitelist Contains Manager IP:**
   ```bash
   cat /etc/linux_patch_api/whitelist.yaml
   # Should contain manager IP address in entries list
   ```

4. **mTLS Connection Test:**
   ```bash
   curl --cacert /etc/linux_patch_api/certs/ca.pem \
        --cert /path/to/client.pem \
        --key /path/to/client.key.pem \
        https://localhost:12443/health
   # Expected: {"status": "ok"}
   ```

5. **Service Status:**
   ```bash
   systemctl status linux-patch-api
   # Expected: active (running)
   ```

### Rollback and Re-Enrollment

#### Removing Enrolled Certificates

```bash
# Stop the service
sudo systemctl stop linux-patch-api

# Remove provisioned certificates
sudo rm -f /etc/linux_patch_api/certs/ca.pem
sudo rm -f /etc/linux_patch_api/certs/server.pem
sudo rm -f /etc/linux_patch_api/certs/server.key

# Revert whitelist (remove manager IP entry)
sudo vi /etc/linux_patch_api/whitelist.yaml
```

#### Re-Enrolling a Host

```bash
# Run enrollment again with same or different manager
sudo linux-patch-api --enroll https://manager.example.com

# Or enroll with a different manager
sudo linux-patch-api --enroll https://new-manager.example.com
```

**Notes:**
- Re-enrollment overwrites existing certificates in the configured paths
- The previous polling token is discarded; a new registration request is submitted
- If re-enrolling with the same manager, ensure the old enrollment was purged or approved

### Enrollment vs Manual Certificate Deployment

| Aspect | Self-Enrollment | Manual PKI |
|--------|----------------|------------|
| Certificate distribution | Automatic from manager | Manual SCP/copy |
| Whitelist management | Auto-populated with manager IP | Manual configuration |
| Admin approval required | Yes (on manager side) | N/A |
| Network dependency | Requires outbound HTTPS to manager | None after cert distribution |
| Best for | Large-scale deployments, automation | Air-gapped environments, single hosts |

---

## Configuration

### Configuration File Locations

| File | Path | Permissions | Description |
|------|------|-------------|-------------|
| Main Config | `/etc/linux_patch_api/config.yaml` | 644 | Service configuration |
| Whitelist | `/etc/linux_patch_api/whitelist.yaml` | 644 | IP access control |
| Server Cert | `/etc/linux_patch_api/certs/server.pem` | 644 | Server public certificate |
| Server Key | `/etc/linux_patch_api/certs/server.key` | 600 | Server private key |
| CA Cert | `/etc/linux_patch_api/certs/ca.pem` | 644 | CA public certificate |

### Configuration Reload

Configuration changes are applied automatically:

| Configuration | Reload Method |
|---------------|---------------|
| IP Whitelist | Automatic (file watch) |
| Main Config | Automatic (file watch) |
| Certificates | Service restart required |

### Validate Configuration

```bash
# Test configuration syntax
linux-patch-api --check-config

# View current configuration
linux-patch-api --show-config
```

### File Install Configuration

File install allows uploading local package files (`.deb`, `.rpm`, `.apk`, `.tar.zst`) directly to the API for installation, bypassing repository GPG signing. This feature is **disabled by default** and must be explicitly enabled.

**Security Warning:** File installs bypass repository GPG verification. Only enable this feature if you understand the risk and trust the source of uploaded packages.

```yaml
# File Install Configuration
file_install:
  enabled: false              # Must be true to allow file installs (default: false)
  staging_dir: "/tmp"         # Directory for staging uploaded files before install
```

**Parameter Reference:**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `file_install.enabled` | `false` | Must be `true` to enable file install endpoint. When `false`, `POST /api/v1/packages/install-file` returns HTTP 403. |
| `file_install.staging_dir` | `/tmp` | Directory where uploaded files are staged before installation. Must be root-owned with mode 0700. |

**File Validation Rules:**
- Allowed extensions: `.deb`, `.rpm`, `.apk`, `.tar.zst`
- Maximum file size: 1GB
- Staging directory permissions: root-owned, mode 0700

**Self-Upgrade Workflow:**

The system restart endpoint enables self-upgrade workflows managed by the linux_patch_manager:
1. Manager calls `POST /api/v1/packages/install-file` with new package
2. Manager polls job status until install completes
3. Manager calls `POST /api/v1/system/restart` to restart the service
4. Manager reconnects to verify the upgraded service is healthy

```bash
# Example: Install a .deb package file
curl --cacert ca.pem --cert client.pem --key client.key.pem \
     -F "package=@/path/to/linux-patch-api_1.2.0-1_amd64.deb" \
     https://host:12443/api/v1/packages/install-file

# Example: Trigger service restart after upgrade
curl --cacert ca.pem --cert client.pem --key client.key.pem \
     -X POST https://host:12443/api/v1/system/restart
```

---

## systemd Service Management

### Service Commands

```bash
# Start service
systemctl start linux-patch-api

# Stop service
systemctl stop linux-patch-api

# Restart service
systemctl restart linux-patch-api

# Reload configuration
systemctl reload linux-patch-api

# Check status
systemctl status linux-patch-api

# Enable on boot
systemctl enable linux-patch-api

# Disable on boot
systemctl disable linux-patch-api

# View logs
journalctl -u linux-patch-api -f
```

### Service File Location

```
/lib/systemd/system/linux-patch-api.service
```

### Service Hardening

The service includes systemd security hardening:

```ini
[Service]
User=linux-patch-api
Group=linux-patch-api
ProtectSystem=strict
ProtectHome=true
NoNewPrivileges=true
PrivateTmp=true
SystemCallFilter=@system-service
```

---

## Monitoring and Logging

### Log Locations

| Log Type | Location | Access |
|----------|----------|--------|
| systemd Journal | `journalctl -u linux-patch-api` | root |
| Audit Log | `/var/log/linux_patch_api/audit.log` | root |
| Application Log | `/var/log/linux_patch_api/app.log` | root |

### Viewing Logs

```bash
# Real-time service logs
journalctl -u linux-patch-api -f

# Last 100 log entries
journalctl -u linux-patch-api -n 100

# Logs from specific time
journalctl -u linux-patch-api --since "2026-04-09 10:00:00"

# Audit log
 tail -f /var/log/linux_patch_api/audit.log
```

### Log Levels

| Level | Description | Use Case |
|-------|-------------|----------|
| error | Error conditions | Production default |
| warn | Warning conditions | Debugging |
| info | Informational | Normal operations |
| debug | Debug messages | Development |
| trace | Trace messages | Deep debugging |

### Monitoring Endpoints

```bash
# Health check (for load balancers)
curl https://localhost:12443/health

# System information
curl --cacert ca.pem --cert client.pem --key client.key.pem \
     https://localhost:12443/api/v1/system/info

# Job status
curl --cacert ca.pem --cert client.pem --key client.key.pem \
     https://localhost:12443/api/v1/jobs
```

### Metrics to Monitor

| Metric | Threshold | Alert |
|--------|-----------|-------|
| CPU Usage | >80% sustained | Warning |
| Memory Usage | >90% | Critical |
| Active Jobs | >max_concurrent | Warning |
| Failed Jobs | >5/hour | Warning |
| Certificate Expiry | <30 days | Critical |

---

## Troubleshooting

### Service Won't Start

```bash
# Check service status
systemctl status linux-patch-api

# Check logs for errors
journalctl -u linux-patch-api -n 50 --no-pager

# Common issues:
# 1. Certificate files missing or wrong permissions
# 2. Port 12443 already in use
# 3. Configuration syntax error
# 4. Missing dependencies
```

### Certificate Issues

```bash
# Verify certificate
openssl x509 -in /etc/linux_patch_api/certs/server.pem -text -noout

# Verify key matches certificate
openssl x509 -noout -modulus -in /etc/linux_patch_api/certs/server.pem | openssl md5
openssl rsa -noout -modulus -in /etc/linux_patch_api/certs/server.key.pem | openssl md5
# Hashes should match

# Check certificate expiry
openssl x509 -enddate -noout -in /etc/linux_patch_api/certs/server.pem
```

### Connection Issues

```bash
# Test local connection
curl -v --cacert /etc/linux_patch_api/certs/ca.pem \
     --cert /path/to/client.pem \
     --key /path/to/client.key.pem \
     https://localhost:12443/health

# Check if port is listening
ss -tlnp | grep 12443

# Check firewall rules
iptables -L -n | grep 12443
firewall-cmd --list-all  # RHEL/CentOS/Fedora
ufw status  # Ubuntu/Debian
```

### Whitelist Issues

```bash
# Verify whitelist file
 cat /etc/linux_patch_api/whitelist.yaml

# Check if IP is in whitelist
grep "your.ip.address" /etc/linux_patch_api/whitelist.yaml

# Reload whitelist (automatic, but can force restart)
systemctl restart linux-patch-api
```

### Performance Issues

```bash
# Check resource usage
systemctl status linux-patch-api

# View process details
ps aux | grep linux-patch-api

# Check active jobs
curl --cacert ca.pem --cert client.pem --key client.key.pem \
     https://localhost:12443/api/v1/jobs?status=running

# Check concurrent job limit
grep max_concurrent /etc/linux_patch_api/config.yaml
```

### Common Error Messages

| Error | Cause | Solution |
|-------|-------|----------|
| "Permission denied" | Wrong file permissions | chmod 600 for keys, 644 for certs |
| "Address already in use" | Port 12443 occupied | Stop conflicting service or change port |
| "Certificate validation failed" | Invalid/expired cert | Regenerate certificate |
| "IP not in whitelist" | Source IP blocked | Add IP to whitelist.yaml |
| "Configuration invalid" | YAML syntax error | Validate config.yaml syntax |

---

## Post-Deployment Checklist

### Security Verification

- [ ] mTLS authentication working
- [ ] IP whitelist enforced (test from non-whitelisted IP)
- [ ] TLS 1.3 only (no legacy protocols)
- [ ] Certificate permissions correct (600 for keys)
- [ ] CA private key on separate host
- [ ] systemd hardening active

### Functionality Verification

- [ ] Health endpoint responding
- [ ] Package listing working
- [ ] Package installation (test job)
- [ ] Job status tracking working
- [ ] WebSocket streaming working
- [ ] Audit logging active

### Monitoring Setup

- [ ] Logs visible in journalctl
- [ ] Audit log file created
- [ ] Health check configured for load balancer
- [ ] Alerting configured for failures
- [ ] Certificate expiry monitoring active

### Documentation

- [ ] Certificate inventory documented
- [ ] Client certificates distributed
- [ ] Runbook created for operations team
- [ ] Emergency procedures documented

---

## Support

- **Documentation:** [README.md](./README.md)
- **API Reference:** [API_DOCUMENTATION.md](./API_DOCUMENTATION.md)
- **Security Guide:** [DEPLOYMENT_SECURITY_GUIDE.md](./DEPLOYMENT_SECURITY_GUIDE.md)
- **Build Guide:** [BUILD_PACKAGES.md](./BUILD_PACKAGES.md)
