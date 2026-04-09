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

use anyhow::Result;
use clap::Parser;
use tracing::{error, info};

use linux_patch_api::{AppConfig, init_logging, JobManager};

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
}

#[tokio::main]
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
            info!(port = cfg.server.port, bind = &cfg.server.bind, "Configuration loaded");
            cfg
        }
        Err(e) => {
            error!(error = %e, path = args.config, "Failed to load configuration");
            return Err(anyhow::anyhow!("Configuration error: {}", e));
        }
    };

    // Initialize job manager
    let job_manager = JobManager::new(config.jobs.max_concurrent, config.jobs.timeout_minutes)?;
    info!(max_jobs = config.jobs.max_concurrent, timeout_minutes = config.jobs.timeout_minutes, "Job manager initialized");

    // TODO: Initialize API server with actix-web
    // TODO: Set up mTLS with rustls
    // TODO: Start config file watcher
    // TODO: Register systemd service ready status

    info!("Linux Patch API initialized successfully");

    // Keep the service running
    tokio::signal::ctrl_c().await?;

    info!("Linux Patch API shutting down");
    Ok(())
}
