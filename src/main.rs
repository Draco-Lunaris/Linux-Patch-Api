//! Linux Patch API - Main Entry Point
//!
//! Secure remote package management API for Linux systems.
//!
//! # Configuration
//!
//! Configuration is loaded from `/etc/linux_patch_api/config.yaml` by default.
//! Use `--config` flag to specify a custom configuration path.
//!
//! # Security
//!
//! - mTLS authentication required on port 12443
//! - IP whitelist enforced (deny by default)
//! - Detailed audit logging
//!
//! # Exit Codes
//!
//! - 0: Clean exit (no certs + no enrollment URL, or --enroll/--renew-certs success)
//! - 1: Error (config error, enrollment network failure, cert validation error)
//! - 2: Certs invalid, auto-enrollment in progress (triggers systemd restart with backoff)

use actix_web::middleware::Logger;
use actix_web::{web, App, HttpServer};
use anyhow::Result;
use clap::Parser;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use linux_patch_api::api::{configure_api_routes, configure_health_route};
use linux_patch_api::auth::crl::{self, CrlStatus};
use linux_patch_api::auth::{
    mtls, SecurityHeadersMiddleware, WhitelistManager, WhitelistMiddleware,
};
use linux_patch_api::config::loader::{validate_certs, CertStatus};
use linux_patch_api::enroll;
use linux_patch_api::jobs::scheduler::Scheduler;
use linux_patch_api::packages::cache::PackageCacheState;
use linux_patch_api::packages::create_backend;
use linux_patch_api::{init_logging, AppConfig};

/// Linux Patch API CLI arguments
#[derive(Parser, Debug)]
#[command(name = "linux-patch-api")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Secure remote package management API for Linux systems")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "/etc/linux_patch_api/config.yaml")]
    config: String,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Enroll with manager at URL (skips mTLS startup, runs enrollment flow only, then exits)
    #[arg(
        long,
        help = "Enroll with manager at URL (skips mTLS startup, runs enrollment flow only, then exits)"
    )]
    enroll: Option<String>,

    /// Validate existing certs and re-enroll if expiring within threshold or invalid
    #[arg(
        long,
        help = "Validate existing certs and re-enroll if expiring within threshold or invalid, then exits"
    )]
    renew_certs: bool,
}

/// Exit codes for the daemon
enum ExitCode {
    /// Clean exit: no certs + no enrollment URL, or --enroll/--renew-certs success
    Clean = 0,
    /// Error: config error, enrollment network failure, cert validation error
    Error = 1,
    /// Certs invalid, auto-enrollment in progress (triggers systemd restart with backoff)
    EnrollmentInProgress = 2,
}

/// Normalize a version string for comparison by stripping a leading `v` prefix
/// and any Debian revision suffix (e.g. `-1` in `2.3.0-1`).
#[actix_web::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    // Initialize logging
    let _guard = init_logging(args.verbose)?;

    // Install rustls crypto provider (required for mTLS and HTTPS clients)
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider (aws-lc-rs)");

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config_path = args.config,
        "Linux Patch API starting"
    );

    // Load configuration (skip TLS validation during enrollment mode)
    let skip_tls_validation = args.enroll.is_some();
    let mut config = match AppConfig::load(&args.config, skip_tls_validation) {
        Ok(cfg) => {
            info!(
                port = cfg.server.port,
                bind = &cfg.server.bind,
                "Configuration loaded"
            );
            cfg
        }
        Err(e) => {
            error!(error = %e, path = args.config, "Failed to load configuration");
            std::process::exit(ExitCode::Error as i32);
        }
    };

    // Handle --renew-certs flag: validate certs and re-enroll if needed
    if args.renew_certs {
        info!("Certificate renewal mode activated - validating existing certificates");
        match validate_certs(&config) {
            Ok(CertStatus::Valid) => {
                info!("Certificates are valid and not expiring soon. No renewal needed.");
                std::process::exit(ExitCode::Clean as i32);
            }
            Ok(CertStatus::ExpiringSoon { not_after }) => {
                info!(
                    not_after = %not_after,
                    "Certificates expiring soon - starting re-enrollment"
                );
            }
            Ok(status) => {
                info!(
                    status = %status,
                    "Certificates are {} - starting re-enrollment",
                    status
                );
            }
            Err(e) => {
                error!(error = %e, "Certificate validation failed");
                std::process::exit(ExitCode::Error as i32);
            }
        }

        // Need enrollment URL to re-enroll
        let manager_url = match config.enrollment_manager_url() {
            Some(url) => url.to_string(),
            None => {
                error!(
                    "Cannot re-enroll: enrollment.manager_url not configured. \
                     Add the manager URL to config.yaml or use --enroll <url>"
                );
                std::process::exit(ExitCode::Error as i32);
            }
        };

        match enroll::run_enrollment(&manager_url, &mut config, &args.config).await {
            Ok(()) => {
                info!(
                    "Certificate renewal complete. Start service: systemctl start linux-patch-api"
                );
                std::process::exit(ExitCode::Clean as i32);
            }
            Err(e) => {
                error!(error = %e, "Certificate renewal failed");
                std::process::exit(ExitCode::Error as i32);
            }
        }
    }

    // Handle --enroll flag: run enrollment flow then EXIT
    if let Some(ref manager_url) = args.enroll {
        info!(
            manager_url = manager_url,
            "Enrollment mode activated - running enrollment flow"
        );
        match enroll::run_enrollment(manager_url, &mut config, &args.config).await {
            Ok(()) => {
                info!("Enrollment complete. Start service: systemctl start linux-patch-api");
                std::process::exit(ExitCode::Clean as i32);
            }
            Err(e) => {
                error!(error = %e, "Enrollment failed");
                std::process::exit(ExitCode::Error as i32);
            }
        }
    }

    // Auto-enrollment on startup: validate certs before starting server
    if config.tls_config().is_some() {
        match validate_certs(&config) {
            Ok(CertStatus::Valid) => {
                info!("TLS certificates validated successfully");
            }
            Ok(CertStatus::ExpiringSoon { not_after }) => {
                warn!(
                    not_after = %not_after,
                    "Certificates expiring soon - starting normally, consider re-enrollment"
                );
                // TODO: Schedule background re-enrollment in future phase
            }
            Ok(status @ CertStatus::Missing { .. })
            | Ok(status @ CertStatus::Corrupt { .. })
            | Ok(status @ CertStatus::Expired { .. })
            | Ok(status @ CertStatus::KeyMismatch)
            | Ok(status @ CertStatus::Untrusted) => {
                // Certs are invalid - check if we can auto-enroll
                // Clone the manager URL before mutable borrow of config
                let manager_url_opt = config.enrollment_manager_url().map(|s| s.to_string());
                match manager_url_opt {
                    Some(manager_url) => {
                        info!(
                            status = %status,
                            manager_url = manager_url,
                            "Certs {}. Auto-enrolling with {}",
                            status,
                            manager_url
                        );
                        match enroll::run_enrollment(&manager_url, &mut config, &args.config).await
                        {
                            Ok(()) => {
                                info!("Auto-enrollment complete - continuing to server startup");
                                // Re-load config to pick up any changes from enrollment
                                config = AppConfig::load(&args.config, false)?;
                            }
                            Err(e) => {
                                error!(
                                    error = %e,
                                    "Auto-enrollment failed - will retry on next restart"
                                );
                                std::process::exit(ExitCode::EnrollmentInProgress as i32);
                            }
                        }
                    }
                    None => {
                        // No enrollment URL configured - exit cleanly to avoid crash loop
                        error!(
                            status = %status,
                            "Certs {}. No enrollment URL configured. \
                             To fix this, either:\n\
                             1. Add enrollment.manager_url to config.yaml and restart\n\
                             2. Run: linux-patch-api --enroll <manager_url>\n\
                             3. Place certificates manually in the configured paths",
                            status
                        );
                        std::process::exit(ExitCode::Clean as i32);
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "Certificate validation error");
                std::process::exit(ExitCode::Error as i32);
            }
        }
    }

    // Initialize the unified scheduler — the single authoritative control
    // point for all operation admission and lifecycle decisions.
    // One shared Arc<Scheduler> is injected into web::Data and used by
    // all handlers, the SIGTERM handler, and the startup/recovery path.
    let scheduler = Scheduler::new(config.jobs.max_concurrent, config.jobs.max_queue_depth);
    info!(
        max_jobs = config.jobs.max_concurrent,
        timeout_minutes = config.jobs.timeout_minutes,
        max_queue_depth = config.jobs.max_queue_depth,
        "Unified scheduler initialized"
    );

    // Recover orphaned jobs from a previous boot.
    // If the agent restarted (e.g. after auto-reboot from patching, or the
    // postinst restart during a self-update), any jobs that were running are
    // lost from memory. Load them from disk and give each a terminal status
    // so the manager sees the outcome. A self-update orphan means the upgrade
    // succeeded (the restart proves it); anything else is marked failed.
    let orphaned_jobs = linux_patch_api::jobs::persistence::load_orphaned_jobs().await;
    if !orphaned_jobs.is_empty() {
        info!(
            count = orphaned_jobs.len(),
            "Recovered orphaned jobs from previous boot — resolving terminal status"
        );
        scheduler.recover_orphaned_jobs(&orphaned_jobs).await;
    } else {
        debug!("No orphaned jobs found from previous boot");
    }

    // Initialize package manager backend
    let package_backend = match create_backend() {
        Ok(backend) => {
            info!("Package manager backend initialized");
            backend
        }
        Err(e) => {
            error!(error = %e, "Failed to initialize package manager backend");
            return Err(anyhow::anyhow!("Package backend error: {}", e));
        }
    };

    // Initialize shared repo-config sync state (updated by the background
    // reconciliation task, read by the health endpoint). Created early so
    // the startup self-heal and periodic task can both reference it.
    let shared_repo_sync_state = enroll::new_shared_sync_state();
    let repo_sync_state_data = web::Data::new(shared_repo_sync_state.clone());

    // Startup repo-config self-heal: ensure the manager-hosted package repo
    // is configured so self-update can actually find a newer package. This
    // catches hosts that were enrolled before repo_config was added to the
    // enrollment bundle, or where the repo files were lost. Best-effort —
    // failures are logged but do not block startup.
    if let Some(manager_url) = config.enrollment_manager_url() {
        match enroll::check_and_provision_repo_config(manager_url).await {
            Ok(enroll::RepoHealResult::AlreadyConfigured) => {
                info!("Repo config already present at startup");
            }
            Ok(enroll::RepoHealResult::Provisioned) => {
                info!("Repo config provisioned via startup self-heal");
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Repo config self-heal failed at startup — self-update may be a no-op until repo is configured"
                );
            }
        }
    } else {
        info!("No manager URL configured — skipping repo config self-heal at startup");
    }

    // Spawn the periodic repo-config reconciliation task (6h interval).
    // This mirrors the CRL refresh task: it keeps the agent's sources.list
    // and GPG keyring in sync with manager-side changes (suite renames,
    // URL changes, GPG key rotation) without requiring re-enrollment.
    // See issue #170.
    if let Some(manager_url) = config.enrollment_manager_url() {
        enroll::spawn_repo_config_sync_task(
            manager_url.to_string(),
            shared_repo_sync_state.clone(),
        );
    } else {
        info!("No manager URL configured — repo config periodic sync disabled");
    }

    // Initialize IP whitelist manager
    let whitelist_path = config.whitelist_path();
    info!(
        path = whitelist_path,
        "Initializing IP whitelist enforcement"
    );

    let whitelist_manager = match WhitelistManager::new(whitelist_path) {
        Ok(manager) => {
            info!(
                entries = manager.entry_count(),
                "Whitelist manager initialized"
            );
            Arc::new(manager)
        }
        Err(e) => {
            // Fail-closed: deny all IPs when whitelist cannot be loaded
            warn!(error = %e, "Failed to load whitelist - using deny-all mode (fail-closed)");
            Arc::new(WhitelistManager::new_deny_all())
        }
    };

    // Store scheduler and backend in web::Data for sharing
    let scheduler_data = web::Data::new(scheduler.clone());
    let backend_data = web::Data::new(package_backend);

    // Initialize package cache state with configured stale threshold
    let cache_state = web::Data::new(PackageCacheState::with_threshold(
        config.cache.stale_threshold_secs,
    ));
    info!(
        stale_threshold_secs = config.cache.stale_threshold_secs,
        "Package cache state initialized"
    );

    // Initialize shared CRL state (available even when TLS is off for health reporting)
    let shared_crl_state = crl::new_shared_state();
    let crl_state_data = web::Data::new(shared_crl_state.clone());

    // Clone the scheduler for the SIGTERM handler — it needs to check if a
    // package mutation is in progress and wait for it to complete before
    // allowing the server to shut down. The scheduler is the authoritative
    // source for this check, working across ALL backends (not just APT).
    let sigterm_scheduler = scheduler.clone();

    // Configure bind address
    let bind_address = format!("{}:{}", config.server.bind, config.server.port);

    // Clone whitelist manager for use inside the HttpServer closure
    let wl = whitelist_manager.clone();

    // Clone rate limit config for use inside the HttpServer closure
    let rate_limit_config = config.rate_limit.clone();

    // Create server builder
    // Security middleware stack (order matters):
    //   1. WhitelistMiddleware   — IP-based access control (deny-by-default)
    //   2. SecurityHeadersMiddleware — VULN-006: reject duplicate critical headers
    //   3. RateLimitMiddleware   — per-IP rate limiting (read + destructive tiers)
    //   4. Logger                — request logging (after auth decisions)
    let server_builder = HttpServer::new(move || {
        App::new()
            .wrap(WhitelistMiddleware::new(wl.clone()))
            .wrap(SecurityHeadersMiddleware::new())
            .wrap(linux_patch_api::api::rate_limit::RateLimitMiddleware::new(
                rate_limit_config.clone(),
            ))
            .wrap(Logger::default())
            .app_data(scheduler_data.clone())
            .app_data(backend_data.clone())
            .app_data(cache_state.clone())
            .app_data(crl_state_data.clone())
            .app_data(repo_sync_state_data.clone())
            .configure(|cfg| {
                configure_api_routes(
                    cfg,
                    scheduler_data.clone(),
                    backend_data.clone(),
                    cache_state.clone(),
                );
            })
            .configure(configure_health_route)
    })
    .workers(4)
    // VULN-004: Configure header size limit to 8KB to prevent DoS via oversized headers
    .client_request_timeout(std::time::Duration::from_secs(5))
    // FIX: Set explicit client disconnect timeout to prevent connection resets on larger responses
    .client_disconnect_timeout(std::time::Duration::from_secs(5))
    // Graceful shutdown timeout: how long Actix waits for in-flight requests
    // to complete after receiving the stop signal. Must be less than
    // TimeoutStopSec (120s) minus the package-mutation drain window (100s) =
    // 20s margin. We use 10s for Actix, leaving 10s safety margin.
    .shutdown_timeout(10)
    // Disable Actix's built-in signal handling — we install our own
    // SIGTERM handler (setup_sigterm_handler) that drains package
    // mutations before stopping the server. Without disable_signals(),
    // Actix would install a competing SIGTERM handler that stops the
    // server immediately without waiting for mutations to complete.
    .disable_signals()
    .keep_alive(std::time::Duration::from_secs(15))
    .max_connection_rate(1000);
    info!(
        mtls_enabled = config.tls_config().is_some(),
        whitelist_entries = whitelist_manager.entry_count(),
        "Security layer status (IP whitelist enforced)"
    );

    info!("Linux Patch API initialized successfully");

    // Apply TLS/mTLS configuration if enabled
    if let Some(tls_config) = config.tls_config() {
        info!(
            ca_cert = %tls_config.ca_cert,
            server_cert = %tls_config.server_cert,
            server_key = %tls_config.server_key,
            crl_path = %tls_config.crl_path,
            "Initializing mTLS authentication with TLS 1.3 binding"
        );

        // TLS 1.3 is the only supported version — hardcoded in build_rustls_config()
        let mtls_config = mtls::MtlsConfig {
            ca_cert_path: tls_config.ca_cert.clone(),
            server_cert_path: tls_config.server_cert.clone(),
            server_key_path: tls_config.server_key.clone(),
        };

        // Load CRL from disk into the shared CRL state
        let crl_path = std::path::PathBuf::from(&tls_config.crl_path);
        let ca_cert_der = std::fs::read(&tls_config.ca_cert).unwrap_or_default();

        // Load initial CRL from disk (missing is OK -- degraded mode)
        let initial_crl = crl::load_crl(&crl_path, &ca_cert_der);
        match initial_crl.status {
            CrlStatus::Invalid => {
                error!("CRL signature is invalid -- refusing to start (fail-closed)");
                std::process::exit(ExitCode::Error as i32);
            }
            CrlStatus::Valid | CrlStatus::Expired => {
                info!(
                    status = %initial_crl.status,
                    revoked = initial_crl.revoked_serials.len(),
                    "CRL loaded from disk"
                );
                let was_expired = initial_crl.status == CrlStatus::Expired;
                shared_crl_state.store(std::sync::Arc::new(initial_crl));

                // If CRL is expired, attempt immediate refresh from manager
                if was_expired {
                    if let Some(manager_url) = config.enrollment_manager_url() {
                        info!("CRL is expired -- attempting immediate refresh from manager");
                        match crl::refresh_crl(
                            manager_url,
                            &crl_path,
                            &ca_cert_der,
                            &shared_crl_state,
                        )
                        .await
                        {
                            Ok(()) => info!("Expired CRL refreshed from manager on startup"),
                            Err(e) => warn!(
                                error = %e,
                                "Failed to refresh expired CRL from manager on startup"
                            ),
                        }
                    }
                }
            }
            CrlStatus::Missing => {
                info!("No CRL on disk -- attempting immediate fetch from manager");
                if let Some(manager_url) = config.enrollment_manager_url() {
                    match crl::refresh_crl(manager_url, &crl_path, &ca_cert_der, &shared_crl_state)
                        .await
                    {
                        Ok(()) => {
                            info!("CRL fetched from manager on startup");
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                "CRL fetch from manager failed on startup -- starting in WebPKI-only mode"
                            );
                        }
                    }
                } else {
                    info!("No manager URL configured -- starting in WebPKI-only mode");
                }
            }
            CrlStatus::Degraded => {
                warn!("CRL load failed -- starting in degraded (WebPKI-only) mode");
            }
        }

        // Spawn CRL refresh background task if manager URL is configured
        if let Some(manager_url) = config.enrollment_manager_url() {
            crl::spawn_crl_refresh_task(
                manager_url.to_string(),
                crl_path.clone(),
                ca_cert_der.clone(),
                shared_crl_state.clone(),
            );
        } else {
            info!("No manager URL configured -- CRL auto-refresh disabled");
        }

        // Spawn periodic CRL health re-evaluation (hourly disk reload)
        crl::spawn_crl_health_task(
            crl_path.clone(),
            ca_cert_der.clone(),
            shared_crl_state.clone(),
        );

        // ADR: rustls is the authoritative client-auth gate.
        // Client certificate verification happens at the TLS handshake level
        // via CrlAwareVerifier (which wraps WebPkiClientVerifier). No
        // application-layer certificate validation middleware is needed.
        // See src/auth/mtls.rs for the full ADR.
        let rustls_config = mtls::build_rustls_config(&mtls_config, Some(shared_crl_state.clone()))
            .map_err(|e| anyhow::anyhow!("Failed to build rustls config: {}", e))?;

        info!(
            "mTLS rustls config initialized successfully (client auth enforced at TLS handshake)"
        );

        // Create TCP listener with SO_REUSEADDR using socket2
        // This prevents "Address already in use" errors when restarting after a crash
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create socket: {}", e))?;

        socket
            .set_reuse_address(true)
            .map_err(|e| anyhow::anyhow!("Failed to set SO_REUSEADDR: {}", e))?;

        let bind_addr: std::net::SocketAddr = bind_address
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", bind_address, e))?;

        socket
            .bind(&socket2::SockAddr::from(bind_addr))
            .map_err(|e| anyhow::anyhow!("Failed to bind socket to {}: {}", bind_address, e))?;

        socket
            .listen(128)
            .map_err(|e| anyhow::anyhow!("Failed to listen on socket: {}", e))?;

        let tcp_listener: std::net::TcpListener = socket.into();

        // Log listening AFTER successful bind
        info!("Listening on {} (mTLS enabled)", bind_address);

        // Clone the ServerConfig from Arc for listen_rustls_0_23
        let server_config = (*rustls_config).clone();

        info!("Binding server with TLS 1.3 - non-TLS connections will be rejected");

        // Bind with TLS using rustls 0.23 - non-TLS connections fail at handshake
        let server = server_builder
            .listen_rustls_0_23(tcp_listener, server_config)?
            .run();

        // Spawn SIGTERM handler that waits for in-progress package operations
        // to complete before stopping the server. This prevents SIGKILL from
        // killing apt-get/dnf/apk/pacman mid-transaction.
        let server_handle = server.handle();
        tokio::spawn(async move {
            setup_sigterm_handler(server_handle, sigterm_scheduler.clone()).await;
        });

        // Finalize the self-update restart AFTER the server is constructed
        // Normal startup — send READY=1 to systemd
        notify_systemd_ready();

        server.await?;
    } else {
        // Create TCP listener with SO_REUSEADDR for non-TLS mode
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create socket: {}", e))?;

        socket
            .set_reuse_address(true)
            .map_err(|e| anyhow::anyhow!("Failed to set SO_REUSEADDR: {}", e))?;

        let bind_addr: std::net::SocketAddr = bind_address
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", bind_address, e))?;

        socket
            .bind(&socket2::SockAddr::from(bind_addr))
            .map_err(|e| anyhow::anyhow!("Failed to bind socket to {}: {}", bind_address, e))?;

        socket
            .listen(128)
            .map_err(|e| anyhow::anyhow!("Failed to listen on socket: {}", e))?;

        let tcp_listener: std::net::TcpListener = socket.into();

        // Log listening AFTER successful bind
        info!("Listening on {} (no TLS)", bind_address);

        warn!("TLS is disabled - running without mTLS authentication (INSECURE)");
        let server = server_builder.listen(tcp_listener)?.run();

        // Spawn SIGTERM handler (same as TLS path)
        let server_handle = server.handle();
        tokio::spawn(async move {
            setup_sigterm_handler(server_handle, sigterm_scheduler.clone()).await;
        });

        // Normal startup — send READY=1 to systemd
        notify_systemd_ready();

        server.await?;
    }

    info!("Linux Patch API shutting down");
    Ok(())
}

/// Send a notification message to systemd via the `NOTIFY_SOCKET`
/// environment variable. This is a minimal implementation of
/// `sd_notify` that uses `std::os::unix::net::UnixDatagram` — no
/// external library dependency required (the `systemd` crate pulls
/// in `libelogind` on musl, which doesn't ship a static library).
///
/// If `NOTIFY_SOCKET` is not set (not running under systemd), this
/// is a no-op.
fn sd_notify(message: &str) {
    use std::os::unix::net::UnixDatagram;

    let socket_path = match std::env::var_os("NOTIFY_SOCKET") {
        Some(path) => path,
        None => return,
    };

    let socket_path = socket_path.to_string_lossy().into_owned();
    let result = UnixDatagram::unbound()
        .and_then(|s| s.send_to(message.as_bytes(), std::path::Path::new(&socket_path)));

    if let Err(e) = result {
        tracing::debug!(error = ?e, "sd_notify failed");
    }
}

/// Send READY=1 to systemd's notification socket (for Type=notify services).
///
/// If running under systemd (detected via `/run/systemd/system`), a failure
/// to send READY=1 is treated as a fatal startup error — the service is
/// `Type=notify`, so systemd will kill us if we never notify. We exit with
/// an error code rather than silently continuing.
///
/// If NOT running under systemd (no `/run/systemd/system`), this is a no-op
/// — the notification socket doesn't exist and there's nothing to notify.
fn notify_systemd_ready() {
    let running_under_systemd = std::path::Path::new("/run/systemd/system").exists();
    sd_notify("READY=1");
    if running_under_systemd {
        info!("Notified systemd: READY=1");
    }
}

/// Send STOPPING=1 to systemd's notification socket.
fn notify_systemd_stopping() {
    sd_notify("STOPPING=1");
    info!("Notified systemd: STOPPING=1");
}

/// SIGTERM handler that protects in-progress package operations.
///
/// Package-manager processes (apt-get/dnf/apk/pacman and their children —
/// dpkg, rpm, postinst hooks) are spawned in their own process group via
/// `process_group(0)` in the coordinator. When the init system stops the
/// agent (SIGTERM → SIGKILL after `TimeoutStopSec`), the cgroup kill only
/// affects the agent process — the package-manager transaction continues
/// in its own process group and completes independently.
///
/// This handler:
/// 1. Catches SIGTERM
/// 2. Freezes scheduler admission so no new jobs/mutations start
/// 3. If a mutation is in progress: exits immediately. The package-manager
///    transaction is protected by process-group isolation and will complete
///    on its own. Waiting for it to "complete" would deadlock whenever the
///    SIGTERM came from the agent's own postinst restart (systemctl restart
///    --no-block): the mutation waits for dpkg → dpkg waits for postinst →
///    postinst waits for systemctl restart → systemd waits for the agent
///    to exit. The same deadlock applies to any patch_apply job that
///    upgrades linux-patch-api via dist-upgrade, not just explicit
///    self-update jobs.
/// 4. Stops the HTTP server gracefully (stops accepting new connections,
///    drains in-flight requests).
async fn setup_sigterm_handler(
    server_handle: actix_web::dev::ServerHandle,
    scheduler: Arc<Scheduler>,
) {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to install SIGTERM handler — package operations may be killed mid-transaction on shutdown");
            return;
        }
    };

    // Wait for SIGTERM
    sigterm.recv().await;
    info!("Received SIGTERM — initiating graceful shutdown");

    // Notify systemd that we're stopping
    notify_systemd_stopping();

    // Freeze scheduler admission so no new mutations or jobs start while
    // we drain in-flight operations.
    scheduler.freeze_admission().await;

    // If a package mutation is in progress, exit immediately. The
    // package-manager process is in its own process group and will
    // complete the transaction independently of the agent process.
    //
    // We must NOT wait for the mutation to "complete" because:
    //   - If the SIGTERM came from the agent's own postinst restart
    //     (systemctl restart --no-block), the mutation's stdout/stderr
    //     pipes are held open by the postinst process, which is waiting
    //     for the agent to exit — creating a deadlock:
    //       agent waits for mutation → mutation waits for pipes to close
    //       → pipes held by postinst → postinst waits for agent to exit
    //       → agent waits for mutation (deadlock)
    //   - This applies to ANY job that upgrades linux-patch-api (explicit
    //     self-update OR patch_apply via dist-upgrade), not just
    //     SelfUpdate operations.
    //   - Even for an external `systemctl stop`, waiting is futile: the
    //     process-group isolation protects the transaction regardless.
    //
    // The old 100s wait was a pre-isolation safety measure that is now
    // obsolete and actively harmful — it guarantees the deadlock whenever
    // the agent's own package is being upgraded.
    if scheduler.is_mutation_in_progress().await {
        info!(
            "Package mutation in progress — exiting immediately (transaction protected by process-group isolation)"
        );
        let _ = server_handle.stop(true).await;
        return;
    }

    info!("No package mutation in progress — shutting down immediately");
    let _ = server_handle.stop(true).await;
}
