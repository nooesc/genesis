//! HTTP gateway for the Genesis agent.
//!
//! Exposes a REST API so external services (webhooks, platform bots)
//! can send messages to Eve and receive responses.

pub mod commands;
pub mod platforms;
pub mod verify;
pub mod webhooks;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Response;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use axum::extract::Path;
use genesis_core::agent_loop::StreamEvent;
use genesis_core::execution::{
    delivery_platform_from_str, SessionExecutionService, SessionTurnInput,
};
use genesis_storage::{MemoryStore, PairingStore, ScheduleStore, SessionStore, SkillStore, SkillUsageStore, SubagentStore, UserModelStore};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, info_span, Instrument};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Simple in-memory sliding-window rate limiter keyed by IP address.
///
/// Each entry stores `(request_count, window_start_secs)`.  When a new
/// request arrives and the current timestamp is still within the same
/// 60-second window, the count increments.  Otherwise the window resets.
/// Stale entries (older than 2 minutes) are purged on every check to
/// prevent unbounded memory growth.
#[derive(Debug)]
pub struct RateLimiter {
    /// Max requests per 60-second window.  Stored here so the middleware
    /// doesn't need to re-read `AppState`.
    max_rpm: u32,
    /// Map from IP -> (count, window_start_epoch_secs).
    entries: Mutex<HashMap<IpAddr, (u32, u64)>>,
}

impl RateLimiter {
    pub fn new(max_rpm: u32) -> Self {
        Self {
            max_rpm,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        // Purge stale entries (windows older than 120s)
        map.retain(|_, (_, window_start)| now.saturating_sub(*window_start) < 120);

        let entry = map.entry(ip).or_insert((0, now));
        if now.saturating_sub(entry.1) >= 60 {
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

/// Shared application state for all request handlers.
pub struct AppState {
    pub loaded: genesis_config::LoadedConfig,
    /// Optional API key for gateway authentication.
    /// When set, all non-health requests must include `Authorization: Bearer <key>`.
    pub api_key: Option<String>,
    /// Shared MCP manager for external tool servers (connected at startup).
    pub mcp: Option<std::sync::Arc<genesis_mcp::McpManager>>,
    /// Shared HTTP client for outbound platform API calls (connection pooling).
    pub http_client: reqwest::Client,
    /// Optional per-IP rate limiter.
    pub rate_limiter: Option<RateLimiter>,
    /// Webhook event dispatcher for external notifications.
    pub webhooks: webhooks::WebhookDispatcher,
    /// Timestamp when the gateway started (for uptime reporting).
    pub started_at: std::time::Instant,
}

impl AppState {
    pub fn new(
        loaded: genesis_config::LoadedConfig,
        api_key: Option<String>,
        mcp: Option<std::sync::Arc<genesis_mcp::McpManager>>,
        rate_limit_rpm: Option<u32>,
    ) -> Self {
        let webhook_configs = loaded.config.gateway
            .as_ref()
            .map(|g| g.webhooks.clone())
            .unwrap_or_default();
        Self {
            loaded,
            api_key,
            mcp,
            http_client: reqwest::Client::new(),
            rate_limiter: rate_limit_rpm.map(RateLimiter::new),
            webhooks: webhooks::WebhookDispatcher::new(webhook_configs),
            started_at: std::time::Instant::now(),
        }
    }
}

/// Request body for the `/chat` endpoint.
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
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
pub struct ImageInput {
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

/// Response body from the `/chat` endpoint.
#[derive(Debug, Serialize)]
pub struct ChatResponse {
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
pub struct StreamChunkResponse {
    pub session_id: String,
    pub content: String,
}

/// SSE payload signaling final completion.
#[derive(Debug, Serialize)]
pub struct StreamDoneResponse {
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
pub struct StreamErrorResponse {
    pub session_id: String,
    pub error: String,
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub model: String,
    pub mcp_servers: usize,
    pub active_schedules: usize,
    pub total_sessions: usize,
    pub total_tools: usize,
}

/// Detailed MCP server status response.
#[derive(Debug, Serialize)]
pub struct McpStatusResponse {
    pub servers: Vec<McpServerStatus>,
    pub total_tools: usize,
    pub total_resources: usize,
    pub total_prompts: usize,
}

/// Status of a single MCP server.
#[derive(Debug, Serialize)]
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
}

/// Build the axum Router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Protected routes (require API key when configured)
    let protected = Router::new()
        .route("/chat", post(chat_handler))
        .route("/chat/stream", post(chat_stream_handler))
        .route("/chat/batch", post(chat_batch_handler))
        .route("/sessions", get(list_sessions_handler))
        .route("/sessions/purge", delete(purge_sessions_handler))
        .route("/sessions/import", post(import_session_handler))
        .route("/sessions/export", get(bulk_export_handler))
        .route("/sessions/{id}", get(get_session_handler).delete(delete_session_handler))
        .route("/sessions/{id}/messages", get(session_messages_handler))
        .route("/sessions/{id}/fork", post(fork_session_handler))
        .route("/sessions/{id}/title", patch(update_session_title_handler))
        .route("/sessions/{id}/export", get(export_session_handler))
        .route("/sessions/{id}/tags", get(get_session_tags_handler).put(set_session_tags_handler))
        .route("/sessions/{id}/tags/{tag}", post(add_session_tag_handler).delete(remove_session_tag_handler))
        .route("/sessions/by-tag/{tag}", get(sessions_by_tag_handler))
        .route("/messages/search", get(search_messages_handler))
        .route("/usage", get(usage_handler))
        .route("/insights", get(insights_handler))
        // Skills CRUD
        .route("/skills", get(list_skills_handler).post(upsert_skill_handler))
        .route("/skills/search", get(search_skills_handler))
        .route("/skills/{name}", get(get_skill_handler).delete(delete_skill_handler))
        // Memory endpoints
        .route("/memories", get(list_memories_handler))
        .route("/memories/search", get(search_memories_handler))
        .route("/memories/{id}", delete(delete_memory_handler))
        // Schedule management
        .route("/schedules", get(list_schedules_handler).post(create_schedule_handler))
        .route("/schedules/{id}", get(get_schedule_handler).delete(delete_schedule_handler))
        .route("/schedules/{id}/enabled", patch(set_schedule_enabled_handler))
        // User model (traits/preferences)
        .route("/user/traits", get(list_user_traits_handler).post(observe_user_trait_handler))
        .route("/user/traits/{key}", get(get_user_trait_handler).delete(delete_user_trait_handler))
        // Subagents
        .route("/subagents/{id}", get(get_subagent_handler))
        .route("/sessions/{id}/subagents", get(list_session_subagents_handler))
        // Skill usage stats
        .route("/skills/{name}/usage", get(skill_usage_stats_handler))
        .route("/skills/{name}/usage/recent", get(skill_usage_recent_handler))
        // DM pairing management
        .route("/pairing/approved", get(list_approved_handler))
        .route("/pairing/pending", get(list_pending_handler))
        .route("/pairing/approve", post(approve_pairing_handler))
        .route("/pairing/revoke", post(revoke_pairing_handler))
        .route("/pairing/clear-pending", post(clear_pending_handler))
        // Tool introspection
        .route("/tools", get(list_tools_handler))
        // Config introspection
        .route("/config", get(config_handler))
        // OpenAI-compatible API
        .route("/v1/chat/completions", post(openai_chat_completions_handler))
        .route("/v1/models", get(openai_models_handler))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    // Platform webhook routes (no API key — each platform has its own auth)
    let platform_webhooks = Router::new()
        .route("/telegram/webhook", post(platforms::telegram::webhook_handler))
        .route("/discord/interactions", post(platforms::discord::interactions_handler))
        .route("/slack/events", post(platforms::slack::events_handler))
        .route("/whatsapp/webhook", get(platforms::whatsapp::verify_handler).post(platforms::whatsapp::webhook_handler))
        .route("/homeassistant/webhook", post(platforms::homeassistant::webhook_handler));

    // Rate-limited routes (protected + platform webhooks)
    let rate_limited = Router::new()
        .merge(protected)
        .merge(platform_webhooks)
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            rate_limit_middleware,
        ));

    // Public routes
    Router::new()
        .route("/health", get(health_handler))
        .route("/health/mcp", get(mcp_status_handler))
        .merge(rate_limited)
        .layer(cors)
        .with_state(state)
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected_key = match &state.api_key {
        Some(key) => key,
        None => return Ok(next.run(request).await),
    };

    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let token = auth_header.and_then(|value| {
        let lower = value.get(..7)?;
        if lower.eq_ignore_ascii_case("bearer ") {
            Some(&value[7..])
        } else {
            None
        }
    });

    match token {
        Some(t) if t == expected_key => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Extracts the client IP from the request.
///
/// Checks `X-Forwarded-For` and `X-Real-IP` headers first (for reverse-proxy
/// setups), then falls back to the peer socket address via `ConnectInfo`.
fn client_ip<B>(request: &Request<B>) -> Option<IpAddr> {
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
    // Peer address via ConnectInfo (populated by axum::serve)
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
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
    let ip = client_ip(&request).unwrap_or(IpAddr::from([127, 0, 0, 1]));
    if limiter.check(ip) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::TOO_MANY_REQUESTS)
    }
}

async fn health_handler(
    State(state): State<Arc<AppState>>,
) -> Json<HealthResponse> {
    let mcp_count = match &state.mcp {
        Some(mcp) => mcp.server_count().await,
        None => 0,
    };
    let db_path = &state.loaded.config.storage.database_path;
    let active_schedules = ScheduleStore::new(db_path)
        .list_enabled()
        .map(|s| s.len())
        .unwrap_or(0);
    let total_sessions = SessionStore::new(db_path)
        .session_count()
        .unwrap_or(0) as usize;
    let mcp_tools = match &state.mcp {
        Some(mcp) => mcp.tool_count().await,
        None => 0,
    };
    let builtin_tools = genesis_core::default_tool_count();
    Json(HealthResponse {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        model: format!(
            "{}/{}",
            state.loaded.config.provider.backend,
            state.loaded.config.provider.model
        ),
        mcp_servers: mcp_count,
        active_schedules,
        total_sessions,
        total_tools: builtin_tools + mcp_tools,
    })
}

async fn mcp_status_handler(
    State(state): State<Arc<AppState>>,
) -> Json<McpStatusResponse> {
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

/// Query parameters for listing sessions.
#[derive(Debug, Deserialize)]
struct ListSessionsQuery {
    #[serde(default = "default_session_limit")]
    limit: usize,
    search: Option<String>,
}

fn default_session_limit() -> usize {
    50
}

async fn list_sessions_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListSessionsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let sessions = if let Some(query) = &params.search {
        store.search_sessions(query)
    } else {
        store.list_recent_sessions(params.limit)
    }
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok(Json(serde_json::json!({
        "sessions": sessions,
        "count": sessions.len(),
    })))
}

async fn get_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let session = store
        .get_session(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    match session {
        Some(s) => Ok(Json(serde_json::to_value(s).unwrap())),
        None => Err((StatusCode::NOT_FOUND, format!("session '{id}' not found"))),
    }
}

async fn delete_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let deleted = store
        .delete_session(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
    let messages = store
        .load_messages(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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

    store
        .fork_session(&id, &new_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
    let updated = store
        .set_title(&id, &request.title)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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

    let session_title = store
        .get_session(&id)
        .ok()
        .flatten()
        .and_then(|s| s.title);

    let stored = store
        .load_messages(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
        "jsonl" | "finetune" => (
            export_jsonl(&messages),
            "application/jsonl; charset=utf-8",
        ),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unsupported format '{format}'; use 'markdown', 'json', 'chatml', or 'jsonl'"),
            ))
        }
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .body(axum::body::Body::from(content))
        .unwrap())
}

async fn purge_sessions_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PurgeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let purged = store
        .purge_older_than(params.older_than_days)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
    let tags = store
        .get_tags(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;
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
    store
        .set_tags(&id, &tag_refs)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;
    Ok(Json(serde_json::json!({ "session_id": id, "tags": request.tags })))
}

async fn add_session_tag_handler(
    State(state): State<Arc<AppState>>,
    Path((id, tag)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let added = store
        .add_tag(&id, &tag)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;
    let tags = store
        .get_tags(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;
    Ok(Json(serde_json::json!({ "session_id": id, "tag": tag, "added": added, "tags": tags })))
}

async fn remove_session_tag_handler(
    State(state): State<Arc<AppState>>,
    Path((id, tag)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let removed = store
        .remove_tag(&id, &tag)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;
    let tags = store
        .get_tags(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;
    Ok(Json(serde_json::json!({ "session_id": id, "tag": tag, "removed": removed, "tags": tags })))
}

async fn sessions_by_tag_handler(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let sessions = store
        .sessions_by_tag(&tag)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;
    Ok(Json(serde_json::json!({ "tag": tag, "sessions": sessions, "count": sessions.len() })))
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("import error: {e}")))?;

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
    let sessions = store
        .list_recent_sessions(limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .body(axum::body::Body::from(output))
        .unwrap())
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("search error: {e}")))?;

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
    let data = store
        .insights(params.days)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok(Json(serde_json::to_value(data).unwrap()))
}

async fn usage_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let stats = store
        .usage_stats()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok(Json(serde_json::to_value(stats).unwrap()))
}


// ---------------------------------------------------------------------------
// Skills endpoints
// ---------------------------------------------------------------------------

/// Request body for creating/updating a skill.
#[derive(Debug, Deserialize)]
pub struct UpsertSkillRequest {
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

async fn list_skills_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let skills = store
        .list_all()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    let count = skills.len();
    Ok(Json(serde_json::json!({
        "skills": skills,
        "count": count,
    })))
}

async fn get_skill_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let skill = store
        .get(&name)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    match skill {
        Some(s) => Ok(Json(serde_json::to_value(s).unwrap())),
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok(Json(serde_json::to_value(skill).unwrap()))
}

async fn delete_skill_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let deleted = store
        .delete(&name)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
    let skills = store
        .find_by_tag(&params.tag)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
    #[serde(default = "default_memory_limit")]
    limit: usize,
}

fn default_memory_limit() -> usize {
    50
}

/// Query parameters for searching memories.
#[derive(Debug, Deserialize)]
struct SearchMemoriesQuery {
    q: String,
    #[serde(default = "default_memory_search_limit")]
    limit: usize,
}

fn default_memory_search_limit() -> usize {
    10
}

async fn list_memories_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListMemoriesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = MemoryStore::new(&state.loaded.config.storage.database_path);
    let memories = store
        .list(params.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    let count = memories.len();
    Ok(Json(serde_json::json!({
        "memories": memories,
        "count": count,
    })))
}

async fn search_memories_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<SearchMemoriesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = MemoryStore::new(&state.loaded.config.storage.database_path);
    let memories = store
        .search(&params.q, params.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    let count = memories.len();
    Ok(Json(serde_json::json!({
        "memories": memories,
        "count": count,
    })))
}

async fn delete_memory_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = MemoryStore::new(&state.loaded.config.storage.database_path);
    let deleted = store
        .delete(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    if deleted {
        Ok(Json(serde_json::json!({"deleted": true, "id": id})))
    } else {
        Err((StatusCode::NOT_FOUND, format!("memory '{id}' not found")))
    }
}

// ── Schedule management ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateScheduleRequest {
    pub id: String,
    pub cron_expression: String,
    pub destination: String,
    pub prompt: String,
}

#[derive(Debug, Deserialize)]
struct SetEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
struct ListSchedulesQuery {
    #[serde(default)]
    pub enabled_only: bool,
}

async fn list_schedules_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListSchedulesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let schedules = if params.enabled_only {
        store.list_enabled()
    } else {
        store.list_all()
    }
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    let count = schedules.len();
    Ok(Json(serde_json::json!({
        "schedules": schedules,
        "count": count,
    })))
}

async fn get_schedule_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let schedule = store
        .get(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    match schedule {
        Some(s) => Ok(Json(serde_json::to_value(s).unwrap())),
        None => Err((StatusCode::NOT_FOUND, format!("schedule '{id}' not found"))),
    }
}

async fn create_schedule_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateScheduleRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let schedule = store
        .create(&request.id, &request.cron_expression, &request.destination, &request.prompt)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok((StatusCode::CREATED, Json(serde_json::to_value(schedule).unwrap())))
}

async fn delete_schedule_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = ScheduleStore::new(&state.loaded.config.storage.database_path);
    let deleted = store
        .delete(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
}

async fn list_user_traits_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListTraitsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let traits = if let Some(category) = &params.category {
        store.list_by_category(category)
    } else if let Some(threshold) = params.min_confidence {
        store.confident_traits(threshold)
    } else {
        store.list_all()
    }
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    let count = traits.len();
    Ok(Json(serde_json::json!({
        "traits": traits,
        "count": count,
    })))
}

async fn get_user_trait_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let user_trait = store
        .get(&key)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    match user_trait {
        Some(t) => Ok(Json(serde_json::to_value(t).unwrap())),
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok((StatusCode::OK, Json(serde_json::to_value(observed).unwrap())))
}

async fn delete_user_trait_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let deleted = store
        .delete(&key)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
    let subagent = store
        .get(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    match subagent {
        Some(s) => Ok(Json(serde_json::to_value(s).unwrap())),
        None => Err((StatusCode::NOT_FOUND, format!("subagent '{id}' not found"))),
    }
}

async fn list_session_subagents_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SubagentStore::new(&state.loaded.config.storage.database_path);
    let subagents = store
        .list_by_parent(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
    let stats = store
        .stats(&name)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok(Json(serde_json::to_value(stats).unwrap()))
}

async fn skill_usage_recent_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SkillUsageRecentQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SkillUsageStore::new(&state.loaded.config.storage.database_path);
    let usages = store
        .recent_usages(&name, params.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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

    let tools: Vec<serde_json::Value> = definitions
        .iter()
        .map(|def| {
            serde_json::json!({
                "name": def.name,
                "description": def.description,
                "parameters": def.parameters,
            })
        })
        .collect();

    let count = tools.len();

    // Also include MCP tools if available
    let mcp_tools: Vec<serde_json::Value> = if let Some(mcp) = &state.mcp {
        mcp.tool_definitions()
            .await
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "source": "mcp",
                })
            })
            .collect()
    } else {
        vec![]
    };

    let mcp_count = mcp_tools.len();

    Ok(Json(serde_json::json!({
        "builtin_tools": tools,
        "builtin_count": count,
        "mcp_tools": mcp_tools,
        "mcp_count": mcp_count,
        "total": count + mcp_count,
    })))
}

async fn config_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
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

async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, String)> {
    let loaded = &state.loaded;
    let mut service = SessionExecutionService::new(loaded);
    if let Some(mcp) = &state.mcp {
        service.set_mcp(std::sync::Arc::clone(mcp));
    }
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
    async move {
        info!("received chat request");

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
                (StatusCode::INTERNAL_SERVER_ERROR, format!("execution error: {e}"))
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
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

    let loaded = &state.loaded;
    let mut service = SessionExecutionService::new(loaded);
    if let Some(mcp) = &state.mcp {
        service.set_mcp(std::sync::Arc::clone(mcp));
    }
    if let Some(sp) = system_prompt {
        service.set_system_prompt_override(sp);
    }

    let session_id = default_api_session_id();
    let request_id = default_request_id();
    let model = request.model.clone();
    let span = info_span!(
        "gateway.openai_compat",
        request_id = request_id.as_str(),
        session_id = session_id.as_str(),
        model = model.as_str(),
    );

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

        let finish_reason = if outcome.result.pending_clarification.is_some() {
            "stop"
        } else if outcome.result.tool_calls_made > 0 {
            "stop"
        } else {
            "stop"
        };

        Ok(Json(serde_json::json!({
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
                "finish_reason": finish_reason,
            }],
            "usage": {
                "prompt_tokens": outcome.result.total_input_tokens,
                "completion_tokens": outcome.result.total_output_tokens,
                "total_tokens": outcome.result.total_input_tokens + outcome.result.total_output_tokens,
            },
        })))
    }
    .instrument(span)
    .await
}

/// OpenAI-compatible `/v1/models` endpoint.
async fn openai_models_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
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
    #[serde(default)]
    #[allow(dead_code)]
    temperature: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    max_tokens: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    stream: Option<bool>,
}

async fn chat_stream_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>, (StatusCode, String)>
{
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
    let (tx, mut rx) = mpsc::unbounded_channel::<Result<Event, std::convert::Infallible>>();
    let state_for_task = Arc::clone(&state);
    let session_id_for_task = session_id.clone();
    let request_id_for_task = request_id.clone();

    let spawn_span = info_span!(
        "gateway.chat_stream",
        request_id = request_id_for_task.as_str(),
        session_id = session_id_for_task.as_str(),
        platform = platform.as_str()
    );
    tokio::spawn(async move {
        let mut service = SessionExecutionService::new(&state_for_task.loaded);
        if let Some(mcp) = &state_for_task.mcp {
            service.set_mcp(std::sync::Arc::clone(mcp));
        }
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
            let _ = tx.send(Ok(Event::default().event("session").data(payload)));
        }

        let run_result = service
            .run_turn_streaming(
                SessionTurnInput {
                    session_id: &session_id,
                    session_platform: &platform,
                    delivery_platform: delivery_platform_from_str(&platform),
                    prompt: &message,
                    title: None,
                    images,
                },
                |event| {
                    match event {
                        StreamEvent::Chunk(chunk) => {
                            if let Ok(payload) = serde_json::to_string(&StreamChunkResponse {
                                session_id: session_id.clone(),
                                content: chunk.to_owned(),
                            }) {
                                let _ = tx.send(Ok(Event::default().event("chunk").data(payload)));
                            }
                        }
                        StreamEvent::ToolCallStart { name } => {
                            if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                                "session_id": &session_id,
                                "tool": name,
                            })) {
                                let _ = tx.send(Ok(Event::default().event("tool_call").data(payload)));
                            }
                        }
                        StreamEvent::ToolCallEnd { .. } => {}
                        StreamEvent::ClarificationNeeded { question } => {
                            if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                                "session_id": &session_id,
                                "question": question,
                            })) {
                                let _ = tx.send(Ok(Event::default().event("clarification").data(payload)));
                            }
                        }
                    }
                },
            )
            .await;

        match run_result {
            Ok(outcome) => {
                info!(
                    request_id = request_id_for_task.as_str(),
                    turns_used = outcome.result.turns_used,
                    tool_calls_made = outcome.result.tool_calls_made,
                    "streaming chat request completed"
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
                    let _ = tx.send(Ok(Event::default().event("done").data(payload)));
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
                    let _ = tx.send(Ok(Event::default().event("error").data(payload)));
                }
            }
        }
    }.instrument(spawn_span));

    let stream = async_stream::stream! {
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
pub struct BatchItem {
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
pub struct BatchRequest {
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
pub struct BatchItemResult {
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
pub struct BatchResponse {
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
        return Err((StatusCode::BAD_REQUEST, "batch must contain at least one item".to_owned()));
    }
    if request.items.len() > MAX_BATCH_SIZE {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("batch exceeds maximum size of {MAX_BATCH_SIZE} items"),
        ));
    }

    let concurrency = request.concurrency.min(MAX_BATCH_CONCURRENCY).max(1);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let state = Arc::clone(&state);

    let mut handles = Vec::with_capacity(request.items.len());

    for (index, item) in request.items.into_iter().enumerate() {
        let state = Arc::clone(&state);
        let sem = Arc::clone(&semaphore);

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");

            let mut service = SessionExecutionService::new(&state.loaded);
            if let Some(mcp) = &state.mcp {
                service.set_mcp(std::sync::Arc::clone(mcp));
            }
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
    let total_estimated_cost: f64 = results
        .iter()
        .filter_map(|r| r.estimated_cost)
        .sum();

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
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let users = store
        .list_approved(params.platform.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok(Json(serde_json::json!({
        "approved": users,
        "count": users.len(),
    })))
}

async fn list_pending_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PairingPlatformQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let pending = store
        .list_pending(params.platform.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok(Json(serde_json::json!({
        "pending": pending,
        "count": pending.len(),
    })))
}

async fn approve_pairing_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ApprovePairingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let approved = store
        .approve_code(&request.platform, &request.code)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    match approved {
        Some(user) => Ok(Json(serde_json::json!({
            "approved": true,
            "user": user,
        }))),
        None => Err((StatusCode::NOT_FOUND, "invalid or expired pairing code".to_owned())),
    }
}

async fn revoke_pairing_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RevokePairingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let revoked = store
        .revoke(&request.platform, &request.user_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    if revoked {
        Ok(Json(serde_json::json!({
            "revoked": true,
            "platform": request.platform,
            "user_id": request.user_id,
        })))
    } else {
        Err((StatusCode::NOT_FOUND, format!(
            "no approved user '{}' on platform '{}'",
            request.user_id, request.platform
        )))
    }
}

async fn clear_pending_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ClearPendingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let cleared = store
        .clear_pending(request.platform.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

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
        let state = Arc::new(AppState::new(loaded, None, None, None));
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
                McpServerStatus { name: "filesystem".to_owned(), connected: true },
                McpServerStatus { name: "github".to_owned(), connected: false },
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
        let state = AppState::new(loaded, None, None, None);
        assert!(state.rate_limiter.is_none());
    }

    #[test]
    fn rate_limiter_some_when_rpm_set() {
        let loaded = genesis_config::load(None).expect("default config should load");
        let state = AppState::new(loaded, None, None, Some(60));
        assert!(state.rate_limiter.is_some());
    }

    #[test]
    fn upsert_skill_request_deserializes_minimal() {
        let json = r#"{"name": "greet", "description": "Greet the user", "instructions": "Say hello"}"#;
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
        assert_eq!(req.system_prompt.as_deref(), Some("You are a helpful pirate."));
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
}
