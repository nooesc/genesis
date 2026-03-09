//! Slack Events API webhook handler.
//!
//! Receives Slack events (messages, mentions) and processes them through
//! the agent, then sends replies back via the Slack Web API.
//!
//! ## Setup
//! 1. Create a Slack app at https://api.slack.com/apps
//! 2. Set `SLACK_BOT_TOKEN` and `SLACK_SIGNING_SECRET` environment variables
//! 3. Enable Event Subscriptions and set the URL to `https://your-server/slack/events`
//! 4. Subscribe to `message.im` and `app_mention` events

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use genesis_core::execution::{SessionExecutionService, SessionTurnInput};
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};
use tracing::{error, info, info_span, warn, Instrument};

use crate::verify::verify_slack_signature;
use crate::AppState;

pub fn bot_token() -> Option<String> {
    std::env::var("SLACK_BOT_TOKEN").ok()
}

fn signing_secret() -> Option<String> {
    std::env::var("SLACK_SIGNING_SECRET").ok()
}

// --- Slack API types ---

/// Slack Events API wrapper. This is the top-level payload that Slack sends.
#[derive(Debug, Deserialize)]
pub struct SlackEventPayload {
    #[serde(rename = "type")]
    pub payload_type: String,
    /// URL verification challenge (only present during initial setup)
    pub challenge: Option<String>,
    pub event: Option<SlackEvent>,
}

#[derive(Debug, Deserialize)]
pub struct SlackEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub text: Option<String>,
    pub user: Option<String>,
    pub channel: Option<String>,
    pub ts: Option<String>,
    /// Bot ID — present if the message was sent by a bot (including ourselves)
    pub bot_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct SlackPostMessage {
    channel: String,
    text: String,
    thread_ts: Option<String>,
}

// --- Handler ---

/// Slack Events API endpoint.
///
/// Handles three types of payloads:
/// - `url_verification`: Responds with the challenge string (initial setup)
/// - `event_callback` with `message` event: Process user messages
/// - `event_callback` with `app_mention` event: Process @mentions
pub async fn events_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let secret = match signing_secret() {
        Some(secret) => secret,
        None => {
            warn!("SLACK_SIGNING_SECRET is not configured");
            return Err(StatusCode::FORBIDDEN);
        }
    };

    let timestamp = headers
        .get("X-Slack-Request-Timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let signature = headers
        .get("X-Slack-Signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if !verify_slack_signature(&secret, timestamp, &body, signature) {
        warn!("slack webhook signature verification failed");
        return Err(StatusCode::UNAUTHORIZED);
    }

    let payload: SlackEventPayload =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Handle URL verification challenge
    if payload.payload_type == "url_verification" {
        let challenge = payload.challenge.unwrap_or_default();
        return Ok(Json(serde_json::json!({ "challenge": challenge })));
    }

    // Handle event callbacks
    if payload.payload_type != "event_callback" {
        return Ok(Json(serde_json::json!({ "ok": true })));
    }

    let event = match payload.event {
        Some(e) => e,
        None => return Ok(Json(serde_json::json!({ "ok": true }))),
    };

    // Ignore bot messages to prevent loops
    if event.bot_id.is_some() {
        return Ok(Json(serde_json::json!({ "ok": true })));
    }

    // Only handle message and app_mention events
    if event.event_type != "message" && event.event_type != "app_mention" {
        return Ok(Json(serde_json::json!({ "ok": true })));
    }

    let text = match event.text {
        Some(t) if !t.is_empty() => t,
        _ => return Ok(Json(serde_json::json!({ "ok": true }))),
    };

    let channel = event.channel.unwrap_or_default();
    let user = event.user.unwrap_or_else(|| "unknown".to_owned());
    let thread_ts = event.ts.clone();

    let token = match bot_token() {
        Some(t) => t,
        None => {
            error!("SLACK_BOT_TOKEN not set, cannot process event");
            return Ok(Json(serde_json::json!({ "ok": true })));
        }
    };

    let session_id = format!("slack-{channel}");
    let span = info_span!(
        "slack.event",
        user = user.as_str(),
        channel = channel.as_str(),
        session_id = session_id.as_str()
    );

    info!(parent: &span, event_type = event.event_type.as_str(), "received slack event");

    // DM pairing check
    match super::check_pairing(
        &state.loaded.config.storage.database_path,
        "slack",
        &user,
        &user, // Slack user IDs are opaque; username isn't available without API call
    ) {
        super::PairingCheck::Approved => {}
        super::PairingCheck::NeedsPairing(code) => {
            let reply = super::pairing_reply(&code);
            let client2 = state.http_client.clone();
            let token2 = token.clone();
            let channel2 = channel.clone();
            let ts = thread_ts.clone();
            tokio::spawn(async move {
                if let Err(e) = post_message(&client2, &token2, &channel2, &reply, ts.as_deref()).await {
                    error!(error = %e, "failed to send pairing reply");
                }
            });
            return Ok(Json(serde_json::json!({ "ok": true })));
        }
        super::PairingCheck::AtCapacity => {
            let reply = super::pairing_capacity_reply().to_owned();
            let client2 = state.http_client.clone();
            let token2 = token.clone();
            let channel2 = channel.clone();
            let ts = thread_ts.clone();
            tokio::spawn(async move {
                if let Err(e) = post_message(&client2, &token2, &channel2, &reply, ts.as_deref()).await {
                    error!(error = %e, "failed to send capacity reply");
                }
            });
            return Ok(Json(serde_json::json!({ "ok": true })));
        }
    }

    // Handle gateway slash commands before reaching the agent.
    let store = genesis_storage::SessionStore::new(&state.loaded.config.storage.database_path);
    match crate::commands::handle_command(&text, &session_id, &store, &state.loaded.config) {
        crate::commands::CommandResult::Reply(reply) => {
            let client2 = state.http_client.clone();
            let token2 = token.clone();
            let channel2 = channel.clone();
            let ts = thread_ts.clone();
            tokio::spawn(async move {
                if let Err(e) = post_message(&client2, &token2, &channel2, &reply, ts.as_deref()).await {
                    error!(error = %e, "failed to send command reply");
                }
            });
            return Ok(Json(serde_json::json!({ "ok": true })));
        }
        crate::commands::CommandResult::PassThrough => {}
    }

    // Auto-reset expired sessions before processing.
    crate::commands::check_session_expiry(
        &session_id,
        &store,
        state.loaded.config.gateway.as_ref(),
    );

    // Process in background
    tokio::spawn(
        async move {
            let service = SessionExecutionService::new(&state.loaded);

            let result = service
                .run_turn(SessionTurnInput {
                    session_id: &session_id,
                    session_platform: "slack",
                    delivery_platform: DeliveryPlatform::Slack,
                    prompt: &text,
                    title: Some(&format!("Slack: {user}")),
                    images: Vec::new(),
                })
                .await;

            let reply_text = match result {
                Ok(outcome) => {
                    info!(
                        turns_used = outcome.result.turns_used,
                        tool_calls_made = outcome.result.tool_calls_made,
                        "slack turn completed"
                    );
                    outcome.result.response
                }
                Err(e) => {
                    error!(error = %e, "slack turn failed");
                    format!("Sorry, I encountered an error: {e}")
                }
            };

            if let Err(e) =
                post_message(&state.http_client, &token, &channel, &reply_text, thread_ts.as_deref()).await
            {
                error!(error = %e, "failed to post slack message");
            }
        }
        .instrument(span),
    );

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Post a message to a Slack channel via the Web API.
async fn post_message(
    client: &reqwest::Client,
    token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<(), String> {
    let resp = client
        .post("https://slack.com/api/chat.postMessage")
        .bearer_auth(token)
        .json(&SlackPostMessage {
            channel: channel.to_owned(),
            text: text.to_owned(),
            thread_ts: thread_ts.map(|s| s.to_owned()),
        })
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse response: {e}"))?;

    if body["ok"].as_bool() != Some(true) {
        return Err(format!(
            "Slack API error: {}",
            body["error"].as_str().unwrap_or("unknown")
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_verification_deserializes() {
        let json = r#"{
            "type": "url_verification",
            "challenge": "abc123xyz"
        }"#;
        let payload: SlackEventPayload = serde_json::from_str(json).expect("should parse");
        assert_eq!(payload.payload_type, "url_verification");
        assert_eq!(payload.challenge.as_deref(), Some("abc123xyz"));
    }

    #[test]
    fn message_event_deserializes() {
        let json = r#"{
            "type": "event_callback",
            "event": {
                "type": "message",
                "text": "Hello Eve!",
                "user": "U12345",
                "channel": "C67890",
                "ts": "1234567890.123456"
            }
        }"#;
        let payload: SlackEventPayload = serde_json::from_str(json).expect("should parse");
        let event = payload.event.expect("should have event");
        assert_eq!(event.event_type, "message");
        assert_eq!(event.text.as_deref(), Some("Hello Eve!"));
        assert_eq!(event.user.as_deref(), Some("U12345"));
        assert_eq!(event.channel.as_deref(), Some("C67890"));
    }

    #[test]
    fn app_mention_event_deserializes() {
        let json = r#"{
            "type": "event_callback",
            "event": {
                "type": "app_mention",
                "text": "<@U_BOT> what is rust?",
                "user": "U12345",
                "channel": "C67890"
            }
        }"#;
        let payload: SlackEventPayload = serde_json::from_str(json).expect("should parse");
        let event = payload.event.expect("should have event");
        assert_eq!(event.event_type, "app_mention");
    }

    #[test]
    fn bot_message_has_bot_id() {
        let json = r#"{
            "type": "event_callback",
            "event": {
                "type": "message",
                "text": "I am a bot",
                "bot_id": "B12345",
                "channel": "C67890"
            }
        }"#;
        let payload: SlackEventPayload = serde_json::from_str(json).expect("should parse");
        let event = payload.event.expect("should have event");
        assert!(event.bot_id.is_some());
    }

    #[test]
    fn post_message_request_serializes() {
        let req = SlackPostMessage {
            channel: "C123".to_owned(),
            text: "Hello!".to_owned(),
            thread_ts: Some("1234.5678".to_owned()),
        };
        let json = serde_json::to_string(&req).expect("should serialize");
        assert!(json.contains("\"channel\":\"C123\""));
        assert!(json.contains("\"text\":\"Hello!\""));
        assert!(json.contains("\"thread_ts\":\"1234.5678\""));
    }

    #[test]
    fn session_id_stable_per_channel() {
        let channel = "C67890";
        let session_id = format!("slack-{channel}");
        assert_eq!(session_id, "slack-C67890");
    }
}
