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
- [Configuration](#configuration)
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
wget https://gitea.internal/linux-patch-api/releases/v1.0.0/linux-patch-api_1.0.0-1_amd64.deb

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
  min_tls_version: "1.3"

jobs:
  max_concurrent: 5
  timeout_minutes: 30

logging:
  level: "info"
  journal_enabled: true
  file_path: "/var/log/linux_patch_api/audit.log"
  retention_days: 30
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
wget https://gitea.internal/linux-patch-api/releases/v1.0.0/linux-patch-api-1.0.0-1.x86_64.rpm

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
wget https://gitea.internal/linux-patch-api/releases/v1.0.0/install.sh
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
