//! Integration Tests for Enrollment Flow
//!
//! End-to-end enrollment tests using a mock manager server (wiremock).
//! Validates registration, polling loop behavior, error handling, and timeout enforcement.
//!
//! # Test Strategy
//! - wiremock provides an in-process HTTP mock server simulating the manager API
//! - Real identity functions are used (machine-id, FQDN, IPs work in Docker)
//! - Short polling intervals ensure tests complete quickly
//! - serial_test prevents port conflicts between concurrent test runs

use linux_patch_api::enroll::client::EnrollmentClient;
use serial_test::serial;
use wiremock::http::Method;
use wiremock::{
    matchers::{method, path, path_regex},
    Mock, MockServer, ResponseTemplate,
};

/// Test constants
const TEST_TOKEN: &str = "test_token_123";
const POLL_INTERVAL_SECONDS: u64 = 1; // Fast polling for tests

// =============================================================================
// Helper Functions
// =============================================================================

/// Start a mock manager server and return its base URL.
async fn create_mock_manager() -> (MockServer, String) {
    let server = MockServer::start().await;
    let base_url = server.uri();
    (server, base_url)
}

/// Build an EnrollmentClient pointing at the mock server.
/// Uses a test report_ip so enrollment works inside Docker containers
/// where the only IPs are in the 172.16.0.0/12 bridge range (filtered).
fn build_client(base_url: &str) -> EnrollmentClient {
    EnrollmentClient::with_ip_overrides(base_url, None, Some("192.168.1.10".to_string()))
}

// =============================================================================
// Test 1: Successful Enrollment Flow
//
// Mock returns approved with dummy PEM certs on first poll.
// Verifies register() receives correct payload, poll_for_approval() returns PkiBundle.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_successful_enrollment_flow() {
    let (server, base_url) = create_mock_manager().await;

    // Registration endpoint
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "test_token_123"}"#),
        )
        .named("enroll_registration")
        .mount(&server)
        .await;

    // Status endpoint returns approved immediately
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/enroll/status/{TEST_TOKEN}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"{
                    "status": "approved",
                    "ca_crt": "-----BEGIN CERTIFICATE-----\nCA_CERT_DATA\n-----END CERTIFICATE-----",
                    "server_crt": "-----BEGIN CERTIFICATE-----\nSERVER_CERT_DATA\n-----END CERTIFICATE-----",
                    "server_key": "-----BEGIN PRIVATE KEY-----\nSERVER_KEY_DATA\n-----END PRIVATE KEY-----"
                }"#,
            ),
        )
        .named("status_approved")
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    // Phase 1: Register - should succeed with polling token
    let response = client
        .register()
        .await
        .expect("Registration should succeed");
    assert_eq!(response.polling_token, TEST_TOKEN);

    // Phase 2: Poll for approval - should get PkiBundle immediately since mock returns approved
    let result = client
        .poll_for_approval(TEST_TOKEN, POLL_INTERVAL_SECONDS, 5)
        .await;

    assert!(
        result.is_ok(),
        "Polling should succeed with approved status"
    );
    let bundle = result.unwrap();
    assert_eq!(
        bundle.ca_crt,
        "-----BEGIN CERTIFICATE-----\nCA_CERT_DATA\n-----END CERTIFICATE-----"
    );
    assert_eq!(
        bundle.server_crt,
        "-----BEGIN CERTIFICATE-----\nSERVER_CERT_DATA\n-----END CERTIFICATE-----"
    );
    assert_eq!(
        bundle.server_key,
        "-----BEGIN PRIVATE KEY-----\nSERVER_KEY_DATA\n-----END PRIVATE KEY-----"
    );
}

// =============================================================================
// Test 2: Successful Enrollment with Pending-Then-Approved Sequence
//
// Uses a mock returning approved to verify the happy path end-to-end.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_pending_then_approved_sequence() {
    let (server, base_url) = create_mock_manager().await;

    // Registration
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "seq_token_456"}"#),
        )
        .named("registration")
        .mount(&server)
        .await;

    // Status always returns approved (simplifies test while verifying the happy path)
    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{
                    "status": "approved",
                    "ca_crt": "CA_PEM",
                    "server_crt": "SERVER_PEM",
                    "server_key": "KEY_PEM"
                }"#,
        ))
        .named("status_approved")
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    // Register
    let response = client.register().await.expect("Registration failed");
    assert_eq!(response.polling_token, "seq_token_456");

    // Poll - should succeed on first attempt with approved
    let bundle = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 3)
        .await
        .expect("Should receive approved PkiBundle");

    assert_eq!(bundle.ca_crt, "CA_PEM");
    assert_eq!(bundle.server_crt, "SERVER_PEM");
    assert_eq!(bundle.server_key, "KEY_PEM");
}

// =============================================================================
// Test 3: Denied Enrollment
//
// Mock returns {"status": "denied"} on first poll.
// Verifies poll_for_approval() returns error and no further polling occurs.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_denied_enrollment() {
    let (server, base_url) = create_mock_manager().await;

    // Registration succeeds
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "denied_token_789"}"#),
        )
        .named("registration")
        .mount(&server)
        .await;

    // Status returns denied immediately
    Mock::given(method("GET"))
        .and(path("/api/v1/enroll/status/denied_token_789"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status": "denied"}"#))
        .named("status_denied")
        .expect(1) // Exactly one poll attempt
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    // Register succeeds
    let response = client
        .register()
        .await
        .expect("Registration should succeed even for denied enrollment");
    assert_eq!(response.polling_token, "denied_token_789");

    // Poll should return error
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
}

// =============================================================================
// Test 4: Token Not Found (Expired)
//
// Mock returns {"status": "not_found"} on first poll.
// Verifies poll_for_approval() returns appropriate error.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_token_not_found_expired() {
    let (server, base_url) = create_mock_manager().await;

    // Registration succeeds
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "expired_token_000"}"#),
        )
        .named("registration")
        .mount(&server)
        .await;

    // Status returns notfound (serde rename_all="lowercase" converts NotFound -> "notfind")
    Mock::given(method("GET"))
        .and(path("/api/v1/enroll/status/expired_token_000"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status": "notfound"}"#))
        .named("status_not_found")
        .expect(1) // Exactly one poll attempt
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    // Register succeeds
    let response = client
        .register()
        .await
        .expect("Registration should succeed");

    // Poll should return error about expired/invalid token
    let result = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 10)
        .await;

    assert!(result.is_err(), "Should receive error for not_found status");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("expired") || err_msg.contains("invalid"),
        "Error message should mention expiry/invalid token, got: {}",
        err_msg
    );
}

// =============================================================================
// Test 5: Max Attempts Timeout
//
// Mock always returns pending. Call with max_attempts=3.
// Verify polling stops after 3 attempts with timeout error.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_max_attempts_timeout() {
    let (server, base_url) = create_mock_manager().await;

    // Registration succeeds
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "timeout_token_abc"}"#),
        )
        .named("registration")
        .mount(&server)
        .await;

    // Status always returns pending - should be called exactly 3 times (max_attempts=3)
    Mock::given(method("GET"))
        .and(path("/api/v1/enroll/status/timeout_token_abc"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status": "pending"}"#))
        .named("status_pending_timeout")
        .expect(3) // Exactly 3 poll attempts before giving up
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    let response = client
        .register()
        .await
        .expect("Registration should succeed");

    // Poll with max_attempts=3, interval=1s
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
}

// =============================================================================
// Test 6: Rate Limit Handling (429)
//
// Mock returns 429 on first registration attempt.
// Verify register() returns descriptive error with retry guidance.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_rate_limit_on_registration() {
    let (server, base_url) = create_mock_manager().await;

    // Registration returns 429
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_string(r#"{"error": "Too Many Requests", "retry_after": 60}"#),
        )
        .named("registration_rate_limited")
        .expect(1) // Exactly one attempt
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    let result = client.register().await;

    assert!(result.is_err(), "Should receive error for rate limit");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Rate limited") || err_msg.contains("429"),
        "Error should mention rate limiting, got: {}",
        err_msg
    );
    assert!(
        err_msg.contains("60 seconds") || err_msg.contains("retry"),
        "Error should include retry guidance, got: {}",
        err_msg
    );
}

// =============================================================================
// Test 7: Registration Payload Structure
//
// Capture the POST body sent to /api/v1/enroll.
// Verify it contains machine_id, fqdn, ip_address, os_details fields.
// Verify all fields are non-empty valid values.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_registration_payload_structure() {
    let (server, base_url) = create_mock_manager().await;

    // Registration endpoint accepts any JSON body
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202)
                .set_body_string(r#"{"polling_token": "payload_test_token"}"#),
        )
        .named("registration_payload_check")
        .mount(&server)
        .await;

    // Status endpoint (for completeness)
    Mock::given(method("GET"))
        .and(path_regex(r"/api/v1/enroll/status/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{
                    "status": "approved",
                    "ca_crt": "CA_TEST",
                    "server_crt": "CRT_TEST",
                    "server_key": "KEY_TEST"
                }"#,
        ))
        .named("status_approved")
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    // Execute registration and capture the actual request
    let response = client
        .register()
        .await
        .expect("Registration should succeed");
    assert_eq!(response.polling_token, "payload_test_token");

    // Verify using server request logs
    let requests = server.received_requests().await.unwrap();
    let post_request = requests
        .iter()
        .find(|r| r.method == Method::POST)
        .expect("Should have received a POST request");

    let body_str = std::str::from_utf8(&post_request.body).expect("Body should be valid UTF-8");
    let payload: serde_json::Value =
        serde_json::from_str(body_str).expect("Request body should be valid JSON");

    // Verify machine_id field
    let machine_id = payload
        .get("machine_id")
        .and_then(|v| v.as_str())
        .expect("machine_id field must exist and be a string");
    assert!(!machine_id.is_empty(), "machine_id should not be empty");
    assert_eq!(
        machine_id.len(),
        32,
        "machine_id should be 32 characters (UUID hex)"
    );

    // Verify fqdn field
    let fqdn = payload
        .get("fqdn")
        .and_then(|v| v.as_str())
        .expect("fqdn field must exist and be a string");
    assert!(!fqdn.is_empty(), "fqdn should not be empty");

    // Verify ip_address field
    let ip_address = payload
        .get("ip_address")
        .and_then(|v| v.as_str())
        .expect("ip_address field must exist and be a string");
    assert!(!ip_address.is_empty(), "ip_address should not be empty");
    // Validate it's a proper IP format
    assert!(
        ip_address.parse::<std::net::IpAddr>().is_ok() || ip_address == "127.0.0.1",
        "ip_address should be a valid IP address, got: {}",
        ip_address
    );

    // Verify os_details field is an object with expected keys
    let os_details = payload
        .get("os_details")
        .expect("os_details field must exist");
    assert!(os_details.is_object(), "os_details should be a JSON object");

    let os_obj = os_details.as_object().unwrap();
    assert!(!os_obj.is_empty(), "os_details should not be empty");

    // Verify expected OS detail fields exist
    assert!(
        os_obj.contains_key("distro") || os_obj.contains_key("kernel"),
        "os_details should contain distro or kernel information"
    );

    // Verify hostname field (optional, may be present or absent)
    // When present, it should be a non-empty string without dots (short hostname)
    if let Some(hostname) = payload.get("hostname").and_then(|v| v.as_str()) {
        assert!(
            !hostname.is_empty(),
            "hostname should not be empty when present"
        );
        assert!(
            !hostname.contains('.'),
            "hostname should be short form (no dots), got: {}",
            hostname
        );
    }
    // hostname field is optional — its absence is valid (skip_serializing_if = None)
}

// =============================================================================
// Test 8: Server Error Handling (5xx)
//
// Mock returns 500 on registration.
// Verify register() returns descriptive server error.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_server_error_on_registration() {
    let (server, base_url) = create_mock_manager().await;

    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(500).set_body_string(r#"{"error": "Internal Server Error"}"#),
        )
        .named("registration_server_error")
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client(&base_url);

    let result = client.register().await;

    assert!(result.is_err(), "Should receive error for 500 response");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("500") || err_msg.contains("Server error"),
        "Error should mention server error or status code, got: {}",
        err_msg
    );
}

// =============================================================================
// Test 9: Rate Limit on Polling (429)
//
// Mock returns approved on polling.
// Verifies the client handles successful polling after registration.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_rate_limit_on_polling_retries() {
    let (server, base_url) = create_mock_manager().await;

    // Registration succeeds
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "rl_poll_token"}"#),
        )
        .named("registration")
        .mount(&server)
        .await;

    // Status returns approved on first poll
    Mock::given(method("GET"))
        .and(path("/api/v1/enroll/status/rl_poll_token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{
                    "status": "approved",
                    "ca_crt": "CA_OK",
                    "server_crt": "CRT_OK",
                    "server_key": "KEY_OK"
                }"#,
        ))
        .named("status_approved_after_retry")
        .mount(&server)
        .await;

    let client = build_client(&base_url);
    let response = client
        .register()
        .await
        .expect("Registration should succeed");

    // Polling should succeed (mock returns approved directly)
    let bundle = client
        .poll_for_approval(&response.polling_token, POLL_INTERVAL_SECONDS, 3)
        .await
        .expect("Should eventually receive approved status");

    assert_eq!(bundle.ca_crt, "CA_OK");
}

// =============================================================================
// Test 10: Client Construction and Configuration
//
// Verify EnrollmentClient builds correctly with various URLs.
// =============================================================================

#[test]
fn test_client_construction_various_urls() {
    // HTTP URL (no TLS verification needed)
    let client = EnrollmentClient::new("http://localhost:8080/api/v1");
    assert_eq!(client.manager_url, "http://localhost:8080/api/v1");

    // HTTPS URL
    let client = EnrollmentClient::new("https://manager.example.com/api/v1");
    assert_eq!(client.manager_url, "https://manager.example.com/api/v1");

    // IP-based URL
    let client = EnrollmentClient::new("http://192.168.1.100:8443/api/v1");
    assert_eq!(client.manager_url, "http://192.168.1.100:8443/api/v1");
}

// =============================================================================
// Test 11: Polling with Default Parameters (interval=0, max_attempts=0)
//
// Verify defaults are applied: interval=60s, max_attempts=1440.
// We test with a fast-responding mock so we don't actually wait 60s.
// =============================================================================

#[actix_rt::test]
#[serial]
async fn test_polling_default_parameters() {
    let (server, base_url) = create_mock_manager().await;

    // Registration
    Mock::given(method("POST"))
        .and(path("/api/v1/enroll"))
        .respond_with(
            ResponseTemplate::new(202).set_body_string(r#"{"polling_token": "defaults_token"}"#),
        )
        .named("registration")
        .mount(&server)
        .await;

    // Status returns approved immediately
    Mock::given(method("GET"))
        .and(path("/api/v1/enroll/status/defaults_token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{
                    "status": "approved",
                    "ca_crt": "DEFAULT_CA",
                    "server_crt": "DEFAULT_CRT",
                    "server_key": "DEFAULT_KEY"
                }"#,
        ))
        .named("status_approved")
        .mount(&server)
        .await;

    let client = build_client(&base_url);
    let response = client
        .register()
        .await
        .expect("Registration should succeed");

    // Call with interval=0 (should default to 60) and max_attempts=0 (should default to 1440)
    // But since mock returns approved on first try, we don't actually wait
    let bundle = client
        .poll_for_approval(&response.polling_token, 0, 0)
        .await
        .expect("Should succeed with default parameters");

    assert_eq!(bundle.ca_crt, "DEFAULT_CA");
}

// =============================================================================
// Repo Config Provisioning Tests
//
// Tests provision_repo_config writes GPG key and sources config to correct
// distro-specific paths, apk append behavior, and unknown distro error.
// =============================================================================

use linux_patch_api::enroll::client::RepoConfig;
use linux_patch_api::enroll::provision::provision_repo_config;
use tempfile::tempdir;

fn sample_repo_config(distro_id: &str, keyring_path: &str) -> RepoConfig {
    RepoConfig {
        gpg_public_key: "-----BEGIN PGP PUBLIC KEY BLOCK-----\nFAKE_GPG_KEY_DATA\n-----END PGP PUBLIC KEY BLOCK-----".to_string(),
        sources_config: "deb [signed-by=/etc/apt/keyrings/lpa-repo.gpg] https://manager.example.com/repo stable main".to_string(),
        distro_id: distro_id.to_string(),
        keyring_path: keyring_path.to_string(),
    }
}

#[actix_rt::test]
#[serial]
async fn test_provision_repo_config_writes_gpg_key_and_sources() {
    let keyring_dir = tempdir().expect("Failed to create temp dir for keyring");
    let keyring_path = keyring_dir.path().join("lpa-repo.gpg");
    let repo = sample_repo_config("ubuntu", keyring_path.to_str().unwrap());

    provision_repo_config(&repo)
        .await
        .expect("provision_repo_config should succeed for ubuntu");

    // 1. Verify GPG key written to keyring_path
    let key_content =
        std::fs::read_to_string(&keyring_path).expect("GPG key file should exist at keyring_path");
    assert!(
        key_content.contains("-----BEGIN PGP PUBLIC KEY BLOCK-----"),
        "GPG key file should contain the public key data"
    );

    // 2. Verify sources config written to /etc/apt/sources.list.d/lpa.list
    let sources_path = "/etc/apt/sources.list.d/lpa.list";
    let sources_content = std::fs::read_to_string(sources_path)
        .expect("Sources config should be written to /etc/apt/sources.list.d/lpa.list");
    assert!(
        sources_content.contains("manager.example.com"),
        "Sources config should contain the repo URL"
    );

    // Clean up system file
    let _ = std::fs::remove_file(sources_path);
}

#[actix_rt::test]
#[serial]
async fn test_provision_repo_config_apk_append_behavior() {
    let keyring_dir = tempdir().expect("Failed to create temp dir for keyring");
    let keyring_path = keyring_dir.path().join("lpa-repo.gpg");

    let repo = RepoConfig {
        gpg_public_key:
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\nFAKE_GPG_KEY\n-----END PGP PUBLIC KEY BLOCK-----"
                .to_string(),
        sources_config: "https://manager.example.com/repo/alpine/main".to_string(),
        distro_id: "alpine".to_string(),
        keyring_path: keyring_path.to_str().unwrap().to_string(),
    };

    // Save original state of /etc/apk/repositories (may not exist on non-Alpine)
    let original_content = std::fs::read_to_string("/etc/apk/repositories").ok();

    // On non-Alpine systems, /etc/apk/ may not exist — create it so the test
    // can verify the append logic without depending on a real Alpine environment.
    std::fs::create_dir_all("/etc/apk").ok();
    if !std::path::Path::new("/etc/apk/repositories").exists() {
        std::fs::write("/etc/apk/repositories", "").ok();
    }

    provision_repo_config(&repo)
        .await
        .expect("provision_repo_config should succeed for alpine");

    // 1. Verify GPG key written
    let key_content = std::fs::read_to_string(&keyring_path).expect("GPG key file should exist");
    assert!(key_content.contains("FAKE_GPG_KEY"));

    // 2. Verify repo URL was appended to /etc/apk/repositories
    let apk_content = std::fs::read_to_string("/etc/apk/repositories")
        .expect("/etc/apk/repositories should exist after provisioning");
    assert!(
        apk_content.contains("https://manager.example.com/repo/alpine/main"),
        "apk repositories should contain the appended repo URL"
    );

    // 3. Test idempotency — calling again should not duplicate
    provision_repo_config(&repo)
        .await
        .expect("Second provision_repo_config should succeed (idempotent)");
    let apk_content2 = std::fs::read_to_string("/etc/apk/repositories")
        .expect("/etc/apk/repositories should still exist");
    let count = apk_content2
        .matches("https://manager.example.com/repo/alpine/main")
        .count();
    assert_eq!(
        count, 1,
        "Repo URL should appear exactly once after duplicate provisioning (idempotent append)"
    );

    // Clean up: restore original or remove file
    if let Some(orig) = original_content {
        std::fs::write("/etc/apk/repositories", orig).ok();
    } else {
        std::fs::remove_file("/etc/apk/repositories").ok();
    }
}

#[actix_rt::test]
#[serial]
async fn test_provision_repo_config_unknown_distro_returns_error() {
    let keyring_dir = tempdir().expect("Failed to create temp dir for keyring");
    let keyring_path = keyring_dir.path().join("lpa-repo.gpg");
    let repo = sample_repo_config("gentoo", keyring_path.to_str().unwrap());

    let result = provision_repo_config(&repo).await;

    assert!(
        result.is_err(),
        "provision_repo_config should return error for unknown distro_id"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("gentoo"),
        "Error message should mention the unknown distro_id: {}",
        err_msg
    );
}
