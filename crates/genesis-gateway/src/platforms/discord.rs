//! Discord Interactions endpoint handler.
//!
//! Handles Discord webhook interactions (slash commands and message components).
//! Discord requires responding within 3 seconds, so we use deferred responses
//! and follow up with the agent's reply.
//!
//! ## Setup
//! 1. Create a Discord application at https://discord.com/developers/applications
//! 2. Set `DISCORD_BOT_TOKEN` and `DISCORD_PUBLIC_KEY` environment variables
//! 3. Set the Interactions endpoint URL to `https://your-server/discord/interactions`

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use genesis_core::execution::{SessionExecutionService, SessionTurnInput};
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};
use tracing::{error, info, info_span, warn, Instrument};

use crate::verify::verify_discord_signature;
use crate::AppState;

pub fn bot_token() -> Option<String> {
    std::env::var("DISCORD_BOT_TOKEN").ok()
}

pub fn public_key() -> Option<String> {
    std::env::var("DISCORD_PUBLIC_KEY").ok()
}

// --- Discord API types (subset) ---

/// Interaction type values from Discord API.
const INTERACTION_PING: u8 = 1;
const INTERACTION_APPLICATION_COMMAND: u8 = 2;
const INTERACTION_MESSAGE_COMPONENT: u8 = 3;

/// Response type values for Discord API.
const RESPONSE_PONG: u8 = 1;
const RESPONSE_CHANNEL_MESSAGE: u8 = 4;
const RESPONSE_DEFERRED_CHANNEL_MESSAGE: u8 = 5;

#[derive(Debug, Deserialize)]
pub struct DiscordInteraction {
    #[serde(rename = "type")]
    pub interaction_type: u8,
    pub id: Option<String>,
    pub token: Option<String>,
    pub data: Option<InteractionData>,
    pub channel_id: Option<String>,
    pub guild_id: Option<String>,
    pub member: Option<DiscordMember>,
    pub user: Option<DiscordUser>,
}

#[derive(Debug, Deserialize)]
pub struct InteractionData {
    pub name: Option<String>,
    pub options: Option<Vec<InteractionOption>>,
}

#[derive(Debug, Deserialize)]
pub struct InteractionOption {
    pub name: String,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct DiscordMember {
    pub user: Option<DiscordUser>,
}

#[derive(Debug, Deserialize)]
pub struct DiscordUser {
    pub id: String,
    pub username: String,
}

#[derive(Debug, Serialize)]
struct InteractionResponse {
    #[serde(rename = "type")]
    response_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<InteractionResponseData>,
}

#[derive(Debug, Serialize)]
struct InteractionResponseData {
    content: String,
}

#[derive(Debug, Serialize)]
struct EditFollowup {
    content: String,
}

// --- Handler ---

/// Discord interactions endpoint.
///
/// Handles three interaction types:
/// - Ping (type 1): Required verification, respond with Pong
/// - Application Command (type 2): Slash commands like `/ask`
/// - Message Component (type 3): Button clicks, etc.
pub async fn interactions_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Verify Discord Ed25519 signature if public key is configured
    if let Some(pub_key) = public_key() {
        let signature = headers
            .get("X-Signature-Ed25519")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let timestamp = headers
            .get("X-Signature-Timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_discord_signature(&pub_key, timestamp, &body, signature) {
            warn!("discord webhook signature verification failed");
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    // Parse the interaction
    let interaction: DiscordInteraction =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Handle ping (Discord verification)
    if interaction.interaction_type == INTERACTION_PING {
        return Ok(Json(
            serde_json::to_value(InteractionResponse {
                response_type: RESPONSE_PONG,
                data: None,
            })
            .unwrap(),
        ));
    }

    // For application commands, process the message
    if interaction.interaction_type == INTERACTION_APPLICATION_COMMAND
        || interaction.interaction_type == INTERACTION_MESSAGE_COMPONENT
    {
        let interaction_token = interaction
            .token
            .clone()
            .ok_or(StatusCode::BAD_REQUEST)?;

        // Extract the message from command options
        let message = extract_message(&interaction).unwrap_or_default();
        if message.is_empty() {
            return Ok(Json(
                serde_json::to_value(InteractionResponse {
                    response_type: RESPONSE_CHANNEL_MESSAGE,
                    data: Some(InteractionResponseData {
                        content: "Please provide a message.".to_owned(),
                    }),
                })
                .unwrap(),
            ));
        }

        let user_name = extract_username(&interaction);
        let channel_id = interaction.channel_id.clone().unwrap_or_default();
        let session_id = format!("discord-{channel_id}");

        let span = info_span!(
            "discord.interaction",
            user = user_name.as_str(),
            session_id = session_id.as_str(),
            channel_id = channel_id.as_str()
        );

        info!(parent: &span, "received discord interaction");

        // Respond with deferred message immediately
        let token = match bot_token() {
            Some(t) => t,
            None => {
                error!("DISCORD_BOT_TOKEN not set");
                return Ok(Json(
                    serde_json::to_value(InteractionResponse {
                        response_type: RESPONSE_CHANNEL_MESSAGE,
                        data: Some(InteractionResponseData {
                            content: "Bot configuration error.".to_owned(),
                        }),
                    })
                    .unwrap(),
                ));
            }
        };

        // Spawn background task to process and follow up
        tokio::spawn(
            async move {
                let service = SessionExecutionService::new(&state.loaded);

                let result = service
                    .run_turn(SessionTurnInput {
                        session_id: &session_id,
                        session_platform: "discord",
                        delivery_platform: DeliveryPlatform::Discord,
                        prompt: &message,
                        title: Some(&format!("Discord: {user_name}")),
                        images: Vec::new(),
                    })
                    .await;

                let reply_text = match result {
                    Ok(outcome) => {
                        info!(
                            turns_used = outcome.result.turns_used,
                            tool_calls_made = outcome.result.tool_calls_made,
                            "discord turn completed"
                        );
                        outcome.result.response
                    }
                    Err(e) => {
                        error!(error = %e, "discord turn failed");
                        format!("Sorry, I encountered an error: {e}")
                    }
                };

                // Edit the deferred response with the actual reply
                if let Err(e) =
                    edit_followup(&token, &interaction_token, &reply_text).await
                {
                    error!(error = %e, "failed to send discord followup");
                }
            }
            .instrument(span),
        );

        // Return deferred response
        return Ok(Json(
            serde_json::to_value(InteractionResponse {
                response_type: RESPONSE_DEFERRED_CHANNEL_MESSAGE,
                data: None,
            })
            .unwrap(),
        ));
    }

    // Unknown interaction type
    Err(StatusCode::BAD_REQUEST)
}

fn extract_message(interaction: &DiscordInteraction) -> Option<String> {
    let data = interaction.data.as_ref()?;
    let options = data.options.as_ref()?;

    // Look for a "message" or "prompt" option
    options
        .iter()
        .find(|o| o.name == "message" || o.name == "prompt" || o.name == "question")
        .and_then(|o| o.value.as_ref())
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

fn extract_username(interaction: &DiscordInteraction) -> String {
    interaction
        .member
        .as_ref()
        .and_then(|m| m.user.as_ref())
        .or(interaction.user.as_ref())
        .map(|u| u.username.clone())
        .unwrap_or_else(|| "Unknown".to_owned())
}

/// Edit the original deferred interaction response with the agent's reply.
async fn edit_followup(
    bot_token: &str,
    interaction_token: &str,
    content: &str,
) -> Result<(), String> {
    let client = reqwest::Client::new();

    // Discord has a 2000 character limit for messages
    let truncated = if content.len() > 2000 {
        format!("{}...", &content[..1997])
    } else {
        content.to_owned()
    };

    // Use the interaction token to edit the original response
    let resp = client
        .patch(format!(
            "https://discord.com/api/v10/webhooks/{bot_token}/{interaction_token}/messages/@original"
        ))
        .json(&EditFollowup {
            content: truncated,
        })
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Discord API error {status}: {body}"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_ping_interaction_deserializes() {
        let json = r#"{"type": 1}"#;
        let interaction: DiscordInteraction = serde_json::from_str(json).expect("should parse");
        assert_eq!(interaction.interaction_type, INTERACTION_PING);
    }

    #[test]
    fn discord_command_interaction_deserializes() {
        let json = r#"{
            "type": 2,
            "id": "12345",
            "token": "abc-token",
            "channel_id": "chan-1",
            "data": {
                "name": "ask",
                "options": [
                    {"name": "message", "value": "Hello Eve"}
                ]
            },
            "member": {
                "user": {"id": "u1", "username": "cole"}
            }
        }"#;
        let interaction: DiscordInteraction = serde_json::from_str(json).expect("should parse");
        assert_eq!(interaction.interaction_type, INTERACTION_APPLICATION_COMMAND);

        let msg = extract_message(&interaction);
        assert_eq!(msg.as_deref(), Some("Hello Eve"));

        let user = extract_username(&interaction);
        assert_eq!(user, "cole");
    }

    #[test]
    fn extract_message_returns_none_for_no_data() {
        let interaction = DiscordInteraction {
            interaction_type: 2,
            id: None,
            token: None,
            data: None,
            channel_id: None,
            guild_id: None,
            member: None,
            user: None,
        };
        assert!(extract_message(&interaction).is_none());
    }

    #[test]
    fn pong_response_serializes() {
        let resp = InteractionResponse {
            response_type: RESPONSE_PONG,
            data: None,
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"type\":1"));
        assert!(!json.contains("data"));
    }

    #[test]
    fn channel_message_response_serializes() {
        let resp = InteractionResponse {
            response_type: RESPONSE_CHANNEL_MESSAGE,
            data: Some(InteractionResponseData {
                content: "Hello!".to_owned(),
            }),
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"type\":4"));
        assert!(json.contains("Hello!"));
    }

    #[test]
    fn session_id_stable_per_channel() {
        let channel_id = "123456";
        let session_id = format!("discord-{channel_id}");
        assert_eq!(session_id, "discord-123456");
    }
}
