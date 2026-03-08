//! Home Assistant webhook handler.
//!
//! Receives automation/webhook calls from Home Assistant and processes them
//! through the agent. Replies are sent back as the HTTP response body so
//! Home Assistant can use the result in automations.
//!
//! ## Setup
//! 1. In Home Assistant, create a webhook automation
//! 2. Point it at `https://your-server/homeassistant/webhook`
//! 3. Optionally set `HOMEASSISTANT_TOKEN` for authentication
//! 4. Optionally set `HOMEASSISTANT_URL` and `HOMEASSISTANT_LONG_LIVED_TOKEN`
//!    to enable the agent to call back into Home Assistant services

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use genesis_core::execution::{SessionExecutionService, SessionTurnInput};
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};
use tracing::{error, info, info_span, Instrument};

use crate::AppState;

// --- Types ---

#[derive(Debug, Deserialize)]
pub struct HomeAssistantRequest {
    /// The message/prompt from the automation.
    pub message: String,
    /// Optional entity or automation ID for stable session tracking.
    pub entity_id: Option<String>,
    /// Optional title for the session.
    pub title: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HomeAssistantResponse {
    pub response: String,
    pub session_id: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
}

// --- Handler ---

/// Webhook handler for Home Assistant automations.
///
/// Unlike platform bots, we return the response synchronously since
/// Home Assistant automations expect the result in the HTTP response.
pub async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<HomeAssistantRequest>,
) -> Result<Json<HomeAssistantResponse>, (StatusCode, String)> {
    let session_id = request
        .entity_id
        .as_deref()
        .map(|id| format!("ha-{id}"))
        .unwrap_or_else(|| "ha-default".to_owned());

    let title = request
        .title
        .as_deref()
        .unwrap_or("Home Assistant");

    let span = info_span!(
        "homeassistant.webhook",
        session_id = session_id.as_str(),
    );

    info!(parent: &span, "received home assistant webhook");

    // Handle gateway slash commands before reaching the agent.
    let store = genesis_storage::SessionStore::new(&state.loaded.config.storage.database_path);
    if let crate::commands::CommandResult::Reply(reply) =
        crate::commands::handle_command(
            &request.message,
            &session_id,
            &store,
            &state.loaded.config,
        )
    {
        return Ok(Json(HomeAssistantResponse {
            response: reply,
            session_id,
            turns_used: 0,
            tool_calls_made: 0,
        }));
    }

    // Auto-reset expired sessions before processing.
    crate::commands::check_session_expiry(
        &session_id,
        &store,
        state.loaded.config.gateway.as_ref(),
    );

    let service = SessionExecutionService::new(&state.loaded);

    let outcome = service
        .run_turn(SessionTurnInput {
            session_id: &session_id,
            session_platform: "homeassistant",
            delivery_platform: DeliveryPlatform::HomeAssistant,
            prompt: &request.message,
            title: Some(title),
            images: Vec::new(),
        })
        .instrument(span)
        .await
        .map_err(|e| {
            error!(error = %e, "home assistant webhook failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("execution error: {e}"),
            )
        })?;

    info!(
        turns_used = outcome.result.turns_used,
        tool_calls_made = outcome.result.tool_calls_made,
        "home assistant turn completed"
    );

    Ok(Json(HomeAssistantResponse {
        response: outcome.result.response,
        session_id: outcome.session_id,
        turns_used: outcome.result.turns_used,
        tool_calls_made: outcome.result.tool_calls_made,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserializes_minimal() {
        let json = r#"{"message": "Turn off the lights"}"#;
        let req: HomeAssistantRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.message, "Turn off the lights");
        assert!(req.entity_id.is_none());
        assert!(req.title.is_none());
    }

    #[test]
    fn request_deserializes_full() {
        let json = r#"{
            "message": "What is the temperature?",
            "entity_id": "automation.morning_routine",
            "title": "Morning check"
        }"#;
        let req: HomeAssistantRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.message, "What is the temperature?");
        assert_eq!(req.entity_id.as_deref(), Some("automation.morning_routine"));
        assert_eq!(req.title.as_deref(), Some("Morning check"));
    }

    #[test]
    fn response_serializes() {
        let resp = HomeAssistantResponse {
            response: "The lights are off.".to_owned(),
            session_id: "ha-automation.lights".to_owned(),
            turns_used: 1,
            tool_calls_made: 0,
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"response\":\"The lights are off.\""));
        assert!(json.contains("\"session_id\":\"ha-automation.lights\""));
    }

    #[test]
    fn session_id_uses_entity_id_when_provided() {
        let entity_id = Some("automation.morning".to_owned());
        let session_id = entity_id
            .as_deref()
            .map(|id| format!("ha-{id}"))
            .unwrap_or_else(|| "ha-default".to_owned());
        assert_eq!(session_id, "ha-automation.morning");
    }

    #[test]
    fn session_id_defaults_when_no_entity() {
        let entity_id: Option<String> = None;
        let session_id = entity_id
            .as_deref()
            .map(|id| format!("ha-{id}"))
            .unwrap_or_else(|| "ha-default".to_owned());
        assert_eq!(session_id, "ha-default");
    }
}
