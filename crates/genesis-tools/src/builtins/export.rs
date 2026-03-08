use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use genesis_storage::{bootstrap, SessionStore};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Export a conversation session to a file (Markdown or JSON).
pub struct SessionExportTool;

impl ToolHandler for SessionExportTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let session_id = call
            .arguments
            .get("session_id")
            .cloned()
            .unwrap_or_else(|| context.session_id.clone());

        let format = call
            .arguments
            .get("format")
            .map(|f| f.to_lowercase())
            .unwrap_or_else(|| "markdown".to_owned());

        let output_path = call.arguments.get("path");

        let db_path = Path::new(&context.data_dir).join("genesis.db");
        let _ = bootstrap(&db_path);
        let store = SessionStore::new(&db_path);

        // Load session title
        let session_title = store
            .get_session(&session_id)
            .ok()
            .flatten()
            .and_then(|s| s.title);

        // Load messages via storage layer
        let stored_messages = store.load_messages(&session_id).map_err(|e| {
            ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to load messages: {e}"),
            }
        })?;

        if stored_messages.is_empty() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("no messages found for session '{session_id}'"),
            });
        }

        let messages: Vec<(String, Option<String>, Option<String>, String)> = stored_messages
            .into_iter()
            .map(|m| (m.role, m.content, m.tool_calls_json, m.created_at))
            .collect();

        let content = match format.as_str() {
            "json" => export_json(&session_id, session_title.as_deref(), &messages),
            "markdown" | "md" => export_markdown(&session_id, session_title.as_deref(), &messages),
            _ => {
                return Err(ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("unsupported format '{format}'; use 'markdown' or 'json'"),
                })
            }
        };

        // Write to file or return inline
        if let Some(path) = output_path {
            if let Some(parent) = Path::new(path).parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(path, &content).map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to write export file: {e}"),
            })?;
            Ok(ToolOutput {
                content: format!(
                    "Session '{session_id}' exported to {path} ({} messages)",
                    messages.len()
                ),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), call.name.clone()),
                    ("path".to_owned(), path.clone()),
                ]),
            })
        } else {
            Ok(ToolOutput {
                content,
                metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
            })
        }
    }
}

fn export_markdown(
    session_id: &str,
    title: Option<&str>,
    messages: &[(String, Option<String>, Option<String>, String)],
) -> String {
    let mut md = String::new();
    md.push_str(&format!(
        "# {}\n\nSession: `{}`\n\n---\n\n",
        title.unwrap_or("Conversation Export"),
        session_id
    ));

    for (role, content, tool_calls, timestamp) in messages {
        let label = match role.as_str() {
            "user" => "**User**",
            "assistant" => "**Assistant**",
            "system" => "**System**",
            "tool" => "**Tool Result**",
            _ => role.as_str(),
        };

        md.push_str(&format!("### {} ({})\n\n", label, timestamp));

        if let Some(text) = content {
            if !text.is_empty() {
                md.push_str(text);
                md.push_str("\n\n");
            }
        }

        if let Some(tc) = tool_calls {
            if !tc.is_empty() && tc != "null" {
                md.push_str("```json\n");
                md.push_str(tc);
                md.push_str("\n```\n\n");
            }
        }
    }

    md
}

fn export_json(
    session_id: &str,
    title: Option<&str>,
    messages: &[(String, Option<String>, Option<String>, String)],
) -> String {
    let entries: Vec<serde_json::Value> = messages
        .iter()
        .map(|(role, content, tool_calls, timestamp)| {
            let mut entry = serde_json::json!({
                "role": role,
                "timestamp": timestamp,
            });
            if let Some(text) = content {
                entry["content"] = serde_json::Value::String(text.clone());
            }
            if let Some(tc) = tool_calls {
                if !tc.is_empty() && tc != "null" {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(tc) {
                        entry["tool_calls"] = parsed;
                    }
                }
            }
            entry
        })
        .collect();

    let export = serde_json::json!({
        "session_id": session_id,
        "title": title,
        "message_count": entries.len(),
        "messages": entries,
    });

    serde_json::to_string_pretty(&export).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_markdown_formats_messages() {
        let messages = vec![
            (
                "user".to_owned(),
                Some("Hello!".to_owned()),
                None,
                "2024-01-01 10:00:00".to_owned(),
            ),
            (
                "assistant".to_owned(),
                Some("Hi there!".to_owned()),
                None,
                "2024-01-01 10:00:01".to_owned(),
            ),
        ];

        let md = export_markdown("s-1", Some("Test Chat"), &messages);
        assert!(md.contains("# Test Chat"));
        assert!(md.contains("Session: `s-1`"));
        assert!(md.contains("**User**"));
        assert!(md.contains("Hello!"));
        assert!(md.contains("**Assistant**"));
        assert!(md.contains("Hi there!"));
    }

    #[test]
    fn export_json_produces_valid_json() {
        let messages = vec![(
            "user".to_owned(),
            Some("Test message".to_owned()),
            None,
            "2024-01-01 10:00:00".to_owned(),
        )];

        let json = export_json("s-1", Some("Test"), &messages);
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("should produce valid JSON");
        assert_eq!(parsed["session_id"], "s-1");
        assert_eq!(parsed["message_count"], 1);
        assert!(parsed["messages"].is_array());
    }

    #[test]
    fn export_json_includes_tool_calls() {
        let messages = vec![(
            "assistant".to_owned(),
            None,
            Some(r#"[{"name":"echo","args":{"message":"hi"}}]"#.to_owned()),
            "2024-01-01 10:00:00".to_owned(),
        )];

        let json = export_json("s-1", None, &messages);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["messages"][0]["tool_calls"].is_array());
    }

    #[test]
    fn export_markdown_handles_tool_calls() {
        let messages = vec![(
            "assistant".to_owned(),
            Some("Let me check.".to_owned()),
            Some(r#"[{"name":"echo"}]"#.to_owned()),
            "2024-01-01 10:00:00".to_owned(),
        )];

        let md = export_markdown("s-1", None, &messages);
        assert!(md.contains("```json"));
        assert!(md.contains("echo"));
    }

    #[test]
    fn export_tool_requires_no_arguments_uses_context_session() {
        let tool = SessionExportTool;
        let call = ToolCall {
            name: "session_export".to_owned(),
            arguments: BTreeMap::new(),
        };
        let ctx = ToolContext {
            session_id: "test-session".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/nonexistent".to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
        };

        // Will fail on DB open but validates argument handling
        let err = tool.run(&call, &ctx).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(
                    reason.contains("messages") || reason.contains("database") || reason.contains("open"),
                    "unexpected error: {reason}"
                );
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }
}
