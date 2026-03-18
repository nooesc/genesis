use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use super::{ExecResult, SandboxBackend, SandboxConfig, SandboxError, SandboxInstance};

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn mb_to_gib(mb: u32) -> u32 {
    mb.div_ceil(1024)
}

fn cap_disk_gib(gib: u32) -> u32 {
    gib.min(10) // Daytona caps at 10 GB
}

// ---------------------------------------------------------------------------
// Serde types for Daytona HTTP API
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CreateSandboxRequest {
    image: String,
    labels: HashMap<String, String>,
    auto_stop_interval: u32,
    resources: DaytonaResources,
}

#[derive(Debug, Serialize)]
struct DaytonaResources {
    cpu: u32,
    memory: u32, // GiB
    disk: u32,   // GiB
}

#[derive(Debug, Serialize)]
struct DaytonaExecRequest {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DaytonaExecResponse {
    result: String,
    exit_code: i32,
}

#[derive(Debug, Deserialize)]
struct DaytonaSandboxResponse {
    id: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct DaytonaSandboxListResponse {
    #[serde(default)]
    items: Vec<DaytonaSandboxResponse>,
}

// ---------------------------------------------------------------------------
// Backend struct
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct DaytonaSandbox {
    client: reqwest::Client,
    base_url: String,
}

impl DaytonaSandbox {
    pub fn new() -> Result<Self, SandboxError> {
        let api_key = std::env::var("DAYTONA_API_KEY").map_err(|_| SandboxError::AuthError {
            reason: "DAYTONA_API_KEY env var not set".into(),
        })?;

        let base_url = std::env::var("DAYTONA_API_URL")
            .unwrap_or_else(|_| "https://app.daytona.io/api".into());

        let mut default_headers = HeaderMap::new();
        let auth_value = HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|e| {
            SandboxError::AuthError {
                reason: e.to_string(),
            }
        })?;
        default_headers.insert(header::AUTHORIZATION, auth_value);

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .connect_timeout(Duration::from_secs(10))
            // No global read timeout — the execute endpoint can run commands
            // for minutes; per-request timeout is sent server-side via
            // DaytonaExecRequest::timeout.
            .build()
            .map_err(SandboxError::HttpError)?;

        Ok(Self { client, base_url })
    }

    /// Map an HTTP status to a `SandboxError`, falling back to a generic
    /// message that includes the status code.
    async fn map_error_response(&self, resp: reqwest::Response) -> SandboxError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return SandboxError::AuthError {
                reason: format!("HTTP {status}: {body}"),
            };
        }

        if status.is_server_error() {
            return SandboxError::SubprocessFailed {
                reason: format!("Daytona API returned HTTP {status}: {body}"),
            };
        }

        SandboxError::SubprocessFailed {
            reason: format!("Unexpected HTTP {status}: {body}"),
        }
    }
}

// ---------------------------------------------------------------------------
// SandboxBackend impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SandboxBackend for DaytonaSandbox {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, SandboxError> {
        let task_id = &config.task_id;

        // ------------------------------------------------------------------
        // 1. Try to find an existing sandbox by label
        // ------------------------------------------------------------------
        let list_url = format!("{}/sandbox", self.base_url);
        let list_resp = self
            .client
            .get(&list_url)
            .query(&[("labels", format!("genesis_task_id={task_id}"))])
            .send()
            .await?;

        if list_resp.status().is_success() {
            let list: DaytonaSandboxListResponse = list_resp.json().await?;
            // Reuse an existing sandbox — prefer running, then stopped/archived
            if let Some(existing) = list
                .items
                .iter()
                .find(|s| s.state == "running")
                .or_else(|| {
                    list.items
                        .iter()
                        .find(|s| s.state == "stopped" || s.state == "archived")
                })
            {
                // Only start if not already running
                if existing.state != "running" {
                    let start_url = format!("{}/sandbox/{}/start", self.base_url, existing.id);
                    let start_resp = self.client.post(&start_url).send().await?;
                    if !start_resp.status().is_success() {
                        return Err(self.map_error_response(start_resp).await);
                    }
                }

                let now = SystemTime::now();
                return Ok(SandboxInstance {
                    id: existing.id.clone(),
                    backend_type: self.backend_type().to_owned(),
                    task_id: task_id.clone(),
                    snapshot_data: None,
                    persistent: config.persistent,
                    created_at: now,
                    last_active: now,
                    cache_instant: std::time::Instant::now(),
                });
            }
        }

        // ------------------------------------------------------------------
        // 2. No reusable sandbox found — create a fresh one
        // ------------------------------------------------------------------
        let memory_gib = mb_to_gib(config.memory_mb);
        let disk_gib = cap_disk_gib(mb_to_gib(config.disk_mb));
        let cpu = config.cpu.ceil() as u32;

        let mut labels = HashMap::new();
        labels.insert("genesis_task_id".to_owned(), task_id.clone());

        let body = CreateSandboxRequest {
            image: config.image.clone(),
            labels,
            auto_stop_interval: 0,
            resources: DaytonaResources {
                cpu,
                memory: memory_gib,
                disk: disk_gib,
            },
        };

        let create_url = format!("{}/sandbox", self.base_url);
        let resp = self.client.post(&create_url).json(&body).send().await?;

        if !resp.status().is_success() {
            return Err(self.map_error_response(resp).await);
        }

        let created: DaytonaSandboxResponse = resp.json().await?;
        let now = SystemTime::now();

        Ok(SandboxInstance {
            id: created.id,
            backend_type: self.backend_type().to_owned(),
            task_id: task_id.clone(),
            snapshot_data: None,
            persistent: config.persistent,
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
        let url = format!("{}/toolbox/{}/process/execute", self.base_url, instance.id);

        let req_body = DaytonaExecRequest {
            command: command.to_owned(),
            cwd: working_dir.map(|s| s.to_owned()),
            timeout: timeout.map(|d| d.as_secs()),
        };

        let resp = self.client.post(&url).json(&req_body).send().await?;

        if !resp.status().is_success() {
            return Err(self.map_error_response(resp).await);
        }

        let exec_resp: DaytonaExecResponse = resp.json().await?;
        Ok(ExecResult {
            output: exec_resp.result,
            exit_code: exec_resp.exit_code,
        })
    }

    async fn snapshot(&self, _instance: &SandboxInstance) -> Result<Option<String>, SandboxError> {
        // Daytona manages state server-side; no explicit snapshot needed.
        Ok(None)
    }

    async fn cleanup(
        &self,
        instance: &SandboxInstance,
        persistent: bool,
    ) -> Result<(), SandboxError> {
        if persistent {
            let stop_url = format!("{}/sandbox/{}/stop", self.base_url, instance.id);
            let resp = self.client.post(&stop_url).send().await?;
            if !resp.status().is_success() {
                return Err(self.map_error_response(resp).await);
            }
        } else {
            let delete_url = format!("{}/sandbox/{}", self.base_url, instance.id);
            let resp = self.client.delete(&delete_url).send().await?;
            if !resp.status().is_success() {
                return Err(self.map_error_response(resp).await);
            }
        }
        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "daytona"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mb_to_gib_ceiling_conversion() {
        assert_eq!(mb_to_gib(5120), 5);
        assert_eq!(mb_to_gib(5121), 6);
        assert_eq!(mb_to_gib(1024), 1);
        assert_eq!(mb_to_gib(512), 1);
        assert_eq!(mb_to_gib(0), 0);
    }

    #[test]
    fn disk_capped_at_10gb() {
        assert_eq!(cap_disk_gib(mb_to_gib(51200)), 10);
        assert_eq!(cap_disk_gib(mb_to_gib(10240)), 10);
        assert_eq!(cap_disk_gib(mb_to_gib(5120)), 5);
    }

    #[test]
    fn create_request_body_serializes() {
        let body = CreateSandboxRequest {
            image: "nikolaik/python-nodejs:python3.11-nodejs20".to_owned(),
            labels: [("genesis_task_id".to_owned(), "session-1".to_owned())]
                .into_iter()
                .collect(),
            auto_stop_interval: 0,
            resources: DaytonaResources {
                cpu: 1,
                memory: 5,
                disk: 10,
            },
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["resources"]["memory"], 5);
        assert_eq!(json["auto_stop_interval"], 0);
    }

    #[test]
    fn exec_response_parses() {
        let json = r#"{"result":"hello","exit_code":0}"#;
        let resp: DaytonaExecResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result, "hello");
        assert_eq!(resp.exit_code, 0);
    }

    #[test]
    fn missing_api_key_returns_auth_error() {
        // Temporarily ensure the env var is unset
        let original = std::env::var("DAYTONA_API_KEY").ok();
        std::env::remove_var("DAYTONA_API_KEY");
        let result = DaytonaSandbox::new();
        // Restore
        if let Some(val) = original {
            std::env::set_var("DAYTONA_API_KEY", val);
        }
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            super::super::SandboxError::AuthError { .. }
        ));
    }

    #[test]
    fn sandbox_response_parses() {
        let json = r#"{"id":"sb-1","state":"stopped"}"#;
        let resp: DaytonaSandboxResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.state, "stopped");
        assert_eq!(resp.id, "sb-1");
    }
}
