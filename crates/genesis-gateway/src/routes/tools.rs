//! Tool introspection and config handlers.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_tools_handler(
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

    Ok(Json(serde_json::to_value(ToolListResponse {
        builtin_tools,
        builtin_count,
        mcp_tools,
        mcp_count,
        total,
    }).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("serialization error: {e}")))?,
    ))
}

pub(crate) async fn config_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
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
