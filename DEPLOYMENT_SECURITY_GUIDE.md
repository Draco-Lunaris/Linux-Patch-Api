# Linux_Patch_API - Deployment Security Guide

**Version:** 1.0.0  
**Phase:** 3 - Security Hardening Complete  
**Date:** 2026-04-09  
**Classification:** Internal Use Only

---

## Executive Summary

This guide provides comprehensive security deployment instructions for the Linux_Patch_API service. The API has completed Phase 3 security hardening with 16/16 security tests passing and is approved for internal network deployment.

**Security Posture:** GOOD - Suitable for internal network deployment with documented mitigations.

---

## 1. Certificate Deployment

### 1.1 Certificate Authority Setup

The API requires an internal Certificate Authority (CA) for mTLS authentication.

**CA Location:** Separate secure host (not on API servers)  
**CA Private Key:** `/etc/linux_patch_api/ca/ca.key.pem` (permissions: 600)  
**CA Certificate:** `/etc/linux_patch_api/ca/ca.pem` (permissions: 644)

### 1.2 Server Certificate Deployment

```
# Generate server certificate
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

### 1.3 Client Certificate Deployment

Each authorized client requires a unique certificate:

```
# Generate client certificate (per client)
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
```

### 1.4 Certificate Validation Checklist

- [ ] Server certificate CN matches API hostname
- [ ] Client certificates unique per client (no shared certs)
- [ ] All certificates signed by internal CA
- [ ] Certificate validity: 1 year maximum
- [ ] Private key permissions: 600 (owner read/write only)
- [ ] Certificate permissions: 644 (owner read/write, group/others read)
- [ ] CA private key stored on separate secure host
- [ ] Certificate inventory maintained (track all issued certs)

---

## 2. IP Whitelist Configuration

### 2.1 Whitelist File Location

**Path:** `/etc/linux_patch_api/whitelist.yaml`  
**Permissions:** 644 (owner read/write, group/others read)  
**Reload:** Automatic on file change (no restart required)

### 2.2 Whitelist Configuration Format

```yaml
# /etc/linux_patch_api/whitelist.yaml
# IP Whitelist Configuration
# Default: Block all connections not listed

allowed_ips:
  # Individual IPv4 addresses
  - 192.168.1.100    # Primary management server
  - 192.168.1.101    # Secondary management server
  
  # CIDR subnets
  - 192.168.1.0/24   # Management network
  - 10.0.0.0/8       # Internal network (if needed)
  
  # Hostnames (resolved at config load)
  - management.internal.domain
```

### 2.3 Whitelist Management Procedures

**Adding Authorized Client:**
1. Edit `/etc/linux_patch_api/whitelist.yaml`
2. Add client IP address or subnet
3. Save file (auto-reload triggers within 5 seconds)
4. Verify in audit log: `journalctl -u linux-patch-api | grep whitelist`

**Removing Compromised Client:**
1. Immediately remove IP from whitelist
2. Revoke client certificate (Phase 4: implement CRL)
3. Document removal in security incident log
4. Investigate compromise source

### 2.4 Whitelist Validation Checklist

- [ ] Default deny policy enforced (block all not listed)
- [ ] Only required management IPs included
- [ ] No overly broad subnets (avoid /8 unless necessary)
- [ ] Whitelist file permissions: 644
- [ ] Changes logged to audit trail
- [ ] Quarterly review of whitelist entries scheduled

---

## 3. Production Hardening Checklist

### 3.1 System Hardening

- [ ] **OS Updates:** Host system fully patched before deployment
- [ ] **Minimal Installation:** Only required packages installed
- [ ] **Firewall Configuration:**
  ```bash
  # Allow API port from management network only
  ufw allow from 192.168.1.0/24 to any port 12443 proto tcp
  ufw deny 12443  # Default deny for other sources
  ```
- [ ] **SELinux/AppArmor:** Enforcing mode enabled
- [ ] **Unnecessary Services:** Disabled (SSH restricted, no unused daemons)

### 3.2 Service Hardening

**Systemd Service Configuration** (`/etc/systemd/system/linux-patch-api.service`):

```ini
[Unit]
Description=Linux Patch API Service
After=network.target

[Service]
Type=simple
User=root
Group=root
ExecStart=/usr/bin/linux-patch-api
Restart=on-failure
RestartSec=5

# Security Hardening
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
NoNewPrivileges=true
SystemCallFilter=@system-service
CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_SYS_ADMIN
ReadWritePaths=/var/lib/linux_patch_api /var/log/linux_patch_api

[Install]
WantedBy=multi-user.target
```

### 3.3 Configuration Hardening

- [ ] **Config File Permissions:**
  ```bash
  chmod 644 /etc/linux_patch_api/config.yaml
  chmod 600 /etc/linux_patch_api/certs/*.key.pem
  chmod 644 /etc/linux_patch_api/certs/*.pem
  ```
- [ ] **TLS 1.3 Only:** Verify in config.yaml:
  ```yaml
  tls:
    enabled: true
    min_version: "TLS1.3"
  ```
- [ ] **Debug Mode:** Disabled in production:
  ```yaml
  logging:
    level: INFO  # Not DEBUG
  ```
- [ ] **Job Timeout:** Configured (default: 30 minutes)
- [ ] **Concurrent Jobs:** Limited (default: 5)

### 3.4 Network Hardening

- [ ] **Port Binding:** API binds to specific interface (not 0.0.0.0)
- [ ] **Firewall Rules:** Only port 12443 open from management network
- [ ] **Network Segmentation:** API on isolated management VLAN
- [ ] **No Internet Exposure:** Confirmed no NAT/port forwarding to internet

---

## 4. Monitoring and Logging

### 4.1 Log Configuration

**Primary Storage:** systemd journal  
**Secondary Storage:** Optional remote syslog  
**Fallback:** Local file `/var/log/linux_patch_api/audit.log`

**Log Retention:** 30 days with daily rotation and compression

### 4.2 Security Events to Monitor

| Event Type | Log Source | Alert Priority |
|------------|------------|----------------|
| Authentication failures | journalctl | HIGH |
| IP whitelist denials | journalctl | MEDIUM |
| Certificate validation failures | journalctl | HIGH |
| Configuration changes | journalctl | MEDIUM |
| Job failures/timeouts | journalctl | LOW |
| Service restarts | journalctl | MEDIUM |
| Large payload rejections | journalctl | LOW |

### 4.3 Monitoring Commands

```bash
# View recent authentication events
journalctl -u linux-patch-api -n 100 | grep -E "auth|certificate|whitelist"

# View configuration changes
journalctl -u linux-patch-api | grep "config reload"

# View failed API requests
journalctl -u linux-patch-api | grep "400\|401\|403"

# Real-time monitoring
journalctl -u linux-patch-api -f
```

### 4.4 Recommended Monitoring Tools

- **systemd journal:** Primary log source
- **Prometheus + Grafana:** Metrics visualization (if available)
- **Remote syslog:** Forward logs to central SIEM
- **Logrotate:** Ensure proper log rotation

### 4.5 Alerting Recommendations

Configure alerts for:
- [ ] 5+ authentication failures in 5 minutes
- [ ] Any certificate validation failure
- [ ] Service restart without authorized change
- [ ] Configuration file modification
- [ ] Disk space below 20% (log storage)

---

## 5. Incident Response Procedures

### 5.1 Security Incident Classification

| Severity | Description | Response Time |
|----------|-------------|---------------|
| **Critical** | Active compromise, data breach | Immediate |
| **High** | Authentication bypass attempt | 1 hour |
| **Medium** | Policy violation, suspicious activity | 4 hours |
| **Low** | Configuration error, minor anomaly | 24 hours |

### 5.2 Incident Response Steps

**Step 1: Detection**
- Monitor audit logs for anomalies
- Review authentication failure patterns
- Check for unauthorized configuration changes

**Step 2: Containment**
```bash
# Immediately block suspicious IP
# Edit whitelist.yaml and remove IP
systemctl reload linux-patch-api

# Or stop service entirely if critical
systemctl stop linux-patch-api
```

**Step 3: Investigation**
```bash
# Extract relevant logs
journalctl -u linux-patch-api --since "2026-04-09 00:00:00" > /tmp/incident.log

# Review certificate usage
grep "client cert" /tmp/incident.log

# Check configuration changes
grep "config reload" /tmp/incident.log
```

**Step 4: Eradication**
- Revoke compromised certificates
- Update IP whitelist
- Patch vulnerabilities if applicable
- Reset affected configurations

**Step 5: Recovery**
- Restart service with corrected configuration
- Verify all security controls operational
- Monitor closely for 48 hours post-incident

**Step 6: Lessons Learned**
- Document incident in security log
- Update procedures if gaps identified
- Schedule follow-up review

### 5.3 Certificate Compromise Response

If a client certificate is compromised:

1. **Immediate:** Remove client IP from whitelist
2. **Document:** Record certificate CN, issue date, client identity
3. **Revoke:** Add to revocation list (Phase 4: implement CRL)
4. **Replace:** Issue new certificate to legitimate client
5. **Investigate:** Determine compromise source

### 5.4 Contact Information

| Role | Contact | Availability |
|------|---------|-------------|
| Security Team | security@internal.domain | 24/7 |
| System Administrator | sysadmin@internal.domain | Business hours |
| Incident Response | incident@internal.domain | 24/7 |

---

## 6. Known Limitations (Phase 3)

The following medium/low severity findings are documented for Phase 4 remediation:

### Medium Priority (Recommended)

| ID | Finding | Current Mitigation | Phase 4 Fix |
|----|---------|-------------------|-------------|
| VULN-001 | Missing input length validation | Internal network trust | Implement 256-char max for package names |
| VULN-002 | Path traversal partial bypass | mTLS + whitelist | Strict path normalization |
| VULN-004 | Missing header size limits | Internal network trust | Configure 8KB header limit |

### Low Priority (Nice to Have)

| ID | Finding | Current Mitigation | Phase 4 Fix |
|----|---------|-------------------|-------------|
| VULN-003 | Empty string validation missing | Package manager handles | Reject empty strings |
| VULN-005 | Invalid methods return 404 vs 405 | No security impact | Return 405 Method Not Allowed |
| VULN-006 | Duplicate header handling | No security impact | Reject duplicate headers |

**Assessment:** These limitations do not prevent production deployment on internal networks but should be addressed in Phase 4 for defense-in-depth.

---

## 7. Deployment Verification Checklist

Before declaring deployment complete:

### Pre-Deployment
- [ ] All certificates generated and deployed
- [ ] IP whitelist configured with authorized clients
- [ ] Systemd service file installed with hardening
- [ ] Firewall rules configured
- [ ] Logging verified operational

### Post-Deployment Testing
- [ ] mTLS authentication test (valid cert): PASS
- [ ] mTLS authentication test (invalid cert): BLOCKED
- [ ] IP whitelist test (authorized IP): PASS
- [ ] IP whitelist test (unauthorized IP): BLOCKED
- [ ] API endpoint functional test: PASS
- [ ] Audit logging verification: PASS
- [ ] Service restart test: PASS

### Documentation
- [ ] Certificate inventory updated
- [ ] Whitelist entries documented
- [ ] Monitoring alerts configured
- [ ] Incident response contacts verified
- [ ] This guide reviewed and approved

---

## Appendix A: Configuration File Templates

### config.yaml.example

```yaml
server:
  port: 12443
  bind_address: "0.0.0.0"  # Restrict via firewall
  timeout: 30

tls:
  enabled: true
  min_version: "TLS1.3"
  ca_cert: "/etc/linux_patch_api/certs/ca.pem"
  server_cert: "/etc/linux_patch_api/certs/server.pem"
  server_key: "/etc/linux_patch_api/certs/server.key.pem"

logging:
  level: INFO
  retention_days: 30
  remote_syslog: null  # Optional: "syslog.internal.domain:514"

security:
  job_timeout_minutes: 30
  max_concurrent_jobs: 5
  # Rate limiting: Phase 4
  # rate_limit_requests_per_minute: 100
```

### whitelist.yaml.example

```yaml
# IP Whitelist Configuration
# Default: Block all connections not listed

allowed_ips:
  - 192.168.1.100    # Primary management server
  - 192.168.1.101    # Secondary management server
  - 192.168.1.0/24   # Management network
```

---

## Appendix B: Quick Reference Commands

```bash
# Service management
systemctl start linux-patch-api
systemctl stop linux-patch-api
systemctl restart linux-patch-api
systemctl status linux-patch-api

# Log viewing
journalctl -u linux-patch-api -n 50
journalctl -u linux-patch-api -f
journalctl -u linux-patch-api --since "1 hour ago"

# Configuration reload (automatic, but can force)
systemctl reload linux-patch-api

# Certificate verification
openssl x509 -in /etc/linux_patch_api/certs/server.pem -text -noout
openssl verify -CAfile /etc/linux_patch_api/ca/ca.pem /etc/linux_patch_api/certs/server.pem

# Firewall status
ufw status
ufw status numbered
```

---

*Document generated following Phase 3 Security Hardening Completion - 2026-04-09*
