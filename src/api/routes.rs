//! API Routes Configuration
//!
//! Aggregates all endpoint routes and configures the Actix-web application.
//! Rate limiting is applied at the App level in main.rs using actix-governor
//! with method-based filtering:
//! - **Read tier** (120 req/min, burst 30): GET methods
//! - **Destructive tier** (20 req/min, burst 10): POST/PUT/DELETE methods
//! - **Health exempt**: /health, /api/v1/system/info (health-exempt routes)

use actix_web::{web, HttpResponse};
use std::sync::Arc;
use tracing::info;

use crate::jobs::scheduler::Scheduler;
use crate::packages::cache::PackageCacheState;

use super::handlers::{jobs, packages, patches, system, websocket};

/// Default service handler for unsupported HTTP methods (VULN-005)
/// Returns 405 Method Not Allowed instead of 404 for known endpoints
async fn method_not_allowed() -> HttpResponse {
    HttpResponse::MethodNotAllowed()
        .insert_header(("Allow", "GET, POST, PUT, DELETE"))
        .finish()
}

/// Configure all API routes for the application
pub fn configure_api_routes(
    cfg: &mut web::ServiceConfig,
    scheduler: web::Data<Arc<Scheduler>>,
    backend: web::Data<Box<dyn crate::packages::PackageManagerBackend>>,
    cache_state: web::Data<PackageCacheState>,
) {
    info!("Configuring API v1 routes");

    // Health-exempt endpoint: /api/v1/system/info is registered separately
    // so it can bypass rate limiting applied at the App level
    cfg.service(web::resource("/api/v1/system/info").route(web::get().to(system::get_system_info)));

    cfg.app_data(scheduler)
        .app_data(backend)
        .app_data(cache_state)
        .service(
            web::scope("/api/v1")
                // VULN-005: Default handler for unsupported methods returns 405 instead of 404
                .default_service(web::route().to(method_not_allowed))
                .configure(packages::configure_routes)
                .configure(patches::configure_routes)
                .configure(system::configure_routes)
                .configure(jobs::configure_routes)
                .configure(websocket::configure_routes),
        );
}

/// Health check route (outside API scope for load balancer checks)
/// Note: backend and cache_state are injected via app_data registered in main.rs
pub fn configure_health_route(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(system::health_check));
}
