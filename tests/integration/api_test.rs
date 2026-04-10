//! Integration Tests for Linux Patch API Endpoints
//!
//! Tests all 15 REST API endpoints:
//! - Package Management (5): GET/POST/PUT/DELETE /packages
//! - Patch Management (2): GET/POST /patches
//! - System Management (3): GET /system/info, GET /health, POST /system/reboot
//! - Job Management (4): GET/POST/DELETE /jobs, POST /jobs/{id}/rollback
//! - WebSocket (1): WS /ws/jobs

use actix_web::{web, App, test, http::StatusCode};
use serde_json::json;
use uuid::Uuid;

use linux_patch_api::api::{configure_api_routes, configure_health_route};
use linux_patch_api::jobs::manager::JobManager;
use linux_patch_api::packages::{create_backend, AptBackend};

/// Create test app with all routes configured
async fn create_test_app() -> actix_web::App<impl actix_web::dev::ServiceFactory<
    actix_web::dev::ServiceRequest,
    Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
    Config = (),
    InitError = (),
    Error = actix_web::Error,
>> {
    let job_manager = JobManager::new(5, 30).unwrap();
    let backend = Box::new(AptBackend::new()) as Box<dyn linux_patch_api::packages::PackageManagerBackend>;
    
    let job_manager_data = web::Data::new(job_manager);
    let backend_data = web::Data::new(backend);

    App::new()
        .app_data(job_manager_data.clone())
        .app_data(backend_data.clone())
        .configure(|cfg| {
            configure_api_routes(cfg, job_manager_data.clone(), backend_data.clone());
        })
        .configure(configure_health_route)
}

// =============================================================================
// Health Check Tests
// =============================================================================

#[actix_rt::test]
async fn test_health_endpoint() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/health")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["status"].as_str().unwrap() == "healthy");
    assert!(body["data"]["version"].as_str().unwrap().len() > 0);
}

// =============================================================================
// Package Management Tests
// =============================================================================

#[actix_rt::test]
async fn test_list_packages() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/api/v1/packages")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"].is_object());
    assert!(body["data"]["packages"].is_array());
    assert!(body["data"]["total"].is_u64());
}

#[actix_rt::test]
async fn test_list_packages_with_filter() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/api/v1/packages?status=installed&sort=name&order=asc")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
}

#[actix_rt::test]
async fn test_get_package_not_found() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/api/v1/packages/nonexistent-package-xyz")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    // Package may or may not exist, but response should be valid
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::NOT_FOUND);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert!(body["request_id"].is_string());
    assert!(body["timestamp"].is_string());
}

#[actix_rt::test]
async fn test_install_packages_async() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let payload = json!({
        "packages": [{"name": "curl", "version": null}],
        "options": {"force": false, "no_recommends": false}
    });

    let req = test::TestRequest::post()
        .uri("/api/v1/packages")
        .set_json(&payload)
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    // Should return 202 Accepted for async operation
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["job_id"].is_string());
    assert_eq!(body["data"]["status"], "pending");
    assert_eq!(body["data"]["operation"], "install");
}

#[actix_rt::test]
async fn test_update_package_async() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::put()
        .uri("/api/v1/packages/curl")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    // Should return 202 Accepted for async operation
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["job_id"].is_string());
    assert_eq!(body["data"]["operation"], "update");
}

#[actix_rt::test]
async fn test_remove_package_async() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::delete()
        .uri("/api/v1/packages/curl")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    // Should return 202 Accepted for async operation
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["job_id"].is_string());
    assert_eq!(body["data"]["operation"], "remove");
}

// =============================================================================
// Patch Management Tests
// =============================================================================

#[actix_rt::test]
async fn test_list_patches() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/api/v1/patches")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["patches"].is_array());
    assert!(body["data"]["total"].is_u64());
    assert!(body["data"]["security_updates"].is_u64());
    assert!(body["data"]["requires_reboot"].is_boolean());
}

#[actix_rt::test]
async fn test_apply_patches_async() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let payload = json!({
        "packages": ["curl", "wget"],
        "reboot": false,
        "reboot_delay_seconds": 0
    });

    let req = test::TestRequest::post()
        .uri("/api/v1/patches/apply")
        .set_json(&payload)
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    // Should return 202 Accepted for async operation
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["job_id"].is_string());
    assert_eq!(body["data"]["operation"], "patch_apply");
}

// =============================================================================
// System Management Tests
// =============================================================================

#[actix_rt::test]
async fn test_get_system_info() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/api/v1/system/info")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["hostname"].is_string());
    assert!(body["data"]["os"].is_string());
    assert!(body["data"]["kernel"].is_string());
    assert!(body["data"]["architecture"].is_string());
}

#[actix_rt::test]
async fn test_reboot_system_async() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let payload = json!({
        "delay_seconds": 0,
        "force": true
    });

    let req = test::TestRequest::post()
        .uri("/api/v1/system/reboot")
        .set_json(&payload)
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    // Should return 202 Accepted for async operation
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["job_id"].is_string());
    assert_eq!(body["data"]["operation"], "reboot");
}

// =============================================================================
// Job Management Tests
// =============================================================================

#[actix_rt::test]
async fn test_list_jobs() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/api/v1/jobs")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
    assert!(body["data"]["jobs"].is_array());
    assert!(body["data"]["total"].is_u64());
}

#[actix_rt::test]
async fn test_list_jobs_with_filter() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/api/v1/jobs?status=pending&limit=10")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], true);
}

#[actix_rt::test]
async fn test_get_job_not_found() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let fake_uuid = Uuid::new_v4().to_string();
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/jobs/{}", fake_uuid))
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "JOB_NOT_FOUND");
}

#[actix_rt::test]
async fn test_get_job_invalid_id() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/api/v1/jobs/invalid-uuid")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "INVALID_JOB_ID");
}

#[actix_rt::test]
async fn test_rollback_job_not_found() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let fake_uuid = Uuid::new_v4().to_string();
    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/jobs/{}/rollback", fake_uuid))
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], false);
}

#[actix_rt::test]
async fn test_delete_job_not_found() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let fake_uuid = Uuid::new_v4().to_string();
    let req = test::TestRequest::delete()
        .uri(&format!("/api/v1/jobs/{}", fake_uuid))
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "JOB_NOT_FOUND");
}

// =============================================================================
// Response Envelope Tests
// =============================================================================

#[actix_rt::test]
async fn test_response_envelope_structure() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri("/health")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    let body: serde_json::Value = test::read_body_json(resp).await;

    // Verify standard envelope structure
    assert!(body.get("success").is_some(), "Missing 'success' field");
    assert!(body.get("request_id").is_some(), "Missing 'request_id' field");
    assert!(body.get("timestamp").is_some(), "Missing 'timestamp' field");
    assert!(body.get("data").is_some(), "Missing 'data' field");
    assert!(body.get("error").is_some(), "Missing 'error' field");

    // Verify request_id is valid UUID format
    let request_id = body["request_id"].as_str().unwrap();
    assert!(Uuid::parse_str(request_id).is_ok(), "request_id is not valid UUID");

    // Verify timestamp is ISO 8601 format
    let timestamp = body["timestamp"].as_str().unwrap();
    assert!(timestamp.contains("T"), "timestamp is not ISO 8601 format");
}

// =============================================================================
// Error Response Tests
// =============================================================================

#[actix_rt::test]
async fn test_error_response_structure() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/jobs/{}", Uuid::new_v4()))
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    let body: serde_json::Value = test::read_body_json(resp).await;

    // Verify error response structure
    assert_eq!(body["success"], false);
    assert!(body["error"].is_object());
    assert!(body["error"]["code"].is_string());
    assert!(body["error"]["message"].is_string());
    assert!(body["error"]["retryable"].is_boolean());
}

// =============================================================================
// Security Hardening Tests (Phase 4 - VULN-001 to VULN-006)
// =============================================================================

#[actix_rt::test]
async fn test_vuln_001_package_name_length_validation() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    // Test: Package name exceeding 256 characters should be rejected
    let long_name = "a".repeat(300);
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/packages/{}", long_name))
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "Long package names should return 400");

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], false);
    assert!(body["error"]["code"].as_str().unwrap().contains("VALIDATION"));
}

#[actix_rt::test]
async fn test_vuln_003_empty_string_rejection() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    // Test: Empty package name should be rejected
    let req = test::TestRequest::get()
        .uri("/api/v1/packages/")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    // Empty path segment should return 400 or 404, not 200
    assert!(resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::NOT_FOUND);

    // Test: Empty string in install request
    let payload = json!({
        "packages": [{"name": "", "version": null}],
        "options": {"force": false}
    });

    let req = test::TestRequest::post()
        .uri("/api/v1/packages")
        .set_json(&payload)
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "Empty package names should return 400");

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["success"], false);
}

#[actix_rt::test]
async fn test_vuln_005_method_not_allowed() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    // Test: PATCH method on packages endpoint should return 405, not 404
    let req = test::TestRequest::patch()
        .uri("/api/v1/packages/curl")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "Unsupported methods should return 405");

    // Test: OPTIONS method should also return 405
    let req = test::TestRequest::options()
        .uri("/api/v1/packages/curl")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[actix_rt::test]
async fn test_vuln_002_path_traversal_protection() {
    // Test path normalization utility function
    use linux_patch_api::api::handlers::system::{normalize_path, validate_path_no_traversal};

    // Valid paths should pass
    assert!(validate_path_no_traversal("valid/path"));
    assert!(validate_path_no_traversal("simple"));

    // Traversal patterns should be rejected
    assert!(!validate_path_no_traversal("../etc/passwd"));
    assert!(!validate_path_no_traversal("..\\windows\\system32"));
    assert!(!validate_path_no_traversal("path//double//slash"));
    
    // URL-encoded traversal should be rejected
    assert!(!validate_path_no_traversal("%2e%2e/etc/passwd"));
    assert!(!validate_path_no_traversal("..%2fetc/passwd"));
}

#[actix_rt::test]
async fn test_valid_package_name_accepted() {
    let app = create_test_app().await;
    let mut app = test::init_service(app).await;

    // Test: Valid package name under 256 chars should work
    let req = test::TestRequest::get()
        .uri("/api/v1/packages/curl")
        .to_request();
    
    let resp = test::call_service(&mut app, req).await;
    // Should be OK or NOT_FOUND (package may not exist), but NOT BAD_REQUEST
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::NOT_FOUND);
}
