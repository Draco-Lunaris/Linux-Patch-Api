#!/usr/bin/env bash
# Generate development/test certificates for Linux Patch API.
#
# This script creates a self-signed CA, server certificate, and client
# certificate suitable for local development and testing.  It is NOT
# intended for production use.
#
# Usage:
#   ./scripts/generate-dev-certs.sh [OUTPUT_DIR]
#
# If OUTPUT_DIR is omitted, certificates are written to configs/certs/
# relative to the repository root.  The e2e Python test certs are also
# regenerated under tests/e2e/certs/.
#
# Private keys (*.key, *.key.pem) are excluded from git via .gitignore
# and must NEVER be committed to version control.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

OUTPUT_DIR="${1:-$REPO_ROOT/configs/certs}"
E2E_DIR="$REPO_ROOT/tests/e2e/certs"

DAYS_CA=3650
DAYS_CERT=365

echo "Generating development certificates..."
echo "  Output dir: $OUTPUT_DIR"
echo "  E2E dir:    $E2E_DIR"

mkdir -p "$OUTPUT_DIR"
mkdir -p "$E2E_DIR"

# CA
echo "[1/6] Generating CA key and certificate..."
openssl genrsa -out "$OUTPUT_DIR/ca.key.pem" 4096 2>/dev/null
chmod 600 "$OUTPUT_DIR/ca.key.pem"
openssl req -x509 -new -nodes -key "$OUTPUT_DIR/ca.key.pem" -sha256 -days "$DAYS_CA" -out "$OUTPUT_DIR/ca.pem" -subj "/CN=LinuxPatchAPI Dev CA/O=Internal/C=US"

# Server certificate
echo "[2/6] Generating server key and certificate..."
openssl genrsa -out "$OUTPUT_DIR/server.key.pem" 2048 2>/dev/null
chmod 600 "$OUTPUT_DIR/server.key.pem"
openssl req -new -key "$OUTPUT_DIR/server.key.pem" -out "$OUTPUT_DIR/server.csr.pem" -subj "/CN=localhost/O=Internal/C=US"
openssl x509 -req -in "$OUTPUT_DIR/server.csr.pem" -CA "$OUTPUT_DIR/ca.pem" -CAkey "$OUTPUT_DIR/ca.key.pem" -CAcreateserial -out "$OUTPUT_DIR/server.pem" -days "$DAYS_CERT" -sha256

# Client certificate
echo "[3/6] Generating client key and certificate..."
openssl genrsa -out "$OUTPUT_DIR/client001.key.pem" 2048 2>/dev/null
chmod 600 "$OUTPUT_DIR/client001.key.pem"
openssl req -new -key "$OUTPUT_DIR/client001.key.pem" -out "$OUTPUT_DIR/client001.csr.pem" -subj "/CN=client001/O=Internal/C=US"
openssl x509 -req -in "$OUTPUT_DIR/client001.csr.pem" -CA "$OUTPUT_DIR/ca.pem" -CAkey "$OUTPUT_DIR/ca.key.pem" -CAcreateserial -out "$OUTPUT_DIR/client001.pem" -days "$DAYS_CERT" -sha256

# E2E test certificates
echo "[4/6] Generating e2e test CA certificate..."
cp "$OUTPUT_DIR/ca.pem" "$E2E_DIR/ca.crt"

echo "[5/6] Generating e2e test client certificate..."
openssl genrsa -out "$E2E_DIR/client.key" 2048 2>/dev/null
chmod 600 "$E2E_DIR/client.key"
openssl req -new -key "$E2E_DIR/client.key" -out "$E2E_DIR/client.csr" -subj "/CN=e2e-test-client/O=Internal/C=US"
openssl x509 -req -in "$E2E_DIR/client.csr" -CA "$OUTPUT_DIR/ca.pem" -CAkey "$OUTPUT_DIR/ca.key.pem" -CAcreateserial -out "$E2E_DIR/client.crt" -days "$DAYS_CERT" -sha256

# Cleanup CSR files
echo "[6/6] Cleaning up CSR files..."
rm -f "$OUTPUT_DIR/server.csr.pem" "$OUTPUT_DIR/client001.csr.pem" "$E2E_DIR/client.csr"

echo
echo "Development certificates generated successfully."
echo "  CA cert:         $OUTPUT_DIR/ca.pem"
echo "  Server cert:     $OUTPUT_DIR/server.pem"
echo "  Server key:      $OUTPUT_DIR/server.key.pem"
echo "  Client cert:     $OUTPUT_DIR/client001.pem"
echo "  Client key:      $OUTPUT_DIR/client001.key.pem"
echo "  E2E CA cert:     $E2E_DIR/ca.crt"
echo "  E2E client cert: $E2E_DIR/client.crt"
echo "  E2E client key:  $E2E_DIR/client.key"
echo
echo "⚠  WARNING: These are development-only certificates. Do NOT use in production."
echo "⚠  Private keys (*.key, *.key.pem) are excluded from git via .gitignore."
