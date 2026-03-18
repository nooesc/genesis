//! Session mirroring for cross-platform visibility.
//!
//! When a message is dispatched to a platform (Telegram, Slack, Discord, etc.),
//! a "delivery-mirror" record is appended to the target session's transcript.
//! This gives the agent visibility into what was sent across platforms.
//!
//! ## Design
//!
//! - **Defensive**: mirroring never raises fatal errors. All failures are logged
//!   and swallowed so the primary dispatch path is never interrupted.
//! - **Session resolution**: matches `platform + chat_id` to find the right
//!   session, using the same ID conventions as the platform webhook handlers
//!   (e.g. `tg-{chat_id}`, `slack-{channel}`).
//! - **Mirror records**: stored as assistant messages with `mirror = true` and
//!   a `mirror_source` label indicating the origin (e.g. "cli", "api", "schedule").

use std::path::Path;

use genesis_storage::SessionStore;
use tracing::{debug, warn};

/// Append a delivery-mirror record directly to an existing session by its ID.
///
/// Use this when the session ID is already known (e.g. API handlers).
/// Unlike [`append_delivery_mirror`], no ID transformation is applied.
/// This function is intentionally infallible — errors are logged but never
/// propagated.
pub fn append_delivery_mirror_to_session(
    database_path: &Path,
    session_id: &str,
    content: &str,
    source: &str,
) {
    let store = SessionStore::new(database_path);

    match store.append_mirror_message(session_id, content, source) {
        Ok(message_id) => {
            debug!(
                session_id,
                message_id, source, "delivery mirror appended (direct)"
            );
        }
        Err(e) => {
            warn!(
                session_id,
                error = %e,
                "mirror: failed to append mirror message"
            );
        }
    }
}

/// Append a delivery-mirror record to the session for `platform`/`chat_id`.
///
/// Resolves the session ID from platform + chat_id using the same conventions
/// as the platform webhook handlers. If the session doesn't exist yet, it is
/// created automatically. This function is intentionally infallible — errors
/// are logged but never propagated.
///
/// # Arguments
///
/// * `database_path` - Path to the SQLite database.
/// * `platform` - Platform identifier (e.g. "telegram", "slack", "discord").
/// * `chat_id` - Platform-specific chat/channel identifier (NOT a session ID).
/// * `content` - The message text that was dispatched.
/// * `source` - Label for the origin of the dispatch (e.g. "cli", "api", "schedule").
pub fn append_delivery_mirror(
    database_path: &Path,
    platform: &str,
    chat_id: &str,
    content: &str,
    source: &str,
) {
    let store = SessionStore::new(database_path);

    let session_id = resolve_session_id(platform, chat_id);

    // Ensure the session exists (create if needed).
    if let Err(e) = ensure_session_exists(&store, &session_id, platform) {
        warn!(
            platform,
            chat_id,
            error = %e,
            "mirror: failed to ensure session exists, skipping"
        );
        return;
    }

    match store.append_mirror_message(&session_id, content, source) {
        Ok(message_id) => {
            debug!(
                platform,
                chat_id,
                session_id = session_id.as_str(),
                message_id,
                source,
                "delivery mirror appended"
            );
        }
        Err(e) => {
            warn!(
                platform,
                chat_id,
                session_id = session_id.as_str(),
                error = %e,
                "mirror: failed to append mirror message"
            );
        }
    }
}

/// Resolve the deterministic session ID for a platform + chat_id pair.
///
/// Follows the same conventions as the platform webhook handlers.
fn resolve_session_id(platform: &str, chat_id: &str) -> String {
    match platform {
        "telegram" => format!("tg-{chat_id}"),
        "slack" => format!("slack-{chat_id}"),
        "discord" => format!("discord-{chat_id}"),
        "whatsapp" => format!("wa-{chat_id}"),
        "homeassistant" => format!("ha-{chat_id}"),
        "signal" => format!("signal-{chat_id}"),
        other => format!("{other}-{chat_id}"),
    }
}

/// Ensure a session exists, creating it if necessary.
fn ensure_session_exists(
    store: &SessionStore,
    session_id: &str,
    platform: &str,
) -> Result<(), genesis_storage::StorageError> {
    match store.get_session(session_id)? {
        Some(_) => Ok(()),
        None => {
            store.create_session(session_id, platform, None)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_storage::{bootstrap, SessionStore};
    use tempfile::tempdir;

    #[test]
    fn append_delivery_mirror_creates_session_and_message() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        append_delivery_mirror(&db_path, "telegram", "12345", "Hello from CLI", "cli");

        let store = SessionStore::new(&db_path);
        let session = store
            .get_session("tg-12345")
            .expect("get_session")
            .expect("session should exist");
        assert_eq!(session.platform, "telegram");

        let messages = store.load_messages("tg-12345").expect("load_messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[0].content.as_deref(), Some("Hello from CLI"));
        assert!(messages[0].mirror);
        assert_eq!(messages[0].mirror_source.as_deref(), Some("cli"));
    }

    #[test]
    fn append_delivery_mirror_appends_to_existing_session() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = SessionStore::new(&db_path);
        store
            .create_session("slack-general", "slack", Some("General Chat"))
            .expect("create session");
        store
            .append_message("slack-general", "user", Some("hi"), None, None, None)
            .expect("append msg");

        append_delivery_mirror(
            &db_path,
            "slack",
            "general",
            "Scheduled reminder: standup in 5 minutes",
            "schedule",
        );

        let messages = store.load_messages("slack-general").expect("load_messages");
        assert_eq!(messages.len(), 2);

        // First message is the original user message
        assert_eq!(messages[0].role, "user");
        assert!(!messages[0].mirror);

        // Second message is the mirror
        assert_eq!(messages[1].role, "assistant");
        assert!(messages[1].mirror);
        assert_eq!(messages[1].mirror_source.as_deref(), Some("schedule"));
        assert!(messages[1].content.as_deref().unwrap().contains("standup"));
    }

    #[test]
    fn append_delivery_mirror_does_not_panic_on_missing_db() {
        // Non-existent path — should log a warning but not panic.
        append_delivery_mirror(
            Path::new("/nonexistent/path/genesis.db"),
            "telegram",
            "99999",
            "this should not crash",
            "cli",
        );
    }

    #[test]
    fn resolve_session_id_follows_platform_conventions() {
        assert_eq!(resolve_session_id("telegram", "12345"), "tg-12345");
        assert_eq!(resolve_session_id("slack", "C01ABC"), "slack-C01ABC");
        assert_eq!(resolve_session_id("discord", "98765"), "discord-98765");
        assert_eq!(resolve_session_id("whatsapp", "15551234"), "wa-15551234");
        assert_eq!(resolve_session_id("homeassistant", "light"), "ha-light");
        assert_eq!(resolve_session_id("custom", "xyz"), "custom-xyz");
    }

    #[test]
    fn multiple_mirrors_from_different_sources() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        append_delivery_mirror(&db_path, "discord", "chan1", "Message A", "cli");
        append_delivery_mirror(&db_path, "discord", "chan1", "Message B", "api");
        append_delivery_mirror(&db_path, "discord", "chan1", "Message C", "schedule");

        let store = SessionStore::new(&db_path);
        let messages = store.load_messages("discord-chan1").expect("load_messages");
        assert_eq!(messages.len(), 3);
        assert!(messages.iter().all(|m| m.mirror));
        assert_eq!(messages[0].mirror_source.as_deref(), Some("cli"));
        assert_eq!(messages[1].mirror_source.as_deref(), Some("api"));
        assert_eq!(messages[2].mirror_source.as_deref(), Some("schedule"));
    }
}
