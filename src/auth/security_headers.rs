//! Security Headers Middleware Module
//!
//! Provides request-level security header validation for Actix-web.
//! Enforces VULN-006: rejects requests with duplicate critical headers
//! (content-type, authorization, host) to prevent HTTP request smuggling
//! and response-splitting attacks.

use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    Error,
};
use futures_util::future::LocalBoxFuture;
use tracing::warn;

/// Critical headers that MUST NOT appear more than once in a request.
/// Duplicate values for these headers can enable request smuggling,
/// response splitting, and other HTTP parsing ambiguities.
const CRITICAL_HEADERS: &[&str] = &["content-type", "authorization", "host"];

/// Security headers middleware for Actix-web.
///
/// Checks every incoming request for duplicate critical headers (VULN-006)
/// and rejects malformed requests with HTTP 400 Bad Request.
pub struct SecurityHeadersMiddleware;

impl SecurityHeadersMiddleware {
    /// Create a new security headers middleware instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for SecurityHeadersMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

/// Actix-web Transform implementation — wraps SecurityHeadersMiddleware as middleware
impl<S, B> Transform<S, ServiceRequest> for SecurityHeadersMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = SecurityHeadersMiddlewareService<S>;
    type Future = futures_util::future::Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        futures_util::future::ok(SecurityHeadersMiddlewareService { service })
    }
}

/// Security headers middleware service — performs per-request duplicate header checks
pub struct SecurityHeadersMiddlewareService<S> {
    service: S,
}

impl<S, B> Service<ServiceRequest> for SecurityHeadersMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // VULN-006: Check for duplicate critical headers before processing
        if has_duplicate_critical_headers(req.headers()) {
            let peer_addr = req.peer_addr();
            warn!(
                peer_addr = ?peer_addr,
                "Duplicate critical headers detected - rejecting request (VULN-006)"
            );
            return Box::pin(async move {
                Err(actix_web::error::ErrorBadRequest(
                    "Duplicate critical headers not allowed",
                ))
            });
        }

        // All checks passed — call the service
        let fut = self.service.call(req);
        Box::pin(fut)
    }
}

/// Check for duplicate critical headers (VULN-006).
/// Returns true if any critical header appears more than once.
///
/// This function is public for testing purposes.
pub fn has_duplicate_critical_headers(headers: &actix_web::http::header::HeaderMap) -> bool {
    for header_name in CRITICAL_HEADERS.iter() {
        let mut count = 0;
        for (name, _value) in headers.iter() {
            if name.as_str().eq_ignore_ascii_case(header_name) {
                count += 1;
                if count > 1 {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::header;

    #[test]
    fn test_no_duplicate_headers_passes() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        headers.insert(header::AUTHORIZATION, "Bearer test".parse().unwrap());
        headers.insert(header::HOST, "localhost".parse().unwrap());
        assert!(!has_duplicate_critical_headers(&headers));
    }

    #[test]
    fn test_duplicate_content_type_rejected() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        headers.append(header::CONTENT_TYPE, "text/plain".parse().unwrap());
        assert!(has_duplicate_critical_headers(&headers));
    }

    #[test]
    fn test_duplicate_authorization_rejected() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer test1".parse().unwrap());
        headers.append(header::AUTHORIZATION, "Bearer test2".parse().unwrap());
        assert!(has_duplicate_critical_headers(&headers));
    }

    #[test]
    fn test_duplicate_host_rejected() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::HOST, "localhost".parse().unwrap());
        headers.append(header::HOST, "evil.com".parse().unwrap());
        assert!(has_duplicate_critical_headers(&headers));
    }

    #[test]
    fn test_non_critical_duplicate_headers_allowed() {
        // Duplicate non-critical headers should be allowed
        let mut headers = header::HeaderMap::new();
        headers.insert(header::ACCEPT, "text/html".parse().unwrap());
        headers.append(header::ACCEPT, "application/json".parse().unwrap());
        assert!(!has_duplicate_critical_headers(&headers));
    }

    #[test]
    fn test_empty_headers_passes() {
        let headers = header::HeaderMap::new();
        assert!(!has_duplicate_critical_headers(&headers));
    }
}
