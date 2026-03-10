//! Telegram Bot API webhook handler.
//!
//! Receives Telegram webhook updates, processes messages through the agent,
//! and sends replies back via the Telegram Bot API.
//!
//! ## Setup
//! 1. Create a bot via @BotFather on Telegram
//! 2. Set the `TELEGRAM_BOT_TOKEN` environment variable
//! 3. Set `TELEGRAM_WEBHOOK_SECRET` for webhook requests
//! 4. Register the webhook URL: `POST https://api.telegram.org/bot<TOKEN>/setWebhook`
//!    with `{"url": "https://your-server/telegram/webhook"}`

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use genesis_core::execution::{SessionExecutionService, SessionTurnInput};
use genesis_storage::SessionStore;
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};
use tracing::{error, info, info_span, warn, Instrument};

use crate::verify::verify_telegram_webhook_secret;
use crate::AppState;

/// Telegram Bot API token, loaded from environment.
pub fn bot_token() -> Option<String> {
    std::env::var("TELEGRAM_BOT_TOKEN").ok()
}

pub fn webhook_secret() -> Option<String> {
    std::env::var("TELEGRAM_WEBHOOK_SECRET").ok()
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
    /// Voice message (audio recorded inline in Telegram).
    pub voice: Option<TelegramVoice>,
    /// Audio file (uploaded as a document).
    pub audio: Option<TelegramAudio>,
    /// Photo attachments (array of sizes, largest last).
    pub photo: Option<Vec<TelegramPhoto>>,
    /// Caption for photo/audio/voice messages.
    pub caption: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramVoice {
    pub file_id: String,
    pub duration: i64,
}

#[derive(Debug, Deserialize)]
pub struct TelegramAudio {
    pub file_id: String,
    pub duration: Option<i64>,
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramPhoto {
    pub file_id: String,
    pub width: i64,
    pub height: i64,
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

/// Response from Telegram `getFile` API.
#[derive(Debug, Deserialize)]
struct GetFileResponse {
    ok: bool,
    result: Option<TelegramFile>,
}

#[derive(Debug, Deserialize)]
struct TelegramFile {
    file_path: Option<String>,
}

/// Whisper API transcription response.
#[derive(Debug, Deserialize)]
struct WhisperResponse {
    text: String,
}

// --- Voice/Audio transcription helpers ---

/// Download a file from Telegram by file_id, then transcribe via Whisper API.
/// Returns the transcribed text, or an error description.
async fn transcribe_telegram_audio(client: &reqwest::Client, token: &str, file_id: &str) -> Result<String, String> {
    // Step 1: Get the file path from Telegram.
    let get_file_url = format!("https://api.telegram.org/bot{token}/getFile");
    let resp = client
        .post(&get_file_url)
        .json(&serde_json::json!({ "file_id": file_id }))
        .send()
        .await
        .map_err(|e| format!("getFile request failed: {e}"))?;

    let file_resp: GetFileResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse getFile response: {e}"))?;

    if !file_resp.ok {
        return Err("Telegram getFile returned not ok".to_owned());
    }

    let file_path = file_resp
        .result
        .and_then(|r| r.file_path)
        .ok_or_else(|| "no file_path in getFile response".to_owned())?;

    // Step 2: Download the actual audio file.
    let download_url = format!("https://api.telegram.org/file/bot{token}/{file_path}");
    let audio_bytes = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("file download failed: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("failed to read audio bytes: {e}"))?;

    if audio_bytes.is_empty() {
        return Err("downloaded audio file is empty".to_owned());
    }

    // Step 3: Transcribe via Whisper API.
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY not set, cannot transcribe".to_owned())?;

    let api_base = std::env::var("OPENAI_API_BASE")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());

    // Determine file extension from file_path.
    let ext = file_path
        .rsplit('.')
        .next()
        .unwrap_or("ogg");
    let mime = match ext {
        "ogg" | "oga" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "webm" => "audio/webm",
        "flac" => "audio/flac",
        _ => "audio/ogg", // Telegram voice messages default to ogg/opus
    };

    let file_part = reqwest::multipart::Part::bytes(audio_bytes.to_vec())
        .file_name(format!("voice.{ext}"))
        .mime_str(mime)
        .map_err(|e| format!("failed to build multipart: {e}"))?;

    let form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", "whisper-1")
        .text("response_format", "json");

    let whisper_url = format!("{}/audio/transcriptions", api_base.trim_end_matches('/'));

    let whisper_resp = client
        .post(&whisper_url)
        .header("Authorization", format!("Bearer {api_key}"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Whisper API request failed: {e}"))?;

    let status = whisper_resp.status();
    if !status.is_success() {
        let body = whisper_resp.text().await.unwrap_or_default();
        return Err(format!("Whisper API returned {status}: {body}"));
    }

    let result: WhisperResponse = whisper_resp
        .json()
        .await
        .map_err(|e| format!("failed to parse Whisper response: {e}"))?;

    Ok(result.text)
}

// --- Handler ---

/// Webhook handler for Telegram updates.
///
/// Telegram expects a 200 OK response quickly, so we spawn the agent
/// execution in a background task and reply via the Bot API.
pub async fn webhook_handler(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> StatusCode {
    let expected_webhook_secret = match webhook_secret() {
        Some(secret) => secret,
        None => {
            warn!("TELEGRAM_WEBHOOK_SECRET is not configured");
            return StatusCode::FORBIDDEN;
        }
    };

    let provided_webhook_secret = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|value| value.to_str().ok());

    if !verify_telegram_webhook_secret(&expected_webhook_secret, provided_webhook_secret) {
        warn!("telegram webhook secret verification failed");
        return StatusCode::UNAUTHORIZED;
    }

    let update: TelegramUpdate = match serde_json::from_slice(body.as_ref()) {
        Ok(update) => update,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let message = match update.message {
        Some(m) => m,
        None => return StatusCode::OK, // Ignore non-message updates (edits, callbacks, etc.)
    };

    // Classify the incoming message into a typed input.
    enum MessageInput {
        Text(String),
        Voice { file_id: String, duration: i64 },
        Audio { file_id: String, duration: i64, file_name: String },
    }

    let input = if let Some(t) = message.text.filter(|t| !t.is_empty()) {
        MessageInput::Text(t)
    } else if let Some(photos) = &message.photo {
        let best = photos.last();
        let caption = message.caption.as_deref().unwrap_or("");
        let file_info = best
            .map(|p| format!("{}x{}, file_id={}", p.width, p.height, p.file_id))
            .unwrap_or_else(|| "unknown".to_owned());
        MessageInput::Text(format!(
            "[Photo received: {file_info}. Caption: \"{caption}\". \
             Use the vision tool to analyze the image if needed.]"
        ))
    } else if let Some(voice) = &message.voice {
        MessageInput::Voice {
            file_id: voice.file_id.clone(),
            duration: voice.duration,
        }
    } else if let Some(audio) = &message.audio {
        MessageInput::Audio {
            file_id: audio.file_id.clone(),
            duration: audio.duration.unwrap_or(0),
            file_name: audio.file_name.clone().unwrap_or_else(|| "audio".to_owned()),
        }
    } else {
        return StatusCode::OK; // Ignore unsupported message types
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

    // DM pairing check — gate unknown users
    let user_id_str = message
        .from
        .as_ref()
        .map(|u| u.id.to_string())
        .unwrap_or_else(|| chat_id.to_string());

    match super::check_pairing(
        &state.loaded.config.storage.database_path,
        "telegram",
        &user_id_str,
        &user_name,
    ) {
        Ok(super::PairingCheck::Approved) => {} // proceed
        Ok(super::PairingCheck::NeedsPairing(code)) => {
            let reply = super::pairing_reply(&code);
            let client2 = state.http_client.clone();
            let token2 = token.clone();
            tokio::spawn(async move {
                if let Err(e) = send_reply(&client2, &token2, chat_id, &reply, Some(message_id)).await {
                    error!(error = %e, "failed to send pairing reply");
                }
            });
            return StatusCode::OK;
        }
        Ok(super::PairingCheck::AtCapacity) => {
            let reply = super::pairing_capacity_reply().to_owned();
            let client2 = state.http_client.clone();
            let token2 = token.clone();
            tokio::spawn(async move {
                if let Err(e) = send_reply(&client2, &token2, chat_id, &reply, Some(message_id)).await {
                    error!(error = %e, "failed to send capacity reply");
                }
            });
            return StatusCode::OK;
        }
        Err(_) => {
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }

    // Handle gateway slash commands (only for text messages).
    if let MessageInput::Text(ref text) = input {
        let store = SessionStore::new(&state.loaded.config.storage.database_path);
        if let crate::commands::CommandResult::Reply(reply) =
            crate::commands::handle_command(text, &session_id, &store, &state.loaded.config)
        {
            let client2 = state.http_client.clone();
            let token2 = token.clone();
            tokio::spawn(async move {
                if let Err(e) = send_reply(&client2, &token2, chat_id, &reply, Some(message_id)).await {
                    error!(error = %e, "failed to send command reply");
                }
            });
            return StatusCode::OK;
        }
    }

    // Auto-reset expired sessions before processing.
    {
        let store = SessionStore::new(&state.loaded.config.storage.database_path);
        crate::commands::check_session_expiry(
            &session_id,
            &store,
            state.loaded.config.gateway.as_ref(),
        );
    }

    // Spawn background task so we return 200 immediately
    let state = Arc::clone(&state);
    tokio::spawn(
        async move {
            // Resolve input to a prompt string, auto-transcribing voice/audio.
            let prompt = match input {
                MessageInput::Text(text) => text,
                MessageInput::Voice { file_id, duration } => {
                    match transcribe_telegram_audio(&state.http_client, &token, &file_id).await {
                        Ok(transcript) => {
                            info!(duration, "voice message transcribed successfully");
                            format!("[Voice message transcription ({duration}s)]: {transcript}")
                        }
                        Err(e) => {
                            warn!(error = %e, "voice transcription failed, falling back to metadata");
                            format!(
                                "[Voice message received: {duration}s audio, file_id={file_id}. \
                                 Auto-transcription failed: {e}. \
                                 Use the transcribe tool if needed.]"
                            )
                        }
                    }
                }
                MessageInput::Audio { file_id, duration, file_name } => {
                    match transcribe_telegram_audio(&state.http_client, &token, &file_id).await {
                        Ok(transcript) => {
                            info!(file_name = file_name.as_str(), "audio file transcribed successfully");
                            format!("[Audio \"{file_name}\" transcription ({duration}s)]: {transcript}")
                        }
                        Err(e) => {
                            warn!(error = %e, "audio transcription failed, falling back to metadata");
                            format!(
                                "[Audio file received: \"{file_name}\", {duration}s, file_id={file_id}. \
                                 Auto-transcription failed: {e}. \
                                 Use the transcribe tool if needed.]"
                            )
                        }
                    }
                }
            };

            let service = SessionExecutionService::new(&state.loaded);

            let result = service
                .run_turn(SessionTurnInput {
                    session_id: &session_id,
                    session_platform: "telegram",
                    delivery_platform: DeliveryPlatform::Telegram,
                    prompt: &prompt,
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

            if let Err(e) = send_reply(&state.http_client, &token, chat_id, &reply_text, Some(message_id)).await {
                error!(error = %e, "failed to send telegram reply");
            }
        }
        .instrument(span),
    );

    StatusCode::OK
}

/// Send a message via the Telegram Bot API.
async fn send_reply(
    client: &reqwest::Client,
    token: &str,
    chat_id: i64,
    text: &str,
    reply_to: Option<i64>,
) -> Result<(), String> {
    // Telegram messages have a 4096 character limit.
    // Split long responses into chunks.
    let chunks = split_message(text, 4096);

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
    fn telegram_photo_message_deserializes() {
        let json = r#"{
            "update_id": 102,
            "message": {
                "message_id": 7,
                "chat": {"id": 42, "type": "private"},
                "photo": [
                    {"file_id": "small", "width": 90, "height": 90},
                    {"file_id": "medium", "width": 320, "height": 320},
                    {"file_id": "large", "width": 800, "height": 800}
                ],
                "caption": "Check this out"
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).expect("should parse");
        let msg = update.message.expect("should have message");
        let photos = msg.photo.expect("should have photos");
        assert_eq!(photos.len(), 3);
        assert_eq!(photos.last().unwrap().file_id, "large");
        assert_eq!(msg.caption.as_deref(), Some("Check this out"));
    }

    #[test]
    fn telegram_voice_message_deserializes() {
        let json = r#"{
            "update_id": 100,
            "message": {
                "message_id": 5,
                "chat": {"id": 42, "type": "private"},
                "from": {"id": 100, "first_name": "Cole"},
                "voice": {"file_id": "AwACAgIAAxkBAAIB", "duration": 12}
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).expect("should parse");
        let msg = update.message.expect("should have message");
        assert!(msg.text.is_none());
        let voice = msg.voice.expect("should have voice");
        assert_eq!(voice.file_id, "AwACAgIAAxkBAAIB");
        assert_eq!(voice.duration, 12);
    }

    #[test]
    fn telegram_audio_message_deserializes() {
        let json = r#"{
            "update_id": 101,
            "message": {
                "message_id": 6,
                "chat": {"id": 42, "type": "private"},
                "audio": {"file_id": "CQACAgI", "duration": 180, "file_name": "podcast.mp3"}
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).expect("should parse");
        let msg = update.message.expect("should have message");
        let audio = msg.audio.expect("should have audio");
        assert_eq!(audio.file_id, "CQACAgI");
        assert_eq!(audio.file_name.as_deref(), Some("podcast.mp3"));
    }

    #[test]
    fn session_id_is_stable_per_chat() {
        let chat_id: i64 = 12345;
        let session_id = format!("tg-{chat_id}");
        assert_eq!(session_id, "tg-12345");
    }

    #[test]
    fn voice_message_fields_extracted() {
        let voice = TelegramVoice {
            file_id: "AwACAgIAAxkBAAIB".to_owned(),
            duration: 12,
        };
        assert_eq!(voice.file_id, "AwACAgIAAxkBAAIB");
        assert_eq!(voice.duration, 12);
    }

    #[test]
    fn audio_message_fields_extracted() {
        let audio = TelegramAudio {
            file_id: "CQACAgI".to_owned(),
            duration: Some(180),
            file_name: Some("podcast.mp3".to_owned()),
        };
        assert_eq!(audio.file_id, "CQACAgI");
        assert_eq!(audio.duration, Some(180));
        assert_eq!(audio.file_name.as_deref(), Some("podcast.mp3"));
    }

    #[test]
    fn get_file_response_deserializes() {
        let json = r#"{
            "ok": true,
            "result": {
                "file_id": "abc",
                "file_unique_id": "def",
                "file_path": "voice/file_0.oga"
            }
        }"#;
        let resp: GetFileResponse = serde_json::from_str(json).expect("should parse");
        assert!(resp.ok);
        assert_eq!(
            resp.result.unwrap().file_path.as_deref(),
            Some("voice/file_0.oga")
        );
    }

    #[test]
    fn whisper_response_deserializes() {
        let json = r#"{"text": "Hello world, this is a test."}"#;
        let resp: WhisperResponse = serde_json::from_str(json).expect("should parse");
        assert_eq!(resp.text, "Hello world, this is a test.");
    }
}
