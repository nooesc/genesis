//! HTTP gateway for the Genesis agent.
//!
//! Exposes a REST API so external services (webhooks, platform bots)
//! can send messages to Eve and receive responses.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use genesis_core::agent_loop::{AgentLoopConfig, AgentLoop};
use genesis_core::prompt::build_system_prompt;
use genesis_core::{build_default_tool_runtime, build_execution_context_from_loaded};
use genesis_provider::client_from_config;
use genesis_storage::{bootstrap, SessionStore};
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};

/// Shared application state for all request handlers.
pub struct AppState {
    pub loaded: genesis_config::LoadedConfig,
}

/// Request body for the `/chat` endpoint.
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    pub session_id: Option<String>,
}

fn default_platform() -> String {
    "api".to_owned()
}

/// Response body from the `/chat` endpoint.
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub session_id: String,
    pub response: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Build the axum Router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/chat", post(chat_handler))
        .with_state(state)
}

async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, String)> {
    let loaded = &state.loaded;

    bootstrap(&loaded.config.storage.database_path).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}"))
    })?;

    let session_id = request.session_id.unwrap_or_else(|| {
        format!(
            "api-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        )
    });

    let execution_context = build_execution_context_from_loaded(
        loaded,
        session_id.clone(),
        DeliveryPlatform::Cli, // API maps to CLI for now
    );
    let tool_runtime = build_default_tool_runtime(&execution_context);
    let system_prompt = build_system_prompt(
        &execution_context.plan.profile,
        &tool_runtime.definitions(),
        None,
    );

    let client = client_from_config(
        &loaded.config.provider.backend,
        &loaded.config.provider.model,
        loaded.config.provider.base_url.as_deref(),
        loaded.config.provider.api_key_env.as_deref(),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("provider error: {e}")))?;

    let mut agent = AgentLoop::new(
        client,
        tool_runtime,
        AgentLoopConfig {
            system_prompt: Some(system_prompt),
            ..AgentLoopConfig::default()
        },
    );

    // Create session and load history if resuming
    let store = SessionStore::new(&loaded.config.storage.database_path);
    if store.get_session(&session_id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}"))
    })?.is_none() {
        store.create_session(&session_id, &request.platform, None).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("storage error: {e}"))
        })?;
    }

    let result = agent.run_turn(&request.message).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("agent error: {e}"))
    })?;

    // Persist messages
    let messages = agent.messages();
    // Skip system prompt (index 0), persist only conversation messages
    for msg in messages.iter().skip(1) {
        let tool_calls_json = msg
            .tool_calls
            .as_ref()
            .and_then(|tc| serde_json::to_string(tc).ok());

        let _ = store.append_message(
            &session_id,
            &msg.role,
            msg.content.as_deref(),
            msg.tool_call_id.as_deref(),
            tool_calls_json.as_deref(),
        );
    }

    Ok(Json(ChatResponse {
        session_id,
        response: result.response,
        turns_used: result.turns_used,
        tool_calls_made: result.tool_calls_made,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_response_serializes() {
        let resp = HealthResponse {
            status: "ok".to_owned(),
            version: "0.1.0".to_owned(),
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"status\":\"ok\""));
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
    }

    #[test]
    fn chat_response_serializes() {
        let resp = ChatResponse {
            session_id: "api-123".to_owned(),
            response: "Hello!".to_owned(),
            turns_used: 1,
            tool_calls_made: 0,
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"session_id\":\"api-123\""));
        assert!(json.contains("\"response\":\"Hello!\""));
    }

    #[test]
    fn build_router_creates_routes() {
        let loaded = genesis_config::load(None).expect("default config should load");
        let state = Arc::new(AppState { loaded });
        let _router = build_router(state);
        // If this doesn't panic, routes were created successfully
    }
}
