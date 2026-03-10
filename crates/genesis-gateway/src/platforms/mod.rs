pub mod discord;
pub mod homeassistant;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod whatsapp;

use std::collections::HashSet;
use std::path::Path;
use genesis_storage::PairingStore;

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
#[derive(Debug)]
pub enum PairingCheckError {
    /// The pairing store is unavailable or returned an unexpected error.
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

    let platform_allowlist = std::env::var(platform_allowlist_env(platform.as_str()))
        .unwrap_or_default();
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
    let mut candidates = HashSet::new();
    candidates.insert(user_id.to_owned());
    if let Some((short_user_id, _)) = user_id.split_once('@') {
        if !short_user_id.is_empty() {
            candidates.insert(short_user_id.to_owned());
        }
    }

    !candidates.is_disjoint(allowed)
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
