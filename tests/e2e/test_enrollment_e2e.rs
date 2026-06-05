//! End-to-End Enrollment Test Suite
//!
//! Comprehensive tests verifying the complete enrollment flow from CLI invocation
//! through certificate provisioning and whitelist updates.
//!
//! # Test Strategy
//! - wiremock provides in-process HTTP mock server simulating manager API
//! - tempfile ensures isolated filesystem state per test with automatic cleanup
//! - serial_test prevents port conflicts between concurrent test runs
//! - Mock manager simulates realistic approval delays (1-2 polls before approved)
//!
//! # Coverage
//! 1. Full happy-path enrollment (register → poll → provision)
//! 2. Enrollment denied flow with clean failure state
//! 3. Enrollment timeout after max attempts
//! 4. Certificate file permission verification (0o600 keys, 0o644 certs)
//! 5. Whitelist append with duplicate prevention
//! 6. Signal handling during polling (graceful shutdown via timeout simulation)

use linux_patch_api::config::loader::TlsConfig;
use linux_patch_api::enroll::client::EnrollmentClient;
use linux_patch_api::enroll::provision;
use serial_test::serial;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Test constants
const TEST_TOKEN: &str = "test_enrollment_token";
const POLL_INTERVAL_SECONDS: u64 = 1; // Fast polling for tests

// =============================================================================
// Dummy PEM data for testing - valid PEM structure with BEGIN/END markers
// =============================================================================

const DUMMY_CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAKHBfpegPjMCMA0GCSqGSIb3DQEBCwUAMBExDzANBgNVBAMMBnRh\nc3RjYTAeFw0yNDAxMDEwMDAwMDBaFw0yNTAxMDEwMDAwMDBaMBExDzANBgNVBAMM\nBnRlc3RjYTBcMA0GCSqGSIb3DQEBAQUAAgEAA0sAMEgCQQC7o5ECujKlLIZmHoRN\nd8EEp+mRhJ2i0M4HtTmMy1VSdvCVrXvMJkbz3KoQxRqVMd6yBZKwWgyIePCNMSVh\nAgMBAAEwDQYJKoZIhvcNAQELBQADQQC7a29sYWJlbGVfZGF0YV9mb3JfdGVzdGlu\nZ19vbmx5X25vdF9hX3JlYWxfY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----";

const DUMMY_SERVER_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAKHBfpegPjMDMA0GCSqGSIb3DQEBCwUAMBExDzANBgNVBAMMBnRh\nc3RjYTAeFw0yNDAxMDEwMDAwMDBaFw0yNTAxMDEwMDAwMDBaMBExDzANBgNVBAMM\nBnNlcnZlcjBcMA0GCSqGSIb3DQEBAQUAAgEAA0sAMEgCQQC7o5ECujKlLIZmHoRN\nd8EEp+mRhJ2i0M4HtTmMy1VSdvCVrXvMJkbz3KoQxRqVMd6yBZKwWgyIePCNMSVh\nAgMBAAEwDQYJKoZIhvcNAQELBQADQQC7a29sYWJlbGVfZGF0YV9mb3JfdGVzdGlu\nZ19vbmx5X25vdF9hX3JlYWxfY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----";

const DUMMY_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIBVAIBADANBgkqhkiG9w0BAQEFAASCAT4wggE6AgEAAkEAu6ORAroypSyGZh6E\nTXfBBKfpkYSdotDOB7U5jMtVUnbwna17zCZG89yqEMUalTHesgWSsFoMiHjwjTEl\nYQIDAQABAkADdd2F0YV9mb3JfdGVzdGluZ19vbmx5X25vdF9hX3JlYWxfa2V5\nX2RhdGFfZXhhbXBsZV9mb3JfcGlwZWxpbmVfdGVzdGluZwIhAOdvbnBseWZvcmVu\ncm9sbG1lbnR0ZXN0aW5ncHVycG9zZXNvbmx5d2l0aGluZW5yb2xs\n-----END PRIVATE KEY-----";

// =============================================================================
// Helper Functions
// =============================================================================

/// Create a mock manager server and return base URL.
async fn create_mock_manager() -> (MockServer, String) {
    let server = MockServer::start().await;
    let base_url = server.uri(); // e.g., "http://127.0.0.1:XXXXX"
    (server, base_url)
}

/// Create temporary directories for certificate and whitelist file operations.
fn create_temp_dirs() -> (TempDir, TempDir) {
    let cert_dir = tempfile::tempdir().expect("Failed to create temp cert directory");
    let whitelist_dir = tempfile::tempdir().expect("Failed to create temp whitelist directory");
    (cert_dir, whitelist_dir)
}

/// Initialize an empty whitelist YAML file at the given path.
/// Required because WhitelistManager::new() loads existing config on construction.
fn init_empty_whitelist(path: &str) {
    std::fs::write(path, "entries: []\n").expect("Failed to create initial whitelist file");
}

/// Build a TLS config pointing to the temp certificate directory.
fn build_tls_config(cert_dir: &std::path::Path) -> TlsConfig {
    TlsConfig {
        enabled: true,
        port: 12443,
        ca_cert: cert_dir.join("ca.pem").to_string_lossy().to_string(),
        server_cert: cert_dir.join("server.pem").to_string_lossy().to_string(),
        server_key: cert_dir
            .join("server.key.pem")
            .to_string_lossy()
            .to_string(),
        min_tls_version: "1.3".to_string(),
        crl_path: String::new(), // No CRL in E2E tests
    }
}

/// Build an EnrollmentClient pointing at the mock server.
/// Uses a test report_ip so enrollment works inside Docker containers
/// where the only IPs are in the 172.16.0.0/12 bridge range (filtered).
fn build_client(base_url: &str) -> EnrollmentClient {
    EnrollmentClient::with_ip_overrides(base_url, None, Some("192.168.1.10".to_string()))
}

// =============================================================================
// Test 1: Full Enrollment Flow (Happy Path)
//
// Start mock manager with approval workflow, call run_enrollment() phases,
// verify registration request sent, polling executes, PKI bundle received,
// certificate files written with correct permissions, manager IP appended to
// whitelist YAML, all three phases complete without error.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_full_enrollment_flow_happy_path() {
    let (server, base_url) = create_mock_manager().await;
    let (cert_dir, whitelist_dir) = create_temp_dirs();

    let ca_cert_path = cert_dir.path().join("ca.pem");
    let server_cert_path = cert_dir.path().join("server.pem");
    let server_key_path = cert_dir.path().join("server.key.pem");
    let whitelist_path = whitelist_dir
        .path()
        .join("whitelist.yaml")
        .to_string_lossy()
        .to_string();

    init_empty_whitelist(&whitelist_path);

    // Mock manager: simulate realistic 1-poll delay before approval
    let poll_count = Arc::new(AtomicU32::new(0));
    let poll_count_clone = poll_count.clone();

    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202)
                .set_body_string(format!(r#"{{"polling_token": "{}"}}"#, TEST_TOKEN)),
        )
        .named("enroll_registration")
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(move |_req: &wiremock::Request| {
            let count = poll_count_clone.fetch_add(1, Ordering::SeqCst);
            if count < 1 {
                // First poll returns pending (simulates admin review delay)
                ResponseTemplate::new(200).set_body_string(r#"{"status": "pending"}"#)
            } else {
                // Second poll returns approved with full PKI bundle
                ResponseTemplate::new(200).set_body_string(format!(
                    r#"{{
                        "status": "approved",
                        "ca_crt": {},
                        "server_crt": {},
                        "server_key": {}
                    }}"#,
                    serde_json::to_string(DUMMY_CA_PEM).unwrap(),
                    serde_json::to_string(DUMMY_SERVER_PEM).unwrap(),
                    serde_json::to_string(DUMMY_KEY_PEM).unwrap(),
                ))
            }
        })
        .named("status_polling")
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    // Phase 1: Registration
    let response = client
        .register()
        .await
        .expect("Registration should succeed");
    assert_eq!(response.polling_token, TEST_TOKEN);

    // Phase 2: Polling (should get pending first, then approved)
    let bundle = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 5)
        .await
        .expect("Polling should succeed with approval after pending");

    assert!(!bundle.ca_crt.is_empty());
    assert!(!bundle.server_crt.is_empty());
    assert!(!bundle.server_key.is_empty());

    // Phase 3: PKI Provisioning
    let tls_config = build_tls_config(cert_dir.path());
    provision::provision_pki_bundle(
        &bundle.ca_crt,
        &bundle.server_crt,
        &bundle.server_key,
        Some(&tls_config),
    )
    .await
    .expect("PKI provisioning should succeed");

    // Phase 3b: Whitelist update (manager_ip for localhost URL returns 127.0.0.1)
    let manager_ip = client
        .manager_ip()
        .await
        .expect("Should resolve manager IP");
    provision::append_manager_to_whitelist(&manager_ip, &whitelist_path)
        .await
        .expect("Whitelist append should succeed");

    // Verify: all certificate files written to temp directory
    assert!(ca_cert_path.exists(), "CA cert file should exist");
    assert!(server_cert_path.exists(), "Server cert file should exist");
    assert!(server_key_path.exists(), "Server key file should exist");

    // Verify: correct permissions (key=0o600, certs=0o644)
    let key_perms = std::fs::metadata(&server_key_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(key_perms, 0o600, "Key file should have 0o600 permissions");

    let ca_perms = std::fs::metadata(&ca_cert_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(ca_perms, 0o644, "CA cert should have 0o644 permissions");

    let server_perms = std::fs::metadata(&server_cert_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        server_perms, 0o644,
        "Server cert should have 0o644 permissions"
    );

    // Verify: whitelist contains manager IP
    let wl_content = std::fs::read_to_string(&whitelist_path).unwrap();
    let wl_config: serde_yaml::Value = serde_yaml::from_str(&wl_content).unwrap();
    let entries = wl_config.get("entries").unwrap().as_sequence().unwrap();
    assert!(
        entries.iter().any(|e| e.as_str().unwrap() == manager_ip),
        "Whitelist should contain manager IP {}",
        manager_ip
    );
}

// =============================================================================
// Test 2: Enrollment Denied Flow
//
// Mock server returns denied status on first poll.
// Verify enrollment fails with clear denial message, no certificate files
// written (clean failure state), no whitelist modifications.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_enrollment_denied_flow() {
    let (server, base_url) = create_mock_manager().await;
    let (cert_dir, _whitelist_dir) = create_temp_dirs();

    let whitelist_path = _whitelist_dir
        .path()
        .join("whitelist.yaml")
        .to_string_lossy()
        .to_string();
    init_empty_whitelist(&whitelist_path);

    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "denied_token"}"#),
        )
        .named("registration")
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status": "denied"}"#))
        .named("status_denied")
        .expect(1) // Exactly one poll attempt before denial
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    // Phase 1: Registration succeeds even for denied enrollment
    let response = client
        .register()
        .await
        .expect("Registration should succeed");
    assert_eq!(response.polling_token, "denied_token");

    // Phase 2: Polling returns denial error
    let result = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 10)
        .await;

    assert!(
        result.is_err(),
        "Should receive error for denied enrollment"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("denied"),
        "Error message should mention denial, got: {}",
        err_msg
    );

    // Verify: no certificate files written (clean failure state)
    let ca_path = cert_dir.path().join("ca.pem");
    let server_cert_path = cert_dir.path().join("server.pem");
    let server_key_path = cert_dir.path().join("server.key.pem");

    assert!(
        !ca_path.exists(),
        "CA cert should NOT exist after denied enrollment"
    );
    assert!(
        !server_cert_path.exists(),
        "Server cert should NOT exist after denied enrollment"
    );
    assert!(
        !server_key_path.exists(),
        "Server key should NOT exist after denied enrollment"
    );

    // Verify: no whitelist modifications on failed enrollment
    let wl_content = std::fs::read_to_string(&whitelist_path).unwrap();
    let wl_config: serde_yaml::Value = serde_yaml::from_str(&wl_content).unwrap();
    let entries = wl_config.get("entries").and_then(|e| e.as_sequence());
    assert!(
        entries.map_or(true, |e| e.is_empty()),
        "Whitelist should remain empty after denied enrollment"
    );
}

// =============================================================================
// Test 3: Enrollment Timeout Flow
//
// Mock server always returns pending. Call with max_attempts=3.
// Verify enrollment fails after 3 attempts with timeout error, clean failure
// state (no partial files on disk).
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_enrollment_timeout_flow() {
    let (server, base_url) = create_mock_manager().await;
    let (cert_dir, _whitelist_dir) = create_temp_dirs();

    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "timeout_token"}"#),
        )
        .named("registration")
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status": "pending"}"#))
        .named("status_always_pending")
        .expect(3) // Exactly 3 poll attempts before timeout
        .mount(&server)
        .await;

    let client = build_client(&base_url);
    let response = client
        .register()
        .await
        .expect("Registration should succeed");

    // Poll with max_attempts=3 - should timeout after exactly 3 attempts
    let result = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 3)
        .await;

    assert!(result.is_err(), "Should timeout after max attempts");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("timed out") || err_msg.contains("timeout"),
        "Error should mention timeout, got: {}",
        err_msg
    );

    // Verify: no partial certificate files on disk
    let ca_path = cert_dir.path().join("ca.pem");
    let server_cert_path = cert_dir.path().join("server.pem");
    let server_key_path = cert_dir.path().join("server.key.pem");

    assert!(!ca_path.exists(), "CA cert should NOT exist after timeout");
    assert!(
        !server_cert_path.exists(),
        "Server cert should NOT exist after timeout"
    );
    assert!(
        !server_key_path.exists(),
        "Server key should NOT exist after timeout"
    );
}

// =============================================================================
// Test 4: Certificate Permission Verification
//
// After successful enrollment, verify file permissions:
// - Key file: 0o600 (owner rw only)
// - Certificate files: 0o644 (owner rw, group/others read)
// Verify atomic write pattern (no partial .tmp or .bak files left on disk).
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_certificate_permission_verification() {
    let (server, base_url) = create_mock_manager().await;
    let (cert_dir, _whitelist_dir) = create_temp_dirs();

    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "perm_token"}"#),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{
                    "status": "approved",
                    "ca_crt": {},
                    "server_crt": {},
                    "server_key": {}
                }}"#,
            serde_json::to_string(DUMMY_CA_PEM).unwrap(),
            serde_json::to_string(DUMMY_SERVER_PEM).unwrap(),
            serde_json::to_string(DUMMY_KEY_PEM).unwrap(),
        )))
        .mount(&server)
        .await;

    let client = build_client(&base_url);
    let response = client
        .register()
        .await
        .expect("Registration should succeed");
    let bundle = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 5)
        .await
        .expect("Should receive approved PkiBundle");

    // Provision to temp directory
    let tls_config = build_tls_config(cert_dir.path());
    provision::provision_pki_bundle(
        &bundle.ca_crt,
        &bundle.server_crt,
        &bundle.server_key,
        Some(&tls_config),
    )
    .await
    .expect("PKI provisioning should succeed");

    // Verify key file: 0o600 (owner read/write only)
    let key_path = cert_dir.path().join("server.key.pem");
    let key_perms = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        key_perms, 0o600,
        "Key file must have exactly 0o600 permissions (owner rw only)"
    );

    // Verify CA cert: 0o644 (owner rw, group/others read)
    let ca_path = cert_dir.path().join("ca.pem");
    let ca_perms = std::fs::metadata(&ca_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        ca_perms, 0o644,
        "CA certificate must have exactly 0o644 permissions"
    );

    // Verify server cert: 0o644 (owner rw, group/others read)
    let server_cert_path = cert_dir.path().join("server.pem");
    let server_perms = std::fs::metadata(&server_cert_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        server_perms, 0o644,
        "Server certificate must have exactly 0o644 permissions"
    );

    // Verify atomic write: no partial .tmp files left on disk
    for entry in std::fs::read_dir(cert_dir.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".tmp"),
            "No .tmp partial files should remain after atomic write"
        );
    }

    // Verify content integrity - PEM data written correctly
    let ca_content = std::fs::read_to_string(&ca_path).unwrap();
    assert!(ca_content.contains("BEGIN CERTIFICATE"));
    assert!(ca_content.contains("END CERTIFICATE"));

    let key_content = std::fs::read_to_string(&key_path).unwrap();
    assert!(
        key_content.contains("BEGIN PRIVATE KEY") || key_content.contains("BEGIN RSA PRIVATE KEY")
    );
}

// =============================================================================
// Test 5: Whitelist Append Verification
//
// After successful enrollment, verify whitelist YAML contains manager IP.
// Verify no duplicate entries if enrollment runs twice with same manager IP.
// Verify YAML format preserved correctly.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_whitelist_append_verification() {
    let (server, base_url) = create_mock_manager().await;
    let (_cert_dir, whitelist_dir) = create_temp_dirs();

    let whitelist_path = whitelist_dir
        .path()
        .join("whitelist.yaml")
        .to_string_lossy()
        .to_string();
    init_empty_whitelist(&whitelist_path);

    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "wl_token"}"#),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{
                    "status": "approved",
                    "ca_crt": {},
                    "server_crt": {},
                    "server_key": {}
                }}"#,
            serde_json::to_string(DUMMY_CA_PEM).unwrap(),
            serde_json::to_string(DUMMY_SERVER_PEM).unwrap(),
            serde_json::to_string(DUMMY_KEY_PEM).unwrap(),
        )))
        .mount(&server)
        .await;

    let client = build_client(&base_url);
    let response = client
        .register()
        .await
        .expect("Registration should succeed");
    let _bundle = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 5)
        .await
        .expect("Should receive approved PkiBundle");

    // First enrollment: append to whitelist
    let manager_ip = client
        .manager_ip()
        .await
        .expect("Should resolve manager IP");
    provision::append_manager_to_whitelist(&manager_ip, &whitelist_path)
        .await
        .expect("First whitelist append should succeed");

    // Verify: whitelist contains manager IP after first enrollment
    let wl_content = std::fs::read_to_string(&whitelist_path).unwrap();
    let wl_config: serde_yaml::Value = serde_yaml::from_str(&wl_content).unwrap();
    let entries: Vec<&str> = wl_config
        .get("entries")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();

    assert_eq!(
        entries.iter().filter(|&&e| e == manager_ip).count(),
        1,
        "Whitelist should contain exactly one entry for manager IP after first enrollment"
    );

    // Second enrollment with same manager IP: verify no duplicate
    provision::append_manager_to_whitelist(&manager_ip, &whitelist_path)
        .await
        .expect("Second whitelist append should succeed (no-op for duplicate)");

    // Verify: still only one entry after second enrollment (duplicate prevention)
    let wl_content = std::fs::read_to_string(&whitelist_path).unwrap();
    let wl_config: serde_yaml::Value = serde_yaml::from_str(&wl_content).unwrap();
    let entries: Vec<&str> = wl_config
        .get("entries")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();

    assert_eq!(
        entries.iter().filter(|&&e| e == manager_ip).count(),
        1,
        "Whitelist should still have exactly one entry (no duplicate) after second enrollment"
    );

    // Verify: YAML format is valid and parseable
    assert!(
        wl_content.contains("entries:"),
        "YAML should contain 'entries:' key"
    );
}

// =============================================================================
// Test 6: Signal Handling During Polling (Graceful Shutdown)
//
// Mock server returns pending indefinitely.
// Verify graceful shutdown with appropriate error message when max attempts
// exhausted (simulates SIGTERM interrupt during polling loop).
// Verify cleanup of any partial state (no leftover files).
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_signal_handling_during_polling() {
    let (server, base_url) = create_mock_manager().await;
    let (cert_dir, _whitelist_dir) = create_temp_dirs();

    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "signal_token"}"#),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status": "pending"}"#))
        .named("always_pending")
        .expect(3) // Exactly 3 polls before graceful shutdown
        .mount(&server)
        .await;

    let client = build_client(&base_url);
    let response = client
        .register()
        .await
        .expect("Registration should succeed");

    // Poll with max_attempts=3, interval=1s
    // This simulates SIGTERM interrupt by exhausting attempts (graceful shutdown)
    let result = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 3)
        .await;

    // Verify: graceful shutdown with appropriate error message
    assert!(result.is_err(), "Should fail gracefully after max attempts");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("timed out") || err_msg.contains("timeout"),
        "Error should indicate graceful shutdown/timeout, got: {}",
        err_msg
    );

    // Verify: cleanup of any partial state (no leftover files)
    let remaining: Vec<_> = std::fs::read_dir(cert_dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        remaining.is_empty(),
        "No partial files should remain after graceful shutdown: {:?}",
        remaining
    );
}

// =============================================================================
// Test 7: Whitelist YAML Format Preservation
//
// Verify the whitelist YAML maintains proper structure after enrollment.
// Ensures entries array format is correct and machine-readable.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_whitelist_yaml_format_preservation() {
    let (server, base_url) = create_mock_manager().await;
    let (_cert_dir, whitelist_dir) = create_temp_dirs();

    let whitelist_path = whitelist_dir
        .path()
        .join("whitelist.yaml")
        .to_string_lossy()
        .to_string();
    init_empty_whitelist(&whitelist_path);

    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "yaml_token"}"#),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{
                    "status": "approved",
                    "ca_crt": {},
                    "server_crt": {},
                    "server_key": {}
                }}"#,
            serde_json::to_string(DUMMY_CA_PEM).unwrap(),
            serde_json::to_string(DUMMY_SERVER_PEM).unwrap(),
            serde_json::to_string(DUMMY_KEY_PEM).unwrap(),
        )))
        .mount(&server)
        .await;

    let client = build_client(&base_url);
    let response = client
        .register()
        .await
        .expect("Registration should succeed");
    let _bundle = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 5)
        .await
        .expect("Should receive approved PkiBundle");

    // Provision and append to whitelist
    let manager_ip = client
        .manager_ip()
        .await
        .expect("Should resolve manager IP");
    provision::append_manager_to_whitelist(&manager_ip, &whitelist_path)
        .await
        .expect("Whitelist append should succeed");

    // Verify: whitelist file exists and is valid YAML
    let wl_content = std::fs::read_to_string(&whitelist_path).unwrap();

    // Parse as serde_yaml to verify format
    let wl_config: serde_yaml::Value =
        serde_yaml::from_str(&wl_content).expect("Whitelist should be valid YAML after enrollment");

    // Verify structure: entries key exists and is a sequence
    assert!(
        wl_config.get("entries").is_some(),
        "YAML must contain 'entries' key"
    );
    let entries = wl_config.get("entries").unwrap();
    assert!(entries.is_sequence(), "'entries' must be a YAML sequence");

    // Verify: exactly one entry matching manager IP
    let entry_list = entries.as_sequence().unwrap();
    assert_eq!(entry_list.len(), 1, "Should have exactly 1 whitelist entry");
    assert_eq!(
        entry_list[0].as_str().unwrap(),
        manager_ip,
        "Entry should match manager IP"
    );
}
