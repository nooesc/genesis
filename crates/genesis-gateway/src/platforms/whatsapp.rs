//! WhatsApp Cloud API webhook handler.
//!
//! Receives WhatsApp webhook notifications, processes text messages through
//! the agent, and sends replies back via the WhatsApp Cloud API.
//!
//! ## Setup
//! 1. Create a WhatsApp Business app in Meta Developer Portal
//! 2. Set `WHATSAPP_TOKEN` (permanent token) and `WHATSAPP_PHONE_NUMBER_ID`
//! 3. Set `WHATSAPP_APP_SECRET` for signed webhook verification
//! 4. Configure the webhook URL: `https://your-server/whatsapp/webhook`
//! 5. Set `WHATSAPP_VERIFY_TOKEN` for webhook verification

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use genesis_core::execution::SessionTurnInput;
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};
use tracing::{error, info, info_span, warn, Instrument};

use crate::verify::verify_whatsapp_signature;
use crate::AppState;

const MAX_WHATSAPP_MESSAGE_LEN: usize = 4096;

// --- Environment config ---

fn whatsapp_token() -> Option<String> {
    std::env::var("WHATSAPP_TOKEN").ok()
}

fn phone_number_id() -> Option<String> {
    std::env::var("WHATSAPP_PHONE_NUMBER_ID").ok()
}

fn verify_token() -> Option<String> {
    std::env::var("WHATSAPP_VERIFY_TOKEN").ok()
}

fn app_secret() -> Option<String> {
    std::env::var("WHATSAPP_APP_SECRET").ok()
}

// --- WhatsApp Cloud API types ---

#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub object: String,
    #[serde(default)]
    pub entry: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub changes: Vec<Change>,
}

#[derive(Debug, Deserialize)]
pub struct Change {
    pub field: String,
    pub value: ChangeValue,
}

#[derive(Debug, Deserialize)]
pub struct ChangeValue {
    pub messaging_product: Option<String>,
    #[serde(default)]
    pub messages: Vec<IncomingMessage>,
    #[serde(default)]
    pub contacts: Vec<Contact>,
}

#[derive(Debug, Deserialize)]
pub struct IncomingMessage {
    pub from: String,
    pub id: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub text: Option<TextBody>,
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
pub struct TextBody {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct Contact {
    pub profile: ContactProfile,
    pub wa_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ContactProfile {
    pub name: String,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    messaging_product: &'static str,
    to: String,
    #[serde(rename = "type")]
    msg_type: &'static str,
    text: TextPayload,
}

#[derive(Debug, Serialize)]
struct TextPayload {
    body: String,
}

// --- Webhook verification (GET) ---

#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    #[serde(rename = "hub.mode")]
    pub mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub challenge: Option<String>,
}

/// GET handler for WhatsApp webhook verification.
pub async fn verify_handler(Query(query): Query<VerifyQuery>) -> (StatusCode, String) {
    let expected_token = match verify_token() {
        Some(t) => t,
        None => {
            warn!("WHATSAPP_VERIFY_TOKEN not set");
            return (StatusCode::FORBIDDEN, String::new());
        }
    };

    match (
        query.mode.as_deref(),
        query.verify_token.as_deref(),
        query.challenge,
    ) {
        (Some("subscribe"), Some(token), Some(challenge)) if token == expected_token => {
            info!("whatsapp webhook verified");
            (StatusCode::OK, challenge)
        }
        _ => {
            warn!("whatsapp webhook verification failed");
            (StatusCode::FORBIDDEN, String::new())
        }
    }
}

// --- Webhook handler (POST) ---

/// POST handler for WhatsApp webhook events.
pub async fn webhook_handler(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> StatusCode {
    let app_secret = match app_secret() {
        Some(secret) => secret,
        None => {
            warn!("WHATSAPP_APP_SECRET is not configured");
            return StatusCode::FORBIDDEN;
        }
    };

    let signature = headers
        .get("X-Hub-Signature-256")
        .and_then(|value| value.to_str().ok());

    if !verify_whatsapp_signature(&app_secret, signature, body.as_ref()) {
        warn!("whatsapp webhook signature verification failed");
        return StatusCode::UNAUTHORIZED;
    }

    let payload: WebhookPayload = match serde_json::from_slice(body.as_ref()) {
        Ok(payload) => payload,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    if payload.object != "whatsapp_business_account" {
        return StatusCode::OK;
    }

    let token = match whatsapp_token() {
        Some(t) => t,
        None => {
            error!("WHATSAPP_TOKEN not set");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let phone_id = match phone_number_id() {
        Some(id) => id,
        None => {
            error!("WHATSAPP_PHONE_NUMBER_ID not set");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    for entry in &payload.entry {
        for change in &entry.changes {
            if change.field != "messages" {
                continue;
            }

            let contact_name = change
                .value
                .contacts
                .first()
                .map(|c| c.profile.name.clone())
                .unwrap_or_else(|| "Unknown".to_owned());

            let store =
                genesis_storage::SessionStore::new(&state.loaded.config.storage.database_path);

            for message in &change.value.messages {
                if message.msg_type != "text" {
                    continue;
                }
                let text = match &message.text {
                    Some(t) if !t.body.is_empty() => t.body.clone(),
                    _ => continue,
                };

                let from = message.from.clone();
                let session_id = format!("wa-{from}");

                let span = info_span!(
                    "whatsapp.webhook",
                    from = from.as_str(),
                    contact = contact_name.as_str(),
                    session_id = session_id.as_str()
                );

                info!(parent: &span, "received whatsapp message");

                // DM pairing check
                match super::check_pairing(
                    &state.loaded.config.storage.database_path,
                    "whatsapp",
                    &from,
                    &contact_name,
                ) {
                    Ok(super::PairingCheck::Approved) => {}
                    Ok(super::PairingCheck::NeedsPairing(code)) => {
                        let reply = super::pairing_reply(&code);
                        let client2 = state.http_client.clone();
                        let token2 = token.clone();
                        let phone2 = phone_id.clone();
                        let from2 = from.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                send_reply(&client2, &token2, &phone2, &from2, &reply).await
                            {
                                error!(error = %e, "failed to send pairing reply");
                            }
                        });
                        continue;
                    }
                    Ok(super::PairingCheck::AtCapacity) => {
                        let reply = super::pairing_capacity_reply().to_owned();
                        let client2 = state.http_client.clone();
                        let token2 = token.clone();
                        let phone2 = phone_id.clone();
                        let from2 = from.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                send_reply(&client2, &token2, &phone2, &from2, &reply).await
                            {
                                error!(error = %e, "failed to send capacity reply");
                            }
                        });
                        continue;
                    }
                    Err(_) => {
                        return StatusCode::SERVICE_UNAVAILABLE;
                    }
                }

                match crate::commands::handle_command(
                    &text,
                    &session_id,
                    &store,
                    &state.loaded.config,
                ) {
                    crate::commands::CommandResult::Reply(reply) => {
                        let client2 = state.http_client.clone();
                        let token2 = token.clone();
                        let phone2 = phone_id.clone();
                        let from2 = from.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                send_reply(&client2, &token2, &phone2, &from2, &reply).await
                            {
                                error!(error = %e, "failed to send command reply");
                            }
                        });
                        continue;
                    }
                    crate::commands::CommandResult::PassThrough => {}
                }

                // Auto-reset expired sessions before processing.
                crate::commands::check_session_expiry(
                    &session_id,
                    &store,
                    state.loaded.config.gateway.as_ref(),
                );

                let state = Arc::clone(&state);
                let token = token.clone();
                let phone_id = phone_id.clone();
                let contact = contact_name.clone();

                tokio::spawn(
                    async move {
                        let service = state.session_service();

                        let result = service
                            .run_turn(SessionTurnInput {
                                session_id: &session_id,
                                session_platform: "whatsapp",
                                delivery_platform: DeliveryPlatform::WhatsApp,
                                prompt: &text,
                                title: Some(&format!("WhatsApp: {contact}")),
                                images: Vec::new(),
                            })
                            .await;

                        let reply_text = super::extract_reply(result, "whatsapp");

                        if let Err(e) =
                            send_reply(&state.http_client, &token, &phone_id, &from, &reply_text)
                                .await
                        {
                            error!(error = %e, "failed to send whatsapp reply");
                        }

                        // Append delivery mirror for cross-platform visibility.
                        crate::mirror::append_delivery_mirror(
                            &state.loaded.config.storage.database_path,
                            "whatsapp",
                            &from,
                            &reply_text,
                            "whatsapp",
                        );
                    }
                    .instrument(span),
                );
            }
        }
    }

    StatusCode::OK
}

/// Send a text message via the WhatsApp Cloud API.
async fn send_reply(
    client: &reqwest::Client,
    token: &str,
    phone_number_id: &str,
    to: &str,
    text: &str,
) -> Result<(), String> {
    let url = format!("https://graph.facebook.com/v21.0/{phone_number_id}/messages");

    // Truncate if needed (WhatsApp limit is ~4096 chars for text)
    let body = if text.len() > MAX_WHATSAPP_MESSAGE_LEN {
        format!("{}...", &text[..MAX_WHATSAPP_MESSAGE_LEN - 3])
    } else {
        text.to_owned()
    };

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&SendMessageRequest {
            messaging_product: "whatsapp",
            to: to.to_owned(),
            msg_type: "text",
            text: TextPayload { body },
        })
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("WhatsApp API error {status}: {body}"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_payload_deserializes() {
        let json = r#"{
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "123",
                "changes": [{
                    "field": "messages",
                    "value": {
                        "messaging_product": "whatsapp",
                        "contacts": [{
                            "profile": {"name": "Cole"},
                            "wa_id": "15551234567"
                        }],
                        "messages": [{
                            "from": "15551234567",
                            "id": "msg-1",
                            "type": "text",
                            "text": {"body": "Hello Eve"},
                            "timestamp": "1709875200"
                        }]
                    }
                }]
            }]
        }"#;
        let payload: WebhookPayload = serde_json::from_str(json).expect("should parse");
        assert_eq!(payload.object, "whatsapp_business_account");
        assert_eq!(payload.entry.len(), 1);
        let change = &payload.entry[0].changes[0];
        assert_eq!(change.field, "messages");
        assert_eq!(change.value.messages[0].from, "15551234567");
        assert_eq!(
            change.value.messages[0]
                .text
                .as_ref()
                .map(|t| t.body.as_str()),
            Some("Hello Eve")
        );
        assert_eq!(change.value.contacts[0].profile.name, "Cole");
    }

    #[test]
    fn webhook_payload_handles_empty_messages() {
        let json = r#"{
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "123",
                "changes": [{
                    "field": "statuses",
                    "value": {
                        "messaging_product": "whatsapp",
                        "messages": [],
                        "contacts": []
                    }
                }]
            }]
        }"#;
        let payload: WebhookPayload = serde_json::from_str(json).expect("should parse");
        assert!(payload.entry[0].changes[0].value.messages.is_empty());
    }

    #[test]
    fn send_message_request_serializes() {
        let req = SendMessageRequest {
            messaging_product: "whatsapp",
            to: "15551234567".to_owned(),
            msg_type: "text",
            text: TextPayload {
                body: "Hello!".to_owned(),
            },
        };
        let json = serde_json::to_string(&req).expect("should serialize");
        assert!(json.contains("\"messaging_product\":\"whatsapp\""));
        assert!(json.contains("\"to\":\"15551234567\""));
        assert!(json.contains("\"body\":\"Hello!\""));
    }

    #[test]
    fn webhook_payload_handles_media_only_message() {
        let json = r#"{
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "123",
                "changes": [{
                    "field": "messages",
                    "value": {
                        "messaging_product": "whatsapp",
                        "contacts": [{
                            "profile": {"name": "Cole"},
                            "wa_id": "15551234567"
                        }],
                        "messages": [{
                            "from": "15551234567",
                            "id": "msg-2",
                            "type": "image",
                            "timestamp": "1709875200"
                        }]
                    }
                }]
            }]
        }"#;
        let payload: WebhookPayload = serde_json::from_str(json).expect("should parse");
        let msg = &payload.entry[0].changes[0].value.messages[0];
        assert_eq!(msg.msg_type, "image");
        assert!(msg.text.is_none(), "media-only messages have no text field");
    }

    #[test]
    fn session_id_is_stable_per_phone_number() {
        let from = "15551234567";
        let session_id = format!("wa-{from}");
        assert_eq!(session_id, "wa-15551234567");
    }

    #[test]
    fn verify_query_fields_accessible() {
        let query = VerifyQuery {
            mode: Some("subscribe".to_owned()),
            verify_token: Some("mytoken".to_owned()),
            challenge: Some("challenge123".to_owned()),
        };
        assert_eq!(query.mode.as_deref(), Some("subscribe"));
        assert_eq!(query.verify_token.as_deref(), Some("mytoken"));
        assert_eq!(query.challenge.as_deref(), Some("challenge123"));
    }
}
