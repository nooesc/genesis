pub mod discord;
pub mod homeassistant;
pub mod slack;
pub mod telegram;
pub mod whatsapp;

use std::path::Path;
use genesis_storage::PairingStore;

/// Result of a pairing check for an incoming platform message.
pub enum PairingCheck {
    /// User is approved — proceed normally.
    Approved,
    /// User is not approved — a pairing code was generated and should be shown.
    NeedsPairing(String),
    /// Pairing is at capacity (too many pending codes) — reject with message.
    AtCapacity,
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
) -> PairingCheck {
    let store = PairingStore::new(database_path);

    // If already approved, fast-path
    match store.is_approved(platform, user_id) {
        Ok(true) => return PairingCheck::Approved,
        Err(e) => {
            tracing::error!(error = %e, "pairing check failed, allowing through");
            return PairingCheck::Approved; // fail-open to avoid locking out users on DB errors
        }
        Ok(false) => {}
    }

    // Not approved — generate a code
    match store.generate_code(platform, user_id, user_name) {
        Ok(Some(code)) => PairingCheck::NeedsPairing(code),
        Ok(None) => PairingCheck::AtCapacity,
        Err(e) => {
            tracing::error!(error = %e, "pairing code generation failed, allowing through");
            PairingCheck::Approved // fail-open
        }
    }
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
