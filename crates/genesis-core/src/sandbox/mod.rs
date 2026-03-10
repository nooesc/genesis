pub mod manager;
pub mod singularity;
pub mod modal;
pub mod daytona;

use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("sandbox backend binary not found (tried: {tried:?})")]
    BinaryNotFound { tried: Vec<&'static str> },

    #[error("prerequisite missing: {0}")]
    PrerequisiteMissing(String),

    #[error("subprocess failed: {reason}")]
    SubprocessFailed { reason: String },

    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("sandbox timed out after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("storage error: {0}")]
    StorageError(String),

    #[error("sandbox not found: {id}")]
    NotFound { id: String },

    #[error("authentication failed: {reason}")]
    AuthError { reason: String },

    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct SandboxInstance {
    pub id: String,
    pub backend_type: String,
    pub task_id: String,
    pub snapshot_data: Option<String>,
    /// Whether the sandbox should be preserved (stopped, not deleted) on cleanup.
    pub persistent: bool,
    pub created_at: SystemTime,
    pub last_active: SystemTime,
    /// For idle timeout tracking in cache only — not persisted.
    pub cache_instant: Instant,
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub output: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub task_id: String,
    pub image: String,
    pub cpu: f32,
    pub memory_mb: u32,
    pub disk_mb: u32,
    pub persistent: bool,
    pub working_dir: Option<String>,
    /// Snapshot data from a previous session, used to resume a sandbox.
    pub snapshot_data: Option<String>,
    pub backend_specific: BackendSpecific,
}

#[derive(Debug, Clone)]
pub enum BackendSpecific {
    Singularity { bind: Option<Vec<String>> },
    Modal { gpu: Option<String>, app: Option<String> },
    Daytona { target: Option<String>, api_url: Option<String> },
}

#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Create or resume a sandbox. Returns a SandboxInstance for tracking.
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, SandboxError>;

    /// Execute a command in the sandbox.
    async fn execute(
        &self,
        instance: &SandboxInstance,
        command: &str,
        working_dir: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<ExecResult, SandboxError>;

    /// Snapshot/persist the sandbox state. Returns optional snapshot data JSON.
    async fn snapshot(&self, instance: &SandboxInstance) -> Result<Option<String>, SandboxError>;

    /// Stop and clean up the sandbox.
    async fn cleanup(&self, instance: &SandboxInstance, persistent: bool) -> Result<(), SandboxError>;

    /// Backend name for logging/storage.
    fn backend_type(&self) -> &'static str;
}
