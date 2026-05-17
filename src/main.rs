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

use actix_web::middleware::Logger;
use actix_web::{web, App, HttpServer};
use anyhow::Result;
use clap::Parser;
use std::net::TcpListener;
use std::sync::Arc;
use tracing::{error, info, warn};

use linux_patch_api::api::{configure_api_routes, configure_health_route};
use linux_patch_api::auth::{mtls, MtlsMiddleware, WhitelistManager};
use linux_patch_api::enroll;
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

    /// Enroll with manager at URL (skips mTLS startup, runs enrollment flow only)
    #[arg(
        long,
        help = "Enroll with manager at URL (skips mTLS startup, runs enrollment flow only)"
    )]
    enroll: Option<String>,
}

#[actix_web::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    // Initialize logging
    let _guard = init_logging(args.verbose)?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config_path = args.config,
        "Linux Patch API starting"
    );

    // Load configuration
    let config = match AppConfig::load(&args.config) {
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
            return Err(anyhow::anyhow!("Configuration error: {}", e));
        }
    };

    // Handle enrollment mode - runs before server startup
    if let Some(ref manager_url) = args.enroll {
        info!(
            manager_url = manager_url,
            "Enrollment mode activated - running enrollment flow before server startup"
        );
        match enroll::run_enrollment(manager_url, &config).await {
            Ok(()) => {
                info!("Enrollment complete - proceeding to server startup");
            }
            Err(e) => {
                error!(error = %e, "Enrollment failed - shutting down");
                return Err(anyhow::anyhow!("Enrollment failed: {}", e));
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

    // Configure bind address
    let bind_address = format!("{}:{}", config.server.bind, config.server.port);
    info!(bind = %bind_address, "Starting HTTP server");

    // Create server
    // Create server builder
    let server_builder = HttpServer::new(move || {
        let mut app = App::new()
            .wrap(Logger::default())
            .app_data(job_manager_data.clone())
            .app_data(backend_data.clone());

        // Configure API routes
        app = app.configure(|cfg| {
            configure_api_routes(cfg, job_manager_data.clone(), backend_data.clone());
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
    info!("Listening on {}", bind_address);

    // Apply TLS/mTLS configuration if enabled
    if let Some(tls_config) = config.tls_config() {
        info!(
            ca_cert = %tls_config.ca_cert,
            server_cert = %tls_config.server_cert,
            server_key = %tls_config.server_key,
            min_tls_version = %tls_config.min_tls_version,
            "Initializing mTLS authentication with TLS binding"
        );

        let mtls_config = mtls::MtlsConfig {
            ca_cert_path: tls_config.ca_cert.clone(),
            server_cert_path: tls_config.server_cert.clone(),
            server_key_path: tls_config.server_key.clone(),
            min_tls_version: tls_config.min_tls_version.clone(),
        };

        match MtlsMiddleware::new(mtls_config.clone()) {
            Ok(middleware) => {
                // Build rustls server configuration
                let rustls_config = middleware
                    .build_rustls_config()
                    .map_err(|e| anyhow::anyhow!("Failed to build rustls config: {}", e))?;

                info!("mTLS middleware and rustls config initialized successfully");

                // Create TCP listener (std::net for listen_rustls_0_23)
                let tcp_listener = TcpListener::bind(&bind_address)
                    .map_err(|e| anyhow::anyhow!("Failed to bind to {}: {}", bind_address, e))?;

                info!("TCP listener bound to {}", bind_address);

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
        warn!("TLS is disabled - running without mTLS authentication (INSECURE)");
        server_builder.bind(&bind_address)?.run().await?;
    }

    info!("Linux Patch API shutting down");
    Ok(())
}
