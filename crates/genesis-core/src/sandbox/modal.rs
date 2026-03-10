use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use super::{
    BackendSpecific, ExecResult, SandboxBackend, SandboxConfig, SandboxError, SandboxInstance,
};

const MODAL_SIDECAR_SCRIPT: &str = include_str!("../../../../scripts/modal_sandbox.py");

// ---------------------------------------------------------------------------
// Sidecar response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateResponse {
    pub sandbox_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExecResponse {
    pub output: String,
    pub exit_code: i32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotResponse {
    pub image_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModalSnapshotData {
    pub sandbox_id: String,
    pub image_id: Option<String>,
}

// ---------------------------------------------------------------------------
// ModalSandbox
// ---------------------------------------------------------------------------

pub struct ModalSandbox {
    data_dir: PathBuf,
}

impl ModalSandbox {
    pub fn new(data_dir: &str) -> Result<Self, SandboxError> {
        // Check that `uv` is available on PATH.
        let uv_check = std::process::Command::new("uv")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if uv_check.is_err() || !uv_check.unwrap().success() {
            return Err(SandboxError::PrerequisiteMissing(
                "uv not found — install via: curl -LsSf https://astral.sh/uv/install.sh | sh"
                    .to_owned(),
            ));
        }

        // Warn if Modal auth is not configured (don't hard-fail — the user may
        // configure auth later or rely on env vars set at runtime).
        let has_env_auth = std::env::var("MODAL_TOKEN_ID").is_ok()
            && std::env::var("MODAL_TOKEN_SECRET").is_ok();
        let has_toml_auth = dirs::home_dir()
            .map(|h| h.join(".modal.toml").exists())
            .unwrap_or(false);
        if !has_env_auth && !has_toml_auth {
            tracing::warn!("Modal authentication not configured");
        }

        Ok(Self {
            data_dir: PathBuf::from(data_dir),
        })
    }

    /// Write the embedded sidecar script to `{data_dir}/modal_sandbox.py` if
    /// it doesn't exist or its content differs from the embedded version.
    async fn ensure_script(&self) -> Result<PathBuf, SandboxError> {
        let script_path = self.data_dir.join("modal_sandbox.py");

        let needs_write = if script_path.exists() {
            tokio::fs::read_to_string(&script_path)
                .await
                .map(|existing| existing != MODAL_SIDECAR_SCRIPT)
                .unwrap_or(true)
        } else {
            true
        };

        if needs_write {
            tokio::fs::create_dir_all(&self.data_dir).await.map_err(|e| {
                SandboxError::Other(format!("failed to create data dir: {e}"))
            })?;
            tokio::fs::write(&script_path, MODAL_SIDECAR_SCRIPT).await.map_err(|e| {
                SandboxError::Other(format!("failed to write sidecar script: {e}"))
            })?;
        }

        Ok(script_path)
    }

    /// Invoke the Python sidecar for a given `command`, passing `args` as JSON
    /// on stdin. Returns the parsed JSON stdout on success.
    async fn call_sidecar(
        &self,
        command: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, SandboxError> {
        let script_path = self.ensure_script().await?;
        let script_path_str = script_path.to_string_lossy().into_owned();

        let mut child = tokio::process::Command::new("uv")
            .args(["run", &script_path_str, command])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| SandboxError::Other(format!("failed to spawn uv: {e}")))?;

        // Write args JSON to stdin.
        if let Some(mut stdin) = child.stdin.take() {
            let payload = args.to_string();
            stdin
                .write_all(payload.as_bytes())
                .await
                .map_err(|e| SandboxError::Other(format!("failed to write stdin: {e}")))?;
            // stdin is dropped here, closing the pipe.
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| SandboxError::Other(format!("failed to wait for sidecar: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Try to parse structured error first.
            let reason = if let Ok(err) =
                serde_json::from_str::<ErrorResponse>(stderr.trim())
            {
                err.error
            } else {
                stderr.trim().to_owned()
            };
            return Err(SandboxError::SubprocessFailed { reason });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: serde_json::Value = serde_json::from_str(stdout.trim())?;
        Ok(value)
    }
}

// ---------------------------------------------------------------------------
// SandboxBackend impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SandboxBackend for ModalSandbox {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, SandboxError> {
        // Extract Modal-specific options from BackendSpecific.
        let (gpu, snapshot_id) = match &config.backend_specific {
            BackendSpecific::Modal { gpu, app: _ } => (gpu.clone(), None::<String>),
            _ => (None, None),
        };

        let mut args = serde_json::json!({
            "image": config.image,
            "cpu": config.cpu,
            "memory_mb": config.memory_mb,
            "disk_mb": config.disk_mb,
        });

        if let Some(sid) = snapshot_id {
            args["snapshot_id"] = serde_json::Value::String(sid);
        }
        if let Some(g) = gpu {
            args["gpu"] = serde_json::Value::String(g);
        }

        let response = self.call_sidecar("create", &args).await?;
        let parsed: CreateResponse = serde_json::from_value(response)?;

        let now = SystemTime::now();
        Ok(SandboxInstance {
            id: parsed.sandbox_id.clone(),
            backend_type: self.backend_type().to_owned(),
            task_id: config.task_id.clone(),
            snapshot_data: None,
            created_at: now,
            last_active: now,
            cache_instant: std::time::Instant::now(),
        })
    }

    async fn execute(
        &self,
        instance: &SandboxInstance,
        command: &str,
        working_dir: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<ExecResult, SandboxError> {
        let timeout_secs = timeout
            .map(|d| d.as_secs())
            .unwrap_or(120);

        let args = serde_json::json!({
            "sandbox_id": instance.id,
            "command": command,
            "cwd": working_dir.unwrap_or("/root"),
            "timeout": timeout_secs,
        });

        let response = self.call_sidecar("exec", &args).await?;
        let parsed: ExecResponse = serde_json::from_value(response)?;

        Ok(ExecResult {
            output: parsed.output,
            exit_code: parsed.exit_code,
        })
    }

    async fn snapshot(&self, instance: &SandboxInstance) -> Result<Option<String>, SandboxError> {
        let args = serde_json::json!({ "sandbox_id": instance.id });
        let response = self.call_sidecar("snapshot", &args).await?;
        let parsed: SnapshotResponse = serde_json::from_value(response)?;

        let data = ModalSnapshotData {
            sandbox_id: instance.id.clone(),
            image_id: Some(parsed.image_id),
        };
        let json = serde_json::to_string(&data)?;
        Ok(Some(json))
    }

    async fn cleanup(
        &self,
        instance: &SandboxInstance,
        persistent: bool,
    ) -> Result<(), SandboxError> {
        // If persistent, snapshot before terminating so state can be resumed.
        if persistent {
            match self.snapshot(instance).await {
                Ok(Some(_)) => debug!(id = %instance.id, "modal snapshot saved before cleanup"),
                Ok(None) => debug!(id = %instance.id, "no modal snapshot data before cleanup"),
                Err(e) => warn!(id = %instance.id, error = %e, "modal snapshot failed before cleanup"),
            }
        }

        let args = serde_json::json!({ "sandbox_id": instance.id });
        self.call_sidecar("terminate", &args).await?;
        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "modal"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_create_response() {
        let json = r#"{"sandbox_id":"sb-abc123"}"#;
        let resp: CreateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.sandbox_id, "sb-abc123");
    }

    #[test]
    fn parse_exec_response() {
        let json = r#"{"output":"hello world","exit_code":0}"#;
        let resp: ExecResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.output, "hello world");
        assert_eq!(resp.exit_code, 0);
    }

    #[test]
    fn parse_snapshot_response() {
        let json = r#"{"image_id":"img-xyz789"}"#;
        let resp: SnapshotResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.image_id, "img-xyz789");
    }

    #[test]
    fn parse_error_response() {
        let json = r#"{"error":"Modal auth failed"}"#;
        let resp: ErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error, "Modal auth failed");
    }

    #[test]
    fn snapshot_data_round_trips() {
        let data = ModalSnapshotData {
            sandbox_id: "sb-1".to_owned(),
            image_id: Some("img-1".to_owned()),
        };
        let json = serde_json::to_string(&data).unwrap();
        let parsed: ModalSnapshotData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sandbox_id, "sb-1");
        assert_eq!(parsed.image_id.as_deref(), Some("img-1"));
    }

    #[test]
    fn sidecar_script_is_embedded() {
        assert!(!MODAL_SIDECAR_SCRIPT.is_empty());
        assert!(MODAL_SIDECAR_SCRIPT.contains("modal"));
    }
}
