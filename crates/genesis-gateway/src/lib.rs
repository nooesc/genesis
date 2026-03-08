//! HTTP gateway for the Genesis agent.
//!
//! Exposes a REST API so external services (webhooks, platform bots)
//! can send messages to Eve and receive responses.

pub mod commands;
pub mod platforms;
pub mod verify;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum::extract::Path;
use genesis_core::agent_loop::StreamEvent;
use genesis_core::execution::{
    delivery_platform_from_str, SessionExecutionService, SessionTurnInput,
};
use genesis_storage::SessionStore;
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
        Self {
            loaded,
            api_key,
            mcp,
            http_client: reqwest::Client::new(),
            rate_limiter: rate_limit_rpm.map(RateLimiter::new),
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
}

/// Detailed MCP server status response.
#[derive(Debug, Serialize)]
pub struct McpStatusResponse {
    pub servers: Vec<McpServerStatus>,
    pub total_tools: usize,
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
        .route("/sessions", get(list_sessions_handler))
        .route("/sessions/{id}", get(get_session_handler).delete(delete_session_handler))
        .route("/usage", get(usage_handler))
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
    })
}

async fn mcp_status_handler(
    State(state): State<Arc<AppState>>,
) -> Json<McpStatusResponse> {
    match &state.mcp {
        Some(mcp) => {
            let status = mcp.server_status().await;
            let total_tools = mcp.tool_count().await;
            let servers = status
                .into_iter()
                .map(|(name, connected)| McpServerStatus { name, connected })
                .collect();
            Json(McpStatusResponse {
                servers,
                total_tools,
            })
        }
        None => Json(McpStatusResponse {
            servers: vec![],
            total_tools: 0,
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

async fn usage_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    let stats = store
        .usage_stats()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}")))?;

    Ok(Json(serde_json::to_value(stats).unwrap()))
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

    async move {
        info!("received chat request");
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
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"uptime_seconds\":42"));
        assert!(json.contains("\"model\":\"openai/gpt-4.1-mini\""));
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
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"servers\":[]"));
        assert!(json.contains("\"total_tools\":0"));
    }

    #[test]
    fn mcp_status_response_serializes_with_servers() {
        let resp = McpStatusResponse {
            servers: vec![
                McpServerStatus { name: "filesystem".to_owned(), connected: true },
                McpServerStatus { name: "github".to_owned(), connected: false },
            ],
            total_tools: 5,
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
}
