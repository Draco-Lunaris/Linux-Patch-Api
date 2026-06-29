# Self-Update Operational Runbook

**Date:** 2026-06-29
**Applies to:** Linux Patch API v2.0.0+

---

## Triggering a Self-Update

### From the Manager

```bash
# Upgrade to latest available version
curl -X POST --cacert ca.pem --cert client.pem --key client.key \
  -H 'Content-Type: application/json' \
  -d '{}' \
  https://agent-host:12443/api/v1/system/update

# Upgrade to specific version (downgrade or pin)
curl -X POST --cacert ca.pem --cert client.pem --key client.key \
  -H 'Content-Type: application/json' \
  -d '{"target_version":"1.5.6-1"}' \
  https://agent-host:12443/api/v1/system/update
```

**Response:** HTTP 202 Accepted
```json
{"success":true,"data":{"status":"pending","target_version":"1.5.6-1","message":"Self-update initiated; agent will restart with new version"}}
```

### Expected Behavior

1. Agent validates request, writes request file
2. Agent starts detached systemd unit (`linux-patch-api-update.service`)
3. Update unit runs `self-update.sh` in its own cgroup (`system.slice`)
4. Script detects package manager, refreshes repo metadata, runs upgrade
5. dpkg/prerm stops agent; update service survives (different cgroup)
6. dpkg/postinst starts new agent binary
7. Script waits up to 60s for service to become active (health check)
8. If healthy: writes success marker, cleans up request file
9. If not healthy: rolls back to previous version, writes failure marker

## Monitoring Progress

### Check Update Status

```bash
# Via API (requires mTLS)
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://agent-host:12443/api/v1/system/update/status

# Via marker file (on the agent host)
cat /var/lib/linux_patch_api/last_self_update.json
```

**Marker file format:**
```json
{
  "previous_version": "1.5.6",
  "new_version": "1.6.0",
  "changed": true,
  "status": "success",
  "error": null,
  "at": "2026-06-27T02:00:00Z"
}
```

**Status values:** `pending`, `success`, `failed`

### Check Service Health

```bash
# API health endpoint
curl -s --cacert ca.pem --cert client.pem --key client.key \
  https://agent-host:12443/health

# systemd status
systemctl status linux-patch-api.service

# NRestarts count (should not climb)
systemctl show -p NRestarts linux-patch-api.service
```

## Diagnosing a Failed Self-Update

### Step 1: Check Marker File

```bash
cat /var/lib/linux_patch_api/last_self_update.json
```

If `status` is `failed`, the `error` field contains the failure reason.

### Step 2: Check Update Service Logs

```bash
# Update service logs
journalctl -u linux-patch-api-update.service --no-pager -n 50

# Agent service logs
journalctl -u linux-patch-api.service --no-pager -n 50

# File logs (if configured)
tail -100 /var/log/linux_patch_api/agent.log
```

### Step 3: Check Request File

```bash
# If request file still exists, the update service may not have run
cat /var/lib/linux_patch_api/self-update.request

# Check if update service is active
systemctl is-active linux-patch-api-update.service
```

### Step 4: Check Package State

```bash
# Installed version
dpkg-query -W -f='${Version}' linux-patch-api  # Debian/Ubuntu
rpm -q --qf '%{VERSION}-%{RELEASE}' linux-patch-api  # RPM
pacman -Q linux-patch-api  # Arch
apk info -v linux-patch-api  # Alpine

# Check if package is in half-configured state
dpkg -l linux-patch-api | grep -v '^ii'
```

## Manual Rollback

### Via API (Manager-Initiated Downgrade)

```bash
curl -X POST --cacert ca.pem --cert client.pem --key client.key \
  -H 'Content-Type: application/json' \
  -d '{"target_version":"1.5.6-1"}' \
  https://agent-host:12443/api/v1/system/update
```

### Via Package Manager (Direct)

```bash
# Debian/Ubuntu
apt-get install -y --allow-downgrades -- linux-patch-api=1.5.6-1

# RPM
dnf install -y -- linux-patch-api-1.5.6-1

# Alpine
apk add -- linux-patch-api=1.5.6-r0

# Arch (from cache)
pacman -U --noconfirm /var/cache/pacman/pkg/linux-patch-api-1.5.6-*.pkg.tar.zst
```

After manual rollback:
```bash
systemctl restart linux-patch-api.service
# Clean up request/marker files
rm -f /var/lib/linux_patch_api/self-update.request
rm -f /var/lib/linux_patch_api/last_self_update.json
```

## Auto-Rollback Behavior

`self-update.sh` includes an automatic health check after package install:

1. Waits up to 60 seconds for `systemctl is-active linux-patch-api.service`
2. If service does not become active: installs the previously recorded version
3. Writes failure marker with rollback status
4. Exits with error code

**If auto-rollback also fails:** Manual intervention required. Check:
- Package cache: is the previous version still available?
- Repo: is the repo reachable and signed correctly?
- Service: does the binary segfault on startup?

## Log Locations

| Log Source | Location |
|------------|----------|
| Agent service (systemd) | `journalctl -u linux-patch-api.service` |
| Update service (systemd) | `journalctl -u linux-patch-api-update.service` |
| Agent file logs | `/var/log/linux_patch_api/agent.log` |
| Marker file | `/var/lib/linux_patch_api/last_self_update.json` |
| Request file | `/var/lib/linux_patch_api/self-update.request` |
| Package manager | `journalctl` or `/var/log/apt/term.log` (apt) |
