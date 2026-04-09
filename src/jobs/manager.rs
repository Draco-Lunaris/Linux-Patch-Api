//! Job Manager - Async job queue management
//!
//! Manages async job execution with concurrency limits and timeout enforcement.

use anyhow::Result;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Job status
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

/// Job information
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub status: JobStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Job Manager - handles async job queue with limits
pub struct JobManager {
    max_concurrent: usize,
    timeout_minutes: u64,
    jobs: RwLock<Vec<Job>>,
}

impl JobManager {
    /// Create a new job manager
    pub fn new(max_concurrent: usize, timeout_minutes: u64) -> Result<Self> {
        Ok(Self {
            max_concurrent,
            timeout_minutes,
            jobs: RwLock::new(Vec::new()),
        })
    }

    /// Get the timeout duration
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_minutes * 60)
    }

    /// Get max concurrent jobs
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }
}
