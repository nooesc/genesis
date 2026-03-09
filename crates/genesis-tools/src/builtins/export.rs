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
            "chatml" => export_chatml(&messages),
            "jsonl" | "finetune" => export_jsonl(&messages),
            _ => {
                return Err(ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!(
                        "unsupported format '{format}'; use 'markdown', 'json', 'chatml', or 'jsonl'"
                    ),
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

pub fn export_markdown(
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

pub fn export_json(
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

/// Export as OpenAI fine-tuning JSONL format.
///
/// Each output line is a complete training example:
/// `{"messages": [{"role": "...", "content": "..."}]}`
///
/// Tool calls and tool results are omitted — only user/assistant/system
/// text turns are included.
pub fn export_jsonl(messages: &[(String, Option<String>, Option<String>, String)]) -> String {
    let filtered: Vec<serde_json::Value> = messages
        .iter()
        .filter(|(role, content, _, _)| {
            matches!(role.as_str(), "user" | "assistant" | "system")
                && content.as_ref().map_or(false, |c| !c.is_empty())
        })
        .map(|(role, content, _, _)| {
            serde_json::json!({
                "role": role,
                "content": content.as_deref().unwrap_or(""),
            })
        })
        .collect();

    if filtered.is_empty() {
        return String::new();
    }

    let example = serde_json::json!({ "messages": filtered });
    serde_json::to_string(&example).unwrap_or_default() + "\n"
}

pub fn export_chatml(messages: &[(String, Option<String>, Option<String>, String)]) -> String {
    let mut output = String::new();

    for (role, content, tool_calls, _) in messages {
        let role = match role.as_str() {
            "user" => "user",
            "assistant" => "assistant",
            "system" => "system",
            "tool" => "tool",
            other => other,
        };

        let text = match (content.as_deref(), tool_calls.as_deref()) {
            (Some(text), Some(tc)) if !text.is_empty() && !tc.is_empty() && tc != "null" => {
                format!("{text}\n{tc}")
            }
            (Some(text), _) if !text.is_empty() => text.to_owned(),
            (_, Some(tc)) if !tc.is_empty() && tc != "null" => tc.to_owned(),
            _ => String::new(),
        };

        output.push_str(&format!("<|im_start|>{role}\n{text}<|im_end|>\n"));
    }

    output
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
    fn export_chatml_formats_roles_and_tool_calls() {
        let messages = vec![
            (
                "system".to_owned(),
                Some("You are helpful.".to_owned()),
                None,
                "2024-01-01 10:00:00".to_owned(),
            ),
            (
                "user".to_owned(),
                Some("Hello!".to_owned()),
                None,
                "2024-01-01 10:00:01".to_owned(),
            ),
            (
                "assistant".to_owned(),
                Some("Let me check.".to_owned()),
                Some(r#"[{"name":"echo","arguments":{"message":"hi"}}]"#.to_owned()),
                "2024-01-01 10:00:02".to_owned(),
            ),
            (
                "tool".to_owned(),
                Some("echo: hi".to_owned()),
                None,
                "2024-01-01 10:00:03".to_owned(),
            ),
        ];

        let chatml = export_chatml(&messages);
        assert!(chatml.contains("<|im_start|>system\nYou are helpful.<|im_end|>\n"));
        assert!(chatml.contains("<|im_start|>user\nHello!<|im_end|>\n"));
        assert!(chatml.contains("<|im_start|>assistant\nLet me check.\n[{\"name\":\"echo\",\"arguments\":{\"message\":\"hi\"}}]<|im_end|>\n"));
        assert!(chatml.contains("<|im_start|>tool\necho: hi<|im_end|>\n"));
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
            default_working_dir: None,
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

    #[test]
    fn export_jsonl_produces_valid_finetune_format() {
        let messages = vec![
            (
                "system".to_owned(),
                Some("You are helpful.".to_owned()),
                None,
                "2024-01-01 10:00:00".to_owned(),
            ),
            (
                "user".to_owned(),
                Some("Hello!".to_owned()),
                None,
                "2024-01-01 10:00:01".to_owned(),
            ),
            (
                "assistant".to_owned(),
                Some("Hi there!".to_owned()),
                None,
                "2024-01-01 10:00:02".to_owned(),
            ),
        ];

        let jsonl = export_jsonl(&messages);
        let parsed: serde_json::Value =
            serde_json::from_str(jsonl.trim()).expect("should be valid JSON");
        let msgs = parsed["messages"].as_array().expect("should have messages array");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Hello!");
        assert_eq!(msgs[2]["role"], "assistant");
    }

    #[test]
    fn export_jsonl_filters_tool_messages() {
        let messages = vec![
            (
                "user".to_owned(),
                Some("Do something".to_owned()),
                None,
                "2024-01-01 10:00:00".to_owned(),
            ),
            (
                "assistant".to_owned(),
                None, // tool call only, no text
                Some(r#"[{"name":"echo"}]"#.to_owned()),
                "2024-01-01 10:00:01".to_owned(),
            ),
            (
                "tool".to_owned(),
                Some("echo result".to_owned()),
                None,
                "2024-01-01 10:00:02".to_owned(),
            ),
            (
                "assistant".to_owned(),
                Some("Done!".to_owned()),
                None,
                "2024-01-01 10:00:03".to_owned(),
            ),
        ];

        let jsonl = export_jsonl(&messages);
        let parsed: serde_json::Value =
            serde_json::from_str(jsonl.trim()).expect("should be valid JSON");
        let msgs = parsed["messages"].as_array().expect("should have messages");
        // Should only include user "Do something" and assistant "Done!"
        // (tool messages and assistant with no text content are filtered)
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["content"], "Do something");
        assert_eq!(msgs[1]["content"], "Done!");
    }
}
