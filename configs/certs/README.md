# Development Certificates

**⚠️ Private keys are NOT committed to version control.**

This directory is used for local development certificates only. Private key
files (`*.key`, `*.key.pem`) are excluded from git via `.gitignore`.

## Generating Development Certificates

Run the generation script from the repository root:

```bash
./scripts/generate-dev-certs.sh
```

This creates:
- `ca.pem` / `ca.key.pem` — Internal CA certificate and key
- `server.pem` / `server.key.pem` — Server certificate and key
- `client001.pem` / `client001.key.pem` — Client certificate and key
- `tests/e2e/certs/` — E2E test certificates

## Production Deployments

Production deployments should use certificates issued by the organisation's
internal CA. The `install.sh` script and systemd unit handle production
certificate paths at `/etc/linux_patch_api/certs/`.

## Security

- **Never commit private keys** (`*.key`, `*.key.pem`) to version control
- Private keys must have `0600` permissions in production
- The `gitleaks` CI check scans for accidentally committed secrets
- See `SECURITY_FINDINGS_REPORT.md` and `SECURITY.md` for full details
