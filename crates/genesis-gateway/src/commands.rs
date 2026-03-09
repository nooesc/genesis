//! Gateway slash command handler.
//!
//! Intercepts commands like `/new`, `/stop`, `/help` from gateway
//! platforms before they reach the agent. Returns a response if the command
//! was handled, or `None` to pass through to the agent.

use chrono::{Local, NaiveDateTime};
use genesis_config::{GatewayConfig, GenesisConfig};
use genesis_storage::SessionStore;
use tracing::{info, warn};

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
/// - `/config` — show current model and session info
pub fn handle_command(
    text: &str,
    session_id: &str,
    store: &SessionStore,
    config: &GenesisConfig,
) -> CommandResult {
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
             \u{2022} /config \u{2014} Show current model and session info\n\
             \u{2022} /model \u{2014} Show current model\n\
             \u{2022} /cost \u{2014} Show estimated cost for this session\n\
             \u{2022} /help \u{2014} Show this message"
                .to_owned(),
        ),

        "/stop" => CommandResult::Reply(
            "Acknowledged. If I'm still processing, the response will be discarded.".to_owned(),
        ),

        "/id" => CommandResult::Reply(format!("Session ID: `{session_id}`")),

        "/config" => {
            let session = store.get_session(session_id).ok().flatten();
            let message_count = store
                .load_messages(session_id)
                .map(|m| m.len())
                .unwrap_or(0);

            let mut lines = vec![
                format!(
                    "Model: {} / {}",
                    config.provider.backend, config.provider.model
                ),
                format!("Session ID: `{session_id}`"),
                format!("Messages: {message_count}"),
            ];

            if let Some(session) = session {
                lines.push(format!("Platform: {}", session.platform));
                if let Some(title) = session.title {
                    lines.push(format!("Title: {title}"));
                }
            }

            CommandResult::Reply(lines.join("\n"))
        }

        "/model" => CommandResult::Reply(format!(
            "{} / {}",
            config.provider.backend, config.provider.model
        )),

        "/cost" => {
            let session = store.get_session(session_id).ok().flatten();
            match session {
                Some(s) => {
                    let model = &config.provider.model;
                    let cost = genesis_provider::pricing::estimate_cost(
                        model,
                        s.total_input_tokens as u32,
                        s.total_output_tokens as u32,
                    );
                    let mut lines = vec![format!(
                        "Token usage: {} input + {} output",
                        s.total_input_tokens, s.total_output_tokens
                    )];
                    if let Some(cost) = cost {
                        lines.push(format!("Estimated cost: {cost}"));
                    } else {
                        lines.push("Estimated cost: unknown (model not in pricing table)".to_owned());
                    }
                    CommandResult::Reply(lines.join("\n"))
                }
                None => CommandResult::Reply("No session data available.".to_owned()),
            }
        }

        _ => {
            // Unknown slash command — pass through to agent
            // (it might be a skill invocation like /gif-search)
            CommandResult::PassThrough
        }
    }
}

/// Check if a session should be auto-reset based on gateway config policies.
///
/// Returns `true` if the session was expired and deleted (caller should
/// treat this as a fresh session). Returns `false` if the session is
/// still valid or doesn't exist.
pub fn check_session_expiry(
    session_id: &str,
    store: &SessionStore,
    gateway: Option<&GatewayConfig>,
) -> bool {
    let config = match gateway {
        Some(c) => c,
        None => return false,
    };

    // No policies configured
    if config.idle_timeout_minutes.is_none() && config.daily_reset_hour.is_none() {
        return false;
    }

    let session = match store.get_session(session_id) {
        Ok(Some(s)) => s,
        _ => return false, // No session to expire
    };

    // Parse the updated_at timestamp (SQLite CURRENT_TIMESTAMP format: "YYYY-MM-DD HH:MM:SS")
    let updated_at = match NaiveDateTime::parse_from_str(&session.updated_at, "%Y-%m-%d %H:%M:%S") {
        Ok(dt) => dt,
        Err(_) => {
            warn!(
                session_id,
                updated_at = session.updated_at.as_str(),
                "could not parse session updated_at timestamp"
            );
            return false;
        }
    };

    let now = Local::now().naive_local();

    // Check idle timeout
    if let Some(idle_minutes) = config.idle_timeout_minutes {
        let elapsed = now.signed_duration_since(updated_at);
        if elapsed.num_minutes() >= idle_minutes as i64 {
            info!(
                session_id,
                idle_minutes = elapsed.num_minutes(),
                threshold = idle_minutes,
                "session expired due to idle timeout"
            );
            let _ = store.delete_session(session_id);
            return true;
        }
    }

    // Check daily reset hour
    if let Some(reset_hour) = config.daily_reset_hour {
        if reset_hour < 24 {
            let today_reset = now
                .date()
                .and_hms_opt(reset_hour as u32, 0, 0)
                .unwrap_or(now);

            // If the reset time has passed today and the session was last
            // updated before the reset time, it should be cleared.
            if now >= today_reset && updated_at < today_reset {
                info!(
                    session_id,
                    reset_hour,
                    "session expired due to daily reset"
                );
                let _ = store.delete_session(session_id);
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_config::{ProviderConfig, RuntimeConfig, StorageConfig};
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
        match handle_command("/help", "test-session", &store, &test_config()) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("/new"));
                assert!(msg.contains("/help"));
                assert!(msg.contains("/config"));
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

        match handle_command("/new", "s1", &store, &test_config()) {
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
        match handle_command("/id", "tg-42", &store, &test_config()) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("tg-42"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    #[test]
    fn regular_message_passes_through() {
        let store = test_store();
        match handle_command("Hello there", "test", &store, &test_config()) {
            CommandResult::PassThrough => {} // expected
            CommandResult::Reply(_) => panic!("expected PassThrough"),
        }
    }

    #[test]
    fn unknown_slash_command_passes_through() {
        let store = test_store();
        match handle_command("/gif-search cats", "test", &store, &test_config()) {
            CommandResult::PassThrough => {} // expected — could be a skill
            CommandResult::Reply(_) => panic!("expected PassThrough for unknown command"),
        }
    }

    #[test]
    fn stop_command_returns_reply() {
        let store = test_store();
        match handle_command("/stop", "test", &store, &test_config()) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("Acknowledged"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    #[test]
    fn reset_alias_works() {
        let store = test_store();
        match handle_command("/reset", "test", &store, &test_config()) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("fresh"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    #[test]
    fn config_command_shows_model_and_session_info() {
        let store = test_store();
        store.create_session("s1", "telegram", Some("Support")).unwrap();
        store
            .append_message("s1", "user", Some("Hello"), None, None)
            .unwrap();

        match handle_command("/config", "s1", &store, &test_config()) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("Model: openai / gpt-4.1-mini"));
                assert!(msg.contains("Session ID: `s1`"));
                assert!(msg.contains("Messages: 1"));
                assert!(msg.contains("Platform: telegram"));
                assert!(msg.contains("Title: Support"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    #[test]
    fn model_command_shows_backend_and_model() {
        let store = test_store();
        match handle_command("/model", "test", &store, &test_config()) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("openai"));
                assert!(msg.contains("gpt-4.1-mini"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    #[test]
    fn cost_command_shows_token_usage() {
        let store = test_store();
        store.create_session("s1", "telegram", None).unwrap();
        store
            .append_message("s1", "user", Some("Hello"), None, None)
            .unwrap();

        match handle_command("/cost", "s1", &store, &test_config()) {
            CommandResult::Reply(msg) => {
                assert!(msg.contains("Token usage:"));
            }
            CommandResult::PassThrough => panic!("expected Reply"),
        }
    }

    // --- Session expiry tests ---

    /// Set a session's updated_at to a specific timestamp for testing.
    fn set_updated_at(store: &SessionStore, session_id: &str, timestamp: &str) {
        let conn = rusqlite::Connection::open(store.database_path()).unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![timestamp, session_id],
        )
        .unwrap();
    }

    fn test_config() -> GenesisConfig {
        GenesisConfig {
            schema_version: 1,
            profile: "default".to_owned(),
            provider: ProviderConfig {
                backend: "openai".to_owned(),
                model: "gpt-4.1-mini".to_owned(),
                base_url: None,
                api_key_env: None,
                extra_body: None,
                tool_call_parser: None,
            },
            tool_provider: None,
            fallback_providers: Vec::new(),
            mcp_servers: std::collections::HashMap::new(),
            storage: StorageConfig {
                data_dir: std::path::PathBuf::from("/tmp/genesis"),
                database_path: std::path::PathBuf::from("/tmp/genesis/genesis.db"),
            },
            runtime: RuntimeConfig {
                max_concurrency: 4,
                allow_destructive_tools: false,
                max_turns: 20,
                max_context_messages: None,
                budget_limit: None,
                terminal: None,
                thinking_budget: None,
                max_context_tokens: None,
                max_iterations: None,
                context_security: genesis_config::ContextSecurityPolicy::default(),
                reasoning_effort: None,
                cache: None,
                tool_filter: None,
            },
            gateway: None,
            toolsets: std::collections::HashMap::new(),
            personality: None,
        }
    }

    #[test]
    fn expiry_returns_false_when_no_config() {
        let store = test_store();
        store.create_session("s1", "test", None).unwrap();
        assert!(!check_session_expiry("s1", &store, None));
    }

    #[test]
    fn expiry_returns_false_when_no_policies() {
        let store = test_store();
        store.create_session("s1", "test", None).unwrap();
        let config = GatewayConfig {
            idle_timeout_minutes: None,
            daily_reset_hour: None,
            rate_limit_rpm: None,
            webhooks: Vec::new(),
        };
        assert!(!check_session_expiry("s1", &store, Some(&config)));
    }

    #[test]
    fn expiry_returns_false_for_nonexistent_session() {
        let store = test_store();
        let config = GatewayConfig {
            idle_timeout_minutes: Some(30),
            daily_reset_hour: None,
            rate_limit_rpm: None,
            webhooks: Vec::new(),
        };
        assert!(!check_session_expiry("nonexistent", &store, Some(&config)));
    }

    #[test]
    fn expiry_idle_timeout_triggers_for_old_session() {
        let store = test_store();
        store.create_session("s1", "test", None).unwrap();
        // Set updated_at to 2 hours ago
        let two_hours_ago = (Local::now() - chrono::Duration::hours(2))
            .naive_local()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        set_updated_at(&store, "s1", &two_hours_ago);

        let config = GatewayConfig {
            idle_timeout_minutes: Some(60),
            daily_reset_hour: None,
            rate_limit_rpm: None,
            webhooks: Vec::new(),
        };
        assert!(check_session_expiry("s1", &store, Some(&config)));
        // Session should be deleted
        assert!(store.get_session("s1").unwrap().is_none());
    }

    #[test]
    fn expiry_idle_timeout_does_not_trigger_for_recent_session() {
        let store = test_store();
        store.create_session("s1", "test", None).unwrap();
        // Just created — updated_at is CURRENT_TIMESTAMP (now)

        let config = GatewayConfig {
            idle_timeout_minutes: Some(60),
            daily_reset_hour: None,
            rate_limit_rpm: None,
            webhooks: Vec::new(),
        };
        assert!(!check_session_expiry("s1", &store, Some(&config)));
        // Session should still exist
        assert!(store.get_session("s1").unwrap().is_some());
    }

    #[test]
    fn expiry_daily_reset_triggers_for_yesterday_session() {
        let store = test_store();
        store.create_session("s1", "test", None).unwrap();
        // Set updated_at to yesterday
        let yesterday = (Local::now() - chrono::Duration::days(1))
            .naive_local()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        set_updated_at(&store, "s1", &yesterday);

        // Reset at hour 0 — any session from yesterday is expired
        let config = GatewayConfig {
            idle_timeout_minutes: None,
            daily_reset_hour: Some(0),
            rate_limit_rpm: None,
            webhooks: Vec::new(),
        };
        assert!(check_session_expiry("s1", &store, Some(&config)));
        assert!(store.get_session("s1").unwrap().is_none());
    }
}
