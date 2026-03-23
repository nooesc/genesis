//! Health check, metrics, and agent card endpoints.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

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

/// JSON metrics response for the dashboard.
#[derive(Debug, Serialize)]
pub(crate) struct MetricsJsonResponse {
    uptime_seconds: u64,
    requests_total: u64,
    errors_total: u64,
    input_tokens_total: u64,
    output_tokens_total: u64,
    stream_requests_total: u64,
    total_sessions: usize,
    active_schedules: usize,
}

// ---------------------------------------------------------------------------
// Shared DB stats
// ---------------------------------------------------------------------------

/// Shared DB stats used by health, metrics, and prometheus handlers.
pub(crate) fn fetch_db_stats(db_path: &std::path::Path) -> (usize, usize) {
    let total_sessions = genesis_storage::SessionStore::new(db_path)
        .session_count()
        .unwrap_or(0) as usize;
    let active_schedules = genesis_storage::ScheduleStore::new(db_path)
        .list_enabled()
        .map(|s| s.len())
        .unwrap_or(0);
    (total_sessions, active_schedules)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
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

pub(crate) async fn mcp_status_handler(
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

/// A2A Agent Card — describes this agent's capabilities for discovery.
/// See: <https://github.com/a2aproject/A2A>
pub(crate) async fn agent_card_handler(headers: HeaderMap) -> impl IntoResponse {
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
pub(crate) async fn prometheus_metrics_handler(State(state): State<Arc<AppState>>) -> Response {
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
pub(crate) async fn metrics_json_handler(
    State(state): State<Arc<AppState>>,
) -> Json<MetricsJsonResponse> {
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
