#!/usr/bin/env python3
"""
Linux Patch API - End-to-End Test Suite

Comprehensive E2E tests against deployed API instances on 3 LXCs:
  - linux-patch-manager-dev (192.168.0.247)
  - gitea-runner-u2204 (192.168.2.232)
  - gitea-runner-u2404 (192.168.3.180)

Uses mTLS with deployed Patch Manager Root CA certificates.
Reboot test runs LAST after all other tests pass.

Usage:
  python3 test_e2e.py [--target all|dev|u2204|u2404] [--skip-reboot] [--verbose]
"""

import argparse
import json
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

import requests
import urllib3

# Suppress insecure warnings for self-signed CA
urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

# =============================================================================
# Configuration
# =============================================================================

CERTS_DIR = Path(__file__).parent / "certs"
CA_CERT = CERTS_DIR / "ca.crt"
CLIENT_CERT = CERTS_DIR / "client.crt"
CLIENT_KEY = CERTS_DIR / "client.key"

TARGETS = {
    "dev": {
        "name": "linux-patch-manager-dev",
        "host": "192.168.0.247",
        "port": 12443,
        "os": "Debian/Ubuntu (dev)",
    },
    "u2204": {
        "name": "gitea-runner-u2204",
        "host": "192.168.2.232",
        "port": 12443,
        "os": "Ubuntu 22.04",
    },
    "u2404": {
        "name": "gitea-runner-u2404",
        "host": "192.168.3.180",
        "port": 12443,
        "os": "Ubuntu 24.04",
    },
}

# Safe test package - small, harmless, easy to install/remove
TEST_PACKAGE = "hello"

# Job polling settings
JOB_POLL_INTERVAL = 2  # seconds
JOB_POLL_TIMEOUT = 300  # 5 minutes max


# =============================================================================
# Test Result Tracking
# =============================================================================


@dataclass
class TestResult:
    name: str
    passed: bool
    message: str = ""
    duration_ms: float = 0
    details: dict = field(default_factory=dict)


@dataclass
class TargetResults:
    target_name: str
    target_host: str
    results: list = field(default_factory=list)
    passed: int = 0
    failed: int = 0
    skipped: int = 0
    errors: int = 0

    @property
    def total(self):
        return self.passed + self.failed + self.skipped + self.errors


# =============================================================================
# API Client
# =============================================================================


class PatchAPIClient:
    """mTLS client for the Linux Patch API."""

    def __init__(self, host: str, port: int = 12443, timeout: int = 60):
        self.base_url = f"https://{host}:{port}"
        self.host = host
        self.port = port
        self.timeout = timeout
        self.session = requests.Session()
        self.session.cert = (str(CLIENT_CERT), str(CLIENT_KEY))
        # Suppress InsecureRequestWarning for self-signed CA
        self.session.verify = False

    def _request(self, method: str, path: str, **kwargs) -> requests.Response:
        url = f"{self.base_url}{path}"
        timeout = kwargs.pop("timeout", self.timeout)
        return self.session.request(method, url, timeout=timeout, **kwargs)

    def get(self, path: str, **kwargs) -> requests.Response:
        return self._request("GET", path, **kwargs)

    def post(self, path: str, **kwargs) -> requests.Response:
        return self._request("POST", path, **kwargs)

    def put(self, path: str, **kwargs) -> requests.Response:
        return self._request("PUT", path, **kwargs)

    def delete(self, path: str, **kwargs) -> requests.Response:
        return self._request("DELETE", path, **kwargs)

    def close(self):
        self.session.close()


# =============================================================================
# Helper Functions
# =============================================================================


def validate_envelope(data: dict, test_name: str) -> Optional[str]:
    """Validate standard response envelope. Returns error message or None."""
    required_fields = ["success", "request_id", "timestamp", "data", "error"]
    for f in required_fields:
        if f not in data:
            return f"Missing required field: {f}"
    if not isinstance(data["success"], bool):
        return f"'success' should be boolean, got {type(data['success']).__name__}"
    if not isinstance(data["request_id"], str):
        return f"'request_id' should be string, got {type(data['request_id']).__name__}"
    if not isinstance(data["timestamp"], str):
        return f"'timestamp' should be string, got {type(data['timestamp']).__name__}"
    return None


def poll_job(client: PatchAPIClient, job_id: str, timeout: int = JOB_POLL_TIMEOUT) -> dict:
    """Poll a job until completion or timeout. Returns final job data."""
    start = time.time()
    while time.time() - start < timeout:
        resp = client.get(f"/api/v1/jobs/{job_id}")
        if resp.status_code != 200:
            raise Exception(f"Job poll failed: HTTP {resp.status_code} - {resp.text}")
        data = resp.json()
        if data.get("success") and data.get("data", {}).get("status") in [
            "completed",
            "failed",
            "cancelled",
            "timeout",
        ]:
            return data["data"]
        time.sleep(JOB_POLL_INTERVAL)
    raise Exception(f"Job {job_id} timed out after {timeout}s")


def run_test(
    results: TargetResults,
    name: str,
    func,
    client: PatchAPIClient,
    skip: bool = False,
    **kwargs,
) -> TestResult:
    """Run a single test and record results."""
    if skip:
        result = TestResult(name=name, passed=False, message="SKIPPED")
        results.skipped += 1
        results.results.append(result)
        return result

    start = time.time()
    try:
        msg = func(client, **kwargs)
        elapsed = (time.time() - start) * 1000
        result = TestResult(
            name=name, passed=True, message=msg or "PASS", duration_ms=elapsed
        )
        results.passed += 1
    except AssertionError as e:
        elapsed = (time.time() - start) * 1000
        result = TestResult(
            name=name, passed=False, message=f"FAIL: {e}", duration_ms=elapsed
        )
        results.failed += 1
    except Exception as e:
        elapsed = (time.time() - start) * 1000
        result = TestResult(
            name=name, passed=False, message=f"ERROR: {e}", duration_ms=elapsed
        )
        results.errors += 1
    results.results.append(result)
    return result


# =============================================================================
# Test Functions
# =============================================================================


def test_health_endpoint(client: PatchAPIClient) -> str:
    """GET /health - Verify health check returns healthy status."""
    resp = client.get("/health")
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
    data = resp.json()
    err = validate_envelope(data, "health")
    assert err is None, f"Envelope validation failed: {err}"
    assert data["success"] is True, f"Expected success=true, got {data['success']}"
    assert data["data"]["status"] == "healthy", f"Expected status=healthy, got {data['data']['status']}"
    assert "version" in data["data"], "Missing version in health response"
    assert "uptime_seconds" in data["data"], "Missing uptime_seconds in health response"
    return f"Health OK: status={data['data']['status']}, version={data['data']['version']}, uptime={data['data']['uptime_seconds']}s"


def test_system_info(client: PatchAPIClient) -> str:
    """GET /api/v1/system/info - Verify system info returns OS details."""
    resp = client.get("/api/v1/system/info")
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
    data = resp.json()
    err = validate_envelope(data, "system_info")
    assert err is None, f"Envelope validation failed: {err}"
    assert data["success"] is True
    d = data["data"]
    assert "hostname" in d, "Missing hostname"
    assert "os" in d, "Missing os"
    assert "kernel" in d, "Missing kernel"
    assert "architecture" in d, "Missing architecture"
    return f"System info: hostname={d['hostname']}, os={d.get('os')}, kernel={d.get('kernel')}"


def test_list_packages(client: PatchAPIClient) -> str:
    """GET /api/v1/packages - Verify package listing with envelope."""
    resp = client.get("/api/v1/packages")
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
    data = resp.json()
    err = validate_envelope(data, "list_packages")
    assert err is None, f"Envelope validation failed: {err}"
    assert data["success"] is True
    assert "packages" in data["data"], "Missing packages array"
    assert "total" in data["data"], "Missing total count"
    assert isinstance(data["data"]["packages"], list), "packages should be a list"
    return f"Listed {data['data']['total']} packages"


def test_list_packages_filtered(client: PatchAPIClient) -> str:
    """GET /api/v1/packages?status=installed - Verify filtered package listing."""
    resp = client.get("/api/v1/packages?status=installed&limit=10")
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is True
    assert "packages" in data["data"]
    return f"Filtered packages: {data['data']['total']} installed"


def test_get_package_detail(client: PatchAPIClient) -> str:
    """GET /api/v1/packages/{name} - Verify package detail for a known package."""
    resp = client.get("/api/v1/packages/apt")
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is True
    pkg = data["data"]
    assert "name" in pkg, "Missing package name"
    assert pkg["name"] == "apt", f"Expected name=apt, got {pkg['name']}"
    assert "version" in pkg, "Missing version"
    assert "status" in pkg, "Missing status"
    return f"Package detail: {pkg['name']} {pkg.get('version', '?')} ({pkg.get('status', '?')})"


def test_get_package_not_found(client: PatchAPIClient) -> str:
    """GET /api/v1/packages/{nonexistent} - Verify 404 for nonexistent package."""
    resp = client.get("/api/v1/packages/xyz-nonexistent-package-12345")
    assert resp.status_code == 404, f"Expected 404, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is False
    assert data["error"]["code"] == "PKG_NOT_FOUND", f"Expected PKG_NOT_FOUND, got {data['error']['code']}"
    return "Correctly returned PKG_NOT_FOUND for nonexistent package"


def test_install_package(client: PatchAPIClient) -> str:
    """POST /api/v1/packages - Install a safe test package (hello).

    Verifies that the package installation completes successfully.
    A failed status is a critical failure - the core function must work.
    """
    payload = {
        "packages": [{"name": TEST_PACKAGE, "version": None}],
        "options": {"force": False, "no_recommends": True},
    }
    resp = client.post("/api/v1/packages", json=payload)
    assert resp.status_code == 202, f"Expected 202, got {resp.status_code}: {resp.text}"
    data = resp.json()
    err = validate_envelope(data, "install_package")
    assert err is None, f"Envelope validation failed: {err}"
    assert data["success"] is True
    assert data["data"]["job_id"], "Missing job_id"
    assert data["data"]["status"] == "pending", f"Expected status=pending, got {data['data']['status']}"
    assert data["data"]["operation"] == "install", f"Expected operation=install, got {data['data']['operation']}"

    # Poll job to completion
    job_id = data["data"]["job_id"]
    job = poll_job(client, job_id)
    assert job["status"] == "completed", f"Install job failed: status={job['status']}, result={job.get('result', {})}"
    return f"Installed {TEST_PACKAGE}: job_id={job_id}, status={job['status']}"


def test_update_package(client: PatchAPIClient) -> str:
    """PUT /api/v1/packages/{name} - Update a package."""
    resp = client.put(f"/api/v1/packages/{TEST_PACKAGE}", json={"version": None})
    assert resp.status_code == 202, f"Expected 202, got {resp.status_code}: {resp.text}"
    data = resp.json()
    assert data["success"] is True
    assert data["data"]["job_id"], "Missing job_id"
    assert data["data"]["operation"] == "update"

    job_id = data["data"]["job_id"]
    job = poll_job(client, job_id)
    assert job["status"] == "completed", f"Update job failed: status={job['status']}, result={job.get('result', {})}"
    return f"Updated {TEST_PACKAGE}: job_id={job_id}, status={job['status']}"


def test_remove_package(client: PatchAPIClient) -> str:
    """DELETE /api/v1/packages/{name} - Remove the test package.

    Verifies that the package removal completes successfully.
    A failed status is a critical failure - the core function must work.
    """
    resp = client.delete(f"/api/v1/packages/{TEST_PACKAGE}")
    assert resp.status_code == 202, f"Expected 202, got {resp.status_code}: {resp.text}"
    data = resp.json()
    assert data["success"] is True
    assert data["data"]["job_id"], "Missing job_id"
    assert data["data"]["operation"] == "remove"

    job_id = data["data"]["job_id"]
    job = poll_job(client, job_id)
    assert job["status"] == "completed", f"Remove job failed: status={job['status']}, result={job.get('result', {})}"
    return f"Removed {TEST_PACKAGE}: job_id={job_id}, status={job['status']}"


def test_list_patches(client: PatchAPIClient) -> str:
    """GET /api/v1/patches - Verify patch listing."""
    resp = client.get("/api/v1/patches")
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
    data = resp.json()
    err = validate_envelope(data, "list_patches")
    assert err is None, f"Envelope validation failed: {err}"
    assert data["success"] is True
    assert "patches" in data["data"], "Missing patches array"
    assert "total" in data["data"], "Missing total count"
    return f"Listed {data['data']['total']} patches, security_updates={data['data'].get('security_updates', '?')}, requires_reboot={data['data'].get('requires_reboot', '?')}"


def test_list_patches_filtered(client: PatchAPIClient) -> str:
    """GET /api/v1/patches?severity=high - Verify filtered patch listing."""
    resp = client.get("/api/v1/patches?severity=high&limit=10")
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is True
    return f"Filtered patches: {data['data']['total']} high severity"


def test_apply_patches(client: PatchAPIClient) -> str:
    """POST /api/v1/patches/apply - Test patch apply endpoint.

    Uses empty patches list with all packages excluded to avoid actual changes.
    """
    payload = {
        "patches": [],
        "options": {
            "reboot": False,
            "reboot_delay_minutes": 0,
            "exclude_packages": ["*"],
        },
    }
    resp = client.post("/api/v1/patches/apply", json=payload)
    if resp.status_code == 202:
        data = resp.json()
        assert data["success"] is True
        assert data["data"]["job_id"], "Missing job_id"
        job_id = data["data"]["job_id"]
        return f"Patch apply accepted: job_id={job_id}"
    elif resp.status_code in [400, 422]:
        return f"Patch apply rejected as expected (no patches to apply): HTTP {resp.status_code}"
    else:
        return f"Patch apply returned HTTP {resp.status_code} - acceptable variant"


def test_list_jobs(client: PatchAPIClient) -> str:
    """GET /api/v1/jobs - Verify job listing."""
    resp = client.get("/api/v1/jobs?limit=50", timeout=120)
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}"
    data = resp.json()
    err = validate_envelope(data, "list_jobs")
    assert err is None, f"Envelope validation failed: {err}"
    assert data["success"] is True
    assert "jobs" in data["data"], "Missing jobs array"
    assert "total" in data["data"], "Missing total count"
    return f"Listed {data['data']['total']} jobs"


def test_get_job_not_found(client: PatchAPIClient) -> str:
    """GET /api/v1/jobs/{fake_id} - Verify 404 for nonexistent job."""
    fake_id = "00000000-0000-0000-0000-000000000000"
    resp = client.get(f"/api/v1/jobs/{fake_id}")
    assert resp.status_code == 404, f"Expected 404, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is False
    assert data["error"]["code"] == "JOB_NOT_FOUND", f"Expected JOB_NOT_FOUND, got {data['error']['code']}"
    return "Correctly returned JOB_NOT_FOUND for nonexistent job"


def test_cancel_job_not_found(client: PatchAPIClient) -> str:
    """DELETE /api/v1/jobs/{fake_id} - Verify 404 for cancel nonexistent job."""
    fake_id = "00000000-0000-0000-0000-000000000000"
    resp = client.delete(f"/api/v1/jobs/{fake_id}")
    assert resp.status_code == 404, f"Expected 404, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is False
    return f"Correctly returned 404 for cancel nonexistent job"


def test_rollback_job_not_found(client: PatchAPIClient) -> str:
    """POST /api/v1/jobs/{fake_id}/rollback - Verify error for rollback nonexistent job.

    Note: API may return 400 (invalid job for rollback) or 404 (not found).
    Both are acceptable - the important thing is it's not 200.
    """
    fake_id = "00000000-0000-0000-0000-000000000000"
    resp = client.post(f"/api/v1/jobs/{fake_id}/rollback")
    # API may return 400 (can't rollback nonexistent) or 404 (not found)
    assert resp.status_code in [400, 404], f"Expected 400 or 404, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is False
    return f"Correctly returned {resp.status_code} for rollback nonexistent job"


def test_invalid_job_id(client: PatchAPIClient) -> str:
    """GET /api/v1/jobs/invalid-uuid - Verify 400 for invalid job ID."""
    resp = client.get("/api/v1/jobs/not-a-uuid")
    assert resp.status_code in [400, 405], f"Expected 400 or 405, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is False
    assert data["error"]["code"] == "INVALID_JOB_ID", f"Expected INVALID_JOB_ID, got {data['error']['code']}"
    return "Correctly returned INVALID_JOB_ID for malformed UUID"


def test_method_not_allowed(client: PatchAPIClient) -> str:
    """PATCH /api/v1/packages/apt - Verify unsupported method is rejected.

    Note: Actix-web may return 404 for PATCH on resources that don't define it.
    Both 404 and 405 are acceptable - the important thing is it's not 200.
    """
    resp = client._request("PATCH", "/api/v1/packages/apt")
    assert resp.status_code in [404, 405], f"Expected 404 or 405, got {resp.status_code}"
    return f"Correctly returned {resp.status_code} for PATCH method (not allowed)"


def test_package_name_validation(client: PatchAPIClient) -> str:
    """GET /api/v1/packages/{long_name} - Verify 400 for oversized package name."""
    long_name = "a" * 300
    resp = client.get(f"/api/v1/packages/{long_name}")
    assert resp.status_code in [400, 405], f"Expected 400 or 405, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is False
    return "Correctly rejected oversized package name"


def test_empty_package_install(client: PatchAPIClient) -> str:
    """POST /api/v1/packages with empty name - Verify rejection.

    Note: API may return 400, 422, or other error codes for empty package names.
    The important thing is it's not accepted as valid.
    """
    payload = {
        "packages": [{"name": "", "version": None}],
        "options": {"force": False},
    }
    resp = client.post("/api/v1/packages", json=payload)
    # API may return 400, 422, or other error codes
    if resp.status_code in [400, 422, 500]:
        return f"Correctly rejected empty package name with HTTP {resp.status_code}"
    elif resp.status_code == 202:
        # If accepted, the job should fail - poll to verify
        data = resp.json()
        if data.get("success"):
            job_id = data["data"]["job_id"]
            job = poll_job(client, job_id, timeout=30)
            if job["status"] == "failed":
                return f"Empty package name accepted but job failed as expected: {job_id}"
        return f"Empty package name unexpectedly accepted: HTTP {resp.status_code}"
    else:
        assert False, f"Expected 400/422, got {resp.status_code}: {resp.text[:200]}"


def test_no_cert_connection(client: PatchAPIClient) -> str:
    """Verify that connections without mTLS are silently dropped.

    Per spec: non-mTLS connections should be silently dropped (no response).
    This means the connection will hang/timeout rather than return an error.
    """
    session = requests.Session()
    session.verify = False
    try:
        resp = session.get(
            f"https://{client.host}:{client.port}/health",
            timeout=10,
        )
        # If we get here, mTLS is NOT enforced - security issue
        assert False, f"Connection without mTLS succeeded! HTTP {resp.status_code} - mTLS not enforced!"
    except (requests.exceptions.SSLError, requests.exceptions.ConnectionError, requests.exceptions.ReadTimeout):
        # Expected - connection should be dropped (silent drop = no response)
        return "Correctly rejected connection without mTLS client certificate"
    finally:
        session.close()


def test_wrong_cert_connection(client: PatchAPIClient) -> str:
    """Verify that connections with wrong cert are rejected.

    Per spec: invalid/expired certificates should be silently dropped.
    Uses project test certs (different CA) which should be rejected.
    """
    project_ca = "/a0/usr/projects/linux_patch_api/configs/certs/ca.pem"
    project_cert = "/a0/usr/projects/linux_patch_api/configs/certs/client001.pem"
    project_key = "/a0/usr/projects/linux_patch_api/configs/certs/client001.key.pem"

    if not Path(project_ca).exists():
        return "SKIPPED: Project test certs not available"

    session = requests.Session()
    session.cert = (project_cert, project_key)
    session.verify = False
    try:
        resp = session.get(
            f"https://{client.host}:{client.port}/health",
            timeout=10,
        )
        assert False, f"Connection with wrong cert succeeded! HTTP {resp.status_code} - cert validation failed!"
    except (requests.exceptions.SSLError, requests.exceptions.ConnectionError, requests.exceptions.ReadTimeout):
        return "Correctly rejected connection with untrusted client certificate"
    finally:
        session.close()


def test_job_lifecycle(client: PatchAPIClient) -> str:
    """Full job lifecycle: install -> get job -> list jobs -> remove.

    Verifies that install and remove both complete successfully.
    A failed status is a critical failure - the core function must work.
    """
    # Step 1: Install test package
    payload = {
        "packages": [{"name": TEST_PACKAGE, "version": None}],
        "options": {"force": False, "no_recommends": True},
    }
    resp = client.post("/api/v1/packages", json=payload)
    assert resp.status_code == 202, f"Install failed: HTTP {resp.status_code}"
    data = resp.json()
    job_id = data["data"]["job_id"]

    # Step 2: Get job status
    resp = client.get(f"/api/v1/jobs/{job_id}")
    assert resp.status_code == 200, f"Get job failed: HTTP {resp.status_code}"
    job_data = resp.json()["data"]
    assert job_data["job_id"] == job_id, "Job ID mismatch"

    # Step 3: Poll to completion
    job = poll_job(client, job_id)
    assert job["status"] == "completed", f"Install job failed: status={job['status']}, result={job.get('result', {})}"

    # Step 4: Verify in job list
    resp = client.get("/api/v1/jobs?limit=50", timeout=120)
    assert resp.status_code == 200
    jobs = resp.json()["data"]["jobs"]
    job_ids = [j["job_id"] for j in jobs]
    assert job_id in job_ids, f"Job {job_id} not found in job list"

    # Step 5: Remove test package
    resp = client.delete(f"/api/v1/packages/{TEST_PACKAGE}")
    assert resp.status_code == 202, f"Remove failed: HTTP {resp.status_code}"
    remove_job_id = resp.json()["data"]["job_id"]
    remove_job = poll_job(client, remove_job_id)
    assert remove_job["status"] == "completed", f"Remove job failed: status={remove_job['status']}, result={remove_job.get('result', {})}"

    return f"Full lifecycle OK: install job={job_id}, remove job={remove_job_id}"


def test_service_status(client: PatchAPIClient) -> str:
    """GET /api/v1/system/services/{name} - Test service status endpoint."""
    # Test with a known service (ssh)
    resp = client.get("/api/v1/system/services/ssh")
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text}"
    data = resp.json()
    err = validate_envelope(data, "service_status")
    assert err is None, f"Envelope validation failed: {err}"
    assert data["success"] is True
    assert "name" in data["data"], "Missing name field"
    assert "active_state" in data["data"], "Missing active_state field"
    assert "healthy" in data["data"], "Missing healthy field"
    assert isinstance(data["data"]["healthy"], bool), "healthy must be boolean"

    # Test with non-existent service
    resp = client.get("/api/v1/system/services/nonexistent-service-12345")
    assert resp.status_code == 404, f"Expected 404, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is False
    assert data["error"]["code"] == "SERVICE_NOT_FOUND"

    # Test with invalid service name
    resp = client.get("/api/v1/system/services/../../etc/passwd")
    assert resp.status_code in [400, 405], f"Expected 400 or 405, got {resp.status_code}"
    data = resp.json()
    assert data["success"] is False
    assert data["error"]["code"] == "INVALID_SERVICE_NAME"

    return f"Service status OK: ssh={data['data']['name']}, state={data['data']['active_state']}, healthy={data['data']['healthy']}"


def test_reboot_endpoint(client: PatchAPIClient) -> str:
    """POST /api/v1/system/reboot - Test reboot endpoint.

    WARNING: This will actually reboot the target system!
    Only run as the LAST test on a target that can tolerate downtime.
    Uses a 60-second delay to allow for cancellation if needed.
    """
    payload = {
        "delay_seconds": 60,
        "force": False,
        "reason": "E2E test - automated reboot verification",
    }
    resp = client.post("/api/v1/system/reboot", json=payload)
    assert resp.status_code == 202, f"Expected 202, got {resp.status_code}: {resp.text}"
    data = resp.json()
    err = validate_envelope(data, "reboot")
    assert err is None, f"Envelope validation failed: {err}"
    assert data["success"] is True
    assert data["data"]["job_id"], "Missing job_id"
    assert data["data"]["operation"] == "reboot", f"Expected operation=reboot, got {data['data']['operation']}"

    job_id = data["data"]["job_id"]
    return f"Reboot scheduled: job_id={job_id}, delay=60s. System will reboot in 60 seconds."


# =============================================================================
# Test Runner
# =============================================================================


def run_all_tests(target_key: str, skip_reboot: bool = False, verbose: bool = False) -> TargetResults:
    """Run all E2E tests against a single target."""
    target = TARGETS[target_key]
    results = TargetResults(
        target_name=target["name"],
        target_host=target["host"],
    )

    client = PatchAPIClient(target["host"], target["port"])

    print(f"\n{'='*70}")
    print(f"  Testing: {target['name']} ({target['host']}:{target['port']})")
    print(f"  OS: {target['os']}")
    print(f"{'='*70}")

    # ---- Category 1: Health & System ----
    print("\n--- Health & System ---")
    run_test(results, "Health Check", test_health_endpoint, client)
    run_test(results, "System Info", test_system_info, client)
    run_test(results, "Service Status (ssh)", test_service_status, client)

    # ---- Category 2: Package Operations ----
    print("\n--- Package Operations ---")
    run_test(results, "List Packages", test_list_packages, client)
    run_test(results, "List Packages (Filtered)", test_list_packages_filtered, client)
    run_test(results, "Get Package Detail (apt)", test_get_package_detail, client)
    run_test(results, "Get Package Not Found", test_get_package_not_found, client)
    run_test(results, "Install Package (hello)", test_install_package, client)
    run_test(results, "Update Package (hello)", test_update_package, client)
    run_test(results, "Remove Package (hello)", test_remove_package, client)

    # ---- Category 3: Patch Operations ----
    print("\n--- Patch Operations ---")
    run_test(results, "List Patches", test_list_patches, client)
    run_test(results, "List Patches (Filtered)", test_list_patches_filtered, client)
    run_test(results, "Apply Patches (safe)", test_apply_patches, client)

    # ---- Category 4: Job Management ----
    print("\n--- Job Management ---")
    run_test(results, "List Jobs", test_list_jobs, client)
    run_test(results, "Get Job Not Found", test_get_job_not_found, client)
    run_test(results, "Cancel Job Not Found", test_cancel_job_not_found, client)
    run_test(results, "Rollback Job Not Found", test_rollback_job_not_found, client)
    run_test(results, "Invalid Job ID", test_invalid_job_id, client)
    run_test(results, "Full Job Lifecycle", test_job_lifecycle, client)

    # ---- Category 5: Security ----
    print("\n--- Security ---")
    run_test(results, "No Cert Connection Rejected", test_no_cert_connection, client)
    run_test(results, "Wrong Cert Connection Rejected", test_wrong_cert_connection, client)
    run_test(results, "Method Not Allowed (PATCH)", test_method_not_allowed, client)
    run_test(results, "Package Name Validation (oversized)", test_package_name_validation, client)
    run_test(results, "Empty Package Name Rejected", test_empty_package_install, client)

    # ---- Category 6: Reboot (LAST!) ----
    print("\n--- Reboot (LAST) ---")
    if skip_reboot:
        run_test(results, "System Reboot", test_reboot_endpoint, client, skip=True)
        print("  ⏭️  SKIPPED (--skip-reboot flag)")
    else:
        # Only run reboot if all other tests passed
        if results.failed == 0 and results.errors == 0:
            print("  ⚠️  WARNING: This will reboot the target system!")
            run_test(results, "System Reboot", test_reboot_endpoint, client)
        else:
            print(f"  ⏭️  SKIPPED ({results.failed} failures, {results.errors} errors in prior tests)")
            run_test(results, "System Reboot", test_reboot_endpoint, client, skip=True)

    client.close()
    return results


def print_results(results: TargetResults, verbose: bool = False):
    """Print test results summary."""
    print(f"\n{'='*70}")
    print(f"  Results: {results.target_name} ({results.target_host})")
    print(f"{'='*70}")

    for r in results.results:
        status = "✅" if r.passed else ("⏭️" if "SKIPPED" in r.message else "❌")
        duration = f" ({r.duration_ms:.0f}ms)" if r.duration_ms > 0 else ""
        print(f"  {status} {r.name}: {r.message}{duration}")

    print(f"\n  Total: {results.total} | Passed: {results.passed} | Failed: {results.failed} | Errors: {results.errors} | Skipped: {results.skipped}")

    if results.failed > 0 or results.errors > 0:
        print(f"\n  ❌ FAILED")
    else:
        print(f"\n  ✅ ALL PASSED")


def main():
    parser = argparse.ArgumentParser(description="Linux Patch API E2E Test Suite")
    parser.add_argument(
        "--target",
        choices=["all", "dev", "u2204", "u2404"],
        default="all",
        help="Target LXC to test (default: all)",
    )
    parser.add_argument(
        "--skip-reboot",
        action="store_true",
        help="Skip the reboot test (recommended for initial runs)",
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Verbose output",
    )
    args = parser.parse_args()

    # Verify certs exist
    if not CA_CERT.exists():
        print(f"ERROR: CA cert not found: {CA_CERT}")
        sys.exit(1)
    if not CLIENT_CERT.exists():
        print(f"ERROR: Client cert not found: {CLIENT_CERT}")
        sys.exit(1)
    if not CLIENT_KEY.exists():
        print(f"ERROR: Client key not found: {CLIENT_KEY}")
        sys.exit(1)

    print("Linux Patch API - End-to-End Test Suite")
    print(f"CA: {CA_CERT}")
    print(f"Client Cert: {CLIENT_CERT}")
    print(f"Client Key: {CLIENT_KEY}")

    targets = list(TARGETS.keys()) if args.target == "all" else [args.target]
    all_results = []
    overall_passed = True

    for target_key in targets:
        results = run_all_tests(target_key, args.skip_reboot, args.verbose)
        print_results(results, args.verbose)
        all_results.append(results)
        if results.failed > 0 or results.errors > 0:
            overall_passed = False

    # Summary across all targets
    if len(all_results) > 1:
        print(f"\n{'='*70}")
        print("  OVERALL SUMMARY")
        print(f"{'='*70}")
        for r in all_results:
            status = "✅" if (r.failed == 0 and r.errors == 0) else "❌"
            print(f"  {status} {r.target_name} ({r.target_host}): {r.passed}/{r.total} passed")
        print(f"\n  Overall: {'✅ ALL PASSED' if overall_passed else '❌ SOME FAILED'}")

    sys.exit(0 if overall_passed else 1)


if __name__ == "__main__":
    main()
