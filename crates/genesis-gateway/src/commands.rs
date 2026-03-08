//! Gateway slash command handler.
//!
//! Intercepts commands like `/new`, `/stop`, `/help` from gateway
//! platforms before they reach the agent. Returns a response if the command
//! was handled, or `None` to pass through to the agent.

use genesis_storage::SessionStore;
use tracing::info;

/// Result of processing a gateway command.
pub enum CommandResult {
    /// Command was handled; send this reply to the user.
    Reply(String),
    /// Not a command; pass the message through to the agent.
    PassThrough,
}

/// Check if a message is a gateway slash command and handle it.
///
/// Supported commands:
/// - `/new` — reset the current session (start fresh)
/// - `/help` — show available commands
/// - `/stop` — acknowledge stop
/// - `/id` — show the current session ID
pub fn handle_command(text: &str, session_id: &str, store: &SessionStore) -> CommandResult {
    let trimmed = text.trim();

    // Only handle messages that start with /
    if !trimmed.starts_with('/') {
        return CommandResult::PassThrough;
    }

    // Parse command and args
    let (cmd, _args) = match trimmed.split_once(' ') {
        Some((c, a)) => (c, a.trim()),
        None => (trimmed, ""),
    };

    match cmd {
        "/new" | "/reset" => match store.delete_session(session_id) {
            Ok(_) => {
                info!(session_id, "session reset via gateway command");
                CommandResult::Reply(
                    "Session cleared. I'm starting fresh \u{2014} what can I help you with?"
                        .to_owned(),
                )
            }
            Err(e) => CommandResult::Reply(format!("Failed to reset session: {e}")),
        },

        "/help" => CommandResult::Reply(
            "Available commands:\n\
             \u{2022} /new \u{2014} Start a fresh conversation\n\
             \u{2022} /id \u{2014} Show current session ID\n\
             \u{2022} /help \u{2014} Show this message"
                .to_owned(),
        ),

        "/stop" => CommandResult::Reply(
            "Acknowledged. If I'm still processing, the response will be discarded.".to_owned(),
        ),

        "/id" => CommandResult::Reply(format!("Session ID: `{session_id}`")),

        _ => {
            // Unknown slash command — pass through to agent
            // (it might be a skill invocation like /gif-search)
            CommandResult::PassThrough
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_storage::{bootstrap, SessionStore};
    use tempfile::tempdir;

    fn test_store() -> SessionStore {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        bootstrap(&db_path).expect("bootstrap");
        // Keep dir alive by leaking (test only)
        let store = SessionStore::new(&db_path);
        std::mem::forget(dir);
        store
    }

    #[test]
    fn help_command_returns_reply() {
        let store = test_store();
        match handle_command("/help", "test-session", &store) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("/new"));
                assert!(msg.contains("/help"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    #[test]
    fn new_command_resets_session() {
        let store = test_store();
        // Create a session first
        store.create_session("s1", "telegram", Some("Test")).unwrap();
        store
            .append_message("s1", "user", Some("Hello"), None, None)
            .unwrap();

        match handle_command("/new", "s1", &store) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("fresh"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }

        // Session messages should be gone
        let msgs = store.load_messages("s1").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn id_command_shows_session_id() {
        let store = test_store();
        match handle_command("/id", "tg-42", &store) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("tg-42"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    #[test]
    fn regular_message_passes_through() {
        let store = test_store();
        match handle_command("Hello there", "test", &store) {
            CommandResult::PassThrough => {} // expected
            CommandResult::Reply(_) => panic!("expected PassThrough"),
        }
    }

    #[test]
    fn unknown_slash_command_passes_through() {
        let store = test_store();
        match handle_command("/gif-search cats", "test", &store) {
            CommandResult::PassThrough => {} // expected — could be a skill
            CommandResult::Reply(_) => panic!("expected PassThrough for unknown command"),
        }
    }

    #[test]
    fn stop_command_returns_reply() {
        let store = test_store();
        match handle_command("/stop", "test", &store) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("Acknowledged"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    #[test]
    fn reset_alias_works() {
        let store = test_store();
        match handle_command("/reset", "test", &store) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("fresh"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }
}
