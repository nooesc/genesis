use std::collections::BTreeMap;

use genesis_storage::{bootstrap, SessionStore};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that searches past session content using full-text search.
pub struct SessionSearchTool;

impl ToolHandler for SessionSearchTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = call
            .arguments
            .get("query")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "query",
            })?;

        let db_path = context.db_path();
        bootstrap(&db_path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("database initialization failed: {e}"),
        })?;
        let store = SessionStore::new(&db_path);

        let sessions = store
            .search_sessions(query)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("search failed: {e}"),
            })?;

        if sessions.is_empty() {
            return Ok(ToolOutput {
                content: format!("No past sessions found matching \"{query}\""),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), call.name.clone()),
                    ("count".to_owned(), "0".to_owned()),
                ]),
            });
        }

        let mut lines = vec![format!(
            "Found {} sessions matching \"{query}\":",
            sessions.len()
        )];
        for session in &sessions {
            let title = session.title.as_deref().unwrap_or("(untitled)");
            lines.push(format!(
                "- {} [{}] {} ({})",
                session.id, session.platform, title, session.updated_at
            ));
        }

        Ok(ToolOutput {
            content: lines.join("\n"),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("query".to_owned(), query.clone()),
                ("count".to_owned(), sessions.len().to_string()),
            ]),
        })
    }
}

/// Tool that loads recent messages from a specific session.
pub struct SessionHistoryTool;

impl ToolHandler for SessionHistoryTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let session_id =
            call.arguments
                .get("session_id")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "session_id",
                })?;

        let limit = call
            .arguments
            .get("limit")
            .map(|v| {
                v.parse::<usize>().map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("invalid limit: {e}"),
                })
            })
            .transpose()?
            .unwrap_or(20);

        let db_path = context.db_path();
        bootstrap(&db_path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("database initialization failed: {e}"),
        })?;
        let store = SessionStore::new(&db_path);

        let messages = store
            .load_messages(session_id)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to load messages: {e}"),
            })?;

        if messages.is_empty() {
            return Ok(ToolOutput {
                content: format!("Session \"{session_id}\" has no messages or does not exist."),
                metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
            });
        }

        // Take the most recent messages up to the limit
        let start = if messages.len() > limit {
            messages.len() - limit
        } else {
            0
        };
        let recent = &messages[start..];

        let mut lines = vec![format!(
            "Session {} ({} messages, showing last {}):",
            session_id,
            messages.len(),
            recent.len()
        )];

        for msg in recent {
            let content = msg.content.as_deref().unwrap_or("[tool call]");
            let truncated = if content.len() > 300 {
                let end = content.floor_char_boundary(300);
                format!("{}...", &content[..end])
            } else {
                content.to_owned()
            };
            lines.push(format!("[{}] {}: {}", msg.created_at, msg.role, truncated));
        }

        Ok(ToolOutput {
            content: lines.join("\n"),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("session_id".to_owned(), session_id.clone()),
                ("total_messages".to_owned(), messages.len().to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_storage::{bootstrap, SessionStore};
    use tempfile::tempdir;

    fn ctx_with_dir(data_dir: &str) -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: data_dir.to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
        }
    }

    #[test]
    fn session_search_finds_matching_sessions() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let store = SessionStore::new(&db_path);
        store
            .create_session("s-1", "cli", Some("Rust debugging"))
            .expect("create session");
        store
            .append_message("s-1", "user", Some("How do I debug Rust code?"), None, None)
            .expect("append");

        let call = ToolCall {
            name: "session_search".to_owned(),
            arguments: BTreeMap::from([("query".to_owned(), "debug Rust".to_owned())]),
        };
        let output = SessionSearchTool.run(&call, &ctx).expect("search");
        assert!(output.content.contains("s-1"));
        assert!(output.content.contains("Rust debugging"));
    }

    #[test]
    fn session_search_returns_none_for_no_matches() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "session_search".to_owned(),
            arguments: BTreeMap::from([("query".to_owned(), "nonexistent topic".to_owned())]),
        };
        let output = SessionSearchTool.run(&call, &ctx).expect("search");
        assert!(output.content.contains("No past sessions"));
    }

    #[test]
    fn session_history_loads_messages() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let store = SessionStore::new(&db_path);
        store
            .create_session("s-2", "cli", None)
            .expect("create session");
        store
            .append_message("s-2", "user", Some("Hello Eve"), None, None)
            .expect("append");
        store
            .append_message("s-2", "assistant", Some("Hi there!"), None, None)
            .expect("append");

        let call = ToolCall {
            name: "session_history".to_owned(),
            arguments: BTreeMap::from([("session_id".to_owned(), "s-2".to_owned())]),
        };
        let output = SessionHistoryTool.run(&call, &ctx).expect("history");
        assert!(output.content.contains("Hello Eve"));
        assert!(output.content.contains("Hi there!"));
        assert!(output.content.contains("2 messages"));
    }

    #[test]
    fn session_history_respects_limit() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let store = SessionStore::new(&db_path);
        store
            .create_session("s-3", "cli", None)
            .expect("create session");
        for i in 0..10 {
            store
                .append_message("s-3", "user", Some(&format!("Message {i}")), None, None)
                .expect("append");
        }

        let call = ToolCall {
            name: "session_history".to_owned(),
            arguments: BTreeMap::from([
                ("session_id".to_owned(), "s-3".to_owned()),
                ("limit".to_owned(), "3".to_owned()),
            ]),
        };
        let output = SessionHistoryTool.run(&call, &ctx).expect("history");
        assert!(output.content.contains("10 messages, showing last 3"));
        assert!(output.content.contains("Message 7"));
        assert!(output.content.contains("Message 8"));
        assert!(output.content.contains("Message 9"));
    }

    #[test]
    fn session_history_handles_missing_session() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "session_history".to_owned(),
            arguments: BTreeMap::from([("session_id".to_owned(), "nonexistent".to_owned())]),
        };
        let output = SessionHistoryTool.run(&call, &ctx).expect("history");
        assert!(output.content.contains("no messages"));
    }

    #[test]
    fn session_history_truncates_multibyte_content_without_panic() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let store = SessionStore::new(&db_path);
        store
            .create_session("s-utf8", "cli", None)
            .expect("create session");

        // Build a string >300 bytes where byte 300 falls inside a multi-byte
        // character. Each emoji is 4 bytes; 76 emojis = 304 bytes.
        let emoji_content = "\u{1F30D}".repeat(76);
        assert!(emoji_content.len() > 300);
        store
            .append_message("s-utf8", "user", Some(&emoji_content), None, None)
            .expect("append");

        let call = ToolCall {
            name: "session_history".to_owned(),
            arguments: BTreeMap::from([("session_id".to_owned(), "s-utf8".to_owned())]),
        };
        // This used to panic with `&content[..300]` on a multi-byte boundary.
        let output = SessionHistoryTool.run(&call, &ctx).expect("history");
        assert!(output.content.contains("..."));
    }
}
