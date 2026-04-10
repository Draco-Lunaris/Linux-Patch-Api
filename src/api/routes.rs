//! API Routes Configuration
//!
//! Aggregates all endpoint routes and configures the Actix-web application.

use actix_web::{web, HttpResponse, http::Method};
use tracing::info;

use crate::packages::create_backend;
use crate::jobs::manager::JobManager;

use super::handlers::{packages, patches, system, jobs, websocket};

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
    job_manager: web::Data<JobManager>,
    backend: web::Data<Box<dyn crate::packages::PackageManagerBackend>>,
) {
    info!("Configuring API v1 routes");

    cfg.app_data(job_manager)
        .app_data(backend)
        .service(
            web::scope("/api/v1")
                // VULN-005: Default handler for unsupported methods returns 405 instead of 404
                .default_service(web::route().to(method_not_allowed))
                // Package Management Endpoints
                .configure(packages::configure_routes)
                // Patch Management Endpoints
                .configure(patches::configure_routes)
                // System Management Endpoints
                .configure(system::configure_routes)
                // Job Management Endpoints
                .configure(jobs::configure_routes)
                // WebSocket Endpoint
                .configure(websocket::configure_routes),
        );
}

/// Health check route (outside API scope for load balancer checks)
pub fn configure_health_route(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(system::health_check));
}
