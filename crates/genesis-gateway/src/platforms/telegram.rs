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

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use genesis_core::execution::SessionTurnInput;
use genesis_storage::{SessionStore, StickerCacheStore};
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, info_span, warn, Instrument};

use crate::verify::verify_secret_token;
use crate::AppState;

/// Telegram Bot API token, loaded from environment.
pub fn bot_token() -> Option<String> {
    genesis_config::env::get_opt(genesis_config::env::TELEGRAM_BOT_TOKEN)
}

pub fn webhook_secret() -> Option<String> {
    genesis_config::env::get_opt(genesis_config::env::TELEGRAM_WEBHOOK_SECRET)
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
    /// Sticker message.
    pub sticker: Option<TelegramSticker>,
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
pub struct TelegramSticker {
    pub file_id: String,
    pub file_unique_id: String,
    #[serde(default)]
    pub is_animated: bool,
    #[serde(default)]
    pub is_video: bool,
    pub emoji: Option<String>,
    pub set_name: Option<String>,
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

// --- Telegram file download helper ---

/// Call the Telegram `getFile` API and download the file bytes.
///
/// This is shared between voice/audio transcription and sticker image analysis
/// to avoid duplicating the two-step getFile + download flow.
async fn telegram_fetch_file(
    client: &reqwest::Client,
    token: &str,
    file_id: &str,
) -> Result<(Vec<u8>, String), super::PlatformError> {
    use genesis_types::DeliveryPlatform;

    let get_file_url = format!("https://api.telegram.org/bot{token}/getFile");
    let resp = client
        .post(&get_file_url)
        .json(&serde_json::json!({ "file_id": file_id }))
        .send()
        .await
        .map_err(|source| super::PlatformError::HttpRequest {
            platform: DeliveryPlatform::Telegram,
            source,
        })?;

    let file_resp: GetFileResponse = resp.json().await.map_err(|source| {
        super::PlatformError::ResponseParse {
            platform: DeliveryPlatform::Telegram,
            source,
        }
    })?;

    if !file_resp.ok {
        return Err(super::PlatformError::ApiLogicError {
            platform: DeliveryPlatform::Telegram,
            detail: "getFile returned not ok".to_owned(),
        });
    }

    let file_path = file_resp
        .result
        .and_then(|r| r.file_path)
        .ok_or_else(|| super::PlatformError::OperationFailed {
            platform: DeliveryPlatform::Telegram,
            operation: "getFile",
            detail: "no file_path in response".to_owned(),
        })?;

    let download_url = format!("https://api.telegram.org/file/bot{token}/{file_path}");
    let download_resp = client.get(&download_url).send().await.map_err(|source| {
        super::PlatformError::HttpRequest {
            platform: DeliveryPlatform::Telegram,
            source,
        }
    })?;

    let file_bytes = download_resp.bytes().await.map_err(|source| {
        super::PlatformError::ResponseParse {
            platform: DeliveryPlatform::Telegram,
            source,
        }
    })?;

    if file_bytes.is_empty() {
        return Err(super::PlatformError::OperationFailed {
            platform: DeliveryPlatform::Telegram,
            operation: "file download",
            detail: "downloaded file is empty".to_owned(),
        });
    }

    Ok((file_bytes.to_vec(), file_path))
}

// --- Voice/Audio transcription helpers ---

/// Download a file from Telegram by file_id, then transcribe via Whisper API.
/// Returns the transcribed text, or an error description.
async fn transcribe_telegram_audio(
    client: &reqwest::Client,
    token: &str,
    file_id: &str,
) -> Result<String, super::PlatformError> {
    use genesis_types::DeliveryPlatform;

    let (audio_bytes, file_path) = telegram_fetch_file(client, token, file_id).await?;

    // Transcribe via Whisper API.
    let api_key = genesis_config::env::get(genesis_config::env::OPENAI_API_KEY).map_err(
        |_| super::PlatformError::ConfigMissing {
            platform: DeliveryPlatform::Telegram,
            detail: "OPENAI_API_KEY not set, cannot transcribe".to_owned(),
        },
    )?;

    let api_base = genesis_config::env::get_or(
        genesis_config::env::OPENAI_API_BASE,
        "https://api.openai.com/v1",
    );

    // Determine file extension from file_path.
    let ext = file_path.rsplit('.').next().unwrap_or("ogg");
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
        .map_err(|e| super::PlatformError::OperationFailed {
            platform: DeliveryPlatform::Telegram,
            operation: "multipart build",
            detail: e.to_string(),
        })?;

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
        .map_err(|source| super::PlatformError::HttpRequest {
            platform: DeliveryPlatform::Telegram,
            source,
        })?;

    let status = whisper_resp.status();
    if !status.is_success() {
        let body = whisper_resp.text().await.unwrap_or_default();
        return Err(super::PlatformError::ApiError {
            platform: DeliveryPlatform::Telegram,
            status,
            body,
        });
    }

    let result: WhisperResponse = whisper_resp.json().await.map_err(|source| {
        super::PlatformError::ResponseParse {
            platform: DeliveryPlatform::Telegram,
            source,
        }
    })?;

    Ok(result.text)
}

// --- Sticker analysis helpers ---

/// Build a context string for a sticker message.
///
/// For static (non-animated, non-video) stickers, downloads the image and
/// analyzes it with a vision LLM, caching the result. For animated/video
/// stickers, returns emoji-based context only since they can't be analyzed
/// as static images.
async fn resolve_sticker_prompt(
    sticker: &TelegramSticker,
    client: &reqwest::Client,
    token: &str,
    config: &genesis_config::GenesisConfig,
) -> String {
    let emoji = sticker.emoji.as_deref().unwrap_or("");
    let set_name = sticker.set_name.as_deref().unwrap_or("unknown");

    // Animated and video stickers can't be analyzed as static images.
    if sticker.is_animated || sticker.is_video {
        return format!(
            "[The user sent an animated sticker {emoji} from \"{set_name}\". \
             Animated stickers cannot be visually analyzed.]"
        );
    }

    // Check the cache first.
    let cache = StickerCacheStore::new(&config.storage.database_path);
    match cache.get(&sticker.file_unique_id) {
        Ok(Some(cached)) => {
            debug!(
                file_unique_id = sticker.file_unique_id.as_str(),
                "sticker cache hit"
            );
            return format_sticker_context(&cached.emoji, &cached.sticker_set, &cached.description);
        }
        Ok(None) => {}
        Err(e) => {
            warn!(error = %e, "sticker cache lookup failed, proceeding with analysis");
        }
    }

    // Download and analyze the sticker via vision LLM.
    match analyze_sticker_image(client, token, &sticker.file_id, config).await {
        Ok(description) => {
            info!(
                file_unique_id = sticker.file_unique_id.as_str(),
                "sticker analyzed via vision"
            );
            // Cache the result.
            if let Err(e) = cache.set(&sticker.file_unique_id, &description, emoji, set_name) {
                warn!(error = %e, "failed to cache sticker description");
            }
            format_sticker_context(emoji, set_name, &description)
        }
        Err(e) => {
            warn!(error = %e, "sticker vision analysis failed, using emoji fallback");
            format!(
                "[The user sent a sticker {emoji} from \"{set_name}\". \
                 Vision analysis failed: {e}]"
            )
        }
    }
}

/// Format a sticker context string for the agent prompt.
fn format_sticker_context(emoji: &str, set_name: &str, description: &str) -> String {
    format!("[The user sent a sticker {emoji} from \"{set_name}\". It shows: \"{description}\"]")
}

/// Download a sticker image from Telegram and analyze it with a vision LLM.
async fn analyze_sticker_image(
    client: &reqwest::Client,
    token: &str,
    file_id: &str,
    config: &genesis_config::GenesisConfig,
) -> Result<String, super::PlatformError> {
    use genesis_types::DeliveryPlatform;

    let (image_bytes, file_path) = telegram_fetch_file(client, token, file_id).await?;

    // Determine MIME type from file extension.
    let ext = file_path.rsplit('.').next().unwrap_or("webp");
    let mime = match ext {
        "webp" => "image/webp",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "image/webp", // Telegram stickers are typically WebP
    };

    // Step 3: Encode as base64 data URI.
    let b64 = base64::engine::general_purpose::STANDARD.encode(&image_bytes);
    let data_uri = format!("data:{mime};base64,{b64}");

    // Step 4: Call the vision LLM.
    let env: BTreeMap<String, String> = genesis_config::env::all_vars();
    let provider = genesis_provider::resolve(
        &config.provider.backend,
        &config.provider.model,
        config.provider.base_url.as_deref(),
        config.provider.api_key_env.as_deref(),
        &env,
    );

    let vision_client = genesis_provider::ChatClient::new(&provider).map_err(|e| {
        super::PlatformError::OperationFailed {
            platform: DeliveryPlatform::Telegram,
            operation: "vision client creation",
            detail: e.to_string(),
        }
    })?;

    let request = genesis_provider::ChatCompletionRequest {
        model: String::new(), // ChatClient fills this from its config
        messages: vec![genesis_provider::ChatMessage::user_with_images(
            "Describe this sticker in 1-2 sentences. Focus on what it depicts — \
             character, action, emotion, and any text visible in the image.",
            vec![genesis_provider::ImageUrl {
                url: data_uri,
                detail: Some("low".to_owned()),
            }],
        )],
        tools: Vec::new(),
        temperature: Some(0.2),
        max_tokens: Some(150),
        stream: None,
        stream_options: None,
        response_format: None,
        tool_choice: None,
        thinking: None,
        extra_body: config.provider.extra_body.clone(),
    };

    let response = vision_client.complete(request).await.map_err(|e| {
        super::PlatformError::OperationFailed {
            platform: DeliveryPlatform::Telegram,
            operation: "vision LLM call",
            detail: e.to_string(),
        }
    })?;

    let description = response
        .choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .and_then(|c| c.text())
        .map(|t| t.trim().to_owned())
        .ok_or_else(|| super::PlatformError::OperationFailed {
            platform: DeliveryPlatform::Telegram,
            operation: "vision LLM call",
            detail: "empty response".to_owned(),
        })?;

    Ok(description)
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

    if !verify_secret_token(&expected_webhook_secret, provided_webhook_secret) {
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
        Voice {
            file_id: String,
            duration: i64,
        },
        Audio {
            file_id: String,
            duration: i64,
            file_name: String,
        },
        Sticker(TelegramSticker),
    }

    let input = if let Some(t) = message.text.filter(|t| !t.is_empty()) {
        MessageInput::Text(t)
    } else if let Some(sticker) = message.sticker {
        MessageInput::Sticker(sticker)
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
            file_name: audio
                .file_name
                .clone()
                .unwrap_or_else(|| "audio".to_owned()),
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
                if let Err(e) =
                    send_reply(&client2, &token2, chat_id, &reply, Some(message_id)).await
                {
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
                if let Err(e) =
                    send_reply(&client2, &token2, chat_id, &reply, Some(message_id)).await
                {
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
                if let Err(e) =
                    send_reply(&client2, &token2, chat_id, &reply, Some(message_id)).await
                {
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
                MessageInput::Sticker(sticker) => {
                    resolve_sticker_prompt(
                        &sticker,
                        &state.http_client,
                        &token,
                        &state.loaded.config,
                    )
                    .await
                }
            };

            let service = state.session_service();

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

            let reply_text = super::extract_reply(result, "telegram");

            if let Err(e) = send_reply(&state.http_client, &token, chat_id, &reply_text, Some(message_id)).await {
                error!(error = %e, "failed to send telegram reply");
            }

            // Append delivery mirror for cross-platform visibility.
            crate::mirror::append_delivery_mirror(
                &state.loaded.config.storage.database_path,
                "telegram",
                &chat_id.to_string(),
                &reply_text,
                "telegram",
            );
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
) -> Result<(), super::PlatformError> {
    use genesis_types::DeliveryPlatform;

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
            .map_err(|source| super::PlatformError::HttpRequest {
                platform: DeliveryPlatform::Telegram,
                source,
            })?;

        let api_resp: TelegramApiResponse =
            resp.json().await.map_err(|source| {
                super::PlatformError::ResponseParse {
                    platform: DeliveryPlatform::Telegram,
                    source,
                }
            })?;

        if !api_resp.ok {
            return Err(super::PlatformError::ApiLogicError {
                platform: DeliveryPlatform::Telegram,
                detail: api_resp.description.unwrap_or_default(),
            });
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

        // Walk back to a char boundary so we never slice inside a multi-byte
        // character (e.g. emoji). `floor_char_boundary` returns the largest
        // byte index <= max_len that sits on a UTF-8 char boundary.
        let safe_end = remaining.floor_char_boundary(max_len);

        // Try to find a newline within the limit, then a space
        let mut split_at = remaining[..safe_end]
            .rfind('\n')
            .or_else(|| remaining[..safe_end].rfind(' '))
            .unwrap_or(safe_end);

        // Guard: if remaining starts with a delimiter, rfind returns 0 which
        // would push an empty chunk. Fall back to the full safe window.
        if split_at == 0 {
            split_at = safe_end;
        }

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
    fn split_message_multibyte_utf8_does_not_panic() {
        // Each emoji is 4 bytes. Build a string that forces a split in the
        // middle of a multi-byte character when using naive byte indexing.
        let emoji = "\u{1F600}"; // 4 bytes
        let text = emoji.repeat(30); // 120 bytes total
                                     // max_len=50 would land inside an emoji at byte 50 if not handled.
        let chunks = split_message(&text, 50);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 50);
            assert!(!chunk.is_empty());
        }
    }

    #[test]
    fn split_message_leading_delimiter_does_not_produce_empty_chunk() {
        // When remaining starts with '\n' or ' ', rfind returns Some(0).
        // The guard must prevent pushing an empty chunk.
        let text = format!("\n{}", "a".repeat(100));
        let chunks = split_message(&text, 50);
        for chunk in &chunks {
            assert!(!chunk.is_empty(), "empty chunk produced");
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

    #[test]
    fn telegram_sticker_message_deserializes() {
        let json = r#"{
            "update_id": 200,
            "message": {
                "message_id": 10,
                "chat": {"id": 42, "type": "private"},
                "from": {"id": 100, "first_name": "Cole"},
                "sticker": {
                    "file_id": "CAACAgIAAxkBAAIBe2abc",
                    "file_unique_id": "AgADuQAD1234",
                    "is_animated": false,
                    "is_video": false,
                    "emoji": "😀",
                    "set_name": "TestPack"
                }
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).expect("should parse");
        let msg = update.message.expect("should have message");
        let sticker = msg.sticker.expect("should have sticker");
        assert_eq!(sticker.file_id, "CAACAgIAAxkBAAIBe2abc");
        assert_eq!(sticker.file_unique_id, "AgADuQAD1234");
        assert!(!sticker.is_animated);
        assert!(!sticker.is_video);
        assert_eq!(sticker.emoji.as_deref(), Some("😀"));
        assert_eq!(sticker.set_name.as_deref(), Some("TestPack"));
    }

    #[test]
    fn telegram_animated_sticker_deserializes() {
        let json = r#"{
            "update_id": 201,
            "message": {
                "message_id": 11,
                "chat": {"id": 42, "type": "private"},
                "sticker": {
                    "file_id": "CAACAgIAAxkBAAIBe2xyz",
                    "file_unique_id": "AgADuQAD5678",
                    "is_animated": true,
                    "is_video": false,
                    "emoji": "🎉",
                    "set_name": "AnimatedPack"
                }
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).expect("should parse");
        let msg = update.message.expect("should have message");
        let sticker = msg.sticker.expect("should have sticker");
        assert!(sticker.is_animated);
        assert_eq!(sticker.set_name.as_deref(), Some("AnimatedPack"));
    }

    #[test]
    fn telegram_video_sticker_deserializes() {
        let json = r#"{
            "update_id": 202,
            "message": {
                "message_id": 12,
                "chat": {"id": 42, "type": "private"},
                "sticker": {
                    "file_id": "CAACAgIvideo",
                    "file_unique_id": "AgADvideo",
                    "is_animated": false,
                    "is_video": true
                }
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).expect("should parse");
        let msg = update.message.expect("should have message");
        let sticker = msg.sticker.expect("should have sticker");
        assert!(sticker.is_video);
        assert!(sticker.emoji.is_none());
        assert!(sticker.set_name.is_none());
    }

    #[test]
    fn sticker_fields_extracted() {
        let sticker = TelegramSticker {
            file_id: "CAACAgIAAxkBAAIBe2abc".to_owned(),
            file_unique_id: "AgADuQAD1234".to_owned(),
            is_animated: false,
            is_video: false,
            emoji: Some("😺".to_owned()),
            set_name: Some("CatPack".to_owned()),
        };
        assert_eq!(sticker.file_id, "CAACAgIAAxkBAAIBe2abc");
        assert_eq!(sticker.file_unique_id, "AgADuQAD1234");
        assert!(!sticker.is_animated);
        assert!(!sticker.is_video);
        assert_eq!(sticker.emoji.as_deref(), Some("😺"));
        assert_eq!(sticker.set_name.as_deref(), Some("CatPack"));
    }

    #[test]
    fn format_sticker_context_produces_expected_output() {
        let result = format_sticker_context("😀", "MyPack", "A cat waving");
        assert_eq!(
            result,
            "[The user sent a sticker 😀 from \"MyPack\". It shows: \"A cat waving\"]"
        );
    }

    #[test]
    fn format_sticker_context_handles_empty_emoji() {
        let result = format_sticker_context("", "StickerSet", "A dog sleeping");
        assert_eq!(
            result,
            "[The user sent a sticker  from \"StickerSet\". It shows: \"A dog sleeping\"]"
        );
    }

    #[test]
    fn sticker_cache_integration_with_format() {
        // Test that cached sticker data produces the right context string.
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let cache = StickerCacheStore::new(&db_path);
        cache
            .set("unique-123", "A happy frog jumping", "🐸", "FrogPack")
            .expect("cache set");

        let cached = cache.get("unique-123").unwrap().unwrap();
        let context =
            format_sticker_context(&cached.emoji, &cached.sticker_set, &cached.description);
        assert_eq!(
            context,
            "[The user sent a sticker 🐸 from \"FrogPack\". It shows: \"A happy frog jumping\"]"
        );
    }
}
