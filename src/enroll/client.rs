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

/// Detect distro_id for repo-config fetch using the same logic as the manager.
/// Uses NAME field from /etc/os-release (matching manager's detect_distro_id behavior).
fn detect_distro_id_for_repo_config() -> Result<String> {
    let content = std::fs::read_to_string("/etc/os-release").context(
        "Failed to read /etc/os-release — cannot determine distro for repo-config fetch",
    )?;

    let mut name = None;
    let mut id_like = None;

    for line in content.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once('=') {
            let unquoted = value.trim().trim_matches('"').trim_matches('\'');
            match key {
                "NAME" => name = Some(unquoted.to_string()),
                "ID_LIKE" => id_like = Some(unquoted.to_string()),
                _ => {}
            }
        }
    }

    let os = name.unwrap_or_default().to_ascii_lowercase();
    let like = id_like.unwrap_or_default().to_ascii_lowercase();

    let distro_id = if os.contains("ubuntu") || like.contains("ubuntu") {
        "ubuntu"
    } else if os.contains("debian") || like.contains("debian") {
        "debian"
    } else if os.contains("fedora") || like.contains("fedora") {
        "fedora"
    } else if os.contains("alma") || like.contains("alma") {
        "almalinux"
    } else if os.contains("alpine") || like.contains("alpine") {
        "alpine"
    } else if os.contains("arch") || like.contains("arch") {
        "arch"
    } else {
        return Err(anyhow::anyhow!(
            "Could not determine distro_id from /etc/os-release (NAME={}, ID_LIKE={})",
            os,
            like
        ));
    };

    tracing::debug!(distro_id = %distro_id, "Detected distro_id for repo-config fetch");
    Ok(distro_id.to_string())
}

/// Payload sent to `POST /api/v1/enroll`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequest {
    pub machine_id: String,
    pub fqdn: String,
    pub ip_address: String,
    pub os_details: serde_json::Value,
    /// Short hostname (from /etc/hostname or hostname command).
    /// Used by the manager to populate `display_name` on approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
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
#[allow(clippy::large_enum_variant)]
pub enum EnrollmentStatusResponse {
    Pending,
    Approved {
        ca_crt: String,
        #[serde(default)]
        ca_chain: String,
        server_crt: String,
        server_key: String,
        #[serde(default)]
        crl_pem: String,
        /// Optional package repository configuration from the manager.
        /// Absent when the manager doesn't support manager-hosted repos yet.
        #[serde(default)]
        repo_config: Option<RepoConfig>,
    },
    Denied,
    NotFound,
}

/// Package repository configuration delivered via the enrollment bundle.
///
/// The manager includes this in the `Approved` response so the agent can
/// configure its local package manager to pull updates from the manager-hosted
/// repo instead of GitHub Releases. The GPG public key is trusted because it
/// was delivered inside the mTLS-authenticated enrollment bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    /// ASCII-armored GPG public key for verifying repo metadata and package signatures.
    pub gpg_public_key: String,
    /// Distro-specific repository configuration text.
    /// For apt: contents of /etc/apt/sources.list.d/lpa.list
    /// For dnf: contents of /etc/yum.repos.d/lpa.repo
    /// For apk: repository URL line for /etc/apk/repositories
    /// For pacman: pacman.conf include file content
    pub sources_config: String,
    /// Distro identifier (e.g., "ubuntu", "debian", "fedora", "alpine", "arch")
    pub distro_id: String,
    /// Target path where the GPG key should be written (e.g., /etc/apt/keyrings/lpa-repo.gpg)
    pub keyring_path: String,
}

/// PEM-encoded PKI bundle extracted from an `Approved` status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkiBundle {
    pub ca_crt: String,
    pub ca_chain: String,
    pub server_crt: String,
    pub server_key: String,
    pub crl_pem: String,
    /// Optional package repository configuration from the manager.
    /// Present when the manager supports manager-hosted repo provisioning.
    /// Absent for older enrollment responses (agent fetches via fallback).
    #[serde(default)]
    pub repo_config: Option<RepoConfig>,
}

impl From<EnrollmentStatusResponse> for Option<PkiBundle> {
    fn from(response: EnrollmentStatusResponse) -> Self {
        match response {
            EnrollmentStatusResponse::Approved {
                ca_crt,
                ca_chain,
                server_crt,
                server_key,
                crl_pem,
                repo_config,
            } => Some(PkiBundle {
                ca_crt,
                ca_chain,
                server_crt,
                server_key,
                crl_pem,
                repo_config,
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
    /// Network interface whose IP is reported to the manager (overrides auto-detect).
    report_interface: Option<String>,
    /// Explicit IPv4 address reported to the manager (highest priority override).
    report_ip: Option<String>,
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
        Self::with_ip_overrides(manager_url, None, None)
    }

    /// Create a new enrollment client with optional IP reporting overrides.
    ///
    /// See [`identity::get_primary_ip`] for resolution priority:
    /// 1. `report_ip` — explicit IP (highest priority)
    /// 2. `report_interface` — IP from named interface
    /// 3. Route-based — IP from kernel routing table for reaching the manager
    /// 4. Auto-detect — first routable IP (container bridge subnets filtered)
    pub fn with_ip_overrides(
        manager_url: &str,
        report_interface: Option<String>,
        report_ip: Option<String>,
    ) -> Self {
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
            report_interface,
            report_ip,
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
        // 1. Resolve manager IP for route-based IP selection
        let route_target = self.manager_ip().await.ok();

        // 2. Collect identity data
        let machine_id = identity::get_machine_id()
            .context("Failed to read machine-id — host cannot enroll without identity")?;
        let fqdn = identity::get_fqdn()
            .context("Failed to determine FQDN — check hostname configuration")?;
        let ip_address = identity::get_primary_ip(
            self.report_interface.as_deref(),
            self.report_ip.as_deref(),
            route_target.as_deref(),
        )
        .context("Failed to determine reportable IP address — check network configuration or set report_interface/report_ip in config")?;
        let os_details = identity::get_os_details()
            .context("Failed to collect OS details — /etc/os-release may be missing")?;

        // 2. Collect short hostname for display_name on manager
        let hostname = identity::get_hostname()
            .map_err(|e| tracing::warn!(error = %e, "Failed to determine hostname — display_name will use FQDN fallback"))
            .ok();

        // 3. Build EnrollmentRequest struct
        let request = EnrollmentRequest {
            machine_id,
            fqdn,
            ip_address,
            os_details,
            hostname,
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
            409 => {
                // Host already exists - log warning and return special response
                // The caller should skip to polling phase with existing token
                tracing::warn!(
                    "Host already registered with manager (HTTP 409) — will attempt to resume polling"
                );
                Err(anyhow!("ENROLLMENT_CONFLICT: Host already exists"))
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
                    ca_chain,
                    server_crt,
                    server_key,
                    crl_pem,
                    repo_config,
                } => {
                    tracing::info!(
                        elapsed_seconds = start.elapsed().as_secs(),
                        attempts = attempt,
                        "Enrollment approved — received PKI bundle from manager"
                    );
                    return Ok(PkiBundle {
                        ca_crt,
                        ca_chain,
                        server_crt,
                        server_key,
                        crl_pem,
                        repo_config,
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

    /// Fetch repo configuration from the manager's fallback endpoint.
    ///
    /// Called when the enrollment bundle did not include `repo_config` (older
    /// manager that doesn't embed it in the Approved response). Performs
    /// `GET /api/v1/pki/repo-config?distro_id=<distro_id>` on the manager URL
    /// and deserializes the response body into a [`RepoConfig`].
    ///
    /// The `distro_id` query parameter is required by the manager to generate
    /// the correct distro-specific sources config. It is detected from
    /// `/etc/os-release` (the `ID` field, falling back to `ID_LIKE`).
    ///
    /// # Returns
    /// - `Ok(RepoConfig)` on HTTP 200 with a valid JSON body
    /// - `Err` on network error, non-200 status, or malformed JSON
    pub async fn fetch_repo_config(&self) -> Result<RepoConfig> {
        let distro_id = detect_distro_id_for_repo_config()?;

        let url = format!(
            "{}/api/v1/pki/repo-config?distro_id={}",
            self.manager_url, distro_id
        );
        tracing::info!(url = %url, distro_id = %distro_id, "Fetching repo config from manager fallback endpoint");

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .context("Network error — failed to reach repo-config endpoint")?;

        match response.status().as_u16() {
            200 => {
                let body = response
                    .text()
                    .await
                    .context("Failed to read repo-config response body")?;
                let config: RepoConfig = serde_json::from_str(&body)
                    .context("Invalid repo-config response — malformed JSON from manager")?;
                tracing::info!("Repo config fetched successfully from fallback endpoint");
                Ok(config)
            }
            status if status >= 500 => {
                let body = response.text().await.ok();
                Err(anyhow!(
                    "Server error fetching repo-config (HTTP {}) — {}",
                    status,
                    body.as_deref().unwrap_or("no details")
                ))
            }
            other => {
                let body = response.text().await.ok();
                Err(anyhow!(
                    "Unexpected HTTP {} fetching repo-config — {}",
                    other,
                    body.as_deref().unwrap_or("no details")
                ))
            }
        }
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
            hostname: Some("node".into()),
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
            ca_chain: String::new(),
            server_crt: "crt".into(),
            server_key: "key".into(),
            crl_pem: String::new(),
            repo_config: None,
        };
        let bundle: Option<PkiBundle> = status.into();
        assert!(bundle.is_some());
        let bundle = bundle.unwrap();
        assert_eq!(bundle.ca_crt, "ca");
        assert!(bundle.repo_config.is_none());
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
