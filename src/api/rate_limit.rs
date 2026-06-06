//! Rate Limiting Middleware
//!
//! Custom Actix-web middleware that provides per-IP rate limiting with two tiers:
//! - **Destructive tier**: POST/PUT/DELETE methods (20 req/min, burst 10 by default)
//! - **Read tier**: GET methods (120 req/min, burst 30 by default)
//! - **Health exempt**: /health, /api/v1/system/info bypass rate limiting entirely

use actix_governor::governor::clock::{Clock, DefaultClock};
use actix_governor::governor::middleware::NoOpMiddleware;
use actix_governor::governor::state::keyed::DefaultKeyedStateStore;
use actix_governor::governor::{Quota, RateLimiter};
use actix_web::body::BoxBody;
use actix_web::dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::http::Method;
use actix_web::{HttpResponse, ResponseError};
use std::future::{ready, Ready};
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use tracing::info;

use crate::config::loader::RateLimitConfig;

/// Paths exempt from rate limiting
const EXEMPT_PATHS: &[&str] = &["/health", "/api/v1/system/info"];

/// Rate limiting middleware factory
pub struct RateLimitMiddleware {
    config: RateLimitConfig,
}

impl RateLimitMiddleware {
    pub fn new(config: RateLimitConfig) -> Self {
        Self { config }
    }
}

/// Error returned when rate limit is exceeded
#[derive(Debug)]
pub struct RateLimitError {
    retry_after_secs: u64,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rate limit exceeded. Retry after {} seconds.",
            self.retry_after_secs
        )
    }
}

impl ResponseError for RateLimitError {
    fn status_code(&self) -> actix_web::http::StatusCode {
        actix_web::http::StatusCode::TOO_MANY_REQUESTS
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", self.retry_after_secs.to_string()))
            .content_type("text/plain; charset=utf-8")
            .body(self.to_string())
    }
}

/// Type alias for per-IP rate limiter
pub type KeyedRateLimiter =
    RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock, NoOpMiddleware>;

/// Shared rate limiter state
#[derive(Clone)]
pub struct RateLimiters {
    /// Rate limiter for destructive operations (POST/PUT/DELETE)
    destructive: Arc<KeyedRateLimiter>,
    /// Rate limiter for read operations (GET)
    read: Arc<KeyedRateLimiter>,
    /// Whether rate limiting is enabled
    enabled: bool,
}

impl RateLimiters {
    /// Build rate limiters from configuration
    pub fn new(config: &RateLimitConfig) -> Self {
        let destructive_quota =
            Quota::per_minute(NonZeroU32::new(config.destructive_per_minute).unwrap())
                .allow_burst(NonZeroU32::new(config.destructive_burst).unwrap());

        let read_quota = Quota::per_minute(NonZeroU32::new(config.read_per_minute).unwrap())
            .allow_burst(NonZeroU32::new(config.read_burst).unwrap());

        let destructive = Arc::new(KeyedRateLimiter::keyed(destructive_quota));
        let read = Arc::new(KeyedRateLimiter::keyed(read_quota));

        info!(
            enabled = config.enabled,
            destructive_per_min = config.destructive_per_minute,
            destructive_burst = config.destructive_burst,
            read_per_min = config.read_per_minute,
            read_burst = config.read_burst,
            "Rate limiters configured"
        );

        Self {
            destructive,
            read,
            enabled: config.enabled,
        }
    }

    /// Check if a request should be rate limited
    /// Returns Ok(()) if the request is allowed, Err(RateLimitError) if rate limited
    pub fn check(
        &self,
        method: &Method,
        path: &str,
        peer_ip: IpAddr,
    ) -> Result<(), RateLimitError> {
        if !self.enabled {
            return Ok(());
        }

        // Exempt paths bypass rate limiting entirely
        if EXEMPT_PATHS.contains(&path) {
            return Ok(());
        }

        let limiter = match *method {
            Method::POST | Method::PUT | Method::DELETE => &self.destructive,
            Method::GET => &self.read,
            _ => &self.read, // Default to read tier for other methods
        };

        match limiter.check_key(&peer_ip) {
            Ok(()) => Ok(()),
            Err(negative) => {
                let retry_after = negative
                    .wait_time_from(DefaultClock::default().now())
                    .as_secs();
                Err(RateLimitError {
                    retry_after_secs: retry_after.max(1),
                })
            }
        }
    }
}

impl<S> Transform<S, ServiceRequest> for RateLimitMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = actix_web::Error>,
    S::Future: 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = actix_web::Error;
    type Transform = RateLimitService<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(RateLimitService {
            service,
            limiters: RateLimiters::new(&self.config),
        }))
    }
}

/// Rate limiting service wrapper
pub struct RateLimitService<S> {
    service: S,
    limiters: RateLimiters,
}

impl<S> Service<ServiceRequest> for RateLimitService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = actix_web::Error>,
    S::Future: 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = actix_web::Error;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // Extract peer IP
        let peer_ip = req
            .connection_info()
            .peer_addr()
            .and_then(|addr| addr.parse::<IpAddr>().ok());

        // Check rate limiting
        if let Some(ip) = peer_ip {
            let method = req.method().clone();
            let path = req.path().to_string();

            if let Err(e) = self.limiters.check(&method, &path, ip) {
                // Rate limited - return 429 response
                let (http_req, _) = req.into_parts();
                let response = e.error_response();
                let srv_resp = ServiceResponse::new(http_req, response);
                return Box::pin(ready(Ok(srv_resp)));
            }
        }

        // Not rate limited - pass through to the inner service
        Box::pin(self.service.call(req))
    }
}
