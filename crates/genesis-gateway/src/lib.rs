//! HTTP gateway for the Genesis agent.
//!
//! Exposes a REST API so external services (webhooks, platform bots)
//! can send messages to Eve and receive responses.

pub mod commands;
pub mod middleware;
pub mod mirror;
pub mod platforms;
pub mod routes;
pub mod sse;
pub mod state;
pub mod verify;
pub mod webhooks;

// Re-export public API items so existing consumers don't break.
pub use state::AppState;

use std::sync::Arc;

use axum::routing::{delete, get, patch, post};
use axum::{middleware as axum_middleware, Router};

use crate::middleware::{
    auth_middleware, build_cors_layer, rate_limit_middleware, request_logging_middleware,
};
use crate::routes::*;

// ---------------------------------------------------------------------------
// Shared helpers used by route modules
// ---------------------------------------------------------------------------

/// Internal helper types and functions shared across route modules.
///
/// This module is `pub(crate)` so all route modules can use it via
/// `use crate::helpers::*`.
pub(crate) mod helpers {
    use std::sync::atomic::{AtomicU64, Ordering};

    use axum::http::StatusCode;
    use serde::Serialize;
    use tracing::error;

    pub(crate) static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

    pub(crate) fn default_platform() -> String {
        "api".to_owned()
    }

    pub(crate) fn default_api_session_id() -> String {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let seq = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        format!("api-{ts}-{seq}")
    }

    /// Parse a model spec like "openai/gpt-4.1" or "gpt-4.1" into (backend, model).
    /// If no backend prefix is given, uses the default backend.
    pub(crate) fn parse_model_spec(spec: &str, default_backend: &str) -> (String, String) {
        match spec.split_once('/') {
            Some((backend, model)) => (backend.to_owned(), model.to_owned()),
            None => (default_backend.to_owned(), spec.to_owned()),
        }
    }

    pub(crate) fn default_request_id() -> String {
        let next = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        format!("req-{next}")
    }

    /// Map a storage error into an HTTP 500 response pair.
    pub(crate) fn storage_err(e: impl std::fmt::Display) -> (StatusCode, String) {
        error!(error = %e, "storage operation failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("storage error: {e}"),
        )
    }

    // -----------------------------------------------------------------------
    // Shared pagination types
    // -----------------------------------------------------------------------

    /// Maximum value accepted for `limit`.
    pub(crate) const MAX_PAGE_LIMIT: usize = 1000;

    /// Default page size when the caller does not supply one.
    pub(crate) const DEFAULT_PAGE_LIMIT: usize = 50;

    pub(crate) fn default_page_limit() -> usize {
        DEFAULT_PAGE_LIMIT
    }

    /// Clamp a caller-supplied limit to the valid range `[1, MAX_PAGE_LIMIT]`.
    pub(crate) fn clamp_limit(limit: usize) -> usize {
        limit.clamp(1, MAX_PAGE_LIMIT)
    }

    /// Maximum safe offset value.
    pub(crate) const MAX_OFFSET: usize = i64::MAX as usize;

    /// Validate that the caller-supplied offset is within safe bounds.
    pub(crate) fn validate_offset(offset: usize) -> Result<usize, (StatusCode, String)> {
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

    paginated_response!(SessionListResponse, sessions, genesis_storage::SessionSummary);
    paginated_response!(SkillListResponse, skills, genesis_storage::StoredSkill);
    paginated_response!(MemoryListResponse, memories, genesis_storage::StoredMemory);
    paginated_response!(ScheduleListResponse, schedules, genesis_storage::StoredSchedule);
    paginated_response!(TraitListResponse, traits, genesis_storage::StoredUserTrait);
    paginated_response!(TemplateListResponse, templates, serde_json::Value);
    paginated_response!(ApprovedListResponse, approved, genesis_storage::ApprovedUser);
    paginated_response!(PendingListResponse, pending, genesis_storage::PendingPairing);
}

// ---------------------------------------------------------------------------
// Embedded web UI (optional feature)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Router construction
// ---------------------------------------------------------------------------

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
        .layer(axum_middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    // Root-level protected routes: OpenAI-compatible API and Prometheus metrics.
    let root_protected = Router::new()
        .route(
            "/v1/chat/completions",
            post(openai_chat_completions_handler),
        )
        .route("/v1/models", get(openai_models_handler))
        .route("/metrics", get(prometheus_metrics_handler))
        .layer(axum_middleware::from_fn_with_state(
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
        .layer(axum_middleware::from_fn_with_state(
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

    app.layer(axum_middleware::from_fn(request_logging_middleware))
        .layer(cors)
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::*;
    use crate::state::{HistogramBuckets, get_or_try_init_arc};
    use crate::middleware::RateLimiter;
    use std::net::IpAddr;
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, OnceLock};

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
        assert_eq!(req.concurrency, 4);
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
        assert!(!json.contains("\"response\":null"));
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
    #[cfg(test)]
    fn create_test_state() -> (Arc<AppState>, tempfile::TempDir) {
        create_test_state_with_key(None, false, None)
    }

    #[cfg(test)]
    fn create_test_state_with_key(
        api_key: Option<String>,
        api_key_required: bool,
        embedding: Option<genesis_config::EmbeddingConfig>,
    ) -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir should succeed");
        let database_path = dir.path().join("genesis.db");
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

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request should build");
        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK, "/health at root must return 200");

        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK, "/api/health must return 200");
    }

    #[tokio::test]
    async fn api_health_is_public_even_with_auth_configured() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_key(Some("test-key".to_string()), true, None);
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .expect("request should build");
        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/sessions")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
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
        let embed_resp = app.clone().oneshot(embed_req).await.expect("request should succeed");
        assert_eq!(embed_resp.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/memories/search?q=genesis&mode=vector&limit=5")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
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
        store.create_note(NewMemoryNote {
            id: "linked-note".to_owned(), session_id: Some("s1".to_owned()),
            kind: "fact".to_owned(), content: "Rust ownership model".to_owned(),
            keywords: vec!["rust".to_owned()], tags: vec!["language".to_owned()],
            linked_ids: vec![], importance: 0.7,
        }).expect("linked note should store");
        store.create_note(NewMemoryNote {
            id: "primary-note".to_owned(), session_id: Some("s1".to_owned()),
            kind: "fact".to_owned(), content: "Genesis architecture memory".to_owned(),
            keywords: vec!["genesis".to_owned()], tags: vec!["architecture".to_owned()],
            linked_ids: vec!["linked-note".to_owned()], importance: 1.0,
        }).expect("primary note should store");

        let app = build_router(state);
        let req = Request::builder()
            .uri("/api/memories/search?q=Genesis&mode=graph&limit=5")
            .body(Body::empty())
            .expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["mode"], "graph");
        assert_eq!(json["count"], 2);
        let ids = json["memories"].as_array().unwrap().iter()
            .map(|item| item["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
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

        let req = Request::builder().method("POST").uri("/api/memories/embed")
            .body(Body::empty()).expect("request should build");
        let resp = app.clone().oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["embedded"], 1);
        assert_eq!(json["skipped"], 0);
        assert_eq!(json["errors"], 0);
        assert_eq!(json["total"], 1);
        assert_eq!(genesis_storage::EmbeddingStore::new(&db_path).count().unwrap(), 1);
    }

    #[cfg(feature = "local-embeddings")]
    #[tokio::test]
    async fn memories_bulk_embed_resets_embeddings_after_dimension_change() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state_with_local_embedding();
        let db_path = state.loaded.config.storage.database_path.clone();
        let embedding_store = genesis_storage::EmbeddingStore::new(&db_path);
        embedding_store.store("mem-local", &[1.0, 0.0], "legacy-model").expect("legacy embedding should store");
        let app = build_router(state);

        let req = Request::builder().method("POST").uri("/api/memories/embed")
            .body(Body::empty()).expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["embedded"], 1);
        assert_eq!(json["reset"], true);
        assert_eq!(embedding_store.dimensions().unwrap(), Some(384));
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

        let req = Request::builder().method("POST").uri("/api/memories/mem-local/embed")
            .body(Body::empty()).expect("request should build");
        let resp = app.clone().oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["embedded"], true);
        assert_eq!(json["memory_id"], "mem-local");
        assert_eq!(json["model"], "sentence-transformers/all-MiniLM-L6-v2");
        assert_eq!(genesis_storage::EmbeddingStore::new(&db_path).count().unwrap(), 1);
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn app_state_reuses_shared_local_embedding_provider() {
        let (state, _dir) = create_test_state_with_local_embedding();
        let first = state.embedding_provider().expect("provider should initialize").expect("provider should be configured");
        let second = state.embedding_provider().expect("provider should initialize").expect("provider should be configured");
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
            base_url: None, api_key_env: None, dimensions: Some(384),
        };
        let (state, _dir) = create_test_state_with_key(None, false, Some(embedding));
        let app = build_router(state);

        let req = Request::builder().method("POST").uri("/api/memories/embed")
            .body(Body::empty()).expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
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
        }).expect("second init should succeed");
        assert_eq!(*second, 42);
        assert_eq!(attempts.load(Ordering::Relaxed), 2);

        let third = get_or_try_init_arc(&cache, &init_lock, || -> Result<usize, &'static str> {
            attempts.fetch_add(1, Ordering::Relaxed);
            Ok(7)
        }).expect("cached init should succeed");
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
                }).expect("init should succeed")
            }));
        }
        for handle in threads {
            assert_eq!(*handle.join().expect("thread should join"), 42);
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn histogram_buckets_no_double_counting() {
        let buckets: &[u64] = &[100, 500, 1000];
        let static_buckets: &'static [u64] = Box::leak(buckets.to_vec().into_boxed_slice());
        let mut h = HistogramBuckets::new(static_buckets);
        h.observe(50);
        h.observe(200);
        h.observe(800);
        let output = h.format_prometheus("test_duration_ms", "Test histogram");
        assert!(output.contains(r#"test_duration_ms_bucket{le="100"} 1"#));
        assert!(output.contains(r#"test_duration_ms_bucket{le="500"} 2"#));
        assert!(output.contains(r#"test_duration_ms_bucket{le="1000"} 3"#));
        assert!(output.contains(r#"test_duration_ms_bucket{le="+Inf"} 3"#));
        assert!(output.contains("test_duration_ms_sum 1050"));
        assert!(output.contains("test_duration_ms_count 3"));
    }

    #[tokio::test]
    async fn metrics_json_returns_structured_data() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        let req = Request::builder().uri("/api/metrics/json").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("uptime_seconds").is_some());
        assert!(json.get("requests_total").is_some());
        assert!(json.get("errors_total").is_some());
        assert!(json.get("input_tokens_total").is_some());
        assert!(json.get("output_tokens_total").is_some());
    }

    #[test]
    fn paginated_response_serializes_correctly() {
        let resp = PaginatedResponse { items: vec!["a", "b"], total: 5, limit: 2, offset: 0, has_more: true };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["items"], serde_json::json!(["a", "b"]));
        assert_eq!(json["total"], 5);
        assert_eq!(json["has_more"], true);
    }

    #[test]
    fn paginated_response_has_more_false_at_end() {
        let resp = PaginatedResponse { items: vec![1, 2], total: 4, limit: 2, offset: 2, has_more: false };
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
        #[cfg(target_pointer_width = "64")]
        {
            let result = validate_offset(usize::MAX);
            assert!(result.is_err());
            let (status, _msg) = result.unwrap_err();
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn list_sessions_query_defaults_match_pagination() {
        let json = r#"{}"#;
        let q: crate::routes::sessions::ListSessionsQuery = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(q.limit, DEFAULT_PAGE_LIMIT);
        assert_eq!(q.offset, 0);
        assert!(q.search.is_none());
    }

    #[test]
    fn list_sessions_query_accepts_offset() {
        let json = r#"{"limit": 10, "offset": 20}"#;
        let q: crate::routes::sessions::ListSessionsQuery = serde_json::from_str(json).expect("should deserialize");
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
        let req = Request::builder().uri("/api/sessions?limit=10&offset=0").body(Body::empty()).expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("sessions").is_some());
        assert!(json.get("total").is_some());
        assert!(json.get("limit").is_some());
        assert!(json.get("offset").is_some());
        assert!(json.get("has_more").is_some());
    }

    #[tokio::test]
    async fn list_skills_returns_paginated_response() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let (state, _dir) = create_test_state();
        let app = build_router(state);
        let req = Request::builder().uri("/api/skills?limit=5").body(Body::empty()).expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
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
        let req = Request::builder().uri("/api/memories?limit=10&offset=0").body(Body::empty()).expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
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
        let req = Request::builder().uri("/api/tools").body(Body::empty()).expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("builtin_tools").is_some());
        assert!(json.get("mcp_tools").is_some());
        assert!(json.get("builtin_count").is_some());
        assert!(json.get("mcp_count").is_some());
        assert!(json.get("total").is_some());
        let builtin = json["builtin_tools"].as_array().unwrap();
        assert!(builtin.len() >= 50);
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
        let req = Request::builder().uri("/api/templates?limit=50").body(Body::empty()).expect("request should build");
        let resp = app.oneshot(req).await.expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
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

        let (state, _dir) = create_test_state();
        let app = build_router(state);

        for uri in &[
            "/api/sessions", "/api/skills", "/api/memories", "/api/schedules",
            "/api/user/traits", "/api/tools", "/api/templates",
            "/api/pairing/approved", "/api/pairing/pending",
        ] {
            let req = Request::builder().uri(*uri).body(Body::empty()).expect("request should build");
            let resp = app.clone().oneshot(req).await.expect("request should succeed");
            assert_eq!(resp.status(), StatusCode::OK, "{uri} should return 200 with default params");
        }
    }
}
