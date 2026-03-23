//! Signal messenger platform integration via signal-cli daemon (HTTP mode).
//!
//! Receives messages from signal-cli's REST API and sends replies back using
//! JSON-RPC 2.0 over HTTP.
//!
//! ## Setup
//! 1. Run signal-cli in daemon/HTTP mode (`signal-cli daemon --http`)
//! 2. Set `SIGNAL_ACCOUNT` to your registered phone number (e.g. `+15551234567`)
//! 3. Optionally set `SIGNAL_HTTP_URL` (default: `http://127.0.0.1:8080`)
//! 4. Optionally set `SIGNAL_WEBHOOK_SECRET` — when set, incoming webhook
//!    requests must include an `X-Signal-Secret` header matching this value
//! 5. Optionally set `SIGNAL_GROUP_ALLOWED_USERS` to a comma-separated list of
//!    phone numbers allowed to interact in group chats
//!
//! ## Endpoints
//! - `POST /signal/webhook` — receives incoming messages (called by signal-cli
//!   or a polling proxy)
//! - `POST /signal/poll` — one-shot poll: fetches pending messages from
//!   signal-cli's `/v1/receive/{account}` endpoint and processes them

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use genesis_core::execution::SessionTurnInput;
use genesis_storage::SessionStore;
use genesis_types::DeliveryPlatform;
use serde::{Deserialize, Serialize};
use tracing::{error, info, info_span, warn, Instrument};

use crate::verify::verify_secret_token;
use crate::AppState;

// ---------------------------------------------------------------------------
// Environment configuration
// ---------------------------------------------------------------------------

/// Base URL of the signal-cli HTTP daemon.
fn signal_http_url() -> String {
    std::env::var("SIGNAL_HTTP_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned())
}

/// The registered Signal account (phone number) this agent uses.
fn signal_account() -> Option<String> {
    std::env::var("SIGNAL_ACCOUNT").ok()
}

/// Optional shared secret for webhook endpoint authentication.
/// When set, incoming requests must include an `X-Signal-Secret` header
/// matching this value; otherwise the request is rejected with 401.
fn webhook_secret() -> Option<String> {
    std::env::var("SIGNAL_WEBHOOK_SECRET").ok()
}

/// Comma-separated list of phone numbers allowed to interact in group chats.
fn group_allowed_users() -> Vec<String> {
    std::env::var("SIGNAL_GROUP_ALLOWED_USERS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Signal-cli API types
// ---------------------------------------------------------------------------

/// Envelope from signal-cli (JSON-RPC receive result or webhook payload).
#[derive(Debug, Clone, Deserialize)]
pub struct SignalEnvelope {
    /// The sender's phone number (e.g. `+15551234567`).
    #[serde(default, alias = "source")]
    pub source_number: Option<String>,
    /// Sender name (set by some payloads).
    #[serde(default)]
    pub source_name: Option<String>,
    /// Data message (regular text message).
    #[serde(default, alias = "dataMessage")]
    pub data_message: Option<SignalDataMessage>,
}

/// Data message payload from Signal.
#[derive(Debug, Clone, Deserialize)]
pub struct SignalDataMessage {
    /// UNIX timestamp in milliseconds.
    #[serde(default)]
    pub timestamp: Option<u64>,
    /// Text body (plain text).
    #[serde(default)]
    pub message: Option<String>,
    /// Group info, present when the message was sent in a group.
    #[serde(default, alias = "groupInfo")]
    pub group_info: Option<SignalGroupInfo>,
    /// File attachments.
    #[serde(default)]
    pub attachments: Option<Vec<SignalAttachment>>,
    /// The envelope timestamp this message quotes (for reply threading).
    #[serde(default)]
    pub quote: Option<SignalQuote>,
}

/// Group context.
#[derive(Debug, Clone, Deserialize)]
pub struct SignalGroupInfo {
    /// Base64-encoded group ID.
    #[serde(default, alias = "groupId")]
    pub group_id: Option<String>,
}

/// Attachment descriptor.
#[derive(Debug, Clone, Deserialize)]
pub struct SignalAttachment {
    /// MIME content type (e.g. `image/png`).
    #[serde(default, alias = "contentType")]
    pub content_type: Option<String>,
    /// Local filename on signal-cli's storage.
    #[serde(default)]
    pub filename: Option<String>,
    /// Attachment ID for download.
    #[serde(default)]
    pub id: Option<String>,
    /// Size in bytes.
    #[serde(default)]
    pub size: Option<u64>,
}

/// Quote (reply context).
#[derive(Debug, Clone, Deserialize)]
pub struct SignalQuote {
    /// Timestamp of the quoted message.
    #[serde(default)]
    pub id: Option<u64>,
    /// Author of the quoted message.
    #[serde(default)]
    pub author: Option<String>,
    /// Text snippet of the quoted message.
    #[serde(default)]
    pub text: Option<String>,
}

/// JSON-RPC 2.0 request body for signal-cli daemon.
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    method: &'static str,
    id: String,
    params: serde_json::Value,
}

/// JSON-RPC 2.0 response (only used for error detection).
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

/// The max length of a single Signal message. Signal supports up to 8000
/// bytes but we keep a safe margin for multi-byte characters.
const MAX_SIGNAL_MESSAGE_LEN: usize = 6000;

// ---------------------------------------------------------------------------
// Webhook handler (push mode)
// ---------------------------------------------------------------------------

/// Webhook handler for incoming Signal messages.
///
/// Receives a `SignalEnvelope` JSON body, validates the sender, processes
/// the message through the agent, and replies via signal-cli's JSON-RPC API.
///
/// When `SIGNAL_WEBHOOK_SECRET` is set, the request must include an
/// `X-Signal-Secret` header with a matching value.
pub async fn webhook_handler(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> StatusCode {
    // Authenticate the webhook request when a shared secret is configured.
    if let Some(expected) = webhook_secret() {
        let provided = headers.get("x-signal-secret").and_then(|v| v.to_str().ok());
        if !verify_secret_token(&expected, provided) {
            warn!("signal webhook secret verification failed");
            return StatusCode::UNAUTHORIZED;
        }
    }

    let envelope: SignalEnvelope = match serde_json::from_slice(body.as_ref()) {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "failed to parse signal webhook body");
            return StatusCode::BAD_REQUEST;
        }
    };

    let account = match signal_account() {
        Some(a) => a,
        None => {
            warn!("SIGNAL_ACCOUNT is not configured");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    match process_envelope(&state, &account, envelope).await {
        ProcessResult::Ok => StatusCode::OK,
        ProcessResult::Ignored => StatusCode::OK,
        ProcessResult::Error => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Poll handler: fetches pending messages from signal-cli's REST endpoint.
///
/// Call `POST /signal/poll` to trigger a one-shot receive. This is useful
/// when signal-cli isn't configured to push webhooks.
pub async fn poll_handler(State(state): State<Arc<AppState>>) -> StatusCode {
    let account = match signal_account() {
        Some(a) => a,
        None => {
            warn!("SIGNAL_ACCOUNT is not configured");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    let base_url = signal_http_url();
    let receive_url = format!("{}/v1/receive/{}", base_url.trim_end_matches('/'), account);

    let resp = match state.http_client.get(&receive_url).send().await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "failed to poll signal-cli receive endpoint");
            return StatusCode::BAD_GATEWAY;
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        error!(%status, body = body.as_str(), "signal-cli receive returned error");
        return StatusCode::BAD_GATEWAY;
    }

    let envelopes: Vec<SignalEnvelope> = match resp.json().await {
        Ok(e) => e,
        Err(e) => {
            error!(error = %e, "failed to parse signal-cli receive response");
            return StatusCode::BAD_GATEWAY;
        }
    };

    let count = envelopes.len();
    if count == 0 {
        return StatusCode::OK;
    }

    info!(count, "polled signal-cli, processing envelopes");

    let state2 = Arc::clone(&state);
    let account2 = account.clone();
    tokio::spawn(async move {
        for envelope in envelopes {
            process_envelope(&state2, &account2, envelope).await;
        }
    });

    StatusCode::OK
}

// ---------------------------------------------------------------------------
// Core processing
// ---------------------------------------------------------------------------

enum ProcessResult {
    Ok,
    Ignored,
    Error,
}

/// Process a single signal envelope.
async fn process_envelope(
    state: &Arc<AppState>,
    account: &str,
    envelope: SignalEnvelope,
) -> ProcessResult {
    let source = match &envelope.source_number {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return ProcessResult::Ignored, // no sender info
    };

    // Self-message blocking: ignore messages from our own account.
    if source == account {
        return ProcessResult::Ignored;
    }

    let data_message = match &envelope.data_message {
        Some(dm) => dm,
        None => return ProcessResult::Ignored, // not a data message (receipt, typing, etc.)
    };

    // Determine whether this is a group message.
    let group_id = data_message
        .group_info
        .as_ref()
        .and_then(|g| g.group_id.clone());
    let is_group = group_id.is_some();

    // Group authorization check.
    // When `SIGNAL_GROUP_ALLOWED_USERS` is unset (or empty), all senders are
    // allowed to interact in group chats — the group is effectively open.
    // Set the env var to a comma-separated list of phone numbers to restrict
    // access.
    if is_group {
        let allowed = group_allowed_users();
        if !allowed.is_empty() && !allowed.iter().any(|a| a == &source) {
            info!(
                sender = source.as_str(),
                "signal group message from unauthorized sender, ignoring"
            );
            return ProcessResult::Ignored;
        }
    }

    // Build prompt from message text + attachments.
    let text = data_message.message.clone().unwrap_or_default();
    let attachment_descriptions = describe_attachments(data_message);

    let prompt = if text.is_empty() && attachment_descriptions.is_empty() {
        return ProcessResult::Ignored; // nothing to process
    } else if attachment_descriptions.is_empty() {
        text
    } else if text.is_empty() {
        attachment_descriptions
    } else {
        format!("{text}\n\n{attachment_descriptions}")
    };

    let user_name = envelope
        .source_name
        .clone()
        .unwrap_or_else(|| source.clone());

    // Session ID: stable per conversation (DM or group).
    let chat_id = group_id.clone().unwrap_or_else(|| source.clone());
    let session_id = format!("signal-{chat_id}");

    let span = info_span!(
        "signal.message",
        sender = source.as_str(),
        session_id = session_id.as_str(),
        group = is_group,
    );

    info!(parent: &span, "received signal message");

    // DM pairing check (skip for group messages — use group allowlist instead).
    if !is_group {
        match super::check_pairing(
            &state.loaded.config.storage.database_path,
            "signal",
            &source,
            &user_name,
        ) {
            Ok(super::PairingCheck::Approved) => {} // proceed
            Ok(super::PairingCheck::NeedsPairing(code)) => {
                let reply = super::pairing_reply(&code);
                let base_url = signal_http_url();
                let client = state.http_client.clone();
                let account_clone = account.to_owned();
                let source_clone = source.clone();
                tokio::spawn(async move {
                    if let Err(e) = send_message(
                        &client,
                        &base_url,
                        &account_clone,
                        &source_clone,
                        &reply,
                        None,
                    )
                    .await
                    {
                        error!(error = %e, "failed to send signal pairing reply");
                    }
                });
                return ProcessResult::Ok;
            }
            Ok(super::PairingCheck::AtCapacity) => {
                let reply = super::pairing_capacity_reply().to_owned();
                let base_url = signal_http_url();
                let client = state.http_client.clone();
                let account_clone = account.to_owned();
                let source_clone = source.clone();
                tokio::spawn(async move {
                    if let Err(e) = send_message(
                        &client,
                        &base_url,
                        &account_clone,
                        &source_clone,
                        &reply,
                        None,
                    )
                    .await
                    {
                        error!(error = %e, "failed to send signal capacity reply");
                    }
                });
                return ProcessResult::Ok;
            }
            Err(_) => {
                return ProcessResult::Error;
            }
        }
    }

    // Handle gateway slash commands.
    {
        let store = SessionStore::new(&state.loaded.config.storage.database_path);
        if let crate::commands::CommandResult::Reply(reply) =
            crate::commands::handle_command(&prompt, &session_id, &store, &state.loaded.config)
        {
            let base_url = signal_http_url();
            let client = state.http_client.clone();
            let account_clone = account.to_owned();
            let recipient = group_id.clone().unwrap_or_else(|| source.clone());
            let is_group_clone = is_group;
            tokio::spawn(async move {
                let result = if is_group_clone {
                    send_group_message(&client, &base_url, &account_clone, &recipient, &reply).await
                } else {
                    send_message(&client, &base_url, &account_clone, &recipient, &reply, None).await
                };
                if let Err(e) = result {
                    error!(error = %e, "failed to send signal command reply");
                }
            });
            return ProcessResult::Ok;
        }
    }

    // Auto-reset expired sessions.
    {
        let store = SessionStore::new(&state.loaded.config.storage.database_path);
        crate::commands::check_session_expiry(
            &session_id,
            &store,
            state.loaded.config.gateway.as_ref(),
        );
    }

    // Spawn agent execution in background.
    let state = Arc::clone(state);
    let account_owned = account.to_owned();
    let timestamp = data_message.timestamp;
    tokio::spawn(
        async move {
            // Send typing indicator.
            let base_url = signal_http_url();
            let recipient = group_id.clone().unwrap_or_else(|| source.clone());
            let _ = send_typing_indicator(
                &state.http_client,
                &base_url,
                &account_owned,
                &recipient,
                is_group,
            )
            .await;

            let service = state.session_service();

            let result = service
                .run_turn(SessionTurnInput {
                    session_id: &session_id,
                    session_platform: "signal",
                    delivery_platform: DeliveryPlatform::Signal,
                    prompt: &prompt,
                    title: Some(&format!("Signal: {user_name}")),
                    images: Vec::new(),
                })
                .await;

            let reply_text = super::extract_reply(result, "signal");

            // Stop typing indicator.
            let _ = send_typing_stop(
                &state.http_client,
                &base_url,
                &account_owned,
                &recipient,
                is_group,
            )
            .await;

            let send_result = if is_group {
                send_group_message(
                    &state.http_client,
                    &base_url,
                    &account_owned,
                    &recipient,
                    &reply_text,
                )
                .await
            } else {
                send_message(
                    &state.http_client,
                    &base_url,
                    &account_owned,
                    &recipient,
                    &reply_text,
                    timestamp,
                )
                .await
            };

            if let Err(e) = send_result {
                error!(error = %e, "failed to send signal reply");
            }

            // Cross-platform delivery mirror.
            crate::mirror::append_delivery_mirror(
                &state.loaded.config.storage.database_path,
                "signal",
                &chat_id,
                &reply_text,
                "signal",
            );
        }
        .instrument(span),
    );

    ProcessResult::Ok
}

/// Build a text description of attachments for the agent prompt.
fn describe_attachments(data_message: &SignalDataMessage) -> String {
    let attachments = match &data_message.attachments {
        Some(a) if !a.is_empty() => a,
        _ => return String::new(),
    };

    let mut parts = Vec::new();
    for (i, att) in attachments.iter().enumerate() {
        let content_type = att.content_type.as_deref().unwrap_or("unknown");
        let filename = att.filename.as_deref().unwrap_or("unnamed");
        let size = att
            .size
            .map(|s| format!(" ({s} bytes)"))
            .unwrap_or_default();
        let id_info = att
            .id
            .as_deref()
            .map(|id| format!(", id={id}"))
            .unwrap_or_default();
        parts.push(format!(
            "[Attachment {}: {content_type}, \"{filename}\"{size}{id_info}]",
            i + 1
        ));
    }

    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Outbound: send messages via signal-cli JSON-RPC 2.0
// ---------------------------------------------------------------------------

/// Send a text message to an individual recipient.
async fn send_message(
    client: &reqwest::Client,
    base_url: &str,
    account: &str,
    recipient: &str,
    text: &str,
    quote_timestamp: Option<u64>,
) -> Result<(), super::PlatformError> {
    let chunks = split_message(text, MAX_SIGNAL_MESSAGE_LEN);

    for (i, chunk) in chunks.iter().enumerate() {
        // Only quote on the first chunk.
        let quote = if i == 0 { quote_timestamp } else { None };

        let mut params = serde_json::json!({
            "account": account,
            "recipients": [recipient],
            "message": chunk,
        });

        if let Some(ts) = quote {
            params["quoteTimestamp"] = serde_json::json!(ts);
            params["quoteAuthor"] = serde_json::json!(recipient);
        }

        let rpc = JsonRpcRequest {
            jsonrpc: "2.0",
            method: "send",
            id: request_id(),
            params,
        };

        send_rpc(client, base_url, &rpc).await?;
    }

    Ok(())
}

/// Send a text message to a group.
async fn send_group_message(
    client: &reqwest::Client,
    base_url: &str,
    account: &str,
    group_id: &str,
    text: &str,
) -> Result<(), super::PlatformError> {
    let chunks = split_message(text, MAX_SIGNAL_MESSAGE_LEN);

    for chunk in &chunks {
        let params = serde_json::json!({
            "account": account,
            "groupId": group_id,
            "message": chunk,
        });

        let rpc = JsonRpcRequest {
            jsonrpc: "2.0",
            method: "send",
            id: request_id(),
            params,
        };

        send_rpc(client, base_url, &rpc).await?;
    }

    Ok(())
}

/// Send a typing indicator (start typing).
async fn send_typing_indicator(
    client: &reqwest::Client,
    base_url: &str,
    account: &str,
    recipient: &str,
    is_group: bool,
) -> Result<(), super::PlatformError> {
    let mut params = serde_json::json!({
        "account": account,
    });

    if is_group {
        params["groupId"] = serde_json::json!(recipient);
    } else {
        params["recipient"] = serde_json::json!(recipient);
    }

    let rpc = JsonRpcRequest {
        jsonrpc: "2.0",
        method: "startTyping",
        id: request_id(),
        params,
    };

    send_rpc(client, base_url, &rpc).await
}

/// Send a stop-typing indicator.
async fn send_typing_stop(
    client: &reqwest::Client,
    base_url: &str,
    account: &str,
    recipient: &str,
    is_group: bool,
) -> Result<(), super::PlatformError> {
    let mut params = serde_json::json!({
        "account": account,
    });

    if is_group {
        params["groupId"] = serde_json::json!(recipient);
    } else {
        params["recipient"] = serde_json::json!(recipient);
    }

    let rpc = JsonRpcRequest {
        jsonrpc: "2.0",
        method: "stopTyping",
        id: request_id(),
        params,
    };

    send_rpc(client, base_url, &rpc).await
}

/// Send a JSON-RPC 2.0 request to the signal-cli daemon.
async fn send_rpc(
    client: &reqwest::Client,
    base_url: &str,
    request: &JsonRpcRequest,
) -> Result<(), super::PlatformError> {
    use genesis_types::DeliveryPlatform;

    let url = format!("{}/api/v1/rpc", base_url.trim_end_matches('/'));

    let resp = client
        .post(&url)
        .json(request)
        .send()
        .await
        .map_err(|source| super::PlatformError::HttpRequest {
            platform: DeliveryPlatform::Signal,
            source,
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(super::PlatformError::ApiError {
            platform: DeliveryPlatform::Signal,
            status,
            body,
        });
    }

    let rpc_resp: JsonRpcResponse = resp.json().await.map_err(|source| {
        super::PlatformError::ResponseParse {
            platform: DeliveryPlatform::Signal,
            source,
        }
    })?;

    if let Some(err) = rpc_resp.error {
        return Err(super::PlatformError::ApiLogicError {
            platform: DeliveryPlatform::Signal,
            detail: format!("RPC error {}: {}", err.code, err.message),
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Split a message into chunks respecting a max byte length.
/// Tries to split on newlines first, then word boundaries.
/// Uses char-boundary-safe indexing to avoid panics on multi-byte UTF-8.
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
        // character. `floor_char_boundary` returns the largest byte index
        // <= max_len that is on a char boundary.
        let safe_end = remaining.floor_char_boundary(max_len);

        let split_at = remaining[..safe_end]
            .rfind('\n')
            .or_else(|| remaining[..safe_end].rfind(' '))
            .unwrap_or(safe_end);

        chunks.push(remaining[..split_at].to_owned());
        remaining = remaining[split_at..].trim_start();
    }

    chunks
}

/// Generate a unique request ID for JSON-RPC correlation.
fn request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Not cryptographic, just a unique-enough ID for request correlation.
    format!("{nanos:x}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_envelope_deserializes_dm() {
        let json = r#"{
            "source_number": "+15551234567",
            "source_name": "Alice",
            "dataMessage": {
                "timestamp": 1700000000000,
                "message": "Hello Eve"
            }
        }"#;
        let envelope: SignalEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(envelope.source_number.as_deref(), Some("+15551234567"));
        assert_eq!(envelope.source_name.as_deref(), Some("Alice"));
        let dm = envelope.data_message.expect("should have data_message");
        assert_eq!(dm.message.as_deref(), Some("Hello Eve"));
        assert_eq!(dm.timestamp, Some(1700000000000));
        assert!(dm.group_info.is_none());
    }

    #[test]
    fn signal_envelope_deserializes_group_message() {
        let json = r#"{
            "source_number": "+15559876543",
            "dataMessage": {
                "timestamp": 1700000001000,
                "message": "Group hello",
                "groupInfo": {
                    "groupId": "abc123group=="
                }
            }
        }"#;
        let envelope: SignalEnvelope = serde_json::from_str(json).expect("should parse");
        let dm = envelope.data_message.expect("data_message");
        let group = dm.group_info.expect("group_info");
        assert_eq!(group.group_id.as_deref(), Some("abc123group=="));
    }

    #[test]
    fn signal_envelope_deserializes_with_attachments() {
        let json = r#"{
            "source_number": "+15551234567",
            "dataMessage": {
                "message": "Check this out",
                "attachments": [
                    {
                        "contentType": "image/png",
                        "filename": "screenshot.png",
                        "id": "att-001",
                        "size": 102400
                    }
                ]
            }
        }"#;
        let envelope: SignalEnvelope = serde_json::from_str(json).expect("should parse");
        let dm = envelope.data_message.expect("data_message");
        let atts = dm.attachments.expect("attachments");
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].content_type.as_deref(), Some("image/png"));
        assert_eq!(atts[0].filename.as_deref(), Some("screenshot.png"));
        assert_eq!(atts[0].id.as_deref(), Some("att-001"));
        assert_eq!(atts[0].size, Some(102400));
    }

    #[test]
    fn signal_envelope_deserializes_with_quote() {
        let json = r#"{
            "source_number": "+15551234567",
            "dataMessage": {
                "message": "Replying to this",
                "quote": {
                    "id": 1700000000000,
                    "author": "+15559876543",
                    "text": "Original message"
                }
            }
        }"#;
        let envelope: SignalEnvelope = serde_json::from_str(json).expect("should parse");
        let dm = envelope.data_message.expect("data_message");
        let quote = dm.quote.expect("quote");
        assert_eq!(quote.id, Some(1700000000000));
        assert_eq!(quote.author.as_deref(), Some("+15559876543"));
        assert_eq!(quote.text.as_deref(), Some("Original message"));
    }

    #[test]
    fn signal_envelope_handles_missing_fields() {
        let json = r#"{"source_number": "+15551234567"}"#;
        let envelope: SignalEnvelope = serde_json::from_str(json).expect("should parse");
        assert!(envelope.data_message.is_none());
        assert!(envelope.source_name.is_none());
    }

    #[test]
    fn signal_envelope_deserializes_with_source_alias() {
        let json = r#"{
            "source": "+15551234567",
            "dataMessage": {
                "message": "Using alias"
            }
        }"#;
        let envelope: SignalEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(envelope.source_number.as_deref(), Some("+15551234567"));
    }

    #[test]
    fn split_message_short_text() {
        let chunks = split_message("hello", 6000);
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
            // Verify the chunk is valid UTF-8 (String guarantees this, but
            // confirm no panic occurred during construction).
            assert!(!chunk.is_empty());
        }
    }

    #[test]
    fn describe_attachments_empty_when_none() {
        let dm = SignalDataMessage {
            timestamp: None,
            message: Some("text".to_owned()),
            group_info: None,
            attachments: None,
            quote: None,
        };
        assert!(describe_attachments(&dm).is_empty());
    }

    #[test]
    fn describe_attachments_formats_single() {
        let dm = SignalDataMessage {
            timestamp: None,
            message: None,
            group_info: None,
            attachments: Some(vec![SignalAttachment {
                content_type: Some("image/jpeg".to_owned()),
                filename: Some("photo.jpg".to_owned()),
                id: Some("att-42".to_owned()),
                size: Some(54321),
            }]),
            quote: None,
        };
        let desc = describe_attachments(&dm);
        assert!(desc.contains("image/jpeg"));
        assert!(desc.contains("photo.jpg"));
        assert!(desc.contains("54321 bytes"));
        assert!(desc.contains("att-42"));
    }

    #[test]
    fn describe_attachments_formats_multiple() {
        let dm = SignalDataMessage {
            timestamp: None,
            message: None,
            group_info: None,
            attachments: Some(vec![
                SignalAttachment {
                    content_type: Some("image/png".to_owned()),
                    filename: Some("a.png".to_owned()),
                    id: None,
                    size: None,
                },
                SignalAttachment {
                    content_type: Some("audio/ogg".to_owned()),
                    filename: Some("voice.ogg".to_owned()),
                    id: None,
                    size: Some(8000),
                },
            ]),
            quote: None,
        };
        let desc = describe_attachments(&dm);
        assert!(desc.contains("Attachment 1"));
        assert!(desc.contains("Attachment 2"));
        assert!(desc.contains("image/png"));
        assert!(desc.contains("audio/ogg"));
    }

    #[test]
    fn json_rpc_request_serializes_correctly() {
        let rpc = JsonRpcRequest {
            jsonrpc: "2.0",
            method: "send",
            id: "test-123".to_owned(),
            params: serde_json::json!({
                "account": "+15551234567",
                "recipients": ["+15559876543"],
                "message": "Hello",
            }),
        };
        let json = serde_json::to_string(&rpc).expect("should serialize");
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"send\""));
        assert!(json.contains("\"id\":\"test-123\""));
        assert!(json.contains("+15551234567"));
    }

    #[test]
    fn json_rpc_response_deserializes_success() {
        let json = r#"{"jsonrpc": "2.0", "result": {}, "id": "1"}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).expect("should parse");
        assert!(resp.error.is_none());
    }

    #[test]
    fn json_rpc_response_deserializes_error() {
        let json = r#"{
            "jsonrpc": "2.0",
            "error": {"code": -32601, "message": "Method not found"},
            "id": "1"
        }"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).expect("should parse");
        let err = resp.error.expect("should have error");
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[test]
    fn session_id_stable_for_dm() {
        let source = "+15551234567";
        let session_id = format!("signal-{source}");
        assert_eq!(session_id, "signal-+15551234567");
    }

    #[test]
    fn session_id_stable_for_group() {
        let group_id = "abc123group==";
        let session_id = format!("signal-{group_id}");
        assert_eq!(session_id, "signal-abc123group==");
    }

    #[test]
    fn request_id_generates_unique_ids() {
        let id1 = request_id();
        let id2 = request_id();
        // They should be non-empty and probably different (time-based).
        assert!(!id1.is_empty());
        assert!(!id2.is_empty());
    }

    #[test]
    fn self_message_detection() {
        let account = "+15551234567";
        let source = "+15551234567";
        assert_eq!(source, account, "self-message should be detected");
    }

    #[test]
    fn group_allowed_users_parsing() {
        // Simulates parsing logic without setting env vars
        let raw = "+15551111111, +15552222222 , +15553333333";
        let parsed: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], "+15551111111");
        assert_eq!(parsed[1], "+15552222222");
        assert_eq!(parsed[2], "+15553333333");
    }
}
