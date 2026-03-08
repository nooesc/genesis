//! HTTP gateway for the Genesis agent.
//!
//! Exposes a REST API so external services (webhooks, platform bots)
//! can send messages to Eve and receive responses.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use genesis_core::execution::{
    delivery_platform_from_str, SessionExecutionService, SessionTurnInput,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

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

fn default_api_session_id() -> String {
    format!(
        "api-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    )
}

/// Response body from the `/chat` endpoint.
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub session_id: String,
    pub response: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
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
}

/// Build the axum Router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/chat", post(chat_handler))
        .route("/chat/stream", post(chat_stream_handler))
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
    let service = SessionExecutionService::new(loaded);
    let session_id = request.session_id.unwrap_or_else(default_api_session_id);
    let outcome = service
        .run_turn(SessionTurnInput {
            session_id: &session_id,
            session_platform: &request.platform,
            delivery_platform: delivery_platform_from_str(&request.platform),
            prompt: &request.message,
            title: None,
        })
        .await
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("execution error: {e}"))
        })?;

    Ok(Json(ChatResponse {
        session_id: outcome.session_id,
        response: outcome.result.response,
        turns_used: outcome.result.turns_used,
        tool_calls_made: outcome.result.tool_calls_made,
    }))
}

async fn chat_stream_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>, (StatusCode, String)>
{
    let session_id = request.session_id.unwrap_or_else(default_api_session_id);

    let platform = request.platform;
    let message = request.message;
    let (tx, mut rx) = mpsc::unbounded_channel::<Result<Event, std::convert::Infallible>>();
    let state_for_task = Arc::clone(&state);
    let session_id_for_task = session_id.clone();

    tokio::spawn(async move {
        let service = SessionExecutionService::new(&state_for_task.loaded);
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
                },
                |chunk| {
                    if let Ok(payload) = serde_json::to_string(&StreamChunkResponse {
                        session_id: session_id.clone(),
                        content: chunk.to_owned(),
                    }) {
                        let _ = tx.send(Ok(Event::default().event("chunk").data(payload)));
                    }
                },
            )
            .await;

        match run_result {
            Ok(outcome) => {
                if let Ok(payload) = serde_json::to_string(&StreamDoneResponse {
                    session_id: outcome.session_id,
                    response: outcome.result.response,
                    turns_used: outcome.result.turns_used,
                    tool_calls_made: outcome.result.tool_calls_made,
                }) {
                    let _ = tx.send(Ok(Event::default().event("done").data(payload)));
                }
            }
            Err(error) => {
                if let Ok(payload) = serde_json::to_string(&StreamErrorResponse {
                    session_id,
                    error: error.to_string(),
                }) {
                    let _ = tx.send(Ok(Event::default().event("error").data(payload)));
                }
            }
        }
    });

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
    fn default_api_session_id_uses_api_prefix() {
        assert!(default_api_session_id().starts_with("api-"));
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
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"response\":\"hello\""));
        assert!(json.contains("\"turns_used\":1"));
    }
}
