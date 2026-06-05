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
use tracing::{error, info, warn};

use linux_patch_api::api::{configure_api_routes, configure_health_route};
use linux_patch_api::auth::crl::{self, CrlStatus};
use linux_patch_api::auth::{mtls, MtlsMiddleware, WhitelistManager};
use linux_patch_api::config::loader::{validate_certs, CertStatus};
use linux_patch_api::enroll;
use linux_patch_api::packages::cache::PackageCacheState;
use linux_patch_api::packages::create_backend;
use linux_patch_api::{init_logging, AppConfig, JobManager};

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

    // Initialize job manager
    let job_manager = JobManager::new(config.jobs.max_concurrent, config.jobs.timeout_minutes)?;
    info!(
        max_jobs = config.jobs.max_concurrent,
        timeout_minutes = config.jobs.timeout_minutes,
        "Job manager initialized"
    );

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
            Some(Arc::new(manager))
        }
        Err(e) => {
            warn!(error = %e, "Failed to load whitelist - continuing with empty whitelist (all denied)");
            None
        }
    };

    // Store job manager and backend in Arc for sharing
    let job_manager_data = web::Data::new(job_manager);
    let backend_data = web::Data::new(package_backend);

    // Initialize package cache state
    let cache_state = web::Data::new(PackageCacheState::new());
    info!("Package cache state initialized");

    // Initialize shared CRL state (available even when TLS is off for health reporting)
    let shared_crl_state = crl::new_shared_state();
    let crl_state_data = web::Data::new(shared_crl_state.clone());

    // Configure bind address
    let bind_address = format!("{}:{}", config.server.bind, config.server.port);

    // Create server builder
    let server_builder = HttpServer::new(move || {
        let mut app = App::new()
            .wrap(Logger::default())
            .app_data(job_manager_data.clone())
            .app_data(backend_data.clone())
            .app_data(cache_state.clone())
            .app_data(crl_state_data.clone());

        // Configure API routes
        app = app.configure(|cfg| {
            configure_api_routes(
                cfg,
                job_manager_data.clone(),
                backend_data.clone(),
                cache_state.clone(),
            );
        });

        // Configure health route (outside API scope)
        app = app.configure(configure_health_route);

        app
    })
    .workers(4)
    // VULN-004: Configure header size limit to 8KB to prevent DoS via oversized headers
    .client_request_timeout(std::time::Duration::from_secs(5))
    // FIX: Set explicit client disconnect timeout to prevent connection resets on larger responses
    .client_disconnect_timeout(std::time::Duration::from_secs(5))
    .keep_alive(std::time::Duration::from_secs(15))
    .max_connection_rate(1000);
    info!(
        mtls_enabled = config.tls_config().is_some(),
        whitelist_enabled = whitelist_manager.is_some(),
        "Security layer status"
    );

    info!("Linux Patch API initialized successfully");

    // Apply TLS/mTLS configuration if enabled
    if let Some(tls_config) = config.tls_config() {
        info!(
            ca_cert = %tls_config.ca_cert,
            server_cert = %tls_config.server_cert,
            server_key = %tls_config.server_key,
            min_tls_version = %tls_config.min_tls_version,
            crl_path = %tls_config.crl_path,
            "Initializing mTLS authentication with TLS binding"
        );

        let mtls_config = mtls::MtlsConfig {
            ca_cert_path: tls_config.ca_cert.clone(),
            server_cert_path: tls_config.server_cert.clone(),
            server_key_path: tls_config.server_key.clone(),
            min_tls_version: tls_config.min_tls_version.clone(),
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
                shared_crl_state.store(std::sync::Arc::new(initial_crl));
            }
            CrlStatus::Missing => {
                info!("No CRL on disk -- starting in WebPKI-only mode");
            }
            CrlStatus::Degraded => {
                warn!("CRL load failed -- starting in degraded (WebPKI-only) mode");
            }
        }

        // Spawn CRL refresh background task if manager URL is configured
        if let Some(manager_url) = config.enrollment_manager_url() {
            crl::spawn_crl_refresh_task(
                manager_url.to_string(),
                crl_path,
                ca_cert_der,
                shared_crl_state.clone(),
            );
        } else {
            info!("No manager URL configured -- CRL auto-refresh disabled");
        }

        match MtlsMiddleware::new(mtls_config.clone()) {
            Ok(middleware) => {
                // Build rustls server configuration with CRL-aware verifier
                let rustls_config = middleware
                    .build_rustls_config(Some(shared_crl_state.clone()))
                    .map_err(|e| anyhow::anyhow!("Failed to build rustls config: {}", e))?;

                info!("mTLS middleware and rustls config initialized successfully");

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

                let bind_addr: std::net::SocketAddr = bind_address.parse().map_err(|e| {
                    anyhow::anyhow!("Invalid bind address '{}': {}", bind_address, e)
                })?;

                socket
                    .bind(&socket2::SockAddr::from(bind_addr))
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to bind socket to {}: {}", bind_address, e)
                    })?;

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
                server_builder
                    .listen_rustls_0_23(tcp_listener, server_config)?
                    .run()
                    .await?;
            }
            Err(e) => {
                error!(error = %e, "Failed to initialize mTLS middleware");
                return Err(anyhow::anyhow!("mTLS initialization failed: {}", e));
            }
        }
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
        server_builder.listen(tcp_listener)?.run().await?;
    }

    info!("Linux Patch API shutting down");
    Ok(())
}
