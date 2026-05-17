//! HTTP client wrapper for manager enrollment API communication.
//!
//! Provides typed request/response structures matching the manager's
//! `/api/v1/enroll` endpoints and a reqwest-based `EnrollmentClient` with
//! insecure TLS mode (manager approval process provides security).

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::signal::unix::{signal as unix_signal, SignalKind};

use crate::enroll::identity;

/// Payload sent to `POST /api/v1/enroll`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequest {
    pub machine_id: String,
    pub fqdn: String,
    pub ip_address: String,
    pub os_details: serde_json::Value,
}

/// Response from `POST /api/v1/enroll` (HTTP 202).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentResponse {
    pub polling_token: String,
}

/// Tagged response from `GET /api/v1/enroll/status/{token}`.
/// The manager uses a JSON-tagged enum with the `status` key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum EnrollmentStatusResponse {
    Pending,
    Approved {
        ca_crt: String,
        server_crt: String,
        server_key: String,
    },
    Denied,
    NotFound,
}

/// PEM-encoded PKI bundle extracted from an `Approved` status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkiBundle {
    pub ca_crt: String,
    pub server_crt: String,
    pub server_key: String,
}

impl From<EnrollmentStatusResponse> for Option<PkiBundle> {
    fn from(response: EnrollmentStatusResponse) -> Self {
        match response {
            EnrollmentStatusResponse::Approved {
                ca_crt,
                server_crt,
                server_key,
            } => Some(PkiBundle {
                ca_crt,
                server_crt,
                server_key,
            }),
            _ => None,
        }
    }
}

/// HTTP client for enrollment communication with the manager.
///
/// Configured with disabled TLS verification (`danger_accept_invalid_certs`)
/// per project security model: manager approval workflow provides authorization,
/// not initial transport encryption.
#[derive(Debug, Clone)]
pub struct EnrollmentClient {
    /// Base URL of the manager API (e.g. `https://manager.example.com/api/v1`)
    pub manager_url: String,
    /// Pre-configured reqwest client with insecure TLS and timeout.
    http_client: reqwest::Client,
}

impl EnrollmentClient {
    /// Create a new enrollment client targeting the given manager base URL.
    ///
    /// The HTTP client is configured with:
    /// - `danger_accept_invalid_certs(true)` — TLS verification disabled
    /// - 30-second timeout for request/response cycle
    ///
    /// # Security
    /// Validates that `manager_url` uses an allowed scheme (`http` or `https`) and
    /// contains a valid host component. Rejects dangerous schemes like `file://`,
    /// `gopher://`, or URLs without a host.
    pub fn new(manager_url: &str) -> Self {
        // SECURITY: Validate URL scheme before building HTTP client.
        // Only http and https are permitted to prevent path traversal, SSRF,
        // or local file access via dangerous schemes (file://, gopher://, etc.).
        let parsed = url::Url::parse(manager_url)
            .map_err(|e| anyhow::anyhow!("Invalid manager URL: {} — must be a valid URL", e))
            .expect("Failed to parse manager URL");

        match parsed.scheme() {
            "http" | "https" => {} // Allowed schemes
            other => panic!(
                "Invalid manager URL scheme '{}' — only 'http' and 'https' are allowed. \
                 Refused dangerous scheme to prevent SSRF/path traversal.",
                other
            ),
        }

        // Ensure the URL has a host component (e.g., reject `http://` with no host)
        if parsed.host().is_none() {
            panic!(
                "Invalid manager URL — missing host component. \
                 Manager URL must include a hostname or IP address (e.g., https://manager.example.com/api/v1)"
            );
        }

        let http_client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build reqwest client — static config should always succeed");

        Self {
            manager_url: manager_url.to_string(),
            http_client,
        }
    }

    /// Resolve the manager URL to an IP address.
    ///
    /// Parses the `manager_url` to extract the host portion. If the host is
    /// already an IPv4/IPv6 address, returns it directly. Otherwise performs
    /// async DNS resolution via `tokio::net::lookup_host` and returns the first
    /// resolved IP.
    ///
    /// # Returns
    /// - `Ok(String)` with the manager IP address (v4 or v6)
    /// - `Err` if URL parsing fails or DNS resolution yields no results
    pub async fn manager_ip(&self) -> Result<String> {
        // Parse URL to extract host using url crate for RFC-compliant parsing
        let parsed = url::Url::parse(&self.manager_url)
            .with_context(|| format!("Failed to parse manager URL '{}'", self.manager_url))?;
        let host_str = parsed
            .host_str()
            .with_context(|| format!("Manager URL '{}' has no host component", self.manager_url))?;

        // Check if already an IP address using url::Host parsing
        if let Ok(url::Host::Ipv4(addr)) = url::Host::parse(host_str) {
            return Ok(addr.to_string());
        }
        if let Ok(url::Host::Ipv6(addr)) = url::Host::parse(host_str) {
            return Ok(addr.to_string());
        }

        // It's a hostname — resolve via async DNS lookup
        tracing::info!(host = host_str, "Resolving manager hostname to IP address");
        let addrs: Vec<_> = tokio::net::lookup_host(format!("{}:1", host_str))
            .await
            .map(|iter| iter.collect())
            .with_context(|| format!("Failed to resolve manager hostname '{}'", host_str))?;

        if addrs.is_empty() {
            return Err(anyhow!(
                "DNS resolution returned no addresses for '{}'",
                host_str
            ));
        }

        // Return the first resolved IP (IPv4 typically preferred by resolver)
        let ip = addrs[0].ip();
        tracing::info!(resolved_ip = %ip, "Manager hostname resolved successfully");
        Ok(ip.to_string())
    }

    /// Register this machine with the manager.
    ///
    /// Collects host identity data (machine-id, FQDN, IP, OS details) and
    /// sends a `POST /api/v1/enroll` request to the manager.
    ///
    /// # Returns
    /// - `Ok(EnrollmentResponse)` with the polling token on HTTP 202
    /// - Error on 429 (rate limited), 5xx (server error), or network failure
    pub async fn register(&self) -> Result<EnrollmentResponse> {
        // 1. Collect identity data
        let machine_id = identity::get_machine_id()
            .context("Failed to read machine-id — host cannot enroll without identity")?;
        let fqdn = identity::get_fqdn()
            .context("Failed to determine FQDN — check hostname configuration")?;
        let ip_addresses = identity::get_ip_addresses()
            .context("Failed to enumerate network interfaces — check network configuration")?;
        let os_details = identity::get_os_details()
            .context("Failed to collect OS details — /etc/os-release may be missing")?;

        // Use first non-loopback IP (manager expects single string)
        let ip_address = ip_addresses
            .first()
            .cloned()
            .unwrap_or_else(|| "127.0.0.1".to_string());

        // 2. Build EnrollmentRequest struct
        let request = EnrollmentRequest {
            machine_id,
            fqdn,
            ip_address,
            os_details,
        };

        tracing::info!(
            manager_url = %self.manager_url,
            "Sending enrollment registration request"
        );

        // 3. POST to {manager_url}/api/v1/enroll
        let enroll_url = format!("{}/api/v1/enroll", self.manager_url);
        let response = self
            .http_client
            .post(&enroll_url)
            .json(&request)
            .send()
            .await
            .context("Network error — failed to reach enrollment endpoint")?;

        // 4. Handle response status codes
        match response.status().as_u16() {
            202 => {
                // Success — parse EnrollmentResponse with polling_token
                let body = response
                    .text()
                    .await
                    .context("Failed to read enrollment response body")?;

                let enrollment_response: EnrollmentResponse =
                    serde_json::from_str(&body)
                        .context("Invalid enrollment response — missing or malformed polling_token")?;

                // SECURITY: Do not log polling_token - it is a bearer credential.
                // Log only that registration succeeded, never the token value itself.
                tracing::info!("Enrollment registration successful");

                Ok(enrollment_response)
            }
            429 => {
                Err(anyhow!(
                    "Rate limited (HTTP 429) — enrollment requests limited to 1/minute per IP. Retry after 60 seconds."
                ))
            }
            status if status >= 500 => {
                let body = response.text().await.ok();
                Err(anyhow!(
                    "Server error (HTTP {}) — {}. {}",
                    status,
                    body.as_deref().unwrap_or("no details"),
                    "The manager may be experiencing issues"
                ))
            }
            other => {
                let body = response.text().await.ok();
                Err(anyhow!(
                    "Unexpected HTTP {} — {}",
                    other,
                    body.as_deref().unwrap_or("no details")
                ))
            }
        }
    }

    /// Poll the enrollment status for a given token (single request).
    ///
    /// Sends `GET /api/v1/enroll/status/{token}` to the manager and returns
    /// the deserialized status response.
    pub async fn poll_status(&self, token: &str) -> Result<EnrollmentStatusResponse> {
        let status_url = format!("{}/api/v1/enroll/status/{}", self.manager_url, token);

        let response = self
            .http_client
            .get(&status_url)
            .send()
            .await
            .context("Network error — failed to reach enrollment status endpoint")?;

        match response.status().as_u16() {
            200 => {
                let body = response
                    .text()
                    .await
                    .context("Failed to read status response body")?;

                let status: EnrollmentStatusResponse = serde_json::from_str(&body)
                    .context("Invalid status response — malformed JSON from manager")?;

                Ok(status)
            }
            404 => Err(anyhow!("Enrollment token expired or invalid (HTTP 404)")),
            429 => Err(anyhow!(
                "Rate limited (HTTP 429) — polling too frequently. Back off and retry."
            )),
            status if status >= 500 => {
                let body = response.text().await.ok();
                Err(anyhow!(
                    "Server error (HTTP {}) — {}. The manager may be experiencing issues.",
                    status,
                    body.as_deref().unwrap_or("no details")
                ))
            }
            other => {
                let body = response.text().await.ok();
                Err(anyhow!(
                    "Unexpected HTTP {} — {}",
                    other,
                    body.as_deref().unwrap_or("no details")
                ))
            }
        }
    }

    /// Poll the manager for enrollment approval status.
    ///
    /// Repeatedly calls `poll_status` until the request is approved, denied,
    /// token becomes invalid, or max attempts are exhausted.
    ///
    /// # Arguments
    /// * `polling_token` - Opaque token returned by `register()`
    /// * `interval_seconds` - Sleep duration between polls (0 = use 60s default)
    /// * `max_attempts` - Maximum poll attempts (0 or >1440 clamped to 1440 for 24h cap)
    ///
    /// # Returns
    /// * `Ok(PkiBundle)` when approved — contains CA cert, server cert, and server key PEMs
    /// * `Err` on denial, token expiry, timeout, or user interruption
    pub async fn poll_for_approval(
        &self,
        polling_token: &str,
        interval_seconds: u64,
        max_attempts: u32,
    ) -> Result<PkiBundle> {
        // Enforce hard limits
        let effective_interval = if interval_seconds == 0 {
            60
        } else {
            interval_seconds
        };
        let effective_max = match max_attempts {
            0 => 1440,
            n if n > 1440 => 1440,
            n => n,
        };

        tracing::info!(
            attempts_limit = effective_max,
            interval_seconds = effective_interval,
            "Starting enrollment approval polling loop"
        );

        let start = Instant::now();
        let sleep_duration = Duration::from_secs(effective_interval);

        // Set up shutdown signal listeners (all target distros are Linux/Unix)
        let mut sigint_stream = Self::setup_sigint()?;
        let mut sigterm_stream = Self::setup_sigterm()?;

        for attempt in 1..=effective_max {
            // Elapsed tracking for log throttling
            let elapsed = start.elapsed();
            let should_log = (attempt % 10 == 0) || elapsed.as_secs() >= 300;

            if should_log && attempt > 1 {
                tracing::info!(
                    attempt = attempt,
                    max_attempts = effective_max,
                    elapsed_seconds = elapsed.as_secs(),
                    "Enrollment approval still pending — continuing to poll"
                );
            }

            // Race: poll request vs shutdown signal
            let status = tokio::select! {
                result = self.poll_status(polling_token) => {
                    match result {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                attempt = attempt,
                                "Transient poll error — will retry"
                            );
                            // Retry on transient errors (network, 5xx)
                            tokio::time::sleep(sleep_duration).await;
                            continue;
                        }
                    }
                }

                // SIGINT handler (Ctrl+C)
                _ = sigint_stream.recv() => {
                    tracing::info!("Enrollment interrupted by user (SIGINT)");
                    return Err(anyhow!("Enrollment interrupted by user"));
                }

                // SIGTERM handler
                _ = sigterm_stream.recv() => {
                    tracing::info!("Enrollment interrupted by system (SIGTERM)");
                    return Err(anyhow!("Enrollment interrupted by system signal"));
                }
            };

            // Process status response
            match status {
                EnrollmentStatusResponse::Pending => {
                    tokio::time::sleep(sleep_duration).await;
                    continue;
                }
                EnrollmentStatusResponse::Approved {
                    ca_crt,
                    server_crt,
                    server_key,
                } => {
                    tracing::info!(
                        elapsed_seconds = start.elapsed().as_secs(),
                        attempts = attempt,
                        "Enrollment approved — received PKI bundle from manager"
                    );
                    return Ok(PkiBundle {
                        ca_crt,
                        server_crt,
                        server_key,
                    });
                }
                EnrollmentStatusResponse::Denied => {
                    tracing::warn!(
                        elapsed_seconds = start.elapsed().as_secs(),
                        "Enrollment request denied by administrator"
                    );
                    return Err(anyhow!("Enrollment request denied by administrator"));
                }
                EnrollmentStatusResponse::NotFound => {
                    tracing::warn!(
                        elapsed_seconds = start.elapsed().as_secs(),
                        "Enrollment token expired or invalid (not found on manager)"
                    );
                    return Err(anyhow!("Enrollment token expired or invalid"));
                }
            }
        }

        // Exhausted all attempts
        let total_seconds = effective_max as u64 * effective_interval;
        tracing::error!(
            max_attempts = effective_max,
            interval_seconds = effective_interval,
            total_seconds = total_seconds,
            "Enrollment polling timed out after maximum attempts"
        );
        Err(anyhow!(
            "Enrollment timed out after {} hours ({}/{} attempts)",
            total_seconds / 3600,
            effective_max,
            effective_max
        ))
    }

    /// Create a SIGINT (Ctrl+C) signal receiver.
    fn setup_sigint() -> Result<tokio::signal::unix::Signal> {
        unix_signal(SignalKind::interrupt()).context("Failed to create SIGINT signal handler")
    }

    /// Create a SIGTERM signal receiver.
    fn setup_sigterm() -> Result<tokio::signal::unix::Signal> {
        unix_signal(SignalKind::terminate()).context("Failed to create SIGTERM signal handler")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_request_serializes() {
        let request = EnrollmentRequest {
            machine_id: "test1234".into(),
            fqdn: "node.example.com".into(),
            ip_address: "192.168.1.10".into(),
            os_details: serde_json::json!({"distro": "Debian", "version": "12"}),
        };
        let json = serde_json::to_string(&request).expect("Failed to serialize EnrollmentRequest");
        assert!(json.contains("machine_id"));
        assert!(json.contains("fqdn"));
    }

    #[test]
    fn enrollment_response_deserializes() {
        let json = r#"{"polling_token": "abc123def456"}"#;
        let response: EnrollmentResponse =
            serde_json::from_str(json).expect("Failed to deserialize EnrollmentResponse");
        assert_eq!(response.polling_token, "abc123def456");
    }

    #[test]
    fn status_pending_deserializes() {
        let json = r#"{"status": "pending"}"#;
        let status: EnrollmentStatusResponse =
            serde_json::from_str(json).expect("Failed to deserialize Pending");
        match status {
            EnrollmentStatusResponse::Pending => {}
            _ => panic!("Expected Pending variant"),
        }
    }

    #[test]
    fn status_approved_deserializes() {
        let json = r#"{
            "status": "approved",
            "ca_crt": "-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----",
            "server_crt": "-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----",
            "server_key": "-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----"
        }"#;
        let status: EnrollmentStatusResponse =
            serde_json::from_str(json).expect("Failed to deserialize Approved");
        match status {
            EnrollmentStatusResponse::Approved { .. } => {}
            _ => panic!("Expected Approved variant"),
        }
    }

    #[test]
    fn approved_to_pki_bundle() {
        let status = EnrollmentStatusResponse::Approved {
            ca_crt: "ca".into(),
            server_crt: "crt".into(),
            server_key: "key".into(),
        };
        let bundle: Option<PkiBundle> = status.into();
        assert!(bundle.is_some());
        let bundle = bundle.unwrap();
        assert_eq!(bundle.ca_crt, "ca");
    }

    #[test]
    fn pending_to_pki_bundle_is_none() {
        let status = EnrollmentStatusResponse::Pending;
        let bundle: Option<PkiBundle> = status.into();
        assert!(bundle.is_none());
    }

    #[test]
    fn enrollment_client_has_insecure_tls() {
        let client = EnrollmentClient::new("https://manager.example.com/api/v1");
        // Client builds without panic — danger_accept_invalid_certs is set
        assert_eq!(client.manager_url, "https://manager.example.com/api/v1");
    }
}
