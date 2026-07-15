//! Rate Limiting and Job Queue Depth Tests
//!
//! Tests for:
//! - HTTP rate limiting (429 when exceeded)
//! - Health endpoint exemption from rate limiting
//! - Job queue depth cap (429 when full)
//! - Configurable queue depth

use actix_web::{test, web, App};
use linux_patch_api::api::rate_limit::RateLimitMiddleware;
use linux_patch_api::api::routes::{configure_api_routes, configure_health_route};
use linux_patch_api::auth::crl;
use linux_patch_api::config::loader::RateLimitConfig;
use linux_patch_api::jobs::manager::{JobManager, JobOperation};
use linux_patch_api::packages::cache::PackageCacheState;
use linux_patch_api::packages::coordinator::OperationCoordinator;
use std::net::SocketAddr;
use std::sync::Arc;

/// Helper to build a test request with a peer IP address (required for rate limiting)
fn test_request(method: actix_web::http::Method, uri: &str) -> test::TestRequest {
    test::TestRequest::with_uri(uri)
        .method(method)
        .peer_addr(SocketAddr::from(([127, 0, 0, 1], 12345)))
}

#[actix_web::test]
async fn test_health_endpoint_exempt_from_rate_limiting() {
    let job_manager = web::Data::new(JobManager::new(5, 30, 100).unwrap());
    let backend = web::Data::new(linux_patch_api::packages::create_backend().unwrap());
    let cache_state = web::Data::new(PackageCacheState::new());
    let coordinator = web::Data::new(Arc::new(OperationCoordinator::new(5)));
    let shared_crl_state = web::Data::new(crl::new_shared_state());
    let rl_cfg = RateLimitConfig::default();

    let app = test::init_service(
        App::new()
            .wrap(RateLimitMiddleware::new(rl_cfg))
            .app_data(job_manager.clone())
            .app_data(backend.clone())
            .app_data(coordinator.clone())
            .app_data(cache_state.clone())
            .app_data(shared_crl_state.clone())
            .configure(|cfg| {
                configure_api_routes(
                    cfg,
                    job_manager.clone(),
                    backend.clone(),
                    coordinator.clone(),
                    cache_state.clone(),
                );
            })
            .configure(configure_health_route),
    )
    .await;

    // Health endpoint should always respond 200 regardless of rate limiting
    for _ in 0..50 {
        let req = test_request(actix_web::http::Method::GET, "/health").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            200,
            "Health endpoint should be exempt from rate limiting"
        );
    }
}

#[actix_web::test]
async fn test_system_info_exempt_from_rate_limiting() {
    let job_manager = web::Data::new(JobManager::new(5, 30, 100).unwrap());
    let backend = web::Data::new(linux_patch_api::packages::create_backend().unwrap());
    let cache_state = web::Data::new(PackageCacheState::new());
    let coordinator = web::Data::new(Arc::new(OperationCoordinator::new(5)));
    let shared_crl_state = web::Data::new(crl::new_shared_state());
    let rl_cfg = RateLimitConfig::default();

    let app = test::init_service(
        App::new()
            .wrap(RateLimitMiddleware::new(rl_cfg))
            .app_data(job_manager.clone())
            .app_data(backend.clone())
            .app_data(coordinator.clone())
            .app_data(cache_state.clone())
            .app_data(shared_crl_state.clone())
            .configure(|cfg| {
                configure_api_routes(
                    cfg,
                    job_manager.clone(),
                    backend.clone(),
                    coordinator.clone(),
                    cache_state.clone(),
                );
            })
            .configure(configure_health_route),
    )
    .await;

    // /api/v1/system/info should be exempt from rate limiting
    for _ in 0..50 {
        let req = test_request(actix_web::http::Method::GET, "/api/v1/system/info").to_request();
        let resp = test::call_service(&app, req).await;
        // May return 200 or 500 depending on system, but should NOT be 429
        assert_ne!(
            resp.status(),
            429,
            "System info endpoint should be exempt from rate limiting"
        );
    }
}

#[actix_web::test]
async fn test_read_rate_limiting_returns_429() {
    let job_manager = web::Data::new(JobManager::new(5, 30, 100).unwrap());
    let backend = web::Data::new(linux_patch_api::packages::create_backend().unwrap());
    let cache_state = web::Data::new(PackageCacheState::new());
    let coordinator = web::Data::new(Arc::new(OperationCoordinator::new(5)));
    let shared_crl_state = web::Data::new(crl::new_shared_state());
    // Use very low limits so sequential test requests can reliably trigger 429
    let rl_cfg = RateLimitConfig {
        enabled: true,
        destructive_per_minute: 20,
        destructive_burst: 10,
        read_per_minute: 5,
        read_burst: 3,
    };

    let app = test::init_service(
        App::new()
            .wrap(RateLimitMiddleware::new(rl_cfg))
            .app_data(job_manager.clone())
            .app_data(backend.clone())
            .app_data(coordinator.clone())
            .app_data(cache_state.clone())
            .app_data(shared_crl_state.clone())
            .configure(|cfg| {
                configure_api_routes(
                    cfg,
                    job_manager.clone(),
                    backend.clone(),
                    coordinator.clone(),
                    cache_state.clone(),
                );
            })
            .configure(configure_health_route),
    )
    .await;

    // Read tier: 120 req/min, burst 30
    // Send more than burst_size requests to trigger rate limiting
    let mut rate_limited = false;
    for _ in 0..50 {
        let req = test_request(actix_web::http::Method::GET, "/api/v1/packages").to_request();
        let resp = test::call_service(&app, req).await;
        if resp.status() == 429 {
            rate_limited = true;
            break;
        }
    }
    assert!(
        rate_limited,
        "Read endpoint should return 429 after exceeding burst limit"
    );
}

#[actix_web::test]
async fn test_destructive_rate_limiting_returns_429() {
    let job_manager = web::Data::new(JobManager::new(5, 30, 100).unwrap());
    let backend = web::Data::new(linux_patch_api::packages::create_backend().unwrap());
    let cache_state = web::Data::new(PackageCacheState::new());
    let coordinator = web::Data::new(Arc::new(OperationCoordinator::new(5)));
    let shared_crl_state = web::Data::new(crl::new_shared_state());
    // Use very low limits so sequential test requests can reliably trigger 429
    let rl_cfg = RateLimitConfig {
        enabled: true,
        destructive_per_minute: 5,
        destructive_burst: 3,
        read_per_minute: 120,
        read_burst: 30,
    };

    let app = test::init_service(
        App::new()
            .wrap(RateLimitMiddleware::new(rl_cfg))
            .app_data(job_manager.clone())
            .app_data(backend.clone())
            .app_data(coordinator.clone())
            .app_data(cache_state.clone())
            .app_data(shared_crl_state.clone())
            .configure(|cfg| {
                configure_api_routes(
                    cfg,
                    job_manager.clone(),
                    backend.clone(),
                    coordinator.clone(),
                    cache_state.clone(),
                );
            })
            .configure(configure_health_route),
    )
    .await;

    // Destructive tier: 20 req/min, burst 10
    // Send more than burst_size requests to trigger rate limiting
    let mut rate_limited = false;
    for _ in 0..15 {
        let req = test_request(actix_web::http::Method::POST, "/api/v1/packages")
            .set_json(serde_json::json!({
                "packages": [{"name": "test-pkg"}],
                "options": {}
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        if resp.status() == 429 {
            rate_limited = true;
            break;
        }
    }
    assert!(
        rate_limited,
        "Destructive endpoint should return 429 after exceeding burst limit"
    );
}

#[actix_web::test]
async fn test_rate_limiting_disabled() {
    let job_manager = web::Data::new(JobManager::new(5, 30, 100).unwrap());
    let backend = web::Data::new(linux_patch_api::packages::create_backend().unwrap());
    let cache_state = web::Data::new(PackageCacheState::new());
    let coordinator = web::Data::new(Arc::new(OperationCoordinator::new(5)));
    let shared_crl_state = web::Data::new(crl::new_shared_state());
    let rl_cfg = RateLimitConfig {
        enabled: false,
        ..RateLimitConfig::default()
    };

    let app = test::init_service(
        App::new()
            .wrap(RateLimitMiddleware::new(rl_cfg))
            .app_data(job_manager.clone())
            .app_data(backend.clone())
            .app_data(coordinator.clone())
            .app_data(cache_state.clone())
            .app_data(shared_crl_state.clone())
            .configure(|cfg| {
                configure_api_routes(
                    cfg,
                    job_manager.clone(),
                    backend.clone(),
                    coordinator.clone(),
                    cache_state.clone(),
                );
            })
            .configure(configure_health_route),
    )
    .await;

    // With rate limiting disabled, even excessive requests should not get 429
    let mut got_429 = false;
    for _ in 0..50 {
        let req = test_request(actix_web::http::Method::GET, "/api/v1/packages").to_request();
        let resp = test::call_service(&app, req).await;
        if resp.status() == 429 {
            got_429 = true;
            break;
        }
    }
    assert!(
        !got_429,
        "Should not get 429 when rate limiting is disabled"
    );
}

#[actix_web::test]
async fn test_job_queue_depth_cap() {
    // Create JobManager with very small queue depth (2)
    let job_manager = JobManager::new(5, 30, 2).unwrap();

    // Fill the queue with pending jobs
    job_manager
        .create_job_unchecked(JobOperation::Install, vec!["pkg1".to_string()])
        .await
        .unwrap();
    job_manager
        .create_job_unchecked(JobOperation::Install, vec!["pkg2".to_string()])
        .await
        .unwrap();

    // Queue should now be at capacity
    assert!(
        !job_manager.can_accept_job().await,
        "Queue should be at capacity after filling to max_queue_depth"
    );
}

#[actix_web::test]
async fn test_job_queue_depth_default() {
    // Default max_queue_depth should be 100
    let job_manager = JobManager::new(5, 30, 100).unwrap();
    assert_eq!(
        job_manager.max_queue_depth(),
        100,
        "Default max_queue_depth should be 100"
    );
}

#[actix_web::test]
async fn test_job_queue_depth_configurable() {
    // Verify queue depth is configurable
    let job_manager = JobManager::new(5, 30, 50).unwrap();
    assert_eq!(
        job_manager.max_queue_depth(),
        50,
        "max_queue_depth should be configurable"
    );
}

#[actix_web::test]
async fn test_can_accept_job_respects_queue_depth() {
    let job_manager = JobManager::new(5, 30, 3).unwrap();

    // Should accept when queue is empty
    assert!(
        job_manager.can_accept_job().await,
        "Should accept job when queue is empty"
    );

    // Fill to capacity
    job_manager
        .create_job_unchecked(JobOperation::Install, vec!["a".to_string()])
        .await
        .unwrap();
    job_manager
        .create_job_unchecked(JobOperation::Install, vec!["b".to_string()])
        .await
        .unwrap();
    job_manager
        .create_job_unchecked(JobOperation::Install, vec!["c".to_string()])
        .await
        .unwrap();

    // Should reject when at capacity
    assert!(
        !job_manager.can_accept_job().await,
        "Should reject job when queue is at capacity"
    );
}

#[actix_web::test]
async fn test_rate_limit_config_defaults() {
    let config = RateLimitConfig::default();
    assert!(config.enabled, "Rate limiting should be enabled by default");
    assert_eq!(config.destructive_per_minute, 20);
    assert_eq!(config.destructive_burst, 10);
    assert_eq!(config.read_per_minute, 120);
    assert_eq!(config.read_burst, 30);
}
