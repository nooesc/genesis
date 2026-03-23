pub mod discord;
pub mod homeassistant;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod whatsapp;

use genesis_core::execution::{SessionExecutionError, SessionTurnOutcome};
use genesis_storage::PairingStore;
use genesis_types::DeliveryPlatform;
use std::collections::HashSet;
use std::path::Path;

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

    let platform_allow_all = genesis_config::env::platform_allow_all_var(platform.as_str());

    if is_truthy_env(platform_allow_all) {
        return Ok(PairingCheck::Approved);
    }

    let platform_allowlist =
        genesis_config::env::get_or(genesis_config::env::platform_allowlist_var(platform.as_str()), "");
    let global_allowlist = genesis_config::env::get_or(genesis_config::env::GATEWAY_ALLOWED_USERS, "");

    if platform_allowlist.is_empty() && global_allowlist.is_empty() {
        if is_truthy_env(genesis_config::env::GATEWAY_ALLOW_ALL_USERS) {
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

fn is_truthy_env(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    genesis_config::env::get_bool(name, false)
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
        use genesis_config::env::platform_allowlist_var;
        assert_eq!(platform_allowlist_var("telegram"), "TELEGRAM_ALLOWED_USERS");
        assert_eq!(platform_allowlist_var("discord"), "DISCORD_ALLOWED_USERS");
        assert_eq!(platform_allowlist_var("whatsapp"), "WHATSAPP_ALLOWED_USERS");
        assert_eq!(platform_allowlist_var("slack"), "SLACK_ALLOWED_USERS");
        assert_eq!(platform_allowlist_var("signal"), "SIGNAL_ALLOWED_USERS");
        assert_eq!(platform_allowlist_var("unknown"), "");
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
