# Internal CA Setup Guide

## Overview

This document describes how to set up an internal Certificate Authority (CA) for mTLS authentication in the Linux Patch API.

## Certificate Requirements

Per SPEC.md:
- **CA Type:** Internal self-hosted Certificate Authority
- **Certificate Type:** Unique client certificate per client (1-year validity)
- **TLS Version:** TLS 1.3 only
- **Distribution:** Manual certificate distribution
- **Rotation:** 1-year certificate expiry, manual renewal process

## CA Setup Steps

### 1. Create CA Private Key

```bash
# Create CA private key (keep this secure!)
openssl genrsa -aes256 -out ca.key.pem 4096
chmod 600 ca.key.pem
```

### 2. Create CA Certificate

```bash
# Create self-signed CA certificate
openssl req -x509 -new -nodes -key ca.key.pem -sha256 -days 3650 \
  -out ca.pem \
  -subj "/CN=LinuxPatchAPI CA/O=Internal/C=US"
```

### 3. Create Server Certificate

```bash
# Create server private key
openssl genrsa -out server.key.pem 2048
chmod 600 server.key.pem

# Create server CSR
openssl req -new -key server.key.pem -out server.csr.pem \
  -subj "/CN=linux-patch-api/O=Internal/C=US"

# Create server certificate (signed by CA)
openssl x509 -req -in server.csr.pem -CA ca.pem -CAkey ca.key.pem \
  -CAcreateserial -out server.pem -days 365 -sha256

# Verify server certificate
openssl x509 -in server.pem -text -noout | grep -E "(Subject:|DNS:)"
```

### 4. Create Client Certificate (per client)

```bash
# Create client private key
openssl genrsa -out client001.key.pem 2048
chmod 600 client001.key.pem

# Create client CSR
openssl req -new -key client001.key.pem -out client001.csr.pem \
  -subj "/CN=client001/O=Internal/C=US"

# Create client certificate (signed by CA)
openssl x509 -req -in client001.csr.pem -CA ca.pem -CAkey ca.key.pem \
  -CAcreateserial -out client001.pem -days 365 -sha256

# Package client cert + key + CA into PKCS12 (optional, for easier distribution)
openssl pkcs12 -export -in client001.pem -inkey client001.key.pem \
  -certfile ca.pem -out client001.p12
```

## Certificate Deployment

### Server Side

Copy certificates to `/etc/linux_patch_api/certs/`:

```bash
mkdir -p /etc/linux_patch_api/certs/
cp ca.pem /etc/linux_patch_api/certs/
cp server.pem /etc/linux_patch_api/certs/
cp server.key.pem /etc/linux_patch_api/certs/
chmod 600 /etc/linux_patch_api/certs/server.key.pem
chmod 644 /etc/linux_patch_api/certs/ca.pem
chmod 644 /etc/linux_patch_api/certs/server.pem
```

### Client Side

Distribute client certificates securely:
1. Client certificate: `client001.pem`
2. Client private key: `client001.key.pem`
3. CA certificate: `ca.pem`

**Warning:** Never transmit private keys over insecure channels.


## Certificate Renewal

Certificates expire after 1 year. Renewal process:
1. Generate new certificate with same key or new key
2. Sign new certificate with CA
3. Distribute new certificate to client/server
4. Restart service to load new certificate

## Revocation

Not implemented per SPEC.md. Rely on:
- Certificate expiry (1-year max)
- Physical certificate retrieval on employee departure
- IP whitelist for additional access control

## Security Notes

- **CA Private Key:** Store securely, restrict access
- **Client Keys:** 600 permissions, user-read-only
- **Certificates:** 644 permissions (public information)
- **Transport:** All certificate distribution over secure channels
