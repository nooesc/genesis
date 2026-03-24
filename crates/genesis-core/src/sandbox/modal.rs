use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

use genesis_modal_proto::modal::client::modal_client_client::ModalClientClient;
use genesis_modal_proto::modal::client::{
    AuthTokenGetRequest, GpuConfig, Resources, Sandbox, SandboxCreateRequest,
    SandboxGetTaskIdRequest, SandboxSnapshotFsRequest, SandboxTerminateRequest,
    TaskGetCommandRouterAccessRequest,
};
use genesis_modal_proto::modal::task_command_router::task_command_router_client::TaskCommandRouterClient;
use genesis_modal_proto::modal::task_command_router::{
    TaskExecStartRequest, TaskExecStderrConfig, TaskExecStdioFileDescriptor,
    TaskExecStdioReadRequest, TaskExecStdoutConfig, TaskExecWaitRequest,
};

use super::{BackendSpecific, ExecResult, SandboxBackend, SandboxConfig, SandboxError, SandboxInstance};

const MODAL_API_URL: &str = "https://api.modal.com";

// ---------------------------------------------------------------------------
// Snapshot data — serialized into SandboxInstance.snapshot_data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModalSnapshotData {
    pub sandbox_id: String,
    pub image_id: Option<String>,
}

// ===========================================================================
// Task 2: Auth module — credential resolution + JWT utilities
// ===========================================================================

/// Parse `~/.modal.toml` content, extracting token_id and token_secret from
/// the `[default]` section.
fn parse_modal_toml(content: &str) -> Result<(String, String), SandboxError> {
    let table: toml::Table = content
        .parse()
        .map_err(|e| SandboxError::AuthError {
            reason: format!("failed to parse .modal.toml: {e}"),
        })?;

    let section = table
        .get("default")
        .and_then(|v| v.as_table())
        .ok_or_else(|| SandboxError::AuthError {
            reason: "[default] section not found in .modal.toml".to_owned(),
        })?;

    let token_id = section
        .get("token_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SandboxError::AuthError {
            reason: "token_id not found in [default] section of .modal.toml".to_owned(),
        })?
        .to_owned();

    let token_secret = section
        .get("token_secret")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SandboxError::AuthError {
            reason: "token_secret not found in [default] section of .modal.toml".to_owned(),
        })?
        .to_owned();

    Ok((token_id, token_secret))
}

/// Resolve Modal credentials: env vars first, then `~/.modal.toml`.
pub(crate) fn resolve_credentials() -> Result<(String, String), SandboxError> {
    // Try environment variables first.
    let env_id = std::env::var("MODAL_TOKEN_ID").ok();
    let env_secret = std::env::var("MODAL_TOKEN_SECRET").ok();

    if let (Some(id), Some(secret)) = (env_id, env_secret) {
        if !id.is_empty() && !secret.is_empty() {
            return Ok((id, secret));
        }
    }

    // Fall back to ~/.modal.toml
    let home = dirs::home_dir().ok_or_else(|| SandboxError::AuthError {
        reason: "could not determine home directory".to_owned(),
    })?;
    let toml_path = home.join(".modal.toml");
    let content = std::fs::read_to_string(&toml_path).map_err(|e| SandboxError::AuthError {
        reason: format!("failed to read {}: {e}", toml_path.display()),
    })?;

    parse_modal_toml(&content)
}

/// Decode the `exp` claim from a JWT without verifying the signature.
/// Returns `None` if the token is malformed or the `exp` claim is absent.
fn decode_jwt_expiry(token: &str) -> Option<i64> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    value.get("exp")?.as_i64()
}

// ===========================================================================
// Task 3: Auth token manager and gRPC interceptor
// ===========================================================================

#[derive(Debug, Clone)]
struct TokenAndExpiry {
    token: String,
    expires_at: i64,
}

type SharedTokenCache = Arc<RwLock<Option<TokenAndExpiry>>>;

/// Manages the Modal auth token lifecycle: caching, near-expiry refresh, and
/// blocking refresh when expired.
struct AuthTokenManager {
    cache: SharedTokenCache,
    refresh_mutex: tokio::sync::Mutex<()>,
    token_id: String,
    token_secret: String,
}

impl AuthTokenManager {
    fn new(cache: SharedTokenCache, token_id: String, token_secret: String) -> Self {
        Self {
            cache,
            refresh_mutex: tokio::sync::Mutex::new(()),
            token_id,
            token_secret,
        }
    }

    /// Get a valid auth token. If the cached token is still valid, return it.
    /// If near-expiry, try a non-blocking refresh. If expired, block on refresh.
    async fn get_token(
        &self,
        channel: &Channel,
    ) -> Result<String, SandboxError> {
        let now = chrono::Utc::now().timestamp();

        // Fast path: read the cache.
        {
            let guard = self.cache.read().await;
            if let Some(ref cached) = *guard {
                let remaining = cached.expires_at - now;
                if remaining > 60 {
                    // Valid with plenty of margin.
                    return Ok(cached.token.clone());
                }
                if remaining > 0 {
                    // Near-expiry: try a non-blocking background refresh.
                    if let Ok(_lock) = self.refresh_mutex.try_lock() {
                        let cache = self.cache.clone();
                        let channel = channel.clone();
                        let token_id = self.token_id.clone();
                        let token_secret = self.token_secret.clone();
                        tokio::spawn(async move {
                            let _ = Self::do_refresh(&cache, &channel, &token_id, &token_secret).await;
                        });
                    }
                    return Ok(cached.token.clone());
                }
                // Expired: fall through to blocking refresh.
            }
            // No cached token: fall through to blocking refresh.
        }

        // Blocking refresh.
        let _lock = self.refresh_mutex.lock().await;

        // Double-check after acquiring lock — another task may have refreshed.
        {
            let guard = self.cache.read().await;
            if let Some(ref cached) = *guard {
                if cached.expires_at - now > 0 {
                    return Ok(cached.token.clone());
                }
            }
        }

        Self::do_refresh(&self.cache, channel, &self.token_id, &self.token_secret).await
    }

    async fn do_refresh(
        cache: &SharedTokenCache,
        channel: &Channel,
        token_id: &str,
        token_secret: &str,
    ) -> Result<String, SandboxError> {
        // Build a temporary interceptor with static creds only (no auth token yet).
        let interceptor = StaticCredsInterceptor {
            token_id: token_id.to_owned(),
            token_secret: token_secret.to_owned(),
        };

        let mut client = ModalClientClient::with_interceptor(channel.clone(), interceptor);
        let resp = client
            .auth_token_get(AuthTokenGetRequest {})
            .await
            .map_err(|s| map_grpc_error("auth_token_get", s))?;

        let token = resp.into_inner().token;
        let expires_at = decode_jwt_expiry(&token).unwrap_or_else(|| {
            // Default to 10 minutes from now if we can't decode.
            chrono::Utc::now().timestamp() + 600
        });

        let entry = TokenAndExpiry {
            token: token.clone(),
            expires_at,
        };

        {
            let mut guard = cache.write().await;
            *guard = Some(entry);
        }

        Ok(token)
    }
}

/// Minimal interceptor used only during token refresh (before we have an auth token).
#[derive(Clone)]
struct StaticCredsInterceptor {
    token_id: String,
    token_secret: String,
}

impl tonic::service::Interceptor for StaticCredsInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        let md = request.metadata_mut();
        md.insert("x-modal-token-id", self.token_id.parse().unwrap());
        md.insert("x-modal-token-secret", self.token_secret.parse().unwrap());
        md.insert(
            "x-modal-client-type",
            "CLIENT_TYPE_GENESIS".parse().unwrap(),
        );
        md.insert("x-modal-client-version", "1.0.0".parse().unwrap());
        Ok(request)
    }
}

/// Interceptor for the main ModalClient channel. Injects static creds and
/// the cached auth token on every request (best-effort: if the cache is locked
/// or empty, the request proceeds without the auth token).
#[derive(Clone)]
struct ModalAuthInterceptor {
    token_id: String,
    token_secret: String,
    cache: SharedTokenCache,
}

impl tonic::service::Interceptor for ModalAuthInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        let md = request.metadata_mut();
        md.insert("x-modal-token-id", self.token_id.parse().unwrap());
        md.insert("x-modal-token-secret", self.token_secret.parse().unwrap());
        md.insert(
            "x-modal-client-type",
            "CLIENT_TYPE_GENESIS".parse().unwrap(),
        );
        md.insert("x-modal-client-version", "1.0.0".parse().unwrap());

        // Best-effort injection of the auth token.
        if let Ok(guard) = self.cache.try_read() {
            if let Some(ref cached) = *guard {
                if let Ok(val) = cached.token.parse() {
                    md.insert("x-modal-auth-token", val);
                }
            }
        }

        Ok(request)
    }
}

/// Interceptor for the TaskCommandRouter channel — injects the router JWT.
#[derive(Clone)]
struct RouterJwtInterceptor {
    jwt: String,
}

impl tonic::service::Interceptor for RouterJwtInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        let md = request.metadata_mut();
        if let Ok(val) = format!("Bearer {}", self.jwt).parse() {
            md.insert("authorization", val);
        }
        Ok(request)
    }
}

/// Map a gRPC status to the appropriate `SandboxError` variant.
fn map_grpc_error(operation: &str, status: tonic::Status) -> SandboxError {
    match status.code() {
        tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => SandboxError::AuthError {
            reason: format!("{operation}: {status}"),
        },
        tonic::Code::NotFound => SandboxError::NotFound {
            id: format!("{operation}: {status}"),
        },
        tonic::Code::DeadlineExceeded => SandboxError::Timeout {
            seconds: 0, // actual duration unknown from gRPC status alone
        },
        tonic::Code::ResourceExhausted => SandboxError::ResourceLimit {
            reason: format!("{operation}: {status}"),
        },
        _ => SandboxError::Other(format!("{operation}: {status}")),
    }
}

// ===========================================================================
// Task 4: ModalSandbox struct and SYNC constructor
// ===========================================================================

type RouterClient =
    TaskCommandRouterClient<InterceptedService<Channel, RouterJwtInterceptor>>;

pub struct ModalSandbox {
    client: ModalClientClient<InterceptedService<Channel, ModalAuthInterceptor>>,
    raw_channel: Channel,
    auth_manager: Arc<AuthTokenManager>,
    /// Maps genesis sandbox_id -> Modal's internal task_id.
    modal_task_ids: tokio::sync::Mutex<HashMap<String, String>>,
    /// Maps modal_task_id -> router client.
    routers: tokio::sync::Mutex<HashMap<String, RouterClient>>,
    app_name: Option<String>,
}

impl std::fmt::Debug for ModalSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModalSandbox")
            .field("app_name", &self.app_name)
            .finish_non_exhaustive()
    }
}

impl ModalSandbox {
    /// Create a new ModalSandbox. This is a **synchronous** constructor —
    /// uses `connect_lazy()` so it can be called from non-async contexts
    /// (e.g., inside `OnceLock::get_or_init()`).
    pub fn new(
        token_id: String,
        token_secret: String,
        app_name: Option<String>,
    ) -> Result<Self, SandboxError> {
        if token_id.is_empty() || token_secret.is_empty() {
            return Err(SandboxError::AuthError {
                reason: "Modal token_id and token_secret must not be empty".to_owned(),
            });
        }

        let endpoint = Endpoint::from_static(MODAL_API_URL)
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(30));

        // connect_lazy() is synchronous — no .await needed.
        let channel = endpoint.connect_lazy();

        // Shared token cache: same Arc instance for both the manager and the interceptor.
        let cache: SharedTokenCache = Arc::new(RwLock::new(None));

        let auth_interceptor = ModalAuthInterceptor {
            token_id: token_id.clone(),
            token_secret: token_secret.clone(),
            cache: cache.clone(),
        };

        let auth_manager = Arc::new(AuthTokenManager::new(
            cache,
            token_id,
            token_secret,
        ));

        let client = ModalClientClient::with_interceptor(channel.clone(), auth_interceptor);

        Ok(Self {
            client,
            raw_channel: channel,
            auth_manager,
            modal_task_ids: tokio::sync::Mutex::new(HashMap::new()),
            routers: tokio::sync::Mutex::new(HashMap::new()),
            app_name,
        })
    }
}

// ===========================================================================
// Task 5: SandboxBackend — create, snapshot, cleanup
// ===========================================================================

#[async_trait]
impl SandboxBackend for ModalSandbox {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, SandboxError> {
        // Ensure we have a valid auth token before the RPC.
        self.auth_manager
            .get_token(&self.raw_channel)
            .await?;

        // Extract Modal-specific options.
        let (gpu, app_name_override) = match &config.backend_specific {
            BackendSpecific::Modal { gpu, app } => (gpu.clone(), app.clone()),
            _ => (None, None),
        };

        let app_name = app_name_override
            .or_else(|| self.app_name.clone())
            .unwrap_or_default();

        // Build the GPU config if requested.
        let gpu_config = gpu.map(|g| GpuConfig {
            r#type: 0,    // UNSPECIFIED — use gpu_type string instead
            count: 1,
            gpu_type: g,
        });

        // Build resources.
        let resources = Some(Resources {
            milli_cpu: (config.cpu * 1000.0) as u32,
            memory_mb: config.memory_mb,
            ephemeral_disk_mb: config.disk_mb,
            gpu_config,
            // Defaults for fields we don't expose.
            memory_mb_max: 0,
            milli_cpu_max: 0,
            rdma: false,
        });

        // Check for snapshot resume: if snapshot_data is present, use its image_id.
        let image_id = if let Some(ref snap_json) = config.snapshot_data {
            let snap: ModalSnapshotData =
                serde_json::from_str(snap_json).map_err(|e| {
                    SandboxError::Other(format!("invalid snapshot data: {e}"))
                })?;
            snap.image_id.unwrap_or_else(|| config.image.clone())
        } else {
            config.image.clone()
        };

        let definition = Some(Sandbox {
            image_id,
            resources,
            workdir: config.working_dir.clone(),
            timeout_secs: 3600, // 1 hour default
            direct_sandbox_commands_enabled: true,
            // All other fields default.
            ..Default::default()
        });

        let request = SandboxCreateRequest {
            app_id: app_name,
            definition,
            environment_name: String::new(),
        };

        let resp = self
            .client
            .clone()
            .sandbox_create(request)
            .await
            .map_err(|s| map_grpc_error("sandbox_create", s))?;

        let sandbox_id = resp.into_inner().sandbox_id;

        let now = SystemTime::now();
        Ok(SandboxInstance {
            id: sandbox_id,
            backend_type: self.backend_type().to_owned(),
            task_id: config.task_id.clone(),
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
        // Task 6: Execute via TaskCommandRouter

        // 1. Ensure we have Modal's internal task_id for this sandbox.
        let modal_task_id = self.ensure_modal_task_id(&instance.id).await?;

        // 2. Ensure we have a router client for this task.
        let exec_id = uuid::Uuid::new_v4().to_string();
        let timeout_secs = timeout.map(|d| d.as_secs() as u32);

        // Parse command into args for exec.
        let command_args = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            command.to_owned(),
        ];

        // Get a clone of the router client (to avoid holding the mutex during streaming).
        let mut router = {
            self.ensure_router_client(&modal_task_id).await?;
            let guard = self.routers.lock().await;
            guard.get(&modal_task_id).cloned().ok_or_else(|| {
                SandboxError::Other("router client disappeared".to_owned())
            })?
        };

        // 3. TaskExecStart (unary).
        router
            .task_exec_start(TaskExecStartRequest {
                task_id: modal_task_id.clone(),
                exec_id: exec_id.clone(),
                command_args,
                stdout_config: TaskExecStdoutConfig::Pipe as i32,
                stderr_config: TaskExecStderrConfig::Pipe as i32,
                timeout_secs,
                workdir: working_dir.map(|s| s.to_owned()),
                secret_ids: vec![],
                pty_info: None,
                runtime_debug: false,
                container_id: String::new(),
            })
            .await
            .map_err(|s| map_grpc_error("task_exec_start", s))?;

        // 4. Concurrently read stdout and stderr streams.
        let mut stdout_router = router.clone();
        let mut stderr_router = router.clone();
        let stdout_task_id = modal_task_id.clone();
        let stdout_exec_id = exec_id.clone();
        let stderr_task_id = modal_task_id.clone();
        let stderr_exec_id = exec_id.clone();

        let (stdout_result, stderr_result) = tokio::join!(
            async {
                collect_stream(
                    &mut stdout_router,
                    &stdout_task_id,
                    &stdout_exec_id,
                    TaskExecStdioFileDescriptor::Stdout,
                )
                .await
            },
            async {
                collect_stream(
                    &mut stderr_router,
                    &stderr_task_id,
                    &stderr_exec_id,
                    TaskExecStdioFileDescriptor::Stderr,
                )
                .await
            },
        );

        let stdout = stdout_result?;
        let stderr = stderr_result?;

        // 5. TaskExecWait for exit code.
        let wait_resp = router
            .task_exec_wait(TaskExecWaitRequest {
                task_id: modal_task_id,
                exec_id,
            })
            .await
            .map_err(|s| map_grpc_error("task_exec_wait", s))?;

        let exit_code = match wait_resp.into_inner().exit_status {
            Some(
                genesis_modal_proto::modal::task_command_router::task_exec_wait_response::ExitStatus::Code(
                    c,
                ),
            ) => c,
            Some(
                genesis_modal_proto::modal::task_command_router::task_exec_wait_response::ExitStatus::Signal(
                    s,
                ),
            ) => 128 + s,
            None => -1,
        };

        // 6. Combine output: stdout + stderr.
        let mut output = stdout;
        if !stderr.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&stderr);
        }

        Ok(ExecResult { output, exit_code })
    }

    async fn snapshot(&self, instance: &SandboxInstance) -> Result<Option<String>, SandboxError> {
        self.auth_manager
            .get_token(&self.raw_channel)
            .await?;

        let resp = self
            .client
            .clone()
            .sandbox_snapshot_fs(SandboxSnapshotFsRequest {
                sandbox_id: instance.id.clone(),
                timeout: 300.0, // 5 minutes
            })
            .await
            .map_err(|s| map_grpc_error("sandbox_snapshot_fs", s))?;

        let image_id = resp.into_inner().image_id;
        let data = ModalSnapshotData {
            sandbox_id: instance.id.clone(),
            image_id: Some(image_id),
        };
        let json = serde_json::to_string(&data)?;
        Ok(Some(json))
    }

    async fn cleanup(
        &self,
        instance: &SandboxInstance,
        _persistent: bool,
    ) -> Result<(), SandboxError> {
        self.auth_manager
            .get_token(&self.raw_channel)
            .await?;

        self.client
            .clone()
            .sandbox_terminate(SandboxTerminateRequest {
                sandbox_id: instance.id.clone(),
            })
            .await
            .map_err(|s| map_grpc_error("sandbox_terminate", s))?;

        // Drop cached task_id and router client.
        {
            let mut ids = self.modal_task_ids.lock().await;
            if let Some(task_id) = ids.remove(&instance.id) {
                let mut routers = self.routers.lock().await;
                routers.remove(&task_id);
            }
        }

        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "modal"
    }
}

// ===========================================================================
// Task 6: Execute helpers — ensure_modal_task_id, ensure_router_client, collect_stream
// ===========================================================================

impl ModalSandbox {
    /// Polls `SandboxGetTaskId` until Modal returns the internal task_id.
    /// Retries up to 600 times with 500ms delay (5 minutes total).
    async fn ensure_modal_task_id(&self, sandbox_id: &str) -> Result<String, SandboxError> {
        // Check the cache first.
        {
            let guard = self.modal_task_ids.lock().await;
            if let Some(id) = guard.get(sandbox_id) {
                return Ok(id.clone());
            }
        }

        self.auth_manager
            .get_token(&self.raw_channel)
            .await?;

        for _ in 0..600 {
            let resp = self
                .client
                .clone()
                .sandbox_get_task_id(SandboxGetTaskIdRequest {
                    sandbox_id: sandbox_id.to_owned(),
                    timeout: Some(5.0),
                    wait_until_ready: true,
                })
                .await
                .map_err(|s| map_grpc_error("sandbox_get_task_id", s))?;

            let inner = resp.into_inner();

            if let Some(task_id) = inner.task_id {
                if !task_id.is_empty() {
                    let mut guard = self.modal_task_ids.lock().await;
                    guard.insert(sandbox_id.to_owned(), task_id.clone());
                    return Ok(task_id);
                }
            }

            // Check if the task has already exited.
            if inner.task_result.is_some() {
                return Err(SandboxError::Other(
                    "sandbox task exited before task_id could be obtained".to_owned(),
                ));
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        Err(SandboxError::Timeout {
            seconds: 300,
        })
    }

    /// Ensures a router client exists for the given Modal task_id.
    async fn ensure_router_client(&self, modal_task_id: &str) -> Result<(), SandboxError> {
        {
            let guard = self.routers.lock().await;
            if guard.contains_key(modal_task_id) {
                return Ok(());
            }
        }

        self.auth_manager
            .get_token(&self.raw_channel)
            .await?;

        let resp = self
            .client
            .clone()
            .task_get_command_router_access(TaskGetCommandRouterAccessRequest {
                task_id: modal_task_id.to_owned(),
            })
            .await
            .map_err(|s| map_grpc_error("task_get_command_router_access", s))?;

        let inner = resp.into_inner();
        let jwt = inner.jwt;
        let url = inner.url;

        let endpoint = Endpoint::from_shared(url.clone())
            .map_err(|e| SandboxError::Other(format!("invalid router URL {url}: {e}")))?
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(30));

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| SandboxError::Other(format!("failed to connect to router at {url}: {e}")))?;

        let interceptor = RouterJwtInterceptor { jwt };
        let router_client = TaskCommandRouterClient::with_interceptor(channel, interceptor);

        {
            let mut guard = self.routers.lock().await;
            guard.insert(modal_task_id.to_owned(), router_client);
        }

        Ok(())
    }
}

/// Collect all data from a TaskExecStdioRead server-streaming RPC.
async fn collect_stream(
    router: &mut RouterClient,
    task_id: &str,
    exec_id: &str,
    fd: TaskExecStdioFileDescriptor,
) -> Result<String, SandboxError> {
    let resp = router
        .task_exec_stdio_read(TaskExecStdioReadRequest {
            task_id: task_id.to_owned(),
            exec_id: exec_id.to_owned(),
            offset: 0,
            file_descriptor: fd as i32,
        })
        .await
        .map_err(|s| map_grpc_error("task_exec_stdio_read", s))?;

    let mut stream = resp.into_inner();
    let mut buf = Vec::new();

    while let Some(msg) = stream
        .message()
        .await
        .map_err(|s| map_grpc_error("task_exec_stdio_read stream", s))?
    {
        buf.extend_from_slice(&msg.data);
    }

    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- Task 2 tests ---

    #[test]
    fn parse_modal_toml_extracts_credentials() {
        let content = r#"
[default]
token_id = "ak-test123"
token_secret = "as-secret456"
"#;
        let (id, secret) = parse_modal_toml(content).unwrap();
        assert_eq!(id, "ak-test123");
        assert_eq!(secret, "as-secret456");
    }

    #[test]
    fn parse_modal_toml_missing_section_errors() {
        let content = r#"
[other]
token_id = "ak-test"
token_secret = "as-secret"
"#;
        let result = parse_modal_toml(content);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("[default] section not found"), "got: {err}");
    }

    #[test]
    fn parse_modal_toml_handles_extra_sections() {
        let content = r#"
[default]
token_id = "ak-main"
token_secret = "as-main"

[staging]
token_id = "ak-staging"
token_secret = "as-staging"
"#;
        let (id, secret) = parse_modal_toml(content).unwrap();
        assert_eq!(id, "ak-main");
        assert_eq!(secret, "as-main");
    }

    #[test]
    fn decode_jwt_expiry_extracts_exp_claim() {
        // Build a JWT with exp=1700000000.
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"exp":1700000000,"sub":"test"}"#);
        let token = format!("{header}.{payload}.fakesignature");

        assert_eq!(decode_jwt_expiry(&token), Some(1700000000));
    }

    #[test]
    fn decode_jwt_expiry_returns_none_for_missing_exp() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"HS256"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"sub":"test"}"#);
        let token = format!("{header}.{payload}.sig");

        assert_eq!(decode_jwt_expiry(&token), None);
    }

    #[test]
    fn decode_jwt_expiry_returns_none_for_invalid_jwt() {
        assert_eq!(decode_jwt_expiry("not-a-jwt"), None);
        assert_eq!(decode_jwt_expiry(""), None);
        assert_eq!(decode_jwt_expiry("a.b"), None); // only 2 parts
    }

    // --- Task 4 test ---

    #[test]
    fn modal_sandbox_rejects_empty_credentials() {
        let result = ModalSandbox::new(String::new(), "secret".to_owned(), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must not be empty"));

        let result = ModalSandbox::new("id".to_owned(), String::new(), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must not be empty"));
    }

    // --- Task 5 test ---

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

    // --- Task 3 test ---

    #[test]
    fn map_grpc_error_maps_unauthenticated() {
        let err = map_grpc_error("test", tonic::Status::unauthenticated("bad token"));
        assert!(matches!(err, SandboxError::AuthError { .. }));
    }

    #[test]
    fn map_grpc_error_maps_not_found() {
        let err = map_grpc_error("test", tonic::Status::not_found("gone"));
        assert!(matches!(err, SandboxError::NotFound { .. }));
    }

    #[test]
    fn map_grpc_error_maps_deadline_exceeded() {
        let err = map_grpc_error("test", tonic::Status::deadline_exceeded("slow"));
        assert!(matches!(err, SandboxError::Timeout { .. }));
    }

    #[test]
    fn map_grpc_error_maps_resource_exhausted() {
        let err = map_grpc_error("test", tonic::Status::resource_exhausted("quota"));
        assert!(matches!(err, SandboxError::ResourceLimit { .. }));
    }

    #[test]
    fn map_grpc_error_maps_other() {
        let err = map_grpc_error("test", tonic::Status::internal("oops"));
        assert!(matches!(err, SandboxError::Other(_)));
    }
}
