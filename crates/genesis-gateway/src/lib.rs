//! HTTP gateway for the Genesis agent.
//!
//! Exposes a REST API so external services (webhooks, platform bots)
//! can send messages to Eve and receive responses.

pub mod commands;
pub mod mirror;
pub mod platforms;
pub mod verify;
pub mod webhooks;

use std::collections::HashMap;
use std::fmt::Write;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use genesis_core::agent_loop::StreamEvent;
use genesis_core::execution::{
    delivery_platform_from_str, SessionExecutionService, SessionTurnInput,
};
use genesis_storage::{
    EmbeddingStore, MemoryStore, PairingStore, ScheduleStore, SessionStore, SkillStore,
    SkillUsageStore, SubagentStore, UserModelStore,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error, info, info_span, warn, Instrument};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Buffer size for bounded SSE channels.  Provides backpressure: when the
/// buffer fills up, the sender blocks (up to [`SSE_SEND_TIMEOUT`]) rather
/// than silently dropping events.
const SSE_CHANNEL_BUFFER: usize = 64;

/// Default timeout for SSE streaming requests (5 minutes).  Overridable via
/// `gateway.stream_timeout_secs` in the config file.
const DEFAULT_STREAM_TIMEOUT_SECS: u64 = 300;

/// Maximum time to wait when the SSE channel buffer is full before aborting
/// the stream.  This prevents silent data loss: instead of dropping events
/// when a slow consumer falls behind, we apply backpressure and only abort
/// (with cancellation) if the consumer cannot keep up for this duration.
const SSE_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Send an SSE event on a bounded channel with backpressure.
///
/// If the receiver has been dropped (client disconnected) the cancellation
/// flag is set so the agent loop exits at the next opportunity.  If the
/// channel buffer is full, this blocks for up to [`SSE_SEND_TIMEOUT`]
/// waiting for capacity.  If the timeout elapses the stream is aborted via
/// cancellation — this is preferable to silently dropping events which would
/// cause invisible data loss in OpenAI-compatible streaming responses.
///
/// This function is called from synchronous streaming callbacks, so it uses
/// [`tokio::task::block_in_place`] to safely block the current thread while
/// awaiting the async send.
fn send_sse(
    tx: &mpsc::Sender<Result<Event, std::convert::Infallible>>,
    event: Result<Event, std::convert::Infallible>,
    cancelled: &AtomicBool,
) {
    if cancelled.load(Ordering::Relaxed) {
        return;
    }
    // block_in_place moves the current task off the tokio worker thread,
    // allowing us to block on an async send without deadlocking the runtime.
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            match tokio::time::timeout(SSE_SEND_TIMEOUT, tx.send(event)).await {
                Ok(Ok(())) => {}
                Ok(Err(_closed)) => {
                    debug!("SSE client disconnected, signalling cancellation");
                    cancelled.store(true, Ordering::Relaxed);
                }
                Err(_elapsed) => {
                    error!(
                        timeout_secs = SSE_SEND_TIMEOUT.as_secs(),
                        "SSE send timed out (slow consumer), aborting stream"
                    );
                    cancelled.store(true, Ordering::Relaxed);
                }
            }
        });
    });
}

/// Guard that sets a cancellation flag on drop.  Used to ensure the agent
/// loop is cancelled when the SSE stream is dropped — whether that happens
/// because the stream naturally ended or because Axum dropped the future
/// mid-execution (client disconnect).
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Simple in-memory sliding-window rate limiter keyed by IP address.
///
/// Each entry stores `(request_count, window_start_secs)`.  When a new
/// request arrives and the current timestamp is still within the same
/// 60-second window, the count increments.  Otherwise the window resets.
/// Stale entries (older than 2 minutes) are purged on every check to
/// prevent unbounded memory growth.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    /// Max requests per 60-second window.  Stored here so the middleware
    /// doesn't need to re-read `AppState`.
    max_rpm: u32,
    /// Map from IP -> (count, window_start_epoch_secs).
    entries: Mutex<HashMap<IpAddr, (u32, u64)>>,
    /// Epoch second of the last purge, used to amortize cleanup.
    last_purge: std::sync::atomic::AtomicU64,
}

/// How often (in seconds) to purge stale rate-limit entries.
const PURGE_INTERVAL_SECS: u64 = genesis_config::defaults::timeouts::RATE_PURGE_INTERVAL_SECS;

/// Duration of each rate-limit sliding window, in seconds.
const RATE_WINDOW_SECS: u64 = genesis_config::defaults::timeouts::RATE_WINDOW_SECS;

impl RateLimiter {
    pub fn new(max_rpm: u32) -> Self {
        Self {
            max_rpm,
            entries: Mutex::new(HashMap::new()),
            last_purge: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut map = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // Recover from poisoned mutex — another thread panicked while
                // holding the lock. Clear the stale state and continue.
                let mut guard = poisoned.into_inner();
                guard.clear();
                guard
            }
        };

        // Amortized purge: only scan & remove stale entries periodically
        let prev = self.last_purge.load(std::sync::atomic::Ordering::Relaxed);
        if now.saturating_sub(prev) >= PURGE_INTERVAL_SECS {
            map.retain(|_, (_, window_start)| {
                now.saturating_sub(*window_start) < PURGE_INTERVAL_SECS
            });
            self.last_purge
                .store(now, std::sync::atomic::Ordering::Relaxed);
        }

        let entry = map.entry(ip).or_insert((0, now));
        if now.saturating_sub(entry.1) >= RATE_WINDOW_SECS {
            // New window
            *entry = (1, now);
            true
        } else if entry.0 < self.max_rpm {
            entry.0 += 1;
            true
        } else {
            false
        }
    }
}

/// Prometheus-style histogram with fixed bucket boundaries.
pub(crate) struct HistogramBuckets {
    /// Bucket boundaries in milliseconds.
    boundaries: &'static [u64],
    /// Count of observations in each bucket (cumulative).
    counts: Vec<u64>,
    /// Total count of all observations.
    total_count: u64,
    /// Sum of all observed values (for computing mean).
    total_sum: f64,
}

const DURATION_BUCKETS: &[u64] = &[50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000];

impl HistogramBuckets {
    fn new(boundaries: &'static [u64]) -> Self {
        Self {
            boundaries,
            counts: vec![0; boundaries.len()],
            total_count: 0,
            total_sum: 0.0,
        }
    }

    fn observe(&mut self, value_ms: u64) {
        self.total_count += 1;
        self.total_sum += value_ms as f64;
        for (i, &boundary) in self.boundaries.iter().enumerate() {
            if value_ms <= boundary {
                self.counts[i] += 1;
            }
        }
    }

    fn format_prometheus(&self, name: &str, help: &str) -> String {
        let mut out = format!("# HELP {name} {help}\n# TYPE {name} histogram\n");
        for (i, &boundary) in self.boundaries.iter().enumerate() {
            let _ = writeln!(out, "{name}_bucket{{le=\"{boundary}\"}} {}", self.counts[i]);
        }
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {}", self.total_count);
        let _ = writeln!(out, "{name}_sum {}", self.total_sum);
        let _ = writeln!(out, "{name}_count {}", self.total_count);
        out
    }
}

/// Shared application state for all request handlers.
pub struct AppState {
    pub loaded: genesis_config::LoadedConfig,
    /// Optional API key for gateway authentication.
    /// When set, protected routes require `Authorization: Bearer <key>`.
    /// If absent and `api_key_required` is true, protected routes are rejected.
    pub api_key: Option<String>,
    /// Whether protected routes must require an API key.
    pub api_key_required: bool,
    /// Shared MCP manager for external tool servers (connected at startup).
    pub mcp: Option<std::sync::Arc<genesis_mcp::McpManager>>,
    /// Shared HTTP client for outbound platform API calls (connection pooling).
    pub http_client: reqwest::Client,
    /// Optional per-IP rate limiter.
    pub(crate) rate_limiter: Option<RateLimiter>,
    /// Trusted reverse proxy IPs allowed to supply forwarded headers.
    pub trusted_proxies: Vec<IpAddr>,
    /// Webhook event dispatcher for external notifications.
    pub webhooks: webhooks::WebhookDispatcher,
    /// Timestamp when the gateway started (for uptime reporting).
    pub started_at: std::time::Instant,
    // --- Metrics counters ---
    /// Total chat requests processed (including stream and batch).
    pub requests_total: AtomicU64,
    /// Total errors returned across all endpoints.
    pub errors_total: AtomicU64,
    /// Total input tokens processed.
    pub input_tokens_total: AtomicU64,
    /// Total output tokens generated.
    pub output_tokens_total: AtomicU64,
    /// Total streaming requests.
    pub stream_requests_total: AtomicU64,
    /// Request duration histogram buckets (in ms): [50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000, +Inf]
    pub(crate) request_duration_histogram: Mutex<HistogramBuckets>,
    /// Agent message bus for inter-agent communication.
    pub agent_bus: genesis_core::agent_bus::AgentBus,
    /// Process-local plugin runtime overrides supplied by the embedding host.
    pub plugin_runtime_overrides: genesis_core::execution::PluginRuntimeOverrides,
    /// Shared embedding provider cached on first use to avoid rebuilding per request.
    embedding_provider_cache: OnceLock<Arc<genesis_core::embedding::EmbeddingProvider>>,
    /// Serializes first-time provider initialization on toolchains without `OnceLock::get_or_try_init`.
    embedding_provider_init: Mutex<()>,
}

fn get_or_try_init_arc<T, E, F>(
    cache: &OnceLock<Arc<T>>,
    init_lock: &Mutex<()>,
    init: F,
) -> Result<Arc<T>, E>
where
    F: FnOnce() -> Result<T, E>,
{
    if let Some(value) = cache.get() {
        return Ok(Arc::clone(value));
    }

    let _guard = init_lock
        .lock()
        .expect("embedding provider init lock poisoned");
    if let Some(value) = cache.get() {
        return Ok(Arc::clone(value));
    }

    let value = Arc::new(init()?);
    let _ = cache.set(Arc::clone(&value));
    Ok(value)
}

impl AppState {
    pub fn new(
        loaded: genesis_config::LoadedConfig,
        api_key: Option<String>,
        api_key_required: bool,
        mcp: Option<std::sync::Arc<genesis_mcp::McpManager>>,
        rate_limit_rpm: Option<u32>,
        trusted_proxies: Vec<IpAddr>,
        plugin_runtime_overrides: genesis_core::execution::PluginRuntimeOverrides,
    ) -> Self {
        let webhook_configs = loaded
            .config
            .gateway
            .as_ref()
            .map(|g| g.webhooks.clone())
            .unwrap_or_default();
        let bus_db_path = loaded.config.storage.database_path.clone();
        Self {
            api_key,
            api_key_required,
            mcp,
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(
                    genesis_config::defaults::timeouts::GATEWAY_HTTP_CLIENT_SECS,
                ))
                .user_agent("genesis-gateway/0.1")
                .build()
                .unwrap_or_default(),
            rate_limiter: rate_limit_rpm.map(RateLimiter::new),
            trusted_proxies,
            loaded,
            webhooks: webhooks::WebhookDispatcher::new(webhook_configs),
            started_at: std::time::Instant::now(),
            requests_total: AtomicU64::new(0),
            errors_total: AtomicU64::new(0),
            input_tokens_total: AtomicU64::new(0),
            output_tokens_total: AtomicU64::new(0),
            stream_requests_total: AtomicU64::new(0),
            request_duration_histogram: Mutex::new(HistogramBuckets::new(DURATION_BUCKETS)),
            agent_bus: genesis_core::agent_bus::AgentBus::with_persistence(&bus_db_path),
            plugin_runtime_overrides,
            embedding_provider_cache: OnceLock::new(),
            embedding_provider_init: Mutex::new(()),
        }
    }

    pub fn session_service(&self) -> SessionExecutionService<'_> {
        let mut service = SessionExecutionService::new(&self.loaded);
        if let Some(mcp) = &self.mcp {
            service.set_mcp(std::sync::Arc::clone(mcp));
        }
        service.set_plugin_runtime_overrides(self.plugin_runtime_overrides);
        service
    }

    fn embedding_provider(
        &self,
    ) -> Result<Option<Arc<genesis_core::embedding::EmbeddingProvider>>, (StatusCode, String)> {
        let Some(config) = self.loaded.config.embedding.as_ref() else {
            return Ok(None);
        };

        get_or_try_init_arc(
            &self.embedding_provider_cache,
            &self.embedding_provider_init,
            || build_embedding_provider(config),
        )
        .map(Some)
    }
}

/// Request body for the `/chat` endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct ChatRequest {
    pub message: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    pub session_id: Option<String>,
    /// Optional image URLs for multimodal prompts.
    #[serde(default)]
    pub images: Vec<ImageInput>,
    /// Optional system prompt override for this request.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Optional response format constraint (json_object, json_schema, or text).
    #[serde(default)]
    pub response_format: Option<genesis_provider::ResponseFormat>,
    /// Optional model override (e.g. "openai/gpt-4.1" or "anthropic/claude-sonnet-4").
    /// Format: "backend/model" or just "model" (uses default backend).
    #[serde(default)]
    pub model: Option<String>,
}

/// An image input for multimodal chat requests.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ImageInput {
    /// Image URL (http/https) or base64 data URI.
    pub url: String,
    /// Optional detail level: "low", "high", or "auto" (default).
    #[serde(default)]
    pub detail: Option<String>,
}

fn default_platform() -> String {
    "api".to_owned()
}

fn default_api_session_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let seq = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("api-{ts}-{seq}")
}

/// Parse a model spec like "openai/gpt-4.1" or "gpt-4.1" into (backend, model).
/// If no backend prefix is given, uses the default backend.
fn parse_model_spec(spec: &str, default_backend: &str) -> (String, String) {
    match spec.split_once('/') {
        Some((backend, model)) => (backend.to_owned(), model.to_owned()),
        None => (default_backend.to_owned(), spec.to_owned()),
    }
}

fn default_request_id() -> String {
    let next = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("req-{next}")
}

/// Map a storage error into an HTTP 500 response pair.
fn storage_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    error!(error = %e, "storage operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("storage error: {e}"),
    )
}

// ---------------------------------------------------------------------------
// Shared pagination types
// ---------------------------------------------------------------------------

/// Maximum value accepted for `limit`.  Requests with a larger value are
/// silently clamped to this ceiling.
const MAX_PAGE_LIMIT: usize = 1000;

/// Default page size when the caller does not supply one.
const DEFAULT_PAGE_LIMIT: usize = 50;

fn default_page_limit() -> usize {
    DEFAULT_PAGE_LIMIT
}

/// Clamp a caller-supplied limit to the valid range `[1, MAX_PAGE_LIMIT]`.
fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_PAGE_LIMIT)
}

/// Maximum safe offset value.  Offsets beyond this are rejected with 400 to
/// avoid overflow when converting to `i64` for SQLite OFFSET clauses.
const MAX_OFFSET: usize = i64::MAX as usize;

/// Validate that the caller-supplied offset is within safe bounds.
fn validate_offset(offset: usize) -> Result<usize, (StatusCode, String)> {
    if offset > MAX_OFFSET {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("offset {offset} exceeds maximum allowed value ({MAX_OFFSET})"),
        ));
    }
    Ok(offset)
}

/// Generic paginated response wrapper.
///
/// Kept for potential internal use and testing.  Production endpoints use
/// typed response structs below so that JSON field names match the legacy
/// dashboard contract (e.g. `sessions`, `skills` instead of `items`).
#[derive(Debug, Serialize)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct PaginatedResponse<T: Serialize> {
    pub items: Vec<T>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
}

// ── Typed paginated response structs (legacy field names) ────────────

/// Helper macro to avoid repeating the same pagination metadata fields.
macro_rules! paginated_response {
    ($name:ident, $field:ident, $item_ty:ty) => {
        #[derive(Debug, Serialize)]
        pub(crate) struct $name {
            pub $field: Vec<$item_ty>,
            pub total: u64,
            pub limit: usize,
            pub offset: usize,
            pub has_more: bool,
        }
    };
}

paginated_response!(
    SessionListResponse,
    sessions,
    genesis_storage::SessionSummary
);
paginated_response!(SkillListResponse, skills, genesis_storage::StoredSkill);
paginated_response!(MemoryListResponse, memories, genesis_storage::StoredMemory);
paginated_response!(
    ScheduleListResponse,
    schedules,
    genesis_storage::StoredSchedule
);
paginated_response!(TraitListResponse, traits, genesis_storage::StoredUserTrait);
paginated_response!(TemplateListResponse, templates, serde_json::Value);
paginated_response!(
    ApprovedListResponse,
    approved,
    genesis_storage::ApprovedUser
);
paginated_response!(
    PendingListResponse,
    pending,
    genesis_storage::PendingPairing
);

/// Response body for GET /tools.
///
/// The dashboard expects `builtin_tools` and `mcp_tools` as separate arrays,
/// so this endpoint does NOT use the generic paginated envelope.
#[derive(Debug, Serialize)]
pub(crate) struct ToolListResponse {
    pub builtin_tools: Vec<serde_json::Value>,
    pub builtin_count: usize,
    pub mcp_tools: Vec<serde_json::Value>,
    pub mcp_count: usize,
    pub total: usize,
}

/// Response body from the `/chat` endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct ChatResponse {
    pub session_id: String,
    pub response: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    /// If set, the agent is paused waiting for user clarification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_clarification: Option<String>,
}

/// SSE payload for a streamed token chunk.
#[derive(Debug, Serialize)]
pub(crate) struct StreamChunkResponse {
    pub session_id: String,
    pub content: String,
}

/// SSE payload signaling final completion.
#[derive(Debug, Serialize)]
pub(crate) struct StreamDoneResponse {
    pub session_id: String,
    pub response: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

/// SSE payload signaling an execution failure.
#[derive(Debug, Serialize)]
pub(crate) struct StreamErrorResponse {
    pub session_id: String,
    pub error: String,
}

/// Health check response.
#[derive(Debug, Serialize)]
pub(crate) struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub model: String,
    pub mcp_servers: usize,
    pub active_schedules: usize,
    pub total_sessions: usize,
    pub total_tools: usize,
    /// Configured providers with their circuit breaker settings.
    /// Always includes the primary provider; fallback providers follow in order.
    pub providers: Vec<ProviderInfo>,
}

/// Provider configuration info for the health endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct ProviderInfo {
    pub backend: String,
    pub model: String,
    pub role: String,
    pub circuit_breaker: Option<CircuitBreakerInfo>,
}

/// Circuit breaker configuration info for health display.
#[derive(Debug, Serialize)]
pub(crate) struct CircuitBreakerInfo {
    pub failure_threshold: u32,
    pub cooldown_secs: u64,
}

/// JSON metrics response for the dashboard.
#[derive(Debug, Serialize)]
struct MetricsJsonResponse {
    uptime_seconds: u64,
    requests_total: u64,
    errors_total: u64,
    input_tokens_total: u64,
    output_tokens_total: u64,
    stream_requests_total: u64,
    total_sessions: usize,
    active_schedules: usize,
}

/// Detailed MCP server status response.
#[derive(Debug, Serialize)]
pub(crate) struct McpStatusResponse {
    pub servers: Vec<McpServerStatus>,
    pub total_tools: usize,
    pub total_resources: usize,
    pub total_prompts: usize,
}

/// Status of a single MCP server.
#[derive(Debug, Serialize)]
pub(crate) struct McpServerStatus {
    pub name: String,
    pub connected: bool,
}

/// Default localhost origins allowed for development when no CORS origins are configured.
const LOCALHOST_ORIGINS: &[&str] = &[
    "http://localhost:3000",
    "http://localhost:5173",
    "http://localhost:8080",
    "http://127.0.0.1:3000",
    "http://127.0.0.1:5173",
    "http://127.0.0.1:8080",
];

fn parse_origin_values(origins: &[&str]) -> Vec<axum::http::HeaderValue> {
    origins.iter().filter_map(|o| o.parse().ok()).collect()
}

/// Build CORS layer from gateway config.
///
/// - If `cors_origins` contains `"*"`, all origins are allowed.
/// - If `cors_origins` is non-empty, only those origins are allowed.
/// - Default (empty or no gateway config): localhost origins only.
fn build_cors_layer(gateway: Option<&genesis_config::GatewayConfig>) -> CorsLayer {
    use axum::http::{HeaderValue, Method};

    let origins: &[String] = gateway.map(|g| g.cors_origins.as_slice()).unwrap_or(&[]);

    let base = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    if origins.iter().any(|o| o == "*") {
        base.allow_origin(Any)
    } else if origins.is_empty() {
        base.allow_origin(parse_origin_values(LOCALHOST_ORIGINS))
    } else {
        let values: Vec<HeaderValue> = origins
            .iter()
            .filter_map(|o| match o.parse::<HeaderValue>() {
                Ok(v) => Some(v),
                Err(e) => {
                    error!(origin = %o, error = %e, "invalid CORS origin in config, skipping");
                    None
                }
            })
            .collect();
        if values.is_empty() {
            error!("all configured CORS origins are invalid, falling back to default localhost origins ({LOCALHOST_ORIGINS:?})");
            base.allow_origin(parse_origin_values(LOCALHOST_ORIGINS))
        } else {
            base.allow_origin(values)
        }
    }
}

#[cfg(feature = "embed-ui")]
mod web_assets {
    #[derive(rust_embed::Embed)]
    #[folder = "../../web/dist/"]
    pub struct Assets;
}

#[cfg(feature = "embed-ui")]
async fn static_file_handler(uri: axum::http::Uri) -> impl axum::response::IntoResponse {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;

    let path = uri.path().trim_start_matches('/');

    if let Some(file) = web_assets::Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            [(header::CONTENT_TYPE, mime.as_ref().to_string())],
            file.data,
        )
            .into_response();
    }

    // SPA fallback
    match web_assets::Assets::get("index.html") {
        Some(index) => (
            [(header::CONTENT_TYPE, "text/html".to_string())],
            index.data,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Build the axum Router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = build_cors_layer(state.loaded.config.gateway.as_ref());

    // API routes nested under /api/ (require API key when configured/required).
    // These are all the primary REST endpoints for the dashboard and clients.
    let api_routes = Router::new()
        .route("/chat", post(chat_handler))
        .route("/chat/stream", post(chat_stream_handler))
        .route("/chat/ws", get(websocket_handler))
        .route("/chat/batch", post(chat_batch_handler))
        .route("/sessions", get(list_sessions_handler))
        .route("/sessions/purge", delete(purge_sessions_handler))
        .route("/sessions/import", post(import_session_handler))
        .route("/sessions/export", get(bulk_export_handler))
        .route(
            "/sessions/{id}",
            get(get_session_handler).delete(delete_session_handler),
        )
        .route("/sessions/{id}/messages", get(session_messages_handler))
        .route("/sessions/{id}/fork", post(fork_session_handler))
        .route("/sessions/{id}/title", patch(update_session_title_handler))
        .route("/sessions/{id}/export", get(export_session_handler))
        .route(
            "/sessions/{id}/tags",
            get(get_session_tags_handler).put(set_session_tags_handler),
        )
        .route(
            "/sessions/{id}/tags/{tag}",
            post(add_session_tag_handler).delete(remove_session_tag_handler),
        )
        .route("/sessions/by-tag/{tag}", get(sessions_by_tag_handler))
        .route("/messages/search", get(search_messages_handler))
        .route("/usage", get(usage_handler))
        .route("/insights", get(insights_handler))
        // Skills CRUD
        .route(
            "/skills",
            get(list_skills_handler).post(upsert_skill_handler),
        )
        .route("/skills/search", get(search_skills_handler))
        .route(
            "/skills/{name}",
            get(get_skill_handler).delete(delete_skill_handler),
        )
        // Memory endpoints
        .route("/memories", get(list_memories_handler))
        .route("/memories/search", get(search_memories_handler))
        .route("/memories/embed", post(embed_memories_handler))
        .route("/memories/{id}", delete(delete_memory_handler))
        .route("/memories/{id}/embed", post(embed_single_memory_handler))
        // Schedule management
        .route(
            "/schedules",
            get(list_schedules_handler).post(create_schedule_handler),
        )
        .route(
            "/schedules/{id}",
            get(get_schedule_handler).delete(delete_schedule_handler),
        )
        .route(
            "/schedules/{id}/enabled",
            patch(set_schedule_enabled_handler),
        )
        // User model (traits/preferences)
        .route(
            "/user/traits",
            get(list_user_traits_handler).post(observe_user_trait_handler),
        )
        .route(
            "/user/traits/{key}",
            get(get_user_trait_handler).delete(delete_user_trait_handler),
        )
        // Subagents
        .route("/subagents/{id}", get(get_subagent_handler))
        .route(
            "/sessions/{id}/subagents",
            get(list_session_subagents_handler),
        )
        // Skill usage stats
        .route("/skills/{name}/usage", get(skill_usage_stats_handler))
        .route(
            "/skills/{name}/usage/recent",
            get(skill_usage_recent_handler),
        )
        // DM pairing management
        .route("/pairing/approved", get(list_approved_handler))
        .route("/pairing/pending", get(list_pending_handler))
        .route("/pairing/approve", post(approve_pairing_handler))
        .route("/pairing/revoke", post(revoke_pairing_handler))
        .route("/pairing/clear-pending", post(clear_pending_handler))
        // Tool introspection
        .route("/tools", get(list_tools_handler))
        // Cache management
        .route("/cache/stats", get(cache_stats_handler))
        .route("/cache/clear", post(cache_clear_handler))
        .route("/audit", get(audit_recent_handler))
        .route("/audit/stats", get(audit_stats_handler))
        .route("/audit/session/{id}", get(audit_session_handler))
        .route("/audit/purge", post(audit_purge_handler))
        .route("/analytics/tools", get(tool_analytics_handler))
        .route("/analytics/llm", get(llm_analytics_handler))
        .route("/webhooks/status", get(webhooks_status_handler))
        .route(
            "/webhooks/dead-letters",
            get(webhooks_dead_letters_handler).delete(webhooks_clear_dead_letters_handler),
        )
        .route("/templates", get(list_templates_handler))
        .route("/templates/{name}", get(get_template_handler))
        .route("/workflows/validate", post(workflow_validate_handler))
        .route("/workflows/run", post(workflow_run_handler))
        .route("/bus/channels", get(bus_channels_handler))
        .route("/bus/publish", post(bus_publish_handler))
        .route("/bus/history/{channel}", get(bus_history_handler))
        .route("/bus/stats", get(bus_stats_handler))
        .route("/eval/validate", post(eval_validate_handler))
        .route("/eval/run", post(eval_run_handler))
        .route("/guardrails/check", post(guardrails_check_handler))
        // Config introspection
        .route("/config", get(config_handler))
        // JSON metrics for the web dashboard
        .route("/metrics/json", get(metrics_json_handler))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    // Root-level protected routes: OpenAI-compatible API and Prometheus metrics.
    // These stay at the root path (not under /api/) for compatibility with
    // existing integrations, but still require API key authentication.
    let root_protected = Router::new()
        .route(
            "/v1/chat/completions",
            post(openai_chat_completions_handler),
        )
        .route("/v1/models", get(openai_models_handler))
        .route("/metrics", get(prometheus_metrics_handler))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    // Platform webhook routes (no API key — each platform has strict, fail-closed webhook auth)
    let platform_webhooks = Router::new()
        .route(
            "/telegram/webhook",
            post(platforms::telegram::webhook_handler),
        )
        .route(
            "/discord/interactions",
            post(platforms::discord::interactions_handler),
        )
        .route("/slack/events", post(platforms::slack::events_handler))
        .route(
            "/whatsapp/webhook",
            get(platforms::whatsapp::verify_handler).post(platforms::whatsapp::webhook_handler),
        )
        .route(
            "/homeassistant/webhook",
            post(platforms::homeassistant::webhook_handler),
        )
        .route("/signal/webhook", post(platforms::signal::webhook_handler))
        .route("/signal/poll", post(platforms::signal::poll_handler));

    // Rate-limited routes (api_routes nested under /api/, root_protected, and platform webhooks)
    let rate_limited = Router::new()
        .nest("/api", api_routes)
        .merge(root_protected)
        .merge(platform_webhooks)
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            rate_limit_middleware,
        ));

    // Public routes at root (no auth, no rate limiting for health checks)
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/health/mcp", get(mcp_status_handler))
        // /api/health is also public — health checks must not require an API key
        .route("/api/health", get(health_handler))
        .route("/api/health/mcp", get(mcp_status_handler))
        .route("/.well-known/agent.json", get(agent_card_handler))
        .merge(rate_limited);

    #[cfg(not(feature = "embed-ui"))]
    let app = app;

    #[cfg(feature = "embed-ui")]
    let app = app.fallback(static_file_handler);

    app.layer(middleware::from_fn(request_logging_middleware))
        .layer(cors)
        .with_state(state)
}

/// Middleware that logs every request with method, path, status, and duration.
async fn request_logging_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let start = std::time::Instant::now();

    let response = next.run(request).await;

    let duration = start.elapsed();
    let status = response.status().as_u16();

    // Skip logging health checks to reduce noise
    if path != "/health" {
        info!(
            method = %method,
            path = %path,
            status = status,
            duration_ms = duration.as_millis() as u64,
            "request completed"
        );
    }

    response
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected_key = match &state.api_key {
        Some(key) => key,
        None if state.api_key_required => return Err(StatusCode::UNAUTHORIZED),
        None => return Ok(next.run(request).await),
    };

    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let token = auth_header.and_then(|value| {
        let value = value.trim();
        let mut parts = value.splitn(2, ' ');
        let scheme = parts.next()?.trim();
        let credentials = parts.next()?.trim();
        if scheme.eq_ignore_ascii_case("bearer") {
            Some(credentials)
        } else {
            None
        }
    });

    match token {
        Some(t) if verify::constant_time_eq(t.as_bytes(), expected_key.as_bytes()) => {
            Ok(next.run(request).await)
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Extracts the client IP from the request.
///
/// Extracts the client IP from proxy headers when the peer socket belongs to a
/// trusted proxy; otherwise falls back to the peer socket address via
/// `ConnectInfo`.
fn client_ip<B>(state: &AppState, request: &Request<B>) -> Option<IpAddr> {
    let peer_ip = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());

    let peer_is_trusted_proxy = peer_ip
        .map(|ip| state.trusted_proxies.contains(&ip))
        .unwrap_or(false);

    if peer_is_trusted_proxy {
        // X-Forwarded-For: first entry is the original client
        if let Some(forwarded) = request.headers().get("x-forwarded-for") {
            if let Ok(val) = forwarded.to_str() {
                if let Some(first) = val.split(',').next() {
                    if let Ok(ip) = first.trim().parse::<IpAddr>() {
                        return Some(ip);
                    }
                }
            }
        }

        // X-Real-IP
        if let Some(real_ip) = request.headers().get("x-real-ip") {
            if let Ok(val) = real_ip.to_str() {
                if let Ok(ip) = val.trim().parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }

    // Peer address via ConnectInfo (populated by axum::serve)
    peer_ip
}

async fn rate_limit_middleware(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let limiter = match &state.rate_limiter {
        Some(l) => l,
        None => return Ok(next.run(request).await),
    };
    let ip = client_ip(&state, &request).unwrap_or(IpAddr::from([127, 0, 0, 1]));
    if limiter.check(ip) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::TOO_MANY_REQUESTS)
    }
}

/// Shared DB stats used by health, metrics, and prometheus handlers.
fn fetch_db_stats(db_path: &std::path::Path) -> (usize, usize) {
    let total_sessions = genesis_storage::SessionStore::new(db_path)
        .session_count()
        .unwrap_or(0) as usize;
    let active_schedules = genesis_storage::ScheduleStore::new(db_path)
        .list_enabled()
        .map(|s| s.len())
        .unwrap_or(0);
    (total_sessions, active_schedules)
}

async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let mcp_count = match &state.mcp {
        Some(mcp) => mcp.server_count().await,
        None => 0,
    };
    let (total_sessions, active_schedules) =
        fetch_db_stats(&state.loaded.config.storage.database_path);
    let mcp_tools = match &state.mcp {
        Some(mcp) => mcp.tool_count().await,
        None => 0,
    };
    let builtin_tools = genesis_core::default_tool_count();
    // Build provider status list for health reporting.
    let mut providers = Vec::new();
    let primary = &state.loaded.config.provider;
    providers.push(ProviderInfo {
        backend: primary.backend.clone(),
        model: primary.model.clone(),
        role: "primary".to_owned(),
        circuit_breaker: primary
            .circuit_breaker
            .as_ref()
            .map(|cb| CircuitBreakerInfo {
                failure_threshold: cb.failure_threshold,
                cooldown_secs: cb.cooldown_secs,
            }),
    });
    for fp in &state.loaded.config.fallback_providers {
        providers.push(ProviderInfo {
            backend: fp.backend.clone(),
            model: fp.model.clone(),
            role: "fallback".to_owned(),
            circuit_breaker: fp.circuit_breaker.as_ref().map(|cb| CircuitBreakerInfo {
                failure_threshold: cb.failure_threshold,
                cooldown_secs: cb.cooldown_secs,
            }),
        });
    }

    Json(HealthResponse {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        model: format!(
            "{}/{}",
            state.loaded.config.provider.backend, state.loaded.config.provider.model
        ),
        mcp_servers: mcp_count,
        active_schedules,
        total_sessions,
        total_tools: builtin_tools + mcp_tools,
        providers,
    })
}

async fn mcp_status_handler(State(state): State<Arc<AppState>>) -> Json<McpStatusResponse> {
    match &state.mcp {
        Some(mcp) => {
            let status = mcp.server_status().await;
            let total_tools = mcp.tool_count().await;
            let total_resources = mcp.resource_definitions().await.len();
            let total_prompts = mcp.prompt_definitions().await.len();
            let servers = status
                .into_iter()
                .map(|(name, connected)| McpServerStatus { name, connected })
                .collect();
            Json(McpStatusResponse {
                servers,
                total_tools,
                total_resources,
                total_prompts,
            })
        }
        None => Json(McpStatusResponse {
            servers: vec![],
            total_tools: 0,
            total_resources: 0,
            total_prompts: 0,
        }),
    }
}

/// A2A Agent Card — describes this agent's capabilities for discovery.
/// See: <https://github.com/a2aproject/A2A>
async fn agent_card_handler(headers: HeaderMap) -> impl IntoResponse {
    // Derive the A2A service URL from the Host header per the A2A spec
    // (the `url` field must point to the A2A endpoint, not the source repo).
    let url = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|host| format!("https://{host}/.well-known/agent.json"))
        .unwrap_or_else(|| "https://localhost/.well-known/agent.json".to_string());

    Json(serde_json::json!({
        "name": "Eve",
        "description": "Genesis AI agent — a high-performance Rust-based agent harness with 60+ tools, multi-provider LLM support, and cross-platform integration.",
        "url": url,
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": {
            "streaming": true,
            "tools": true,
            "multimodal": true,
            "webhooks": ["telegram", "discord", "slack", "whatsapp", "homeassistant", "signal"],
            "mcp": true
        },
        "defaultInputModes": ["text/plain", "image/png", "image/jpeg"],
        "defaultOutputModes": ["text/plain", "application/json"],
        "skills": [
            {
                "id": "chat",
                "name": "General Chat",
                "description": "Multi-turn conversation with tool use"
            },
            {
                "id": "coding",
                "name": "Code Assistant",
                "description": "Code generation, review, debugging, and refactoring"
            }
        ]
    }))
}

/// Prometheus-compatible metrics endpoint.
///
/// Returns metrics in Prometheus text exposition format (text/plain).
/// No external dependency needed — just formatted strings.
async fn prometheus_metrics_handler(State(state): State<Arc<AppState>>) -> Response {
    let uptime = state.started_at.elapsed().as_secs();
    let requests = state.requests_total.load(Ordering::Relaxed);
    let errors = state.errors_total.load(Ordering::Relaxed);
    let input_tokens = state.input_tokens_total.load(Ordering::Relaxed);
    let output_tokens = state.output_tokens_total.load(Ordering::Relaxed);
    let stream_reqs = state.stream_requests_total.load(Ordering::Relaxed);

    let db_path = &state.loaded.config.storage.database_path;
    let (total_sessions, active_schedules_usize) = fetch_db_stats(db_path);
    let total_sessions = total_sessions as u64;
    let active_schedules = active_schedules_usize as u64;

    let mcp_servers = match &state.mcp {
        Some(mcp) => mcp.server_count().await as u64,
        None => 0,
    };

    let cache_store = genesis_storage::ResponseCacheStore::new(db_path);
    let (cache_entries, cache_hits) = cache_store.stats().unwrap_or((0, 0));

    let model = format!(
        "{}/{}",
        state.loaded.config.provider.backend, state.loaded.config.provider.model
    );

    // Webhook delivery metrics
    let (wh_delivered, wh_retried, wh_failed) = state.webhooks.metrics();

    // Audit log total
    let audit_total: i64 = genesis_storage::AuditLogStore::new(db_path)
        .stats()
        .unwrap_or_default()
        .iter()
        .map(|(_, c)| c)
        .sum();

    // Request duration histogram
    let duration_histogram = if let Ok(hist) = state.request_duration_histogram.lock() {
        hist.format_prometheus(
            "genesis_request_duration_ms",
            "Chat request duration in milliseconds.",
        )
    } else {
        String::new()
    };

    let body = format!(
        "# HELP genesis_uptime_seconds Time since gateway started.\n\
         # TYPE genesis_uptime_seconds gauge\n\
         genesis_uptime_seconds {uptime}\n\
         # HELP genesis_requests_total Total chat requests processed.\n\
         # TYPE genesis_requests_total counter\n\
         genesis_requests_total {requests}\n\
         # HELP genesis_errors_total Total errors returned.\n\
         # TYPE genesis_errors_total counter\n\
         genesis_errors_total {errors}\n\
         # HELP genesis_stream_requests_total Total streaming requests.\n\
         # TYPE genesis_stream_requests_total counter\n\
         genesis_stream_requests_total {stream_reqs}\n\
         # HELP genesis_input_tokens_total Total input tokens processed.\n\
         # TYPE genesis_input_tokens_total counter\n\
         genesis_input_tokens_total {input_tokens}\n\
         # HELP genesis_output_tokens_total Total output tokens generated.\n\
         # TYPE genesis_output_tokens_total counter\n\
         genesis_output_tokens_total {output_tokens}\n\
         # HELP genesis_sessions_total Total sessions in database.\n\
         # TYPE genesis_sessions_total gauge\n\
         genesis_sessions_total {total_sessions}\n\
         # HELP genesis_active_schedules Number of active scheduled tasks.\n\
         # TYPE genesis_active_schedules gauge\n\
         genesis_active_schedules {active_schedules}\n\
         # HELP genesis_mcp_servers Connected MCP server count.\n\
         # TYPE genesis_mcp_servers gauge\n\
         genesis_mcp_servers {mcp_servers}\n\
         # HELP genesis_cache_entries Current response cache entries.\n\
         # TYPE genesis_cache_entries gauge\n\
         genesis_cache_entries {cache_entries}\n\
         # HELP genesis_cache_hits_total Total response cache hits.\n\
         # TYPE genesis_cache_hits_total counter\n\
         genesis_cache_hits_total {cache_hits}\n\
         # HELP genesis_webhook_delivered_total Webhooks successfully delivered.\n\
         # TYPE genesis_webhook_delivered_total counter\n\
         genesis_webhook_delivered_total {wh_delivered}\n\
         # HELP genesis_webhook_retried_total Webhook delivery retries.\n\
         # TYPE genesis_webhook_retried_total counter\n\
         genesis_webhook_retried_total {wh_retried}\n\
         # HELP genesis_webhook_failed_total Webhooks that failed all retries.\n\
         # TYPE genesis_webhook_failed_total counter\n\
         genesis_webhook_failed_total {wh_failed}\n\
         # HELP genesis_audit_entries_total Total audit log entries.\n\
         # TYPE genesis_audit_entries_total gauge\n\
         genesis_audit_entries_total {audit_total}\n\
         {duration_histogram}\
         # HELP genesis_info Build and configuration info.\n\
         # TYPE genesis_info gauge\n\
         genesis_info{{version=\"{version}\",model=\"{model}\"}} 1\n",
        version = env!("CARGO_PKG_VERSION"),
    );

    Response::builder()
        .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
}

/// JSON metrics endpoint for the web dashboard.
///
/// Returns the same counters as the Prometheus endpoint but in a structured
/// JSON format that is easier for browser-based clients to consume.
async fn metrics_json_handler(State(state): State<Arc<AppState>>) -> Json<MetricsJsonResponse> {
    let (total_sessions, active_schedules) =
        fetch_db_stats(&state.loaded.config.storage.database_path);

    Json(MetricsJsonResponse {
        uptime_seconds: state.started_at.elapsed().as_secs(),
        requests_total: state.requests_total.load(Ordering::Relaxed),
        errors_total: state.errors_total.load(Ordering::Relaxed),
        input_tokens_total: state.input_tokens_total.load(Ordering::Relaxed),
        output_tokens_total: state.output_tokens_total.load(Ordering::Relaxed),
        stream_requests_total: state.stream_requests_total.load(Ordering::Relaxed),
        total_sessions,
        active_schedules,
    })
}

/// Query parameters for listing sessions.
#[derive(Debug, Deserialize)]
struct ListSessionsQuery {
    #[serde(default = "default_page_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    search: Option<String>,
}

async fn list_sessions_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListSessionsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = SessionStore::new(&state.loaded.config.storage.database_path);

    let (sessions, total) = if let Some(query) = &params.search {
        store
            .search_sessions_paginated(query, limit, offset)
            .map_err(storage_err)?
    } else {
        store
            .list_recent_sessions_paginated(limit, offset)
            .map_err(storage_err)?
    };

    let has_more = (offset + sessions.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(SessionListResponse {
            sessions,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn get_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let session = store.get_session(&id).map_err(storage_err)?;

    match session {
        Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?)),
        None => Err((StatusCode::NOT_FOUND, format!("session '{id}' not found"))),
    }
}

async fn delete_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let deleted = store.delete_session(&id).map_err(storage_err)?;

    if deleted {
        Ok(Json(serde_json::json!({"deleted": true, "session_id": id})))
    } else {
        Err((StatusCode::NOT_FOUND, format!("session '{id}' not found")))
    }
}

// ---------------------------------------------------------------------------
// Session messages / fork / title / purge / insights endpoints
// ---------------------------------------------------------------------------

/// Request body for forking a session.
#[derive(Debug, Deserialize)]
struct ForkRequest {
    #[serde(default)]
    new_session_id: Option<String>,
}

/// Request body for updating a session title.
#[derive(Debug, Deserialize)]
struct UpdateTitleRequest {
    title: String,
}

/// Query parameters for purging old sessions.
#[derive(Debug, Deserialize)]
struct PurgeQuery {
    #[serde(default = "default_purge_days")]
    older_than_days: u32,
}

fn default_purge_days() -> u32 {
    30
}

/// Query parameters for insights.
#[derive(Debug, Deserialize)]
struct InsightsQuery {
    #[serde(default = "default_insights_days")]
    days: u32,
}

fn default_insights_days() -> u32 {
    30
}

async fn session_messages_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let messages = store.load_messages(&id).map_err(storage_err)?;

    Ok(Json(serde_json::json!({
        "session_id": id,
        "messages": messages,
        "count": messages.len(),
    })))
}

async fn fork_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<ForkRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let new_id = request.new_session_id.unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("fork-{id}-{ts}")
    });

    store.fork_session(&id, &new_id).map_err(storage_err)?;

    Ok(Json(serde_json::json!({
        "source_session_id": id,
        "new_session_id": new_id,
    })))
}

async fn update_session_title_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateTitleRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let updated = store.set_title(&id, &request.title).map_err(storage_err)?;

    Ok(Json(serde_json::json!({
        "session_id": id,
        "title": request.title,
        "updated": updated,
    })))
}

#[derive(Deserialize)]
struct ExportQuery {
    format: Option<String>,
}

async fn export_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<ExportQuery>,
) -> Result<Response, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);

    let session_title = store.get_session(&id).ok().flatten().and_then(|s| s.title);

    let stored = store.load_messages(&id).map_err(storage_err)?;

    if stored.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no messages found for session '{id}'"),
        ));
    }

    let messages: Vec<(String, Option<String>, Option<String>, String)> = stored
        .into_iter()
        .map(|m| (m.role, m.content, m.tool_calls_json, m.created_at))
        .collect();

    let format = params.format.unwrap_or_else(|| "markdown".to_owned());

    use genesis_tools::builtins::export::{
        export_chatml, export_json, export_jsonl, export_markdown,
    };

    let (content, content_type) = match format.as_str() {
        "json" => (
            export_json(&id, session_title.as_deref(), &messages),
            "application/json",
        ),
        "markdown" | "md" => (
            export_markdown(&id, session_title.as_deref(), &messages),
            "text/markdown; charset=utf-8",
        ),
        "chatml" => (export_chatml(&messages), "text/plain; charset=utf-8"),
        "jsonl" | "finetune" => (export_jsonl(&messages), "application/jsonl; charset=utf-8"),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "unsupported format '{format}'; use 'markdown', 'json', 'chatml', or 'jsonl'"
                ),
            ))
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .body(axum::body::Body::from(content))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to build response: {e}"),
            )
        })
}

async fn purge_sessions_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PurgeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let purged = store
        .purge_older_than(params.older_than_days)
        .map_err(storage_err)?;

    Ok(Json(serde_json::json!({
        "purged": purged,
        "older_than_days": params.older_than_days,
    })))
}

async fn get_session_tags_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let tags = store.get_tags(&id).map_err(storage_err)?;
    Ok(Json(serde_json::json!({ "session_id": id, "tags": tags })))
}

#[derive(Deserialize)]
struct SetTagsRequest {
    tags: Vec<String>,
}

async fn set_session_tags_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SetTagsRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let tag_refs: Vec<&str> = request.tags.iter().map(|s| s.as_str()).collect();
    store.set_tags(&id, &tag_refs).map_err(storage_err)?;
    Ok(Json(
        serde_json::json!({ "session_id": id, "tags": request.tags }),
    ))
}

async fn add_session_tag_handler(
    State(state): State<Arc<AppState>>,
    Path((id, tag)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let added = store.add_tag(&id, &tag).map_err(storage_err)?;
    let tags = store.get_tags(&id).map_err(storage_err)?;
    Ok(Json(
        serde_json::json!({ "session_id": id, "tag": tag, "added": added, "tags": tags }),
    ))
}

async fn remove_session_tag_handler(
    State(state): State<Arc<AppState>>,
    Path((id, tag)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let removed = store.remove_tag(&id, &tag).map_err(storage_err)?;
    let tags = store.get_tags(&id).map_err(storage_err)?;
    Ok(Json(
        serde_json::json!({ "session_id": id, "tag": tag, "removed": removed, "tags": tags }),
    ))
}

async fn sessions_by_tag_handler(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let sessions = store.sessions_by_tag(&tag).map_err(storage_err)?;
    Ok(Json(
        serde_json::json!({ "tag": tag, "sessions": sessions, "count": sessions.len() }),
    ))
}

#[derive(Deserialize)]
struct ImportSessionRequest {
    session_id: Option<String>,
    title: Option<String>,
    messages: Vec<ImportMessage>,
}

#[derive(Deserialize)]
struct ImportMessage {
    role: String,
    content: String,
}

async fn import_session_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ImportSessionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);

    let session_id = request.session_id.unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("import-{ts}")
    });

    let message_count = request.messages.len();
    let messages: Vec<(String, String)> = request
        .messages
        .into_iter()
        .map(|m| (m.role, m.content))
        .collect();

    store
        .import_session(&session_id, request.title.as_deref(), messages)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("import error: {e}"),
            )
        })?;

    Ok(Json(serde_json::json!({
        "session_id": session_id,
        "messages_imported": message_count,
    })))
}

#[derive(Deserialize)]
struct BulkExportQuery {
    #[serde(default = "default_bulk_format")]
    format: String,
    #[serde(default)]
    limit: Option<usize>,
}

fn default_bulk_format() -> String {
    "jsonl".to_owned()
}

/// Export multiple sessions as JSONL for fine-tuning or archival.
///
/// Each line is a separate training example with the messages array.
async fn bulk_export_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<BulkExportQuery>,
) -> Result<Response, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);

    let limit = params.limit.unwrap_or(1000);
    let sessions = store.list_recent_sessions(limit).map_err(storage_err)?;

    use genesis_tools::builtins::export::{export_json, export_jsonl};

    let mut output = String::new();

    for session in &sessions {
        let stored = match store.load_messages(&session.id) {
            Ok(msgs) if !msgs.is_empty() => msgs,
            _ => continue,
        };

        let messages: Vec<(String, Option<String>, Option<String>, String)> = stored
            .into_iter()
            .map(|m| (m.role, m.content, m.tool_calls_json, m.created_at))
            .collect();

        match params.format.as_str() {
            "jsonl" | "finetune" => {
                let line = export_jsonl(&messages);
                if !line.is_empty() {
                    output.push_str(&line);
                }
            }
            "json" => {
                let json = export_json(&session.id, session.title.as_deref(), &messages);
                output.push_str(&json);
                output.push('\n');
            }
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!(
                        "unsupported bulk format '{}'; use 'jsonl' or 'json'",
                        params.format
                    ),
                ))
            }
        }
    }

    let content_type = match params.format.as_str() {
        "json" => "application/json; charset=utf-8",
        _ => "application/jsonl; charset=utf-8",
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .body(axum::body::Body::from(output))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to build response: {e}"),
            )
        })
}

#[derive(Deserialize)]
struct SearchMessagesQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    50
}

async fn search_messages_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<SearchMessagesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let results = store
        .search_messages(&params.q, params.limit)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("search error: {e}"),
            )
        })?;

    Ok(Json(serde_json::json!({
        "query": params.q,
        "results": results,
        "count": results.len(),
    })))
}

async fn insights_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<InsightsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let data = store.insights(params.days).map_err(storage_err)?;

    Ok(Json(serde_json::to_value(data).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("serialization error: {e}"),
        )
    })?))
}

async fn usage_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let stats = store.usage_stats().map_err(storage_err)?;

    Ok(Json(serde_json::to_value(stats).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("serialization error: {e}"),
        )
    })?))
}

// ---------------------------------------------------------------------------
// Skills endpoints
// ---------------------------------------------------------------------------

/// Request body for creating/updating a skill.
#[derive(Debug, Deserialize)]
pub(crate) struct UpsertSkillRequest {
    pub name: String,
    pub description: String,
    pub instructions: String,
    #[serde(default)]
    pub trigger_hint: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Query parameters for searching skills by tag.
#[derive(Debug, Deserialize)]
struct SearchSkillsQuery {
    tag: String,
}

#[derive(Debug, Deserialize)]
struct ListSkillsQuery {
    #[serde(default = "default_page_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

async fn list_skills_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListSkillsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let (skills, total) = store
        .list_all_paginated(limit, offset)
        .map_err(storage_err)?;

    let has_more = (offset + skills.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(SkillListResponse {
            skills,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn get_skill_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let skill = store.get(&name).map_err(storage_err)?;

    match skill {
        Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?)),
        None => Err((StatusCode::NOT_FOUND, format!("skill '{name}' not found"))),
    }
}

async fn upsert_skill_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<UpsertSkillRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let tag_refs: Vec<&str> = request.tags.iter().map(|s| s.as_str()).collect();
    let skill = store
        .upsert(
            &request.name,
            &request.description,
            &request.instructions,
            request.trigger_hint.as_deref(),
            &tag_refs,
        )
        .map_err(storage_err)?;

    Ok(Json(serde_json::to_value(skill).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("serialization error: {e}"),
        )
    })?))
}

async fn delete_skill_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let deleted = store.delete(&name).map_err(storage_err)?;

    if deleted {
        Ok(Json(serde_json::json!({"deleted": true, "name": name})))
    } else {
        Err((StatusCode::NOT_FOUND, format!("skill '{name}' not found")))
    }
}

async fn search_skills_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<SearchSkillsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let skills = store.find_by_tag(&params.tag).map_err(storage_err)?;

    let count = skills.len();
    Ok(Json(serde_json::json!({
        "skills": skills,
        "count": count,
    })))
}

// ---------------------------------------------------------------------------
// Memory endpoints
// ---------------------------------------------------------------------------

/// Query parameters for listing memories.
#[derive(Debug, Deserialize)]
struct ListMemoriesQuery {
    #[serde(default = "default_page_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

/// Query parameters for searching memories.
#[derive(Debug, Deserialize)]
struct SearchMemoriesQuery {
    q: String,
    #[serde(default = "default_memory_search_limit")]
    limit: usize,
    /// Search mode: "keyword" (default), "vector", or "hybrid".
    /// Vector and hybrid modes require an embedding provider to be configured.
    #[serde(default)]
    mode: Option<String>,
}

fn default_memory_search_limit() -> usize {
    10
}

fn build_embedding_provider(
    config: &genesis_config::EmbeddingConfig,
) -> Result<genesis_core::embedding::EmbeddingProvider, (StatusCode, String)> {
    genesis_core::embedding::EmbeddingProvider::from_config(config)
        .map_err(|error| embedding_provider_error(config, error))
}

fn embedding_provider_error(
    _config: &genesis_config::EmbeddingConfig,
    error: genesis_core::embedding::EmbeddingError,
) -> (StatusCode, String) {
    #[cfg(not(feature = "local-embeddings"))]
    if _config.is_local_backend()
        && matches!(
            error,
            genesis_core::embedding::EmbeddingError::NotConfigured
        )
    {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "local embedding backend requires the 'local-embeddings' feature; rebuild genesis-gateway with --features local-embeddings to enable it.".to_owned(),
        );
    }

    match error {
        genesis_core::embedding::EmbeddingError::ApiError { status, body } => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            format!("embedding provider error: {body}"),
        ),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("embedding provider error: {other}"),
        ),
    }
}

fn embedding_runtime_error(
    _provider: Option<&genesis_core::embedding::EmbeddingProvider>,
    context: &str,
    error: genesis_core::embedding::EmbeddingError,
) -> (StatusCode, String) {
    #[cfg(not(feature = "local-embeddings"))]
    if let Some(provider) = _provider {
        if provider.backend() == "local"
            && matches!(
                error,
                genesis_core::embedding::EmbeddingError::NotConfigured
            )
        {
            return (
                StatusCode::NOT_IMPLEMENTED,
                "local embedding backend requires the 'local-embeddings' feature; rebuild genesis-gateway with --features local-embeddings to enable it".to_string(),
            );
        }
    }

    match error {
        genesis_core::embedding::EmbeddingError::ApiError { status, body } => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            format!("{context} error: {body}"),
        ),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{context} error: {other}"),
        ),
    }
}

async fn list_memories_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListMemoriesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = MemoryStore::new(&state.loaded.config.storage.database_path);
    let (memories, total) = store.list_paginated(limit, offset).map_err(storage_err)?;

    let has_more = (offset + memories.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(MemoryListResponse {
            memories,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn search_memories_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<SearchMemoriesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db_path = &state.loaded.config.storage.database_path;
    let memory_store = MemoryStore::new(db_path);

    let mode = genesis_core::embedding::SearchMode::from_str_opt(params.mode.as_deref());

    // Build embedding provider only for embedding-backed modes.
    let provider = if matches!(
        mode,
        genesis_core::embedding::SearchMode::Vector | genesis_core::embedding::SearchMode::Hybrid
    ) {
        state.embedding_provider()?
    } else {
        None
    };

    let results = genesis_core::embedding::hybrid_search(
        &params.q,
        params.limit,
        mode,
        &memory_store,
        provider.as_deref(),
    )
    .await
    .map_err(|error| embedding_runtime_error(provider.as_deref(), "search", error))?;

    let count = results.len();
    let mode_str = match mode {
        genesis_core::embedding::SearchMode::Keyword => "keyword",
        genesis_core::embedding::SearchMode::Graph => "graph",
        genesis_core::embedding::SearchMode::Vector => "vector",
        genesis_core::embedding::SearchMode::Hybrid => "hybrid",
    };

    Ok(Json(serde_json::json!({
        "memories": results.iter().map(|r| serde_json::json!({
            "id": r.memory.id,
            "session_id": r.memory.session_id,
            "kind": r.memory.kind,
            "content": r.memory.content,
            "created_at": r.memory.created_at,
            "score": r.score,
            "source": r.source,
        })).collect::<Vec<_>>(),
        "count": count,
        "mode": mode_str,
    })))
}

async fn delete_memory_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db_path = &state.loaded.config.storage.database_path;
    let store = MemoryStore::new(db_path);
    let deleted = store.delete(&id).map_err(storage_err)?;

    if deleted {
        // Also clean up any associated embedding
        if let Err(e) = EmbeddingStore::new(db_path).delete(&id) {
            warn!(memory_id = %id, error = %e, "failed to delete embedding for memory");
        }
        Ok(Json(serde_json::json!({"deleted": true, "id": id})))
    } else {
        Err((StatusCode::NOT_FOUND, format!("memory '{id}' not found")))
    }
}

/// Embed all un-embedded memories. Requires an embedding provider to be configured.
async fn embed_memories_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.loaded.config.embedding.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "no embedding provider configured; add an [embedding] section to config".to_owned(),
        ));
    }

    let provider = state
        .embedding_provider()?
        .expect("embedding config should yield a provider");

    let db_path = &state.loaded.config.storage.database_path;
    let memory_store = MemoryStore::new(db_path);
    let embedding_store = EmbeddingStore::new(db_path);

    let memories = memory_store.list(10000).map_err(storage_err)?;

    let mut embedded = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;
    let mut reset = false;
    let mut first_probe: Option<(String, Vec<f32>)> = None;

    if let (Some(existing_dimensions), Some(first_memory)) = (
        embedding_store.dimensions().map_err(storage_err)?,
        memories.first(),
    ) {
        match provider.embed_one(&first_memory.content).await {
            Ok(embedding) => {
                if embedding.len() != existing_dimensions {
                    embedding_store.clear().map_err(storage_err)?;
                    reset = true;
                    first_probe = Some((first_memory.id.clone(), embedding));
                }
            }
            Err(e) => {
                if provider.backend() == "local"
                    && matches!(e, genesis_core::embedding::EmbeddingError::NotConfigured)
                {
                    return Err(embedding_runtime_error(
                        Some(provider.as_ref()),
                        "bulk embedding",
                        e,
                    ));
                }
                tracing::warn!(memory_id = %first_memory.id, error = %e, "failed to probe embedding dimensions");
                errors += 1;
            }
        }
    }

    for memory in &memories {
        if !reset && embedding_store.has_embedding(&memory.id).unwrap_or(false) {
            skipped += 1;
            continue;
        }

        let result = if first_probe
            .as_ref()
            .is_some_and(|(memory_id, _)| memory_id == &memory.id)
        {
            let (_, embedding) = first_probe.take().expect("probe embedding should exist");
            embedding_store
                .store(&memory.id, &embedding, provider.model())
                .map_err(genesis_core::embedding::EmbeddingError::from)
        } else {
            genesis_core::embedding::embed_and_store(
                &memory.id,
                &memory.content,
                &embedding_store,
                &provider,
                provider.model(),
            )
            .await
        };

        match result {
            Ok(()) => embedded += 1,
            Err(e) => {
                if provider.backend() == "local"
                    && matches!(e, genesis_core::embedding::EmbeddingError::NotConfigured)
                {
                    return Err(embedding_runtime_error(
                        Some(provider.as_ref()),
                        "bulk embedding",
                        e,
                    ));
                }
                tracing::warn!(memory_id = %memory.id, error = %e, "failed to embed memory");
                errors += 1;
            }
        }
    }

    Ok(Json(serde_json::json!({
        "embedded": embedded,
        "skipped": skipped,
        "errors": errors,
        "total": memories.len(),
        "reset": reset,
    })))
}

/// Embed a single memory by ID.
async fn embed_single_memory_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.loaded.config.embedding.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "no embedding provider configured".to_owned(),
        ));
    }

    let provider = state
        .embedding_provider()?
        .expect("embedding config should yield a provider");

    let db_path = &state.loaded.config.storage.database_path;
    let memory_store = MemoryStore::new(db_path);
    let embedding_store = EmbeddingStore::new(db_path);

    // Find the memory by direct ID lookup
    let memory = memory_store
        .get(&id)
        .map_err(storage_err)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("memory '{id}' not found")))?;

    genesis_core::embedding::embed_and_store(
        &memory.id,
        &memory.content,
        &embedding_store,
        &provider,
        provider.model(),
    )
    .await
    .map_err(|error| embedding_runtime_error(Some(provider.as_ref()), "embedding", error))?;

    Ok(Json(serde_json::json!({
        "embedded": true,
        "memory_id": id,
        "model": provider.model(),
    })))
}

// ── Schedule management ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateScheduleRequest {
    pub id: String,
    pub cron_expression: String,
    pub destination: String,
    pub prompt: String,
    /// IANA timezone name (e.g. "America/New_York"). Defaults to UTC.
    #[serde(default)]
    pub timezone: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
struct ListSchedulesQuery {
    #[serde(default)]
    pub enabled_only: bool,
    #[serde(default = "default_page_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

async fn list_schedules_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListSchedulesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let (schedules, total) = store
        .list_paginated(params.enabled_only, limit, offset)
        .map_err(storage_err)?;

    let has_more = (offset + schedules.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(ScheduleListResponse {
            schedules,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn get_schedule_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let schedule = store.get(&id).map_err(storage_err)?;

    match schedule {
        Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?)),
        None => Err((StatusCode::NOT_FOUND, format!("schedule '{id}' not found"))),
    }
}

async fn create_schedule_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateScheduleRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    // Validate cron expression at creation time
    if let Err(e) = genesis_core::scheduler::validate_cron(&request.cron_expression) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("invalid cron expression: {e}"),
        ));
    }

    // Validate timezone if provided
    if let Some(ref tz) = request.timezone {
        if let Err(e) = genesis_core::scheduler::resolve_timezone(Some(tz)) {
            return Err((StatusCode::BAD_REQUEST, e));
        }
    }

    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let schedule = store
        .create_with_timezone(
            &request.id,
            &request.cron_expression,
            &request.destination,
            &request.prompt,
            request.timezone.as_deref(),
        )
        .map_err(storage_err)?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(schedule).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?),
    ))
}

async fn delete_schedule_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let deleted = store.delete(&id).map_err(storage_err)?;

    if deleted {
        Ok(Json(serde_json::json!({"deleted": true, "id": id})))
    } else {
        Err((StatusCode::NOT_FOUND, format!("schedule '{id}' not found")))
    }
}

async fn set_schedule_enabled_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SetEnabledRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let updated = store
        .set_enabled(&id, request.enabled)
        .map_err(storage_err)?;

    if updated {
        Ok(Json(serde_json::json!({
            "id": id,
            "enabled": request.enabled,
            "updated": true,
        })))
    } else {
        Err((StatusCode::NOT_FOUND, format!("schedule '{id}' not found")))
    }
}

// ── User model (traits/preferences) ─────────────────────────────────

#[derive(Debug, Deserialize)]
struct ObserveTraitRequest {
    pub trait_key: String,
    pub category: String,
    pub value: String,
    pub source_session: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListTraitsQuery {
    pub category: Option<String>,
    pub min_confidence: Option<f64>,
    #[serde(default = "default_page_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

async fn list_user_traits_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListTraitsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let (traits_list, total) = store
        .list_paginated(
            params.category.as_deref(),
            params.min_confidence,
            limit,
            offset,
        )
        .map_err(storage_err)?;

    let has_more = (offset + traits_list.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(TraitListResponse {
            traits: traits_list,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn get_user_trait_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let user_trait = store.get(&key).map_err(storage_err)?;

    match user_trait {
        Some(t) => Ok(Json(serde_json::to_value(t).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?)),
        None => Err((StatusCode::NOT_FOUND, format!("trait '{key}' not found"))),
    }
}

async fn observe_user_trait_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ObserveTraitRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let observed = store
        .observe(
            &request.trait_key,
            &request.category,
            &request.value,
            request.source_session.as_deref(),
        )
        .map_err(storage_err)?;

    Ok((
        StatusCode::OK,
        Json(serde_json::to_value(observed).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?),
    ))
}

async fn delete_user_trait_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let deleted = store.delete(&key).map_err(storage_err)?;

    if deleted {
        Ok(Json(serde_json::json!({"deleted": true, "trait_key": key})))
    } else {
        Err((StatusCode::NOT_FOUND, format!("trait '{key}' not found")))
    }
}

// ── Subagents ────────────────────────────────────────────────────────

async fn get_subagent_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SubagentStore::new(&state.loaded.config.storage.database_path);
    let subagent = store.get(&id).map_err(storage_err)?;

    match subagent {
        Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?)),
        None => Err((StatusCode::NOT_FOUND, format!("subagent '{id}' not found"))),
    }
}

async fn list_session_subagents_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SubagentStore::new(&state.loaded.config.storage.database_path);
    let subagents = store.list_by_parent(&id).map_err(storage_err)?;

    let count = subagents.len();
    Ok(Json(serde_json::json!({
        "session_id": id,
        "subagents": subagents,
        "count": count,
    })))
}

// ── Skill usage stats ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SkillUsageRecentQuery {
    #[serde(default = "default_usage_limit")]
    pub limit: usize,
}

fn default_usage_limit() -> usize {
    20
}

async fn skill_usage_stats_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillUsageStore::new(&state.loaded.config.storage.database_path);
    let stats = store.stats(&name).map_err(storage_err)?;

    Ok(Json(serde_json::to_value(stats).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("serialization error: {e}"),
        )
    })?))
}

async fn skill_usage_recent_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SkillUsageRecentQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillUsageStore::new(&state.loaded.config.storage.database_path);
    let usages = store
        .recent_usages(&name, params.limit)
        .map_err(storage_err)?;

    let count = usages.len();
    Ok(Json(serde_json::json!({
        "skill_name": name,
        "usages": usages,
        "count": count,
    })))
}

// ── Tool introspection ───────────────────────────────────────────────

async fn list_tools_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let registry = genesis_tools::default_registry();
    let definitions = registry.definitions();

    let builtin_tools: Vec<serde_json::Value> = definitions
        .iter()
        .map(|def| {
            serde_json::json!({
                "name": def.name,
                "description": def.description,
                "parameters": def.parameters,
                "source": "builtin",
            })
        })
        .collect();

    let mut mcp_tools: Vec<serde_json::Value> = Vec::new();
    if let Some(mcp) = &state.mcp {
        for t in mcp.tool_definitions().await {
            mcp_tools.push(serde_json::json!({
                "name": t.name,
                "description": t.description,
                "source": "mcp",
            }));
        }
    }

    let builtin_count = builtin_tools.len();
    let mcp_count = mcp_tools.len();
    let total = builtin_count + mcp_count;

    Ok(Json(
        serde_json::to_value(ToolListResponse {
            builtin_tools,
            builtin_count,
            mcp_tools,
            mcp_count,
            total,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn config_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = &state.loaded.config;
    Json(serde_json::json!({
        "provider": {
            "backend": config.provider.backend,
            "model": config.provider.model,
        },
        "tool_provider": config.tool_provider.as_ref().map(|tp| serde_json::json!({
            "backend": tp.backend,
            "model": tp.model,
        })),
        "fallback_providers": config.fallback_providers.iter().map(|fp| serde_json::json!({
            "backend": fp.backend,
            "model": fp.model,
        })).collect::<Vec<_>>(),
        "runtime": {
            "max_concurrency": config.runtime.max_concurrency,
            "max_turns": config.runtime.max_turns,
            "max_context_messages": config.runtime.max_context_messages,
            "max_context_tokens": config.runtime.max_context_tokens,
            "max_iterations": config.runtime.max_iterations,
            "budget_limit": config.runtime.budget_limit,
            "allow_destructive_tools": config.runtime.allow_destructive_tools,
            "thinking_budget": config.runtime.thinking_budget,
            "reasoning_effort": config.runtime.reasoning_effort,
            "context_security": format!("{:?}", config.runtime.context_security),
        },
        "gateway": config.gateway.as_ref().map(|g| serde_json::json!({
            "idle_timeout_minutes": g.idle_timeout_minutes,
            "daily_reset_hour": g.daily_reset_hour,
            "rate_limit_rpm": g.rate_limit_rpm,
            "webhooks_count": g.webhooks.len(),
        })),
        "mcp_servers": config.mcp_servers.keys().collect::<Vec<_>>(),
        "profile": config.profile,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn cache_stats_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cache =
        genesis_storage::ResponseCacheStore::new(&state.loaded.config.storage.database_path);
    let (entries, hits) = cache.stats().unwrap_or((0, 0));
    let enabled = state
        .loaded
        .config
        .runtime
        .cache
        .as_ref()
        .is_some_and(|c| c.enabled);
    Json(serde_json::json!({
        "enabled": enabled,
        "entries": entries,
        "total_hits": hits,
    }))
}

async fn cache_clear_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cache =
        genesis_storage::ResponseCacheStore::new(&state.loaded.config.storage.database_path);
    match cache.clear() {
        Ok(deleted) => Json(serde_json::json!({
            "cleared": deleted,
        })),
        Err(e) => Json(serde_json::json!({
            "error": e.to_string(),
        })),
    }
}

// ---------------------------------------------------------------------------
// Audit log endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AuditQueryParams {
    limit: Option<usize>,
    event_type: Option<String>,
}

async fn audit_recent_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuditQueryParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = genesis_storage::AuditLogStore::new(&state.loaded.config.storage.database_path);
    let limit = params.limit.unwrap_or(50);
    let entries = if let Some(ref event_type) = params.event_type {
        store
            .by_event_type(event_type, limit)
            .map_err(storage_err)?
    } else {
        store.recent(limit).map_err(storage_err)?
    };
    Ok(Json(serde_json::json!({
        "entries": entries,
        "count": entries.len(),
    })))
}

async fn audit_stats_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = genesis_storage::AuditLogStore::new(&state.loaded.config.storage.database_path);
    let stats = store.stats().map_err(storage_err)?;
    let total: i64 = stats.iter().map(|(_, c)| c).sum();
    Ok(Json(serde_json::json!({
        "total_entries": total,
        "by_event_type": stats.into_iter().map(|(t, c)| {
            serde_json::json!({"event_type": t, "count": c})
        }).collect::<Vec<_>>(),
    })))
}

async fn audit_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<AuditQueryParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = genesis_storage::AuditLogStore::new(&state.loaded.config.storage.database_path);
    let limit = params.limit.unwrap_or(100);
    let entries = store.by_session(&id, limit).map_err(storage_err)?;
    Ok(Json(serde_json::json!({
        "session_id": id,
        "entries": entries,
        "count": entries.len(),
    })))
}

#[derive(Deserialize)]
struct AuditPurgeRequest {
    older_than_days: Option<u32>,
}

async fn audit_purge_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AuditPurgeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = genesis_storage::AuditLogStore::new(&state.loaded.config.storage.database_path);
    let days = request.older_than_days.unwrap_or(90);
    let deleted = store.purge_older_than(days).map_err(storage_err)?;
    Ok(Json(serde_json::json!({
        "purged": deleted,
        "older_than_days": days,
    })))
}

// ---------------------------------------------------------------------------
// Analytics endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AnalyticsQuery {
    days: Option<u32>,
}

async fn tool_analytics_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AnalyticsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = genesis_storage::AuditLogStore::new(&state.loaded.config.storage.database_path);
    let days = params.days.unwrap_or(30);
    let analytics = store.tool_analytics(days).map_err(storage_err)?;
    Ok(Json(serde_json::json!({
        "period_days": days,
        "tools": analytics,
    })))
}

async fn llm_analytics_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AnalyticsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = genesis_storage::AuditLogStore::new(&state.loaded.config.storage.database_path);
    let days = params.days.unwrap_or(30);
    let analytics = store.llm_analytics(days).map_err(storage_err)?;
    Ok(Json(serde_json::json!({
        "period_days": days,
        "models": analytics,
    })))
}

// ---------------------------------------------------------------------------
// Agent template endpoints
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ListTemplatesQuery {
    #[serde(default = "default_page_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

async fn list_templates_handler(
    axum::extract::Query(params): axum::extract::Query<ListTemplatesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let all_templates = genesis_core::templates::list_templates();
    let total = all_templates.len() as u64;
    let page: Vec<serde_json::Value> = all_templates
        .iter()
        .skip(offset)
        .take(limit)
        .map(|t| serde_json::to_value(t).unwrap_or_default())
        .collect();
    let has_more = (offset + page.len()) < total as usize;

    Ok(Json(
        serde_json::to_value(TemplateListResponse {
            templates: page,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn get_template_handler(
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    match genesis_core::templates::get_template(&name) {
        Some(t) => {
            let prompt = genesis_core::templates::format_template_prompt(t);
            Ok(Json(serde_json::json!({
                "template": t,
                "formatted_prompt": prompt,
            })))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            format!("Template '{name}' not found"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Workflow endpoints
// ---------------------------------------------------------------------------

async fn workflow_validate_handler(
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let yaml = body.get("yaml").and_then(|v| v.as_str()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "Missing 'yaml' field in request body".to_owned(),
        )
    })?;

    let workflow = genesis_core::workflow::parse_workflow(yaml).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to parse workflow: {e}"),
        )
    })?;

    let issues = genesis_core::workflow::validate_workflow(&workflow);
    Ok(Json(serde_json::json!({
        "valid": issues.is_empty(),
        "workflow_name": workflow.name,
        "steps": workflow.steps.len(),
        "issues": issues,
    })))
}

async fn workflow_run_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let yaml = body.get("yaml").and_then(|v| v.as_str()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "Missing 'yaml' field in request body".to_owned(),
        )
    })?;
    let input = body.get("input").and_then(|v| v.as_str()).unwrap_or("");
    let session_id = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("workflow-api");

    let workflow = genesis_core::workflow::parse_workflow(yaml).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to parse workflow: {e}"),
        )
    })?;

    let issues = genesis_core::workflow::validate_workflow(&workflow);
    if !issues.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Workflow validation failed: {}", issues.join("; ")),
        ));
    }

    let service = state.session_service();
    let result = service
        .run_workflow(&workflow, input, session_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Workflow execution failed: {e}"),
            )
        })?;

    Ok(Json(serde_json::json!(result)))
}

// ---------------------------------------------------------------------------
// Agent bus endpoints
// ---------------------------------------------------------------------------

async fn bus_channels_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let channels = state.agent_bus.channels().await;
    Json(serde_json::json!({
        "channels": channels,
        "count": channels.len(),
    }))
}

async fn bus_publish_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let channel = body
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Missing 'channel' field".to_owned(),
            )
        })?;
    let sender = body.get("sender").and_then(|v| v.as_str()).unwrap_or("api");
    let payload = body
        .get("payload")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Missing 'payload' field".to_owned(),
            )
        })?;
    let kind_str = body.get("kind").and_then(|v| v.as_str()).unwrap_or("text");
    let kind: genesis_core::agent_bus::MessageKind =
        serde_json::from_str(&format!("\"{kind_str}\""))
            .unwrap_or(genesis_core::agent_bus::MessageKind::Text);

    let metadata: std::collections::HashMap<String, String> = body
        .get("metadata")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let msg = genesis_core::agent_bus::AgentMessage {
        id: format!("api-{:016x}", {
            use std::hash::{BuildHasher, Hasher};
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let rng = std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish();
            ts as u64 ^ rng
        }),
        channel: channel.to_owned(),
        sender: sender.to_owned(),
        kind,
        payload: payload.to_owned(),
        metadata,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    let subscribers = state.agent_bus.publish(msg.clone()).await;
    Ok(Json(serde_json::json!({
        "published": true,
        "message_id": msg.id,
        "subscribers_notified": subscribers,
    })))
}

async fn bus_history_handler(
    State(state): State<Arc<AppState>>,
    Path(channel): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let messages = state.agent_bus.history(&channel, limit);
    Json(serde_json::json!({
        "channel": channel,
        "messages": messages,
        "count": messages.len(),
    }))
}

async fn bus_stats_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let stats = state.agent_bus.stats();
    let total: i64 = stats.iter().map(|(_, c)| c).sum();
    Json(serde_json::json!({
        "total_messages": total,
        "channels": stats.iter().map(|(ch, count)| {
            serde_json::json!({"channel": ch, "message_count": count})
        }).collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------------------
// Evaluation endpoints
// ---------------------------------------------------------------------------

async fn eval_validate_handler(
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let yaml = body
        .get("yaml")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Missing 'yaml' field".to_owned()))?;

    let suite = genesis_core::eval::parse_suite(yaml).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to parse suite: {e}"),
        )
    })?;

    let issues = genesis_core::eval::validate_suite(&suite);
    Ok(Json(serde_json::json!({
        "valid": issues.is_empty(),
        "suite_name": suite.name,
        "cases": suite.cases.len(),
        "issues": issues,
    })))
}

async fn eval_run_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let yaml = body
        .get("yaml")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Missing 'yaml' field".to_owned()))?;

    let suite = genesis_core::eval::parse_suite(yaml).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to parse suite: {e}"),
        )
    })?;

    let issues = genesis_core::eval::validate_suite(&suite);
    if !issues.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Suite validation failed: {}", issues.join("; ")),
        ));
    }

    let service = state.session_service();
    let report = service.run_eval(&suite).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Eval run failed: {e}"),
        )
    })?;

    Ok(Json(serde_json::json!(report)))
}

// ---------------------------------------------------------------------------
// Guardrails endpoints
// ---------------------------------------------------------------------------

async fn guardrails_check_handler(
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Missing 'text' field".to_owned()))?;
    let direction = body
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("input");

    // Parse config from request body, or use a sensible default
    let config: genesis_core::guardrails::GuardrailConfig = body
        .get("config")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| genesis_core::guardrails::GuardrailConfig {
            detect_pii: true,
            pii_action: genesis_core::guardrails::ViolationAction::Warn,
            ..genesis_core::guardrails::GuardrailConfig::default()
        });

    let result = match direction {
        "output" => genesis_core::guardrails::check_output(&config, text),
        _ => genesis_core::guardrails::check_input(&config, text),
    };

    Ok(Json(serde_json::json!(result)))
}

// ---------------------------------------------------------------------------
// Webhook status endpoints
// ---------------------------------------------------------------------------

async fn webhooks_status_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let (delivered, retried, failed) = state.webhooks.metrics();
    let dead_letter_count = state.webhooks.dead_letters().await.len();
    Json(serde_json::json!({
        "configured": !state.webhooks.is_empty(),
        "delivered": delivered,
        "retried": retried,
        "failed": failed,
        "dead_letter_count": dead_letter_count,
    }))
}

async fn webhooks_dead_letters_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let entries = state.webhooks.dead_letters().await;
    Json(serde_json::json!({
        "entries": entries,
        "count": entries.len(),
    }))
}

async fn webhooks_clear_dead_letters_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let cleared = state.webhooks.clear_dead_letters().await;
    Json(serde_json::json!({
        "cleared": cleared,
    }))
}

async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, String)> {
    let loaded = &state.loaded;
    let mut service = state.session_service();
    if let Some(system_prompt) = request.system_prompt {
        service.set_system_prompt_override(system_prompt);
    }
    if let Some(response_format) = request.response_format {
        service.set_response_format(response_format);
    }
    if let Some(ref model_spec) = request.model {
        let (backend, model) = parse_model_spec(model_spec, &loaded.config.provider.backend);
        service.set_model_override(backend, model);
    }
    let session_id = request.session_id.unwrap_or_else(default_api_session_id);
    let request_id = default_request_id();
    let span = info_span!(
        "gateway.chat",
        request_id = request_id.as_str(),
        session_id = session_id.as_str(),
        platform = request.platform.as_str()
    );
    let images: Vec<genesis_provider::ImageUrl> = request
        .images
        .iter()
        .map(|img| genesis_provider::ImageUrl {
            url: img.url.clone(),
            detail: img.detail.clone(),
        })
        .collect();

    let webhooks = state.webhooks.clone();
    let metrics_state = Arc::clone(&state);
    let request_started = std::time::Instant::now();
    async move {
        info!("received chat request");
        metrics_state.requests_total.fetch_add(1, Ordering::Relaxed);

        // Emit message_received webhook
        webhooks.emit(
            webhooks::WebhookEventType::MessageReceived,
            Some(&session_id),
            Some(&request.platform),
            serde_json::json!({"message_length": request.message.len()}),
        );

        let outcome = service
            .run_turn(SessionTurnInput {
                session_id: &session_id,
                session_platform: &request.platform,
                delivery_platform: delivery_platform_from_str(&request.platform),
                prompt: &request.message,
                title: None,
                images,
            })
            .await
            .map_err(|e| {
                metrics_state.errors_total.fetch_add(1, Ordering::Relaxed);
                // Emit error webhook
                webhooks.emit(
                    webhooks::WebhookEventType::Error,
                    Some(&session_id),
                    Some(&request.platform),
                    serde_json::json!({"error": e.to_string()}),
                );
                error!(
                    request_id = request_id.as_str(),
                    error = %e,
                    "chat request failed"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("execution error: {e}"),
                )
            })?;
        info!(
            request_id = request_id.as_str(),
            turns_used = outcome.result.turns_used,
            tool_calls_made = outcome.result.tool_calls_made,
            "chat request completed"
        );

        // Emit response_sent webhook
        webhooks.emit(
            webhooks::WebhookEventType::ResponseSent,
            Some(&session_id),
            Some(&request.platform),
            serde_json::json!({
                "turns_used": outcome.result.turns_used,
                "tool_calls_made": outcome.result.tool_calls_made,
                "response_length": outcome.result.response.len(),
                "estimated_cost": outcome.result.estimated_cost,
                "input_tokens": outcome.result.total_input_tokens,
                "output_tokens": outcome.result.total_output_tokens,
            }),
        );

        // Append delivery mirror for cross-platform visibility.
        // Use the direct variant since we already have the session ID.
        mirror::append_delivery_mirror_to_session(
            &metrics_state.loaded.config.storage.database_path,
            &session_id,
            &outcome.result.response,
            "api",
        );

        // Record token metrics + duration histogram
        metrics_state
            .input_tokens_total
            .fetch_add(outcome.result.total_input_tokens as u64, Ordering::Relaxed);
        metrics_state
            .output_tokens_total
            .fetch_add(outcome.result.total_output_tokens as u64, Ordering::Relaxed);
        if let Ok(mut hist) = metrics_state.request_duration_histogram.lock() {
            hist.observe(request_started.elapsed().as_millis() as u64);
        }

        Ok(Json(ChatResponse {
            session_id: outcome.session_id,
            response: outcome.result.response,
            turns_used: outcome.result.turns_used,
            tool_calls_made: outcome.result.tool_calls_made,
            estimated_cost: outcome.result.estimated_cost,
            total_input_tokens: outcome.result.total_input_tokens,
            total_output_tokens: outcome.result.total_output_tokens,
            pending_clarification: outcome.result.pending_clarification,
        }))
    }
    .instrument(span)
    .await
}

/// OpenAI-compatible `/v1/chat/completions` endpoint.
///
/// Accepts the standard OpenAI request format and routes the last user message
/// through the Genesis agent, returning a response in OpenAI's format.
/// This allows any OpenAI SDK client to talk to Genesis directly.
async fn openai_chat_completions_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<OpenAiCompletionsRequest>,
) -> Result<Response, (StatusCode, String)> {
    // Extract the last user message as the prompt
    let prompt = request
        .messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "no user message found in messages array".to_owned(),
            )
        })?
        .to_owned();

    // Extract optional system prompt
    let system_prompt = request
        .messages
        .iter()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .map(str::to_owned);

    let session_id = default_api_session_id();
    let request_id = default_request_id();
    let model = request.model.clone();
    let streaming = request.stream.unwrap_or(false);

    let span = info_span!(
        "gateway.openai_compat",
        request_id = request_id.as_str(),
        session_id = session_id.as_str(),
        model = model.as_str(),
        streaming,
    );

    if streaming {
        openai_streaming_response(state, prompt, system_prompt, session_id, model, span).await
    } else {
        openai_blocking_response(state, prompt, system_prompt, session_id, model, span).await
    }
}

/// Non-streaming OpenAI-compatible response.
async fn openai_blocking_response(
    state: Arc<AppState>,
    prompt: String,
    system_prompt: Option<String>,
    session_id: String,
    model: String,
    span: tracing::Span,
) -> Result<Response, (StatusCode, String)> {
    state.requests_total.fetch_add(1, Ordering::Relaxed);

    let mut service = state.session_service();
    if let Some(sp) = system_prompt {
        service.set_system_prompt_override(sp);
    }

    async move {
        info!("received OpenAI-compatible chat completions request");

        let outcome = service
            .run_turn(SessionTurnInput {
                session_id: &session_id,
                session_platform: "api",
                delivery_platform: delivery_platform_from_str("api"),
                prompt: &prompt,
                title: None,
                images: Vec::new(),
            })
            .await
            .map_err(|e| {
                error!(error = %e, "OpenAI-compat request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("execution error: {e}"),
                )
            })?;

        let body = serde_json::json!({
            "id": format!("chatcmpl-{}", session_id),
            "object": "chat.completion",
            "created": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": outcome.result.response,
                },
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": outcome.result.total_input_tokens,
                "completion_tokens": outcome.result.total_output_tokens,
                "total_tokens": outcome.result.total_input_tokens + outcome.result.total_output_tokens,
            },
        });

        Ok(Json(body).into_response())
    }
    .instrument(span)
    .await
}

/// Streaming OpenAI-compatible response (SSE with `data: {...}` chunks).
///
/// Follows the OpenAI streaming format:
/// - Each chunk: `data: {"id":"...","object":"chat.completion.chunk","choices":[{"delta":{"content":"..."}}]}`
/// - Final: `data: [DONE]`
async fn openai_streaming_response(
    state: Arc<AppState>,
    prompt: String,
    system_prompt: Option<String>,
    session_id: String,
    model: String,
    span: tracing::Span,
) -> Result<Response, (StatusCode, String)> {
    state.requests_total.fetch_add(1, Ordering::Relaxed);
    state.stream_requests_total.fetch_add(1, Ordering::Relaxed);

    let (tx, mut rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(SSE_CHANNEL_BUFFER);
    let state_for_task = Arc::clone(&state);
    let session_id_for_task = session_id.clone();
    let model_for_task = model.clone();

    // Shared cancellation flag — set when the client disconnects or on timeout.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_for_task = Arc::clone(&cancelled);

    let timeout_secs = state
        .loaded
        .config
        .gateway
        .as_ref()
        .and_then(|g| g.stream_timeout_secs)
        .unwrap_or(DEFAULT_STREAM_TIMEOUT_SECS);

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let completion_id = format!("chatcmpl-{}", session_id);

    tokio::spawn(async move {
        let cancelled = cancelled_for_task;
        let mut service = state_for_task.session_service();
        if let Some(sp) = system_prompt {
            service.set_system_prompt_override(sp);
        }

        info!("received OpenAI-compatible streaming chat completions request");

        // Send initial role chunk
        let initial_chunk = serde_json::json!({
            "id": &completion_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": &model_for_task,
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "" },
                "finish_reason": null,
            }],
        });
        let initial_data = match serde_json::to_string(&initial_chunk) {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(error = %e, "failed to serialize SSE event");
                String::from(r#"{"id":"error","object":"chat.completion.chunk","choices":[]}"#)
            }
        };
        send_sse(&tx, Ok(Event::default().data(initial_data)), &cancelled);

        let completion_id_for_event = completion_id.clone();
        let model_for_event = model_for_task.clone();
        let tx_for_event = tx.clone();
        let cancelled_cb = Arc::clone(&cancelled);

        let agent_future = service
            .run_turn_streaming(
                SessionTurnInput {
                    session_id: &session_id_for_task,
                    session_platform: "api",
                    delivery_platform: delivery_platform_from_str("api"),
                    prompt: &prompt,
                    title: None,
                    images: Vec::new(),
                },
                |event| {
                    if let StreamEvent::Chunk(chunk) = event {
                        let data = serde_json::json!({
                            "id": &completion_id_for_event,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": &model_for_event,
                            "choices": [{
                                "index": 0,
                                "delta": { "content": chunk },
                                "finish_reason": null,
                            }],
                        });
                        let chunk_data = match serde_json::to_string(&data) {
                            Ok(json) => json,
                            Err(e) => {
                                tracing::error!(error = %e, "failed to serialize SSE event");
                                String::from(r#"{"id":"error","object":"chat.completion.chunk","choices":[]}"#)
                            }
                        };
                        send_sse(
                            &tx_for_event,
                            Ok(Event::default().data(chunk_data)),
                            &cancelled_cb,
                        );
                    }
                },
            );

        let run_result =
            match tokio::time::timeout(Duration::from_secs(timeout_secs), agent_future).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    warn!(timeout_secs, "OpenAI streaming request timed out");
                    // Emit an error chunk so OpenAI-compatible clients know
                    // the response was truncated, then send [DONE].  Without
                    // the error chunk, clients treat timeout as success.
                    let error_event = serde_json::json!({
                        "error": {
                            "message": format!(
                                "Stream timeout exceeded after {timeout_secs}s"
                            ),
                            "type": "timeout",
                        }
                    });
                    if let Ok(payload) = serde_json::to_string(&error_event) {
                        // Use a direct try_send here — we're about to abort
                        // anyway, so blocking is unnecessary.
                        let _ = tx.try_send(Ok(Event::default().data(payload)));
                    }
                    let _ = tx.try_send(Ok(Event::default().data("[DONE]")));
                    cancelled.store(true, Ordering::Relaxed);
                    return;
                }
            };

        // Send finish chunk
        let finish_chunk = serde_json::json!({
            "id": &completion_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": &model_for_task,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
        });
        let finish_data = match serde_json::to_string(&finish_chunk) {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(error = %e, "failed to serialize SSE event");
                String::from(r#"{"id":"error","object":"chat.completion.chunk","choices":[]}"#)
            }
        };
        send_sse(&tx, Ok(Event::default().data(finish_data)), &cancelled);

        // Send usage chunk if we got a successful outcome
        if let Ok(outcome) = run_result {
            let usage_chunk = serde_json::json!({
                "id": &completion_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": &model_for_task,
                "choices": [],
                "usage": {
                    "prompt_tokens": outcome.result.total_input_tokens,
                    "completion_tokens": outcome.result.total_output_tokens,
                    "total_tokens": outcome.result.total_input_tokens + outcome.result.total_output_tokens,
                },
            });
            let usage_data = match serde_json::to_string(&usage_chunk) {
                Ok(json) => json,
                Err(e) => {
                    tracing::error!(error = %e, "failed to serialize SSE event");
                    String::from(r#"{"id":"error","object":"chat.completion.chunk","choices":[]}"#)
                }
            };
            send_sse(&tx, Ok(Event::default().data(usage_data)), &cancelled);
        }

        // Send [DONE] sentinel
        send_sse(
            &tx,
            Ok(Event::default().data("[DONE]")),
            &cancelled,
        );
    }.instrument(span));

    // When the client disconnects, Axum drops the stream future.  The
    // `CancelOnDrop` guard ensures the cancellation flag is set regardless
    // of whether the stream ends naturally or is dropped mid-execution.
    let cancelled_for_stream = Arc::clone(&cancelled);
    let stream = async_stream::stream! {
        let _guard = CancelOnDrop(cancelled_for_stream);
        while let Some(event) = rx.recv().await {
            yield event;
        }
    };

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

// ---------------------------------------------------------------------------
// WebSocket chat endpoint
// ---------------------------------------------------------------------------

/// WebSocket chat handler.
///
/// Accepts a WebSocket connection and processes messages bidirectionally.
/// Client sends JSON: `{"message": "...", "session_id": "...", "platform": "..."}`
/// Server sends JSON events:
/// - `{"type": "chunk", "content": "..."}`
/// - `{"type": "tool_call", "tool": "..."}`
/// - `{"type": "done", "response": "...", "session_id": "...", ...}`
/// - `{"type": "error", "error": "..."}`
async fn websocket_handler(
    State(state): State<Arc<AppState>>,
    ws: axum::extract::WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| websocket_session(state, socket))
}

async fn websocket_session(state: Arc<AppState>, mut socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    info!("WebSocket client connected");

    while let Some(msg) = socket.recv().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue, // Ignore binary/ping/pong
            Err(_) => break,
        };

        // Parse the incoming message
        let request: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::json!({
                    "type": "error",
                    "error": format!("Invalid JSON: {e}"),
                });
                let _ = socket.send(Message::Text(err.to_string().into())).await;
                continue;
            }
        };

        let message = match request.get("message").and_then(|m| m.as_str()) {
            Some(m) => m.to_owned(),
            None => {
                let err = serde_json::json!({
                    "type": "error",
                    "error": "Missing 'message' field",
                });
                let _ = socket.send(Message::Text(err.to_string().into())).await;
                continue;
            }
        };

        let session_id = request
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(str::to_owned)
            .unwrap_or_else(default_api_session_id);
        let platform = request
            .get("platform")
            .and_then(|p| p.as_str())
            .unwrap_or("websocket");
        let system_prompt = request
            .get("system_prompt")
            .and_then(|s| s.as_str())
            .map(str::to_owned);

        state.requests_total.fetch_add(1, Ordering::Relaxed);
        state.stream_requests_total.fetch_add(1, Ordering::Relaxed);

        let mut service = state.session_service();
        if let Some(sp) = system_prompt {
            service.set_system_prompt_override(sp);
        }

        // Collect chunks to send via WebSocket
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let session_id_for_stream = session_id.clone();
        let platform_owned = platform.to_owned();
        let run_result = {
            let tx = tx.clone();
            service
                .run_turn_streaming(
                    SessionTurnInput {
                        session_id: &session_id_for_stream,
                        session_platform: &platform_owned,
                        delivery_platform: delivery_platform_from_str(&platform_owned),
                        prompt: &message,
                        title: None,
                        images: Vec::new(),
                    },
                    move |event| {
                        let json = match event {
                            StreamEvent::Chunk(chunk) => {
                                serde_json::json!({"type": "chunk", "content": chunk})
                            }
                            StreamEvent::TurnStarted => {
                                serde_json::json!({"type": "turn_started"})
                            }
                            StreamEvent::ToolCallStart { name, call_id, args_summary } => {
                                serde_json::json!({"type": "tool_call", "tool": name, "call_id": call_id, "args_summary": args_summary})
                            }
                            StreamEvent::ToolCallEnd { name, call_id, duration_ms, success } => {
                                serde_json::json!({"type": "tool_call_end", "tool": name, "call_id": call_id, "duration_ms": duration_ms, "success": success})
                            }
                            StreamEvent::TokenUsage { input_tokens, output_tokens } => {
                                serde_json::json!({"type": "token_usage", "input_tokens": input_tokens, "output_tokens": output_tokens})
                            }
                            StreamEvent::ClarificationNeeded { question } => {
                                serde_json::json!({"type": "clarification", "question": question})
                            }
                            StreamEvent::Warning(msg) => {
                                serde_json::json!({"type": "warning", "message": msg})
                            }
                        };
                        let _ = tx.send(json.to_string());
                    },
                )
                .await
        };
        drop(tx);

        // Drain buffered events to the WebSocket
        while let Some(event_json) = rx.recv().await {
            if socket.send(Message::Text(event_json.into())).await.is_err() {
                return; // Client disconnected
            }
        }

        // Send final result
        let final_msg = match run_result {
            Ok(outcome) => {
                state
                    .input_tokens_total
                    .fetch_add(outcome.result.total_input_tokens as u64, Ordering::Relaxed);
                state
                    .output_tokens_total
                    .fetch_add(outcome.result.total_output_tokens as u64, Ordering::Relaxed);
                serde_json::json!({
                    "type": "done",
                    "session_id": outcome.session_id,
                    "response": outcome.result.response,
                    "turns_used": outcome.result.turns_used,
                    "tool_calls_made": outcome.result.tool_calls_made,
                    "total_input_tokens": outcome.result.total_input_tokens,
                    "total_output_tokens": outcome.result.total_output_tokens,
                })
            }
            Err(e) => {
                state.errors_total.fetch_add(1, Ordering::Relaxed);
                serde_json::json!({
                    "type": "error",
                    "error": e.to_string(),
                })
            }
        };

        if socket
            .send(Message::Text(final_msg.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }

    info!("WebSocket client disconnected");
}

/// OpenAI-compatible `/v1/models` endpoint.
async fn openai_models_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = &state.loaded.config;
    let model_id = format!("{}/{}", config.provider.backend, config.provider.model);
    Json(serde_json::json!({
        "object": "list",
        "data": [{
            "id": model_id,
            "object": "model",
            "created": 0,
            "owned_by": config.provider.backend,
        }],
    }))
}

#[derive(Debug, Deserialize)]
struct OpenAiCompletionsRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    /// Accepted for OpenAI API compatibility but not yet forwarded to the
    /// underlying provider.  Removing these fields would cause deserialization
    /// errors for clients that send them.
    #[serde(default)]
    #[allow(dead_code)]
    temperature: Option<f64>,
    /// See `temperature` above.
    #[serde(default)]
    #[allow(dead_code)]
    max_tokens: Option<u32>,
    #[serde(default)]
    stream: Option<bool>,
}

async fn chat_stream_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<
    Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, String),
> {
    let session_id = request.session_id.unwrap_or_else(default_api_session_id);
    let request_id = default_request_id();
    info!(
        request_id = request_id.as_str(),
        session_id = session_id.as_str(),
        platform = request.platform.as_str(),
        "accepted streaming chat request"
    );

    let platform = request.platform;
    let message = request.message;
    let system_prompt = request.system_prompt;
    let response_format = request.response_format;
    let images: Vec<genesis_provider::ImageUrl> = request
        .images
        .into_iter()
        .map(|img| genesis_provider::ImageUrl {
            url: img.url,
            detail: img.detail,
        })
        .collect();
    let (tx, mut rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(SSE_CHANNEL_BUFFER);
    let state_for_task = Arc::clone(&state);
    let session_id_for_task = session_id.clone();
    let request_id_for_task = request_id.clone();

    // Shared cancellation flag — set when the client disconnects or on timeout
    // so the agent loop can exit early.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_for_task = Arc::clone(&cancelled);

    let timeout_secs = state
        .loaded
        .config
        .gateway
        .as_ref()
        .and_then(|g| g.stream_timeout_secs)
        .unwrap_or(DEFAULT_STREAM_TIMEOUT_SECS);

    let spawn_span = info_span!(
        "gateway.chat_stream",
        request_id = request_id_for_task.as_str(),
        session_id = session_id_for_task.as_str(),
        platform = platform.as_str()
    );
    tokio::spawn(
        async move {
            let cancelled = cancelled_for_task;
            let mut service = state_for_task.session_service();
            if let Some(system_prompt) = system_prompt {
                service.set_system_prompt_override(system_prompt);
            }
            if let Some(response_format) = response_format {
                service.set_response_format(response_format);
            }
            let initial_payload = serde_json::to_string(&serde_json::json!({
                "session_id": session_id_for_task,
            }));

            if let Ok(payload) = initial_payload {
                send_sse(
                    &tx,
                    Ok(Event::default().event("session").data(payload)),
                    &cancelled,
                );
            }

            let tx_cb = tx.clone();
            let cancelled_cb = Arc::clone(&cancelled);

            let agent_future = service.run_turn_streaming(
                SessionTurnInput {
                    session_id: &session_id,
                    session_platform: &platform,
                    delivery_platform: delivery_platform_from_str(&platform),
                    prompt: &message,
                    title: None,
                    images,
                },
                |event| match event {
                    StreamEvent::Chunk(chunk) => {
                        if let Ok(payload) = serde_json::to_string(&StreamChunkResponse {
                            session_id: session_id.clone(),
                            content: chunk.to_owned(),
                        }) {
                            send_sse(
                                &tx_cb,
                                Ok(Event::default().event("chunk").data(payload)),
                                &cancelled_cb,
                            );
                        }
                    }
                    StreamEvent::ToolCallStart { name, .. } => {
                        if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                            "session_id": &session_id,
                            "tool": name,
                        })) {
                            send_sse(
                                &tx_cb,
                                Ok(Event::default().event("tool_call").data(payload)),
                                &cancelled_cb,
                            );
                        }
                    }
                    StreamEvent::ToolCallEnd { .. }
                    | StreamEvent::TurnStarted
                    | StreamEvent::TokenUsage { .. }
                    | StreamEvent::Warning(_) => {}
                    StreamEvent::ClarificationNeeded { question } => {
                        if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                            "session_id": &session_id,
                            "question": question,
                        })) {
                            send_sse(
                                &tx_cb,
                                Ok(Event::default().event("clarification").data(payload)),
                                &cancelled_cb,
                            );
                        }
                    }
                },
            );

            let run_result =
                match tokio::time::timeout(Duration::from_secs(timeout_secs), agent_future).await {
                    Ok(result) => result,
                    Err(_elapsed) => {
                        warn!(
                            request_id = request_id_for_task.as_str(),
                            timeout_secs, "streaming chat request timed out"
                        );
                        cancelled.store(true, Ordering::Relaxed);
                        if let Ok(payload) = serde_json::to_string(&StreamErrorResponse {
                            session_id,
                            error: format!("streaming request timed out after {timeout_secs}s"),
                        }) {
                            send_sse(
                                &tx,
                                Ok(Event::default().event("error").data(payload)),
                                &cancelled,
                            );
                        }
                        return;
                    }
                };

            match run_result {
                Ok(outcome) => {
                    info!(
                        request_id = request_id_for_task.as_str(),
                        turns_used = outcome.result.turns_used,
                        tool_calls_made = outcome.result.tool_calls_made,
                        "streaming chat request completed"
                    );

                    // Append delivery mirror for cross-platform visibility.
                    // Use the direct variant since we already have the session ID.
                    mirror::append_delivery_mirror_to_session(
                        &state_for_task.loaded.config.storage.database_path,
                        &outcome.session_id,
                        &outcome.result.response,
                        "api",
                    );

                    if let Ok(payload) = serde_json::to_string(&StreamDoneResponse {
                        session_id: outcome.session_id,
                        response: outcome.result.response,
                        turns_used: outcome.result.turns_used,
                        tool_calls_made: outcome.result.tool_calls_made,
                        estimated_cost: outcome.result.estimated_cost,
                        total_input_tokens: outcome.result.total_input_tokens,
                        total_output_tokens: outcome.result.total_output_tokens,
                    }) {
                        send_sse(
                            &tx,
                            Ok(Event::default().event("done").data(payload)),
                            &cancelled,
                        );
                    }
                }
                Err(error) => {
                    error!(
                        request_id = request_id_for_task.as_str(),
                        error = %error,
                        "streaming chat request failed"
                    );
                    if let Ok(payload) = serde_json::to_string(&StreamErrorResponse {
                        session_id,
                        error: error.to_string(),
                    }) {
                        send_sse(
                            &tx,
                            Ok(Event::default().event("error").data(payload)),
                            &cancelled,
                        );
                    }
                }
            }
        }
        .instrument(spawn_span),
    );

    // When the client disconnects, Axum drops the stream future.  The
    // `CancelOnDrop` guard ensures the cancellation flag is set regardless
    // of whether the stream ends naturally or is dropped mid-execution.
    let cancelled_for_stream = Arc::clone(&cancelled);
    let stream = async_stream::stream! {
        let _guard = CancelOnDrop(cancelled_for_stream);
        while let Some(event) = rx.recv().await {
            yield event;
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ---------------------------------------------------------------------------
// Batch chat endpoint
// ---------------------------------------------------------------------------

/// A single prompt within a batch request.
#[derive(Debug, Deserialize)]
pub(crate) struct BatchItem {
    pub message: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    pub session_id: Option<String>,
    #[serde(default)]
    pub images: Vec<ImageInput>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub response_format: Option<genesis_provider::ResponseFormat>,
}

/// Request body for the `/chat/batch` endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct BatchRequest {
    pub items: Vec<BatchItem>,
    /// Maximum concurrent executions (default: 4, max: 16).
    #[serde(default = "default_batch_concurrency")]
    pub concurrency: usize,
}

fn default_batch_concurrency() -> usize {
    4
}

/// Result of a single item in a batch.
#[derive(Debug, Serialize)]
pub(crate) struct BatchItemResult {
    pub index: usize,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub turns_used: usize,
    pub tool_calls_made: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

/// Response body for the `/chat/batch` endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct BatchResponse {
    pub results: Vec<BatchItemResult>,
    pub total_items: usize,
    pub successful: usize,
    pub failed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_estimated_cost: Option<f64>,
}

const MAX_BATCH_CONCURRENCY: usize = 16;
const MAX_BATCH_SIZE: usize = 100;

async fn chat_batch_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BatchRequest>,
) -> Result<Json<BatchResponse>, (StatusCode, String)> {
    if request.items.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "batch must contain at least one item".to_owned(),
        ));
    }
    if request.items.len() > MAX_BATCH_SIZE {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("batch exceeds maximum size of {MAX_BATCH_SIZE} items"),
        ));
    }

    let concurrency = request.concurrency.clamp(1, MAX_BATCH_CONCURRENCY);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let state = Arc::clone(&state);

    let mut handles = Vec::with_capacity(request.items.len());

    for (index, item) in request.items.into_iter().enumerate() {
        let state = Arc::clone(&state);
        let sem = Arc::clone(&semaphore);

        handles.push(tokio::spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(permit) => permit,
                Err(_) => {
                    return BatchItemResult {
                        index,
                        session_id: item.session_id.unwrap_or_else(default_api_session_id),
                        response: None,
                        error: Some("batch semaphore closed".to_string()),
                        turns_used: 0,
                        tool_calls_made: 0,
                        estimated_cost: None,
                        total_input_tokens: 0,
                        total_output_tokens: 0,
                    };
                }
            };

            let mut service = state.session_service();
            if let Some(system_prompt) = item.system_prompt {
                service.set_system_prompt_override(system_prompt);
            }
            if let Some(response_format) = item.response_format {
                service.set_response_format(response_format);
            }

            let session_id = item.session_id.unwrap_or_else(default_api_session_id);
            let images: Vec<genesis_provider::ImageUrl> = item
                .images
                .iter()
                .map(|img| genesis_provider::ImageUrl {
                    url: img.url.clone(),
                    detail: img.detail.clone(),
                })
                .collect();

            match service
                .run_turn(SessionTurnInput {
                    session_id: &session_id,
                    session_platform: &item.platform,
                    delivery_platform: delivery_platform_from_str(&item.platform),
                    prompt: &item.message,
                    title: None,
                    images,
                })
                .await
            {
                Ok(outcome) => BatchItemResult {
                    index,
                    session_id: outcome.session_id,
                    response: Some(outcome.result.response),
                    error: None,
                    turns_used: outcome.result.turns_used,
                    tool_calls_made: outcome.result.tool_calls_made,
                    estimated_cost: outcome.result.estimated_cost,
                    total_input_tokens: outcome.result.total_input_tokens,
                    total_output_tokens: outcome.result.total_output_tokens,
                },
                Err(e) => BatchItemResult {
                    index,
                    session_id,
                    response: None,
                    error: Some(e.to_string()),
                    turns_used: 0,
                    tool_calls_made: 0,
                    estimated_cost: None,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                },
            }
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => {
                error!(error = %e, "batch task panicked");
            }
        }
    }

    // Sort by index to preserve original order
    results.sort_by_key(|r| r.index);

    let total_items = results.len();
    let successful = results.iter().filter(|r| r.error.is_none()).count();
    let failed = total_items - successful;
    let total_estimated_cost: f64 = results.iter().filter_map(|r| r.estimated_cost).sum();

    Ok(Json(BatchResponse {
        results,
        total_items,
        successful,
        failed,
        total_estimated_cost: if total_estimated_cost > 0.0 {
            Some(total_estimated_cost)
        } else {
            None
        },
    }))
}

// ---------------------------------------------------------------------------
// DM Pairing endpoints
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PairingPlatformQuery {
    platform: Option<String>,
    #[serde(default = "default_page_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

#[derive(Debug, Deserialize)]
struct ApprovePairingRequest {
    platform: String,
    code: String,
}

#[derive(Debug, Deserialize)]
struct RevokePairingRequest {
    platform: String,
    user_id: String,
}

#[derive(Debug, Deserialize)]
struct ClearPendingRequest {
    platform: Option<String>,
}

async fn list_approved_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PairingPlatformQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let (approved, total) = store
        .list_approved_paginated(params.platform.as_deref(), limit, offset)
        .map_err(storage_err)?;

    let has_more = (offset + approved.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(ApprovedListResponse {
            approved,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn list_pending_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PairingPlatformQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let (pending, total) = store
        .list_pending_paginated(params.platform.as_deref(), limit, offset)
        .map_err(storage_err)?;

    let has_more = (offset + pending.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(PendingListResponse {
            pending,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

async fn approve_pairing_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ApprovePairingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let approved = store
        .approve_code(&request.platform, &request.code)
        .map_err(storage_err)?;

    match approved {
        Some(user) => Ok(Json(serde_json::json!({
            "approved": true,
            "user": user,
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            "invalid or expired pairing code".to_owned(),
        )),
    }
}

async fn revoke_pairing_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RevokePairingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let revoked = store
        .revoke(&request.platform, &request.user_id)
        .map_err(storage_err)?;

    if revoked {
        Ok(Json(serde_json::json!({
            "revoked": true,
            "platform": request.platform,
            "user_id": request.user_id,
        })))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!(
                "no approved user '{}' on platform '{}'",
                request.user_id, request.platform
            ),
        ))
    }
}

async fn clear_pending_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ClearPendingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let cleared = store
        .clear_pending(request.platform.as_deref())
        .map_err(storage_err)?;

    Ok(Json(serde_json::json!({
        "cleared": cleared,
        "platform": request.platform,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_response_serializes() {
        let resp = HealthResponse {
            status: "ok".to_owned(),
            version: "0.1.0".to_owned(),
            uptime_seconds: 42,
            model: "openai/gpt-4.1-mini".to_owned(),
            mcp_servers: 0,
            active_schedules: 0,
            total_sessions: 5,
            total_tools: 61,
            providers: vec![],
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"uptime_seconds\":42"));
        assert!(json.contains("\"model\":\"openai/gpt-4.1-mini\""));
        assert!(json.contains("\"total_sessions\":5"));
        assert!(json.contains("\"total_tools\":61"));
    }

    #[test]
    fn chat_request_deserializes_minimal() {
        let json = r#"{"message": "hello"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.message, "hello");
        assert_eq!(req.platform, "api");
        assert!(req.session_id.is_none());
    }

    #[test]
    fn chat_request_deserializes_full() {
        let json = r#"{"message": "hi", "platform": "telegram", "session_id": "s-1"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.message, "hi");
        assert_eq!(req.platform, "telegram");
        assert_eq!(req.session_id.as_deref(), Some("s-1"));
        assert!(req.images.is_empty());
    }

    #[test]
    fn chat_request_deserializes_with_images() {
        let json = r#"{
            "message": "What is in this image?",
            "images": [
                {"url": "https://example.com/photo.jpg", "detail": "high"},
                {"url": "data:image/png;base64,iVBOR..."}
            ]
        }"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.images.len(), 2);
        assert_eq!(req.images[0].url, "https://example.com/photo.jpg");
        assert_eq!(req.images[0].detail.as_deref(), Some("high"));
        assert!(req.images[1].detail.is_none());
    }

    #[test]
    fn default_api_session_id_uses_api_prefix() {
        assert!(default_api_session_id().starts_with("api-"));
    }

    #[test]
    fn default_request_id_uses_req_prefix() {
        assert!(default_request_id().starts_with("req-"));
    }

    #[test]
    fn parse_model_spec_with_backend() {
        let (b, m) = parse_model_spec("anthropic/claude-sonnet-4", "openai");
        assert_eq!(b, "anthropic");
        assert_eq!(m, "claude-sonnet-4");
    }

    #[test]
    fn parse_model_spec_without_backend() {
        let (b, m) = parse_model_spec("gpt-4.1-mini", "openai");
        assert_eq!(b, "openai");
        assert_eq!(m, "gpt-4.1-mini");
    }

    #[test]
    fn chat_response_serializes() {
        let resp = ChatResponse {
            session_id: "api-123".to_owned(),
            response: "Hello!".to_owned(),
            turns_used: 1,
            tool_calls_made: 0,
            estimated_cost: Some(0.0042),
            total_input_tokens: 150,
            total_output_tokens: 50,
            pending_clarification: None,
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"session_id\":\"api-123\""));
        assert!(json.contains("\"response\":\"Hello!\""));
        assert!(json.contains("\"estimated_cost\":0.0042"));
        assert!(json.contains("\"total_input_tokens\":150"));
        assert!(json.contains("\"total_output_tokens\":50"));
        assert!(!json.contains("pending_clarification"));
    }

    #[test]
    fn chat_response_includes_clarification_when_present() {
        let resp = ChatResponse {
            session_id: "api-123".to_owned(),
            response: String::new(),
            turns_used: 1,
            tool_calls_made: 1,
            estimated_cost: None,
            total_input_tokens: 100,
            total_output_tokens: 25,
            pending_clarification: Some("Which file do you mean?".to_owned()),
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"pending_clarification\":\"Which file do you mean?\""));
        assert!(!json.contains("estimated_cost"));
    }

    #[test]
    fn build_router_creates_routes() {
        let loaded = genesis_config::load(None).expect("default config should load");
        let state = Arc::new(AppState::new(
            loaded,
            None,
            false,
            None,
            None,
            Vec::new(),
            genesis_core::execution::PluginRuntimeOverrides::default(),
        ));
        let _router = build_router(state);
        // If this doesn't panic, routes were created successfully
    }

    #[test]
    fn stream_chunk_response_serializes() {
        let resp = StreamChunkResponse {
            session_id: "api-123".to_owned(),
            content: "hel".to_owned(),
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"session_id\":\"api-123\""));
        assert!(json.contains("\"content\":\"hel\""));
    }

    #[test]
    fn stream_done_response_serializes() {
        let resp = StreamDoneResponse {
            session_id: "api-123".to_owned(),
            response: "hello".to_owned(),
            turns_used: 1,
            tool_calls_made: 0,
            estimated_cost: Some(0.001),
            total_input_tokens: 100,
            total_output_tokens: 20,
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"response\":\"hello\""));
        assert!(json.contains("\"turns_used\":1"));
        assert!(json.contains("\"total_input_tokens\":100"));
    }

    #[test]
    fn rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::new(5);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..5 {
            assert!(limiter.check(ip), "should allow requests under limit");
        }
    }

    #[test]
    fn rate_limiter_blocks_over_limit() {
        let limiter = RateLimiter::new(3);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(limiter.check(ip));
        assert!(limiter.check(ip));
        assert!(limiter.check(ip));
        assert!(!limiter.check(ip), "4th request should be blocked");
    }

    #[test]
    fn rate_limiter_tracks_ips_independently() {
        let limiter = RateLimiter::new(2);
        let ip_a: IpAddr = "10.0.0.1".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(limiter.check(ip_a));
        assert!(limiter.check(ip_a));
        assert!(!limiter.check(ip_a), "ip_a should be blocked");
        assert!(limiter.check(ip_b), "ip_b should still be allowed");
        assert!(limiter.check(ip_b));
        assert!(!limiter.check(ip_b), "ip_b should now be blocked");
    }

    #[test]
    fn mcp_status_response_serializes_empty() {
        let resp = McpStatusResponse {
            servers: vec![],
            total_tools: 0,
            total_resources: 0,
            total_prompts: 0,
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"servers\":[]"));
        assert!(json.contains("\"total_tools\":0"));
        assert!(json.contains("\"total_resources\":0"));
        assert!(json.contains("\"total_prompts\":0"));
    }

    #[test]
    fn mcp_status_response_serializes_with_servers() {
        let resp = McpStatusResponse {
            servers: vec![
                McpServerStatus {
                    name: "filesystem".to_owned(),
                    connected: true,
                },
                McpServerStatus {
                    name: "github".to_owned(),
                    connected: false,
                },
            ],
            total_tools: 5,
            total_resources: 3,
            total_prompts: 2,
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"filesystem\""));
        assert!(json.contains("\"connected\":true"));
        assert!(json.contains("\"connected\":false"));
        assert!(json.contains("\"total_tools\":5"));
    }

    #[test]
    fn rate_limiter_none_when_no_rpm() {
        let loaded = genesis_config::load(None).expect("default config should load");
        let state = AppState::new(
            loaded,
            None,
            false,
            None,
            None,
            Vec::new(),
            genesis_core::execution::PluginRuntimeOverrides::default(),
        );
        assert!(state.rate_limiter.is_none());
    }

    #[test]
    fn rate_limiter_some_when_rpm_set() {
        let loaded = genesis_config::load(None).expect("default config should load");
        let state = AppState::new(
            loaded,
            None,
            false,
            None,
            Some(60),
            Vec::new(),
            genesis_core::execution::PluginRuntimeOverrides::default(),
        );
        assert!(state.rate_limiter.is_some());
    }

    #[test]
    fn upsert_skill_request_deserializes_minimal() {
        let json =
            r#"{"name": "greet", "description": "Greet the user", "instructions": "Say hello"}"#;
        let req: UpsertSkillRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.name, "greet");
        assert_eq!(req.description, "Greet the user");
        assert_eq!(req.instructions, "Say hello");
        assert!(req.trigger_hint.is_none());
        assert!(req.tags.is_empty());
    }

    #[test]
    fn upsert_skill_request_deserializes_full() {
        let json = r#"{
            "name": "summarize",
            "description": "Summarize text",
            "instructions": "Provide a concise summary",
            "trigger_hint": "summarize this",
            "tags": ["nlp", "text"]
        }"#;
        let req: UpsertSkillRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.name, "summarize");
        assert_eq!(req.trigger_hint.as_deref(), Some("summarize this"));
        assert_eq!(req.tags, vec!["nlp", "text"]);
    }

    #[test]
    fn search_skills_query_deserializes() {
        let json = r#"{"tag": "nlp"}"#;
        let q: SearchSkillsQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.tag, "nlp");
    }

    #[test]
    fn list_memories_query_defaults() {
        let json = r#"{}"#;
        let q: ListMemoriesQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.limit, 50);
    }

    #[test]
    fn list_memories_query_custom_limit() {
        let json = r#"{"limit": 20}"#;
        let q: ListMemoriesQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.limit, 20);
    }

    #[test]
    fn search_memories_query_deserializes() {
        let json = r#"{"q": "hello world"}"#;
        let q: SearchMemoriesQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.q, "hello world");
        assert_eq!(q.limit, 10);
    }

    #[test]
    fn search_memories_query_accepts_graph_mode() {
        let json = r#"{"q": "hello world", "mode": "graph"}"#;
        let q: SearchMemoriesQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.mode.as_deref(), Some("graph"));
    }

    #[test]
    fn search_memories_query_custom_limit() {
        let json = r#"{"q": "test", "limit": 5}"#;
        let q: SearchMemoriesQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.q, "test");
        assert_eq!(q.limit, 5);
    }

    #[test]
    fn chat_request_deserializes_with_system_prompt() {
        let json = r#"{
            "message": "hello",
            "system_prompt": "You are a helpful pirate."
        }"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.message, "hello");
        assert_eq!(
            req.system_prompt.as_deref(),
            Some("You are a helpful pirate.")
        );
        assert!(req.response_format.is_none());
    }

    #[test]
    fn chat_request_deserializes_with_response_format() {
        let json = r#"{
            "message": "give me json",
            "response_format": {"type": "json_object"}
        }"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.message, "give me json");
        assert!(req.response_format.is_some());
        let fmt = req.response_format.unwrap();
        assert!(matches!(fmt, genesis_provider::ResponseFormat::JsonObject));
    }

    #[test]
    fn chat_request_deserializes_with_json_schema() {
        let json = r#"{
            "message": "structured output",
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "my_schema",
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "answer": {"type": "string"}
                        },
                        "required": ["answer"]
                    }
                }
            }
        }"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.message, "structured output");
        match req.response_format {
            Some(genesis_provider::ResponseFormat::JsonSchema { json_schema }) => {
                assert_eq!(json_schema.name, "my_schema");
                assert_eq!(json_schema.strict, Some(true));
                assert!(json_schema.schema["properties"]["answer"]["type"] == "string");
            }
            other => panic!("expected JsonSchema variant, got {:?}", other),
        }
    }

    // --- Batch endpoint tests ---

    #[test]
    fn batch_request_deserializes_minimal() {
        let json = r#"{
            "items": [
                {"message": "hello"},
                {"message": "world"}
            ]
        }"#;
        let req: BatchRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.items.len(), 2);
        assert_eq!(req.items[0].message, "hello");
        assert_eq!(req.items[1].message, "world");
        assert_eq!(req.concurrency, 4); // default
    }

    #[test]
    fn batch_request_deserializes_with_concurrency() {
        let json = r#"{
            "items": [{"message": "hi"}],
            "concurrency": 8
        }"#;
        let req: BatchRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.concurrency, 8);
    }

    #[test]
    fn batch_item_deserializes_full() {
        let json = r#"{
            "message": "analyze this",
            "platform": "telegram",
            "session_id": "batch-1",
            "system_prompt": "Be concise.",
            "response_format": {"type": "json_object"}
        }"#;
        let item: BatchItem = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(item.message, "analyze this");
        assert_eq!(item.platform, "telegram");
        assert_eq!(item.session_id.as_deref(), Some("batch-1"));
        assert!(item.system_prompt.is_some());
        assert!(item.response_format.is_some());
    }

    #[test]
    fn batch_response_serializes() {
        let resp = BatchResponse {
            results: vec![
                BatchItemResult {
                    index: 0,
                    session_id: "s-1".to_owned(),
                    response: Some("Hello!".to_owned()),
                    error: None,
                    turns_used: 1,
                    tool_calls_made: 0,
                    estimated_cost: Some(0.001),
                    total_input_tokens: 100,
                    total_output_tokens: 20,
                },
                BatchItemResult {
                    index: 1,
                    session_id: "s-2".to_owned(),
                    response: None,
                    error: Some("timeout".to_owned()),
                    turns_used: 0,
                    tool_calls_made: 0,
                    estimated_cost: None,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                },
            ],
            total_items: 2,
            successful: 1,
            failed: 1,
            total_estimated_cost: Some(0.001),
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"total_items\":2"));
        assert!(json.contains("\"successful\":1"));
        assert!(json.contains("\"failed\":1"));
        assert!(json.contains("\"response\":\"Hello!\""));
        assert!(json.contains("\"error\":\"timeout\""));
        assert!(!json.contains("\"response\":null")); // skip_serializing_if
    }

    #[test]
    fn batch_response_omits_cost_when_zero() {
        let resp = BatchResponse {
            results: vec![],
            total_items: 0,
            successful: 0,
            failed: 0,
            total_estimated_cost: None,
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(!json.contains("total_estimated_cost"));
    }

    // --- Pairing endpoint request/response tests ---

    #[test]
    fn approve_pairing_request_deserializes() {
        let json = r#"{"platform": "telegram", "code": "ABC12345"}"#;
        let req: ApprovePairingRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.platform, "telegram");
        assert_eq!(req.code, "ABC12345");
    }

    #[test]
    fn revoke_pairing_request_deserializes() {
        let json = r#"{"platform": "discord", "user_id": "123456789"}"#;
        let req: RevokePairingRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.platform, "discord");
        assert_eq!(req.user_id, "123456789");
    }

    #[test]
    fn clear_pending_request_deserializes_with_platform() {
        let json = r#"{"platform": "slack"}"#;
        let req: ClearPendingRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.platform.as_deref(), Some("slack"));
    }

    #[test]
    fn clear_pending_request_deserializes_without_platform() {
        let json = r#"{}"#;
        let req: ClearPendingRequest = serde_json::from_str(json).expect("should deserialize");
        assert!(req.platform.is_none());
    }

    #[test]
    fn pairing_platform_query_deserializes_empty() {
        let json = r#"{}"#;
        let query: PairingPlatformQuery = serde_json::from_str(json).expect("should deserialize");
        assert!(query.platform.is_none());
    }

    #[test]
    fn pairing_platform_query_deserializes_with_platform() {
        let json = r#"{"platform": "whatsapp"}"#;
        let query: PairingPlatformQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(query.platform.as_deref(), Some("whatsapp"));
    }

    #[test]
    fn app_state_metrics_counters_start_at_zero() {
        let config = genesis_config::GenesisConfig {
            schema_version: 1,
            profile: "test".to_owned(),
            provider: genesis_config::ProviderConfig {
                backend: "openai".to_owned(),
                model: "gpt-4.1-mini".to_owned(),
                base_url: None,
                api_key_env: None,
                extra_body: None,
                tool_call_parser: None,
                circuit_breaker: None,
                timeout_secs: None,
            },
            tool_provider: None,
            fallback_providers: Vec::new(),
            mcp_servers: std::collections::HashMap::new(),
            storage: genesis_config::StorageConfig {
                data_dir: std::path::PathBuf::from("/tmp/genesis"),
                database_path: std::path::PathBuf::from("/tmp/genesis/genesis.db"),
            },
            runtime: genesis_config::RuntimeConfig {
                max_concurrency: 4,
                allow_destructive_tools: false,
                max_turns: 20,
                max_context_messages: None,
                budget_limit: None,
                terminal: None,
                thinking_budget: None,
                max_context_tokens: None,
                max_iterations: None,
                context_security: genesis_config::ContextSecurityPolicy::default(),
                reasoning_effort: None,
                cache: None,
                tool_filter: None,
                guardrails: None,
                core_tools: None,
                batch: None,
                tool_policy_path: None,
                approval_mode: genesis_config::ApprovalMode::default(),
                stuck_loop_threshold: genesis_config::DEFAULT_STUCK_LOOP_THRESHOLD,
            },
            gateway: None,
            plugins: genesis_config::PluginsConfig::default(),
            toolsets: std::collections::HashMap::new(),
            personality: None,
            embedding: None,
            display: genesis_config::DisplayConfig::default(),
            tui: genesis_config::TuiConfig::default(),
            telemetry: None,
            routing: None,
        };
        let loaded = genesis_config::LoadedConfig {
            config,
            paths: genesis_config::AppPaths {
                config_path: std::path::PathBuf::from("/tmp/genesis.toml"),
                data_dir: std::path::PathBuf::from("/tmp/genesis"),
                database_path: std::path::PathBuf::from("/tmp/genesis/genesis.db"),
                plugin_dir: std::path::PathBuf::from("/tmp/genesis/plugins"),
            },
        };
        let state = AppState::new(
            loaded,
            None,
            false,
            None,
            None,
            Vec::new(),
            genesis_core::execution::PluginRuntimeOverrides::default(),
        );
        assert_eq!(state.requests_total.load(Ordering::Relaxed), 0);
        assert_eq!(state.errors_total.load(Ordering::Relaxed), 0);
        assert_eq!(state.input_tokens_total.load(Ordering::Relaxed), 0);
        assert_eq!(state.output_tokens_total.load(Ordering::Relaxed), 0);
        assert_eq!(state.stream_requests_total.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn openai_completions_request_deserializes_with_stream() {
        let json = r#"{
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }"#;
        let req: OpenAiCompletionsRequest = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.stream, Some(true));
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn openai_completions_request_stream_defaults_to_none() {
        let json = r#"{
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}]
        }"#;
        let req: OpenAiCompletionsRequest = serde_json::from_str(json).expect("should deserialize");
        assert!(req.stream.is_none());
    }

    /// Build a minimal `AppState` backed by a temp-dir SQLite database.
    ///
    /// Returns both the `Arc<AppState>` and the `TempDir` guard so the caller keeps the
    /// directory alive for the duration of the test.
    ///
    /// Used by router integration tests so they don't touch the real filesystem.
    #[cfg(test)]
    fn create_test_state() -> (Arc<AppState>, tempfile::TempDir) {
        create_test_state_with_key(None, false, None)
    }

    /// Like `create_test_state` but allows configuring API key authentication.
    #[cfg(test)]
    fn create_test_state_with_key(
        api_key: Option<String>,
        api_key_required: bool,
        embedding: Option<genesis_config::EmbeddingConfig>,
    ) -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir should succeed");
        let database_path = dir.path().join("genesis.db");
        // Bootstrap the schema so AgentBus persistence doesn't fail on first access.
        genesis_storage::bootstrap(&database_path).expect("bootstrap should succeed");

        let store = genesis_storage::SessionStore::new(&database_path);
        store
            .create_session("s1", "test", None)
            .expect("session should be created");

        let config = genesis_config::GenesisConfig {
            schema_version: 1,
            profile: "test".to_owned(),
            provider: genesis_config::ProviderConfig {
                backend: "openai".to_owned(),
                model: "gpt-4.1-mini".to_owned(),
                base_url: None,
                api_key_env: None,
                extra_body: None,
                tool_call_parser: None,
                circuit_breaker: None,
                timeout_secs: None,
            },
            tool_provider: None,
            fallback_providers: Vec::new(),
            mcp_servers: std::collections::HashMap::new(),
            storage: genesis_config::StorageConfig {
                data_dir: dir.path().to_path_buf(),
                database_path: database_path.clone(),
            },
            runtime: genesis_config::RuntimeConfig {
                max_concurrency: 4,
                allow_destructive_tools: false,
                max_turns: 20,
                max_context_messages: None,
                budget_limit: None,
                terminal: None,
                thinking_budget: None,
                max_context_tokens: None,
                max_iterations: None,
                context_security: genesis_config::ContextSecurityPolicy::default(),
                reasoning_effort: None,
                cache: None,
                tool_filter: None,
                guardrails: None,
                core_tools: None,
                batch: None,
                tool_policy_path: None,
                approval_mode: genesis_config::ApprovalMode::default(),
                stuck_loop_threshold: genesis_config::DEFAULT_STUCK_LOOP_THRESHOLD,
            },
            gateway: None,
            plugins: genesis_config::PluginsConfig::default(),
            toolsets: std::collections::HashMap::new(),
            personality: None,
            embedding,
            display: genesis_config::DisplayConfig::default(),
            tui: genesis_config::TuiConfig::default(),
            telemetry: None,
            routing: None,
        };
        let loaded = genesis_config::LoadedConfig {
            config,
            paths: genesis_config::AppPaths {
                config_path: dir.path().join("genesis.toml"),
                data_dir: dir.path().to_path_buf(),
                database_path,
                plugin_dir: dir.path().join("plugins"),
            },
        };
        let state = Arc::new(AppState::new(
            loaded,
            api_key,
            api_key_required,
            None,
            None,
            Vec::new(),
            genesis_core::execution::PluginRuntimeOverrides::default(),
        ));
        (state, dir)
    }

    #[cfg(test)]
    fn create_test_state_with_local_embedding() -> (Arc<AppState>, tempfile::TempDir) {
        let embedding = genesis_config::EmbeddingConfig {
            backend: "local".to_owned(),
            model: "sentence-transformers/all-MiniLM-L6-v2".to_owned(),
            base_url: None,
            api_key_env: None,
            dimensions: Some(384),
        };

        let (state, dir) = create_test_state_with_key(None, false, Some(embedding));
        let db_path = dir.path().join("genesis.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem-local', 's1', 'fact', 'genesis memory scaffold', CURRENT_TIMESTAMP)",
            [],
        )
        .expect("seed memory should succeed");

        (state, dir)
    }

    #[tokio::test]
    async fn api_routes_accessible_under_api_prefix() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        // /health at root should return 200
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request should build");
        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("request should succeed");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "/health at root must return 200"
        );

        // /api/health should also return 200
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK, "/api/health must return 200");
    }

    /// Verify that `/api/health` is accessible without authentication even when an API key is
    /// configured, while a protected route like `/api/sessions` correctly returns 401.
    #[tokio::test]
    async fn api_health_is_public_even_with_auth_configured() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_key(Some("test-key".to_string()), true, None);
        let app = build_router(state);

        // /api/health must return 200 with NO Authorization header
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .expect("request should build");
        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("request should succeed");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "/api/health must be reachable without auth even when a key is configured"
        );

        // A protected route must return 401 when no Authorization header is sent
        let req = Request::builder()
            .uri("/api/sessions")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "/api/sessions must require auth when api_key_required is true"
        );
    }

    #[cfg(not(feature = "local-embeddings"))]
    #[tokio::test]
    async fn memories_search_returns_not_implemented_for_local_backend() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_local_embedding();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/memories/search?q=genesis&mode=vector&limit=5")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[cfg(not(feature = "local-embeddings"))]
    #[tokio::test]
    async fn memories_bulk_embed_returns_not_implemented_for_local_backend() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_local_embedding();
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/memories/embed")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[cfg(not(feature = "local-embeddings"))]
    #[tokio::test]
    async fn memories_single_embed_returns_not_implemented_for_local_backend() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_local_embedding();
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/memories/mem-local/embed")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[cfg(feature = "local-embeddings")]
    #[tokio::test]
    async fn memories_search_uses_local_embeddings() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_local_embedding();
        let app = build_router(state);

        let embed_req = Request::builder()
            .method("POST")
            .uri("/api/memories/embed")
            .body(Body::empty())
            .expect("request should build");
        let embed_resp = app
            .clone()
            .oneshot(embed_req)
            .await
            .expect("request should succeed");
        assert_eq!(embed_resp.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/memories/search?q=genesis&mode=vector&limit=5")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["mode"], "vector");
        assert_eq!(json["count"], 1);
        assert_eq!(json["memories"][0]["id"], "mem-local");
        assert_eq!(json["memories"][0]["source"], "vector");
    }

    #[tokio::test]
    async fn memories_search_graph_mode_returns_linked_notes() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use genesis_storage::{MemoryStore, NewMemoryNote};
        use tower::ServiceExt as _;

        let (state, dir) = create_test_state();
        let db_path = dir.path().join("genesis.db");
        let store = MemoryStore::new(&db_path);
        store
            .create_note(NewMemoryNote {
                id: "linked-note".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust ownership model".to_owned(),
                keywords: vec!["rust".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.7,
            })
            .expect("linked note should store");
        store
            .create_note(NewMemoryNote {
                id: "primary-note".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis architecture memory".to_owned(),
                keywords: vec!["genesis".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["linked-note".to_owned()],
                importance: 1.0,
            })
            .expect("primary note should store");

        let app = build_router(state);
        let req = Request::builder()
            .uri("/api/memories/search?q=Genesis&mode=graph&limit=5")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["mode"], "graph");
        assert_eq!(json["count"], 2);
        let ids = json["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"primary-note".to_owned()));
        assert!(ids.contains(&"linked-note".to_owned()));
    }

    #[cfg(feature = "local-embeddings")]
    #[tokio::test]
    async fn memories_bulk_embed_uses_local_embeddings() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_local_embedding();
        let db_path = state.loaded.config.storage.database_path.clone();
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/memories/embed")
            .body(Body::empty())
            .expect("request should build");
        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["embedded"], 1);
        assert_eq!(json["skipped"], 0);
        assert_eq!(json["errors"], 0);
        assert_eq!(json["total"], 1);
        assert_eq!(EmbeddingStore::new(&db_path).count().unwrap(), 1);
    }

    #[cfg(feature = "local-embeddings")]
    #[tokio::test]
    async fn memories_bulk_embed_resets_embeddings_after_dimension_change() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_local_embedding();
        let db_path = state.loaded.config.storage.database_path.clone();
        let embedding_store = EmbeddingStore::new(&db_path);
        embedding_store
            .store("mem-local", &[1.0, 0.0], "legacy-model")
            .expect("legacy embedding should store");
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/memories/embed")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["embedded"], 1);
        assert_eq!(json["skipped"], 0);
        assert_eq!(json["errors"], 0);
        assert_eq!(json["reset"], true);
        assert_eq!(
            embedding_store.dimensions().unwrap(),
            Some(384),
            "bulk embed should rebuild around the active local model dimension"
        );
    }

    #[cfg(feature = "local-embeddings")]
    #[tokio::test]
    async fn memories_single_embed_uses_local_embeddings() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_local_embedding();
        let db_path = state.loaded.config.storage.database_path.clone();
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/memories/mem-local/embed")
            .body(Body::empty())
            .expect("request should build");
        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["embedded"], true);
        assert_eq!(json["memory_id"], "mem-local");
        assert_eq!(json["model"], "sentence-transformers/all-MiniLM-L6-v2");
        assert_eq!(EmbeddingStore::new(&db_path).count().unwrap(), 1);
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn app_state_reuses_shared_local_embedding_provider() {
        let (state, _dir) = create_test_state_with_local_embedding();
        let first = state
            .embedding_provider()
            .expect("provider should initialize")
            .expect("provider should be configured");
        let second = state
            .embedding_provider()
            .expect("provider should initialize")
            .expect("provider should be configured");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[cfg(feature = "local-embeddings")]
    #[tokio::test]
    async fn local_embedding_config_errors_return_bad_request() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let embedding = genesis_config::EmbeddingConfig {
            backend: "local".to_owned(),
            model: "unsupported-local-model".to_owned(),
            base_url: None,
            api_key_env: None,
            dimensions: Some(384),
        };
        let (state, _dir) = create_test_state_with_key(None, false, Some(embedding));
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/memories/embed")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let message = String::from_utf8(body.to_vec()).unwrap();
        assert!(message.contains("unsupported local embedding model"));
    }

    #[test]
    fn get_or_try_init_arc_does_not_cache_failures() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = OnceLock::new();
        let init_lock = Mutex::new(());
        let attempts = AtomicUsize::new(0);

        let first = get_or_try_init_arc(&cache, &init_lock, || -> Result<usize, &'static str> {
            attempts.fetch_add(1, Ordering::Relaxed);
            Err("transient failure")
        });
        assert_eq!(first.unwrap_err(), "transient failure");
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(cache.get().is_none());

        let second = get_or_try_init_arc(&cache, &init_lock, || -> Result<usize, &'static str> {
            attempts.fetch_add(1, Ordering::Relaxed);
            Ok(42)
        })
        .expect("second init should succeed");
        assert_eq!(*second, 42);
        assert_eq!(attempts.load(Ordering::Relaxed), 2);

        let third = get_or_try_init_arc(&cache, &init_lock, || -> Result<usize, &'static str> {
            attempts.fetch_add(1, Ordering::Relaxed);
            Ok(7)
        })
        .expect("cached init should succeed");
        assert_eq!(*third, 42);
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn get_or_try_init_arc_serializes_concurrent_initializers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;
        use std::thread;

        let cache = Arc::new(OnceLock::new());
        let init_lock = Arc::new(Mutex::new(()));
        let attempts = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let mut threads = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let init_lock = Arc::clone(&init_lock);
            let attempts = Arc::clone(&attempts);
            let barrier = Arc::clone(&barrier);
            threads.push(thread::spawn(move || {
                barrier.wait();
                get_or_try_init_arc(&cache, &init_lock, || -> Result<usize, &'static str> {
                    attempts.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    Ok(42)
                })
                .expect("init should succeed")
            }));
        }

        for handle in threads {
            assert_eq!(*handle.join().expect("thread should join"), 42);
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn histogram_buckets_no_double_counting() {
        // Boundaries: 100, 500, 1000
        let buckets: &[u64] = &[100, 500, 1000];
        // SAFETY: we leak a small slice so it lives for 'static, which is fine in tests.
        let static_buckets: &'static [u64] = Box::leak(buckets.to_vec().into_boxed_slice());
        let mut h = HistogramBuckets::new(static_buckets);

        // Observe three values:
        // 50ms  -> fits in buckets le=100, le=500, le=1000
        // 200ms -> fits in buckets le=500, le=1000
        // 800ms -> fits in bucket  le=1000
        h.observe(50);
        h.observe(200);
        h.observe(800);

        let output = h.format_prometheus("test_duration_ms", "Test histogram");

        // Prometheus cumulative buckets should be:
        //   le=100  -> 1  (only 50ms)
        //   le=500  -> 2  (50ms + 200ms)
        //   le=1000 -> 3  (50ms + 200ms + 800ms)
        //   le=+Inf -> 3
        assert!(
            output.contains(r#"test_duration_ms_bucket{le="100"} 1"#),
            "le=100 should be 1, got:\n{output}"
        );
        assert!(
            output.contains(r#"test_duration_ms_bucket{le="500"} 2"#),
            "le=500 should be 2, got:\n{output}"
        );
        assert!(
            output.contains(r#"test_duration_ms_bucket{le="1000"} 3"#),
            "le=1000 should be 3, got:\n{output}"
        );
        assert!(
            output.contains(r#"test_duration_ms_bucket{le="+Inf"} 3"#),
            "le=+Inf should be 3, got:\n{output}"
        );
        assert!(
            output.contains("test_duration_ms_sum 1050"),
            "sum should be 1050, got:\n{output}"
        );
        assert!(
            output.contains("test_duration_ms_count 3"),
            "count should be 3, got:\n{output}"
        );
    }

    #[tokio::test]
    async fn metrics_json_returns_structured_data() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/metrics/json")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json.get("uptime_seconds").is_some());
        assert!(json.get("requests_total").is_some());
        assert!(json.get("errors_total").is_some());
        assert!(json.get("input_tokens_total").is_some());
        assert!(json.get("output_tokens_total").is_some());
    }

    // --- Pagination tests ---

    #[test]
    fn paginated_response_serializes_correctly() {
        let resp = PaginatedResponse {
            items: vec!["a", "b"],
            total: 5,
            limit: 2,
            offset: 0,
            has_more: true,
        };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["items"], serde_json::json!(["a", "b"]));
        assert_eq!(json["total"], 5);
        assert_eq!(json["limit"], 2);
        assert_eq!(json["offset"], 0);
        assert_eq!(json["has_more"], true);
    }

    #[test]
    fn paginated_response_has_more_false_at_end() {
        let resp = PaginatedResponse {
            items: vec![1, 2],
            total: 4,
            limit: 2,
            offset: 2,
            has_more: false,
        };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["has_more"], false);
    }

    #[test]
    fn clamp_limit_enforces_bounds() {
        assert_eq!(clamp_limit(0), 1);
        assert_eq!(clamp_limit(50), 50);
        assert_eq!(clamp_limit(2000), MAX_PAGE_LIMIT);
        assert_eq!(clamp_limit(1000), 1000);
    }

    #[test]
    fn validate_offset_accepts_normal_values() {
        assert_eq!(validate_offset(0).unwrap(), 0);
        assert_eq!(validate_offset(100).unwrap(), 100);
        assert_eq!(validate_offset(MAX_OFFSET).unwrap(), MAX_OFFSET);
    }

    #[test]
    fn validate_offset_rejects_oversized_values() {
        // Values above MAX_OFFSET (i64::MAX as usize) should be rejected.
        // On 64-bit platforms, usize::MAX > i64::MAX.
        #[cfg(target_pointer_width = "64")]
        {
            let result = validate_offset(usize::MAX);
            assert!(result.is_err());
            let (status, _msg) = result.unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn list_sessions_query_defaults_match_pagination() {
        let json = r#"{}"#;
        let q: ListSessionsQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.limit, DEFAULT_PAGE_LIMIT);
        assert_eq!(q.offset, 0);
        assert!(q.search.is_none());
    }

    #[test]
    fn list_sessions_query_accepts_offset() {
        let json = r#"{"limit": 10, "offset": 20}"#;
        let q: ListSessionsQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.limit, 10);
        assert_eq!(q.offset, 20);
    }

    #[test]
    fn list_schedules_query_accepts_pagination() {
        let json = r#"{"enabled_only": true, "limit": 25, "offset": 5}"#;
        let q: ListSchedulesQuery = serde_json::from_str(json).expect("should deserialize");
        assert!(q.enabled_only);
        assert_eq!(q.limit, 25);
        assert_eq!(q.offset, 5);
    }

    #[test]
    fn list_traits_query_accepts_pagination() {
        let json = r#"{"category": "pref", "limit": 10, "offset": 0}"#;
        let q: ListTraitsQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.category.as_deref(), Some("pref"));
        assert_eq!(q.limit, 10);
        assert_eq!(q.offset, 0);
    }

    #[test]
    fn pairing_platform_query_accepts_pagination() {
        let json = r#"{"platform": "telegram", "limit": 10, "offset": 5}"#;
        let q: PairingPlatformQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.platform.as_deref(), Some("telegram"));
        assert_eq!(q.limit, 10);
        assert_eq!(q.offset, 5);
    }

    #[tokio::test]
    async fn list_sessions_returns_paginated_response() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/sessions?limit=10&offset=0")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(
            json.get("sessions").is_some(),
            "response must have 'sessions' field"
        );
        assert!(
            json.get("total").is_some(),
            "response must have 'total' field"
        );
        assert!(
            json.get("limit").is_some(),
            "response must have 'limit' field"
        );
        assert!(
            json.get("offset").is_some(),
            "response must have 'offset' field"
        );
        assert!(
            json.get("has_more").is_some(),
            "response must have 'has_more' field"
        );
    }

    #[tokio::test]
    async fn list_skills_returns_paginated_response() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/skills?limit=5")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json.get("skills").is_some());
        assert!(json.get("total").is_some());
        assert!(json.get("has_more").is_some());
    }

    #[tokio::test]
    async fn list_memories_returns_paginated_response() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/memories?limit=10&offset=0")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json.get("memories").is_some());
        assert!(json.get("total").is_some());
        assert!(json.get("has_more").is_some());
    }

    #[tokio::test]
    async fn list_tools_returns_all_tools_with_legacy_fields() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/tools")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(
            json.get("builtin_tools").is_some(),
            "response must have 'builtin_tools'"
        );
        assert!(
            json.get("mcp_tools").is_some(),
            "response must have 'mcp_tools'"
        );
        assert!(
            json.get("builtin_count").is_some(),
            "response must have 'builtin_count'"
        );
        assert!(
            json.get("mcp_count").is_some(),
            "response must have 'mcp_count'"
        );
        assert!(json.get("total").is_some(), "response must have 'total'");
        // All builtin tools should be returned (no pagination)
        let builtin = json["builtin_tools"].as_array().unwrap();
        assert!(
            builtin.len() >= 50,
            "should return all builtin tools, got {}",
            builtin.len()
        );
        assert_eq!(json["builtin_count"], builtin.len());
        assert_eq!(json["mcp_count"], 0);
    }

    #[tokio::test]
    async fn list_templates_returns_paginated_response() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/templates?limit=50")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json.get("templates").is_some());
        assert!(json.get("total").is_some());
        assert!(json.get("has_more").is_some());
    }

    #[tokio::test]
    async fn list_endpoints_default_params_are_backwards_compatible() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        // Verify that calling endpoints with NO pagination params still works
        let (state, _dir) = create_test_state();
        let app = build_router(state);

        for uri in &[
            "/api/sessions",
            "/api/skills",
            "/api/memories",
            "/api/schedules",
            "/api/user/traits",
            "/api/tools",
            "/api/templates",
            "/api/pairing/approved",
            "/api/pairing/pending",
        ] {
            let req = Request::builder()
                .uri(*uri)
                .body(Body::empty())
                .expect("request should build");
            let resp = app
                .clone()
                .oneshot(req)
                .await
                .expect("request should succeed");
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "{uri} should return 200 with default params"
            );
        }
    }
}
