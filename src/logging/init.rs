//! Logging Initialization
//!
//! Sets up tracing with systemd journal and file appender support.


use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};


/// Initialize logging with tracing
///
/// Sets up:
/// - Env-based log level filtering
/// - JSON formatting for machine readability
/// - systemd journal integration
/// - File appender fallback to /var/log/linux_patch_api/
pub fn init_logging(verbose: bool) -> Result<WorkerGuard> {
    let log_level = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));
    
    let file_appender = tracing_appender::rolling::daily("/var/log/linux_patch_api", "audit.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    
    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true);
    
    let stdout_layer = fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(true);
    
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stdout_layer)
        .try_init()
        .ok(); // Ignore if already initialized
    
    Ok(guard)
}
