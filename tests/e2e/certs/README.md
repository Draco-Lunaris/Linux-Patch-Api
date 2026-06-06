# E2E Test Certificates

**⚠️ Private keys are NOT committed to version control.**

This directory holds mTLS certificates used by the Python E2E test suite.
The `client.key` private key is excluded from git via `.gitignore`.

## Generating Test Certificates

Certificates are generated automatically by the E2E test runner, or manually:

```bash
./scripts/generate-dev-certs.sh
```

This script creates `ca.crt`, `client.crt`, and `client.key` in this directory.

## Files

| File | Committed | Description |
|------|-----------|-------------|
| `ca.crt` | ✅ Yes | CA certificate (public) |
| `client.crt` | ✅ Yes | Client certificate (public) |
| `client.key` | ❌ No | Client private key (gitignored) |
