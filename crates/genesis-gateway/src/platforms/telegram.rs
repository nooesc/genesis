//! Telegram Bot API webhook handler.
//!
//! Receives Telegram webhook updates, processes messages through the agent,
//! and sends replies back via the Telegram Bot API.
//!
//! ## Setup
//! 1. Create a bot via @BotFather on Telegram
//! 2. Set the `TELEGRAM_BOT_TOKEN` environment variable
//! 3. Register the webhook URL: `POST https://api.telegram.org/bot<TOKEN>/setWebhook`
//!    with `{"url": "https://your-server/telegram/webhook"}`

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use genesis_core::execution::{SessionExecutionService, SessionTurnInput};
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};
use tracing::{error, info, info_span, Instrument};

use crate::AppState;

/// Telegram Bot API token, loaded from environment.
pub fn bot_token() -> Option<String> {
    std::env::var("TELEGRAM_BOT_TOKEN").ok()
}

// --- Telegram API types (subset we need) ---

#[derive(Debug, Deserialize)]
pub struct TelegramUpdate {
    pub update_id: i64,
    pub message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub chat: TelegramChat,
    pub from: Option<TelegramUser>,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Debug, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    pub first_name: String,
    pub username: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    chat_id: i64,
    text: String,
    parse_mode: Option<String>,
    reply_to_message_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TelegramApiResponse {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
}

// --- Handler ---

/// Webhook handler for Telegram updates.
///
/// Telegram expects a 200 OK response quickly, so we spawn the agent
/// execution in a background task and reply via the Bot API.
pub async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    Json(update): Json<TelegramUpdate>,
) -> StatusCode {
    let message = match update.message {
        Some(m) => m,
        None => return StatusCode::OK, // Ignore non-message updates (edits, callbacks, etc.)
    };

    let text = match message.text {
        Some(t) if !t.is_empty() => t,
        _ => return StatusCode::OK, // Ignore empty or non-text messages
    };

    let chat_id = message.chat.id;
    let message_id = message.message_id;
    let user_name = message
        .from
        .as_ref()
        .map(|u| u.first_name.clone())
        .unwrap_or_else(|| "Unknown".to_owned());

    let token = match bot_token() {
        Some(t) => t,
        None => {
            error!("TELEGRAM_BOT_TOKEN not set, cannot process webhook");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // Session ID: stable per chat so conversation persists
    let session_id = format!("tg-{chat_id}");

    let span = info_span!(
        "telegram.webhook",
        chat_id,
        user = user_name.as_str(),
        session_id = session_id.as_str()
    );

    info!(parent: &span, "received telegram message");

    // Spawn background task so we return 200 immediately
    let state = Arc::clone(&state);
    tokio::spawn(
        async move {
            let service = SessionExecutionService::new(&state.loaded);

            let result = service
                .run_turn(SessionTurnInput {
                    session_id: &session_id,
                    session_platform: "telegram",
                    delivery_platform: DeliveryPlatform::Telegram,
                    prompt: &text,
                    title: Some(&format!("Telegram: {user_name}")),
                    images: Vec::new(),
                })
                .await;

            let reply_text = match result {
                Ok(outcome) => {
                    info!(
                        turns_used = outcome.result.turns_used,
                        tool_calls_made = outcome.result.tool_calls_made,
                        "telegram turn completed"
                    );
                    outcome.result.response
                }
                Err(e) => {
                    error!(error = %e, "telegram turn failed");
                    format!("Sorry, I encountered an error: {e}")
                }
            };

            if let Err(e) = send_reply(&token, chat_id, &reply_text, Some(message_id)).await {
                error!(error = %e, "failed to send telegram reply");
            }
        }
        .instrument(span),
    );

    StatusCode::OK
}

/// Send a message via the Telegram Bot API.
async fn send_reply(
    token: &str,
    chat_id: i64,
    text: &str,
    reply_to: Option<i64>,
) -> Result<(), String> {
    // Telegram messages have a 4096 character limit.
    // Split long responses into chunks.
    let chunks = split_message(text, 4096);

    let client = reqwest::Client::new();
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");

    for (i, chunk) in chunks.iter().enumerate() {
        let reply_to_id = if i == 0 { reply_to } else { None };

        let resp = client
            .post(&url)
            .json(&SendMessageRequest {
                chat_id,
                text: chunk.clone(),
                parse_mode: None,
                reply_to_message_id: reply_to_id,
            })
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let api_resp: TelegramApiResponse = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse response: {e}"))?;

        if !api_resp.ok {
            return Err(format!(
                "Telegram API error: {}",
                api_resp.description.unwrap_or_default()
            ));
        }
    }

    Ok(())
}

/// Split a message into chunks respecting a max length.
/// Tries to split on newlines first, then word boundaries.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_owned());
            break;
        }

        // Try to find a newline within the limit
        let split_at = remaining[..max_len]
            .rfind('\n')
            .or_else(|| remaining[..max_len].rfind(' '))
            .unwrap_or(max_len);

        chunks.push(remaining[..split_at].to_owned());
        remaining = remaining[split_at..].trim_start();
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_update_deserializes() {
        let json = r#"{
            "update_id": 123456,
            "message": {
                "message_id": 1,
                "chat": {"id": 42, "type": "private"},
                "from": {"id": 100, "first_name": "Cole", "username": "cole"},
                "text": "Hello Eve"
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).expect("should parse");
        assert_eq!(update.update_id, 123456);
        let msg = update.message.expect("should have message");
        assert_eq!(msg.chat.id, 42);
        assert_eq!(msg.text.as_deref(), Some("Hello Eve"));
        assert_eq!(msg.from.unwrap().first_name, "Cole");
    }

    #[test]
    fn telegram_update_handles_missing_message() {
        let json = r#"{"update_id": 789}"#;
        let update: TelegramUpdate = serde_json::from_str(json).expect("should parse");
        assert!(update.message.is_none());
    }

    #[test]
    fn split_message_short_text() {
        let chunks = split_message("hello", 4096);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_long_text_on_newlines() {
        let text = format!("{}\n{}", "a".repeat(100), "b".repeat(100));
        let chunks = split_message(&text, 150);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= 150);
    }

    #[test]
    fn split_message_long_text_on_spaces() {
        let text = format!("{} {}", "word".repeat(30), "more".repeat(30));
        let chunks = split_message(&text, 100);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 100);
        }
    }

    #[test]
    fn send_message_request_serializes() {
        let req = SendMessageRequest {
            chat_id: 42,
            text: "Hello!".to_owned(),
            parse_mode: None,
            reply_to_message_id: Some(1),
        };
        let json = serde_json::to_string(&req).expect("should serialize");
        assert!(json.contains("\"chat_id\":42"));
        assert!(json.contains("\"text\":\"Hello!\""));
        assert!(json.contains("\"reply_to_message_id\":1"));
    }

    #[test]
    fn session_id_is_stable_per_chat() {
        let chat_id: i64 = 12345;
        let session_id = format!("tg-{chat_id}");
        assert_eq!(session_id, "tg-12345");
    }
}
