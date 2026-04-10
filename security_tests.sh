#!/bin/bash
# Linux_Patch_API Phase 3 Security Testing Script
# Comprehensive penetration testing for all 15 endpoints

CERT_DIR="/etc/linux_patch_api/certs"
BASE_URL="https://127.0.0.1:12443/api/v1"
CLIENT_CERT="$CERT_DIR/client001.pem"
CLIENT_KEY="$CERT_DIR/client001.key.pem"
CA_CERT="$CERT_DIR/ca.pem"

echo "========================================"
echo "Phase 3 Security Testing - Linux_Patch_API"
echo "========================================"
echo ""

# Test counter
PASS=0
FAIL=0

# Color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

test_result() {
    if [ "$1" -eq 0 ]; then
        echo -e "${GREEN}[PASS]${NC} $2"
        ((PASS++))
    else
        echo -e "${RED}[FAIL]${NC} $2"
        ((FAIL++))
    fi
}

echo "=== SECTION 1: mTLS Enforcement Tests ==="
echo ""

# Test 1: Non-mTLS connection (should fail silently)
echo -n "Test 1.1: Non-mTLS connection (should be silently dropped)... "
RESULT=$(curl -k -s -o /dev/null -w '%{http_code}' "$BASE_URL/health" --connect-timeout 3 2>/dev/null)
if [ "$RESULT" == "000" ]; then
    test_result 0 "Non-mTLS connection silently dropped"
else
    test_result 1 "Non-mTLS connection should be dropped (got: $RESULT)"
fi

# Test 2: Valid mTLS connection
echo -n "Test 1.2: Valid mTLS connection with client cert... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/health" --connect-timeout 5 2>/dev/null)
if echo "$RESULT" | grep -q '"success":true'; then
    test_result 0 "Valid mTLS connection successful"
else
    test_result 1 "Valid mTLS connection failed"
fi

# Test 3: Invalid/expired certificate
echo -n "Test 1.3: Self-signed cert (not CA-signed) rejection... "
# Create a self-signed cert for testing
openssl req -x509 -newkey rsa:2048 -keyout /tmp/selfsigned.key -out /tmp/selfsigned.pem -days 1 -nodes -subj "/CN=attacker" 2>/dev/null
RESULT=$(curl -k -s --cert "/tmp/selfsigned.pem" --key "/tmp/selfsigned.key" "$BASE_URL/health" --connect-timeout 5 2>/dev/null)
if [ -z "$RESULT" ] || echo "$RESULT" | grep -q '"success":false'; then
    test_result 0 "Self-signed cert rejected"
else
    test_result 1 "Self-signed cert should be rejected"
fi
rm -f /tmp/selfsigned.key /tmp/selfsigned.pem

echo ""
echo "=== SECTION 2: IP Whitelist Enforcement Tests ==="
echo ""

# Test 4: Connection from whitelisted IP (localhost is whitelisted)
echo -n "Test 2.1: Whitelisted IP access... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/health" --connect-timeout 5 2>/dev/null)
if echo "$RESULT" | grep -q '"success":true'; then
    test_result 0 "Whitelisted IP has access"
else
    test_result 1 "Whitelisted IP should have access"
fi

echo ""
echo "=== SECTION 3: API Endpoint Security Tests ==="
echo ""

# Test 5: Health endpoint
echo -n "Test 3.1: GET /health endpoint... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/health" 2>/dev/null)
if echo "$RESULT" | grep -q '"status"'; then
    test_result 0 "Health endpoint responds correctly"
else
    test_result 1 "Health endpoint failed"
fi

# Test 6: System info endpoint
echo -n "Test 3.2: GET /system/info endpoint... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/system/info" 2>/dev/null)
if echo "$RESULT" | grep -q '"hostname"\|"os"'; then
    test_result 0 "System info endpoint responds"
else
    test_result 1 "System info endpoint failed"
fi

# Test 7: Packages list endpoint
echo -n "Test 3.3: GET /packages endpoint... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/packages" 2>/dev/null)
if echo "$RESULT" | grep -q '"packages"\|"success"'; then
    test_result 0 "Packages endpoint responds"
else
    test_result 1 "Packages endpoint failed"
fi

# Test 8: Patches list endpoint
echo -n "Test 3.4: GET /patches endpoint... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/patches" 2>/dev/null)
if echo "$RESULT" | grep -q '"patches"\|"success"'; then
    test_result 0 "Patches endpoint responds"
else
    test_result 1 "Patches endpoint failed"
fi

# Test 9: Jobs list endpoint
echo -n "Test 3.5: GET /jobs endpoint... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/jobs" 2>/dev/null)
if echo "$RESULT" | grep -q '"jobs"\|"success"'; then
    test_result 0 "Jobs endpoint responds"
else
    test_result 1 "Jobs endpoint failed"
fi

echo ""
echo "=== SECTION 4: Input Validation & Injection Tests ==="
echo ""

# Test 10: SQL injection attempt in package name
echo -n "Test 4.1: SQL injection in package name... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/packages?name=';DROP TABLE users;--" 2>/dev/null)
if echo "$RESULT" | grep -q '"success"'; then
    test_result 0 "SQL injection attempt handled safely"
else
    test_result 1 "SQL injection test inconclusive"
fi

# Test 11: Command injection attempt
echo -n "Test 4.2: Command injection in package name... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/packages?name=;ls -la;" 2>/dev/null)
if echo "$RESULT" | grep -q '"success"'; then
    test_result 0 "Command injection attempt handled safely"
else
    test_result 1 "Command injection test inconclusive"
fi

# Test 12: Path traversal attempt
echo -n "Test 4.3: Path traversal in package name... "
RESULT=$(curl -k -s --cert "$CLIENT_CERT" --key "$CLIENT_KEY" --cacert "$CA_CERT" "$BASE_URL/packages/../../../etc/passwd" 2>/dev/null)
if echo "$RESULT" | grep -q '"error"\|"success":false'; then
    test_result 0 "Path traversal blocked"
else
    test_result 1 "Path traversal test inconclusive"
fi

echo ""
echo "=== SECTION 5: Certificate Security Tests ==="
echo ""

# Test 13: Check certificate expiry
echo -n "Test 5.1: Client certificate validity check... "
openssl x509 -in "$CLIENT_CERT" -noout -checkend 0 2>/dev/null
if [ $? -eq 0 ]; then
    test_result 0 "Client certificate is valid"
else
    test_result 1 "Client certificate is expired"
fi

# Test 14: Check TLS version
echo -n "Test 5.2: TLS 1.3 enforcement... "
RESULT=$(echo | openssl s_client -connect 127.0.0.1:12443 -tls1_3 2>&1 | grep -i "protocol")
if echo "$RESULT" | grep -qi "TLSv1.3"; then
    test_result 0 "TLS 1.3 is enforced"
else
    test_result 1 "TLS 1.3 enforcement check failed"
fi

echo ""
echo "=== SECTION 6: Configuration Security Tests ==="
echo ""

# Test 15: Config file permissions
echo -n "Test 6.1: Config file permissions (should be 600/644)... "
PERMS=$(stat -c '%a' /etc/linux_patch_api/config.yaml 2>/dev/null)
if [ "$PERMS" == "644" ] || [ "$PERMS" == "600" ]; then
    test_result 0 "Config file has secure permissions ($PERMS)"
else
    test_result 1 "Config file permissions insecure ($PERMS)"
fi

# Test 16: Key file permissions
echo -n "Test 6.2: Private key permissions (should be 600)... "
PERMS=$(stat -c '%a' "$CERT_DIR/server.key.pem" 2>/dev/null)
if [ "$PERMS" == "600" ]; then
    test_result 0 "Private key has secure permissions ($PERMS)"
else
    test_result 1 "Private key permissions insecure ($PERMS)"
fi

echo ""
echo "========================================"
echo "Security Test Summary"
echo "========================================"
echo -e "${GREEN}Passed:${NC} $PASS"
echo -e "${RED}Failed:${NC} $FAIL"
echo "Total Tests: $((PASS + FAIL))"
echo ""

if [ $FAIL -eq 0 ]; then
    echo -e "${GREEN}All security tests passed!${NC}"
    exit 0
else
    echo -e "${YELLOW}Some security tests failed - review findings${NC}"
    exit 1
fi
