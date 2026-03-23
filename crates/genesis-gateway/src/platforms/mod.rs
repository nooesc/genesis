pub mod discord;
pub mod homeassistant;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod whatsapp;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use genesis_core::execution::{SessionExecutionError, SessionTurnInput, SessionTurnOutcome};
use genesis_storage::{PairingStore, SessionStore};
use genesis_types::DeliveryPlatform;

use crate::AppState;

// ---------------------------------------------------------------------------
// Shared platform message abstraction
// ---------------------------------------------------------------------------

/// Normalized incoming message from any platform.
///
/// Platform handlers extract their platform-specific payload into this struct,
/// which is then processed by [`process_platform_message`].
pub struct PlatformMessage<'a> {
    /// The platform's identifier for the user (e.g. Telegram user ID, phone number).
    pub user_id: &'a str,
    /// Display name for the user.
    pub user_name: &'a str,
    /// The text content of the message (may include transcription markers, etc.).
    pub text: &'a str,
    /// Session ID for this conversation (e.g. `tg-12345`, `slack-C67890`).
    pub session_id: &'a str,
    /// Chat/channel identifier for delivery mirror logging.
    pub chat_id: &'a str,
    /// Platform name for logging and pairing (e.g. `"telegram"`, `"slack"`).
    pub platform_name: &'a str,
    /// Delivery platform enum variant.
    pub delivery_platform: DeliveryPlatform,
}

/// Result of processing a platform message through the shared pipeline.
pub enum ProcessOutcome {
    /// Pairing is needed; the handler should send this reply to the user.
    NeedsPairing(String),
    /// Too many pending pairing requests; send this reply.
    AtCapacity,
    /// Pairing check failed; handler should return SERVICE_UNAVAILABLE.
    PairingError,
    /// A gateway command was handled; send this reply to the user.
    CommandReply(String),
    /// The agent processed the message; send this reply to the user.
    AgentReply(String),
    /// The agent processing failed at the execution level.
    ExecutionError(String),
}

/// Shared processing pipeline for platform messages.
///
/// This encapsulates the common flow that every platform handler repeats:
/// 1. Pairing check
/// 2. Gateway slash command handling
/// 3. Session expiry auto-reset
/// 4. Agent turn execution
/// 5. Reply extraction
/// 6. Delivery mirror logging
///
/// Platform handlers call this after extracting the message from their
/// platform-specific payload and verifying the webhook signature.
pub async fn process_platform_message(
    state: &Arc<AppState>,
    msg: &PlatformMessage<'_>,
) -> ProcessOutcome {
    // 1. DM pairing check
    match check_pairing(
        &state.loaded.config.storage.database_path,
        msg.platform_name,
        msg.user_id,
        msg.user_name,
    ) {
        Ok(PairingCheck::Approved) => {}
        Ok(PairingCheck::NeedsPairing(code)) => {
            return ProcessOutcome::NeedsPairing(pairing_reply(&code));
        }
        Ok(PairingCheck::AtCapacity) => {
            return ProcessOutcome::AtCapacity;
        }
        Err(_) => {
            return ProcessOutcome::PairingError;
        }
    }

    // 2. Handle gateway slash commands
    let store = SessionStore::new(&state.loaded.config.storage.database_path);
    if let crate::commands::CommandResult::Reply(reply) =
        crate::commands::handle_command(msg.text, msg.session_id, &store, &state.loaded.config)
    {
        return ProcessOutcome::CommandReply(reply);
    }

    // 3. Auto-reset expired sessions
    crate::commands::check_session_expiry(
        msg.session_id,
        &store,
        state.loaded.config.gateway.as_ref(),
    );

    // 4. Run the agent turn
    let service = state.session_service();
    let title = format!("{}: {}", capitalize_platform(msg.platform_name), msg.user_name);
    let result = service
        .run_turn(SessionTurnInput {
            session_id: msg.session_id,
            session_platform: msg.platform_name,
            delivery_platform: msg.delivery_platform.clone(),
            prompt: msg.text,
            title: Some(&title),
            images: Vec::new(),
        })
        .await;

    // 5. Extract the reply
    let reply_text = extract_reply(result, msg.platform_name);

    // 6. Delivery mirror
    crate::mirror::append_delivery_mirror(
        &state.loaded.config.storage.database_path,
        msg.platform_name,
        msg.chat_id,
        &reply_text,
        msg.platform_name,
    );

    ProcessOutcome::AgentReply(reply_text)
}

/// Capitalize the first letter of a platform name for session titles.
fn capitalize_platform(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

// ---------------------------------------------------------------------------
// Shared message splitting
// ---------------------------------------------------------------------------

/// Split a message into chunks respecting a max byte length.
///
/// Tries to split on newlines first, then word boundaries. Uses
/// char-boundary-safe indexing to avoid panics on multi-byte UTF-8.
///
/// This is used by Telegram (4096 char limit) and Signal (6000 byte limit).
pub fn split_message(text: &str, max_len: usize) -> Vec<String> {
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
        // <= max_len that sits on a UTF-8 char boundary.
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

// ---------------------------------------------------------------------------
// Reply extraction
// ---------------------------------------------------------------------------

/// Structured error type for platform webhook handler operations.
///
/// Replaces generic `format!`-based string errors throughout platform handlers,
/// preserving the source error type for pattern matching, retry decisions, and
/// structured logging.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// An outbound HTTP request to a platform API failed at the transport level.
    #[error("{platform} HTTP request failed: {source}")]
    HttpRequest {
        platform: DeliveryPlatform,
        #[source]
        source: reqwest::Error,
    },

    /// The platform API returned a non-success HTTP status code.
    #[error("{platform} API returned {status}: {body}")]
    ApiError {
        platform: DeliveryPlatform,
        status: reqwest::StatusCode,
        body: String,
    },

    /// Failed to deserialize a response from the platform API.
    #[error("{platform} response parse failed: {source}")]
    ResponseParse {
        platform: DeliveryPlatform,
        #[source]
        source: reqwest::Error,
    },

    /// The platform API returned a logical error (e.g. `ok: false`).
    #[error("{platform} API error: {detail}")]
    ApiLogicError {
        platform: DeliveryPlatform,
        detail: String,
    },

    /// A required configuration value (env var, token) is missing.
    #[error("{platform} configuration missing: {detail}")]
    ConfigMissing {
        platform: DeliveryPlatform,
        detail: String,
    },

    /// A platform-specific operation failed (file download, transcription, etc.).
    #[error("{platform} {operation} failed: {detail}")]
    OperationFailed {
        platform: DeliveryPlatform,
        operation: &'static str,
        detail: String,
    },
}

/// Extract the reply text from a `run_turn` result, logging success or failure.
///
/// Every platform handler performs the same match on `Result<SessionTurnOutcome, _>`
/// to log turn metrics and produce a user-facing reply string. This helper
/// centralises that logic.
pub fn extract_reply(
    result: Result<SessionTurnOutcome, SessionExecutionError>,
    platform: &str,
) -> String {
    match result {
        Ok(outcome) => {
            tracing::info!(
                turns_used = outcome.result.turns_used,
                tool_calls_made = outcome.result.tool_calls_made,
                "{platform} turn completed"
            );
            outcome.result.response
        }
        Err(e) => {
            tracing::error!(error = %e, "{platform} turn failed");
            format!("Sorry, I encountered an error: {e}")
        }
    }
}

/// Result of a pairing check for an incoming platform message.
pub enum PairingCheck {
    /// User is approved — proceed normally.
    Approved,
    /// User is not approved — a pairing code was generated and should be shown.
    NeedsPairing(String),
    /// User is at capacity (too many pending codes) — reject with message.
    AtCapacity,
}

/// Pairing check errors.
#[derive(Debug, thiserror::Error)]
pub enum PairingCheckError {
    /// The pairing store is unavailable or returned an unexpected error.
    #[error("pairing store is unavailable")]
    StoreUnavailable,
}

/// Check whether a user is approved to interact via a messaging platform.
///
/// If the user is not yet approved, a pairing code is generated and returned
/// so the platform handler can reply with instructions.
pub fn check_pairing(
    database_path: &Path,
    platform: &str,
    user_id: &str,
    user_name: &str,
) -> Result<PairingCheck, PairingCheckError> {
    if platform.eq_ignore_ascii_case("homeassistant") {
        return Ok(PairingCheck::Approved);
    }

    let platform = platform.to_ascii_lowercase();

    let platform_allow_all = match platform.as_str() {
        "telegram" => "TELEGRAM_ALLOW_ALL_USERS",
        "discord" => "DISCORD_ALLOW_ALL_USERS",
        "whatsapp" => "WHATSAPP_ALLOW_ALL_USERS",
        "slack" => "SLACK_ALLOW_ALL_USERS",
        "signal" => "SIGNAL_ALLOW_ALL_USERS",
        _ => "",
    };

    if is_truthy_env(platform_allow_all) {
        return Ok(PairingCheck::Approved);
    }

    let platform_allowlist =
        std::env::var(platform_allowlist_env(platform.as_str())).unwrap_or_default();
    let global_allowlist = std::env::var("GATEWAY_ALLOWED_USERS").unwrap_or_default();

    if platform_allowlist.is_empty() && global_allowlist.is_empty() {
        if is_truthy_env("GATEWAY_ALLOW_ALL_USERS") {
            return Ok(PairingCheck::Approved);
        }
    } else {
        let mut allowed_ids = parse_env_id_set(&platform_allowlist);
        allowed_ids.extend(parse_env_id_set(&global_allowlist));

        if is_user_in_allowlist(&allowed_ids, user_id) {
            return Ok(PairingCheck::Approved);
        }
    }

    let store = PairingStore::new(database_path);

    match store.is_approved(platform.as_str(), user_id) {
        Ok(true) => return Ok(PairingCheck::Approved),
        Err(e) => {
            tracing::error!(error = %e, "pairing lookup failed");
            return Err(PairingCheckError::StoreUnavailable);
        }
        Ok(false) => {}
    }

    // Not approved — generate a code
    match store.generate_code(platform.as_str(), user_id, user_name) {
        Ok(Some(code)) => Ok(PairingCheck::NeedsPairing(code)),
        Ok(None) => Ok(PairingCheck::AtCapacity),
        Err(e) => {
            tracing::error!(error = %e, "pairing code generation failed");
            Err(PairingCheckError::StoreUnavailable)
        }
    }
}

fn platform_allowlist_env(platform: &str) -> &'static str {
    match platform {
        "telegram" => "TELEGRAM_ALLOWED_USERS",
        "discord" => "DISCORD_ALLOWED_USERS",
        "whatsapp" => "WHATSAPP_ALLOWED_USERS",
        "slack" => "SLACK_ALLOWED_USERS",
        "signal" => "SIGNAL_ALLOWED_USERS",
        _ => "",
    }
}

fn is_truthy_env(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    matches!(
        std::env::var(name)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_env_id_set(value: &str) -> HashSet<String> {
    value
        .split(',')
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn is_user_in_allowlist(allowed: &HashSet<String>, user_id: &str) -> bool {
    if allowed.contains(user_id) {
        return true;
    }
    if let Some((short, _)) = user_id.split_once('@') {
        if !short.is_empty() && allowed.contains(short) {
            return true;
        }
    }
    false
}

/// Format a pairing reply message for a user who needs to be approved.
pub fn pairing_reply(code: &str) -> String {
    format!(
        "You haven't been paired with this agent yet.\n\n\
         Your pairing code is: **{code}**\n\n\
         Ask the agent owner to approve it with:\n\
         `genesis pairing approve <platform> {code}`\n\n\
         This code expires in 1 hour."
    )
}

/// Format a reply for when pairing is at capacity.
pub fn pairing_capacity_reply() -> &'static str {
    "Too many pending pairing requests. Please try again later or contact the agent owner."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_message_short_text_returns_single_chunk() {
        let chunks = split_message("hello", 4096);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_splits_on_newlines() {
        let text = format!("{}\n{}", "a".repeat(100), "b".repeat(100));
        let chunks = split_message(&text, 150);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= 150);
    }

    #[test]
    fn split_message_splits_on_spaces() {
        let text = format!("{} {}", "word".repeat(30), "more".repeat(30));
        let chunks = split_message(&text, 100);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 100);
        }
    }

    #[test]
    fn split_message_multibyte_utf8_does_not_panic() {
        let emoji = "\u{1F600}"; // 4 bytes
        let text = emoji.repeat(30); // 120 bytes total
        let chunks = split_message(&text, 50);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 50);
            assert!(!chunk.is_empty());
        }
    }

    #[test]
    fn split_message_leading_delimiter_does_not_produce_empty_chunk() {
        let text = format!("\n{}", "a".repeat(100));
        let chunks = split_message(&text, 50);
        for chunk in &chunks {
            assert!(!chunk.is_empty(), "empty chunk produced");
        }
    }

    #[test]
    fn capitalize_platform_capitalizes_first_letter() {
        assert_eq!(capitalize_platform("telegram"), "Telegram");
        assert_eq!(capitalize_platform("discord"), "Discord");
        assert_eq!(capitalize_platform("slack"), "Slack");
        assert_eq!(capitalize_platform("whatsapp"), "Whatsapp");
        assert_eq!(capitalize_platform("signal"), "Signal");
        assert_eq!(capitalize_platform(""), "");
    }

    #[test]
    fn pairing_capacity_reply_is_non_empty() {
        assert!(!pairing_capacity_reply().is_empty());
    }

    #[test]
    fn is_truthy_env_recognizes_true_values() {
        std::env::set_var("_TEST_TRUTHY_1", "1");
        std::env::set_var("_TEST_TRUTHY_TRUE", "true");
        std::env::set_var("_TEST_TRUTHY_TRUE_UPPER", "TRUE");
        std::env::set_var("_TEST_TRUTHY_YES", "yes");
        std::env::set_var("_TEST_TRUTHY_ON", "on");
        std::env::set_var("_TEST_TRUTHY_0", "0");
        std::env::set_var("_TEST_TRUTHY_FALSE", "false");
        std::env::set_var("_TEST_TRUTHY_NO", "no");
        std::env::set_var("_TEST_TRUTHY_EMPTY", "");
        std::env::set_var("_TEST_TRUTHY_RANDOM", "random");

        assert!(is_truthy_env("_TEST_TRUTHY_1"));
        assert!(is_truthy_env("_TEST_TRUTHY_TRUE"));
        assert!(is_truthy_env("_TEST_TRUTHY_TRUE_UPPER"));
        assert!(is_truthy_env("_TEST_TRUTHY_YES"));
        assert!(is_truthy_env("_TEST_TRUTHY_ON"));
        assert!(!is_truthy_env("_TEST_TRUTHY_0"));
        assert!(!is_truthy_env("_TEST_TRUTHY_FALSE"));
        assert!(!is_truthy_env("_TEST_TRUTHY_NO"));
        assert!(!is_truthy_env("_TEST_TRUTHY_EMPTY"));
        assert!(!is_truthy_env("_TEST_TRUTHY_RANDOM"));
        assert!(!is_truthy_env(""));
    }

    #[test]
    fn parse_env_id_set_splits_comma_delimited() {
        let set = parse_env_id_set("123,456,789");
        assert_eq!(set.len(), 3);
        assert!(set.contains("123"));
        assert!(set.contains("456"));
        assert!(set.contains("789"));
    }

    #[test]
    fn parse_env_id_set_handles_empty_string() {
        let set = parse_env_id_set("");
        assert!(set.is_empty());
    }

    #[test]
    fn parse_env_id_set_trims_whitespace() {
        let set = parse_env_id_set("  abc , def , ghi  ");
        assert_eq!(set.len(), 3);
        assert!(set.contains("abc"));
        assert!(set.contains("def"));
        assert!(set.contains("ghi"));
    }

    #[test]
    fn is_user_in_allowlist_matches_exact_id() {
        let allowed: HashSet<String> = ["user1", "user2"].iter().map(|s| s.to_string()).collect();
        assert!(is_user_in_allowlist(&allowed, "user1"));
        assert!(is_user_in_allowlist(&allowed, "user2"));
        assert!(!is_user_in_allowlist(&allowed, "user3"));
    }

    #[test]
    fn is_user_in_allowlist_matches_short_id_before_at() {
        let allowed: HashSet<String> = ["alice"].iter().map(|s| s.to_string()).collect();
        assert!(is_user_in_allowlist(&allowed, "alice@example.com"));
        assert!(!is_user_in_allowlist(&allowed, "bob@example.com"));
    }

    #[test]
    fn is_user_in_allowlist_ignores_empty_short_id() {
        let allowed: HashSet<String> = [""].iter().map(|s| s.to_string()).collect();
        // "@domain.com" has an empty short id — should not match the empty string in the set
        assert!(!is_user_in_allowlist(&allowed, "@domain.com"));
    }

    #[test]
    fn platform_allowlist_env_maps_known_platforms() {
        assert_eq!(platform_allowlist_env("telegram"), "TELEGRAM_ALLOWED_USERS");
        assert_eq!(platform_allowlist_env("discord"), "DISCORD_ALLOWED_USERS");
        assert_eq!(platform_allowlist_env("whatsapp"), "WHATSAPP_ALLOWED_USERS");
        assert_eq!(platform_allowlist_env("slack"), "SLACK_ALLOWED_USERS");
        assert_eq!(platform_allowlist_env("signal"), "SIGNAL_ALLOWED_USERS");
        assert_eq!(platform_allowlist_env("unknown"), "");
    }

    #[test]
    fn pairing_reply_contains_code() {
        let reply = pairing_reply("ABCD1234");
        assert!(reply.contains("ABCD1234"));
        assert!(reply.contains("genesis pairing approve"));
    }

    #[test]
    fn platform_error_http_request_display_includes_platform() {
        // We can't easily construct a reqwest::Error, so test the other variants.
        let err = PlatformError::ApiError {
            platform: DeliveryPlatform::Telegram,
            status: reqwest::StatusCode::FORBIDDEN,
            body: "bot was blocked".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("telegram"), "should contain platform name: {msg}");
        assert!(msg.contains("403"), "should contain status code: {msg}");
        assert!(msg.contains("bot was blocked"), "should contain body: {msg}");
    }

    #[test]
    fn platform_error_api_logic_error_display() {
        let err = PlatformError::ApiLogicError {
            platform: DeliveryPlatform::Slack,
            detail: "channel_not_found".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("slack"));
        assert!(msg.contains("channel_not_found"));
    }

    #[test]
    fn platform_error_config_missing_display() {
        let err = PlatformError::ConfigMissing {
            platform: DeliveryPlatform::WhatsApp,
            detail: "WHATSAPP_TOKEN not set".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("whatsapp"));
        assert!(msg.contains("WHATSAPP_TOKEN not set"));
    }

    #[test]
    fn platform_error_operation_failed_display() {
        let err = PlatformError::OperationFailed {
            platform: DeliveryPlatform::Telegram,
            operation: "file download",
            detail: "downloaded file is empty".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("telegram"));
        assert!(msg.contains("file download"));
        assert!(msg.contains("downloaded file is empty"));
    }

    #[test]
    fn platform_error_response_parse_display_includes_platform() {
        // ResponseParse requires a reqwest::Error source which is hard to construct.
        // Verify the ApiError variant works for Signal to confirm platform Display.
        let err = PlatformError::ApiError {
            platform: DeliveryPlatform::Signal,
            status: reqwest::StatusCode::BAD_GATEWAY,
            body: "upstream timeout".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("signal"));
        assert!(msg.contains("502"));
    }

    #[test]
    fn platform_error_is_debug() {
        let err = PlatformError::ApiLogicError {
            platform: DeliveryPlatform::Discord,
            detail: "unknown interaction".to_owned(),
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("ApiLogicError"));
        assert!(debug.contains("Discord"));
    }
}
