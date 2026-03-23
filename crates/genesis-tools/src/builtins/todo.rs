use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// In-memory task list for agent planning, keyed by session ID.
///
/// Unlike persistent storage tools, the todo list lives only for the duration
/// of the current process. It helps the agent decompose complex tasks,
/// track progress, and report completion status.
///
/// Each session gets its own isolated todo list so that concurrent sessions
/// (e.g. via the gateway) do not leak state to each other.
static TODO_LISTS: LazyLock<Mutex<HashMap<String, Vec<TodoItem>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone)]
struct TodoItem {
    id: usize,
    text: String,
    status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq)]
enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Done => write!(f, "done"),
        }
    }
}

fn parse_status(s: &str) -> Result<TodoStatus, String> {
    match s {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" => Ok(TodoStatus::InProgress),
        "done" => Ok(TodoStatus::Done),
        other => Err(format!(
            "invalid status `{other}`: expected pending, in_progress, or done"
        )),
    }
}

pub struct TodoTool;

impl ToolHandler for TodoTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let action = call
            .arguments
            .get("action")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "action",
            })?;

        let session_id = &context.session_id;

        match action.as_str() {
            "add" => {
                let text =
                    call.arguments
                        .get("text")
                        .ok_or_else(|| ToolError::MissingArgument {
                            tool: call.name.clone(),
                            argument: "text",
                        })?;

                let mut lists = TODO_LISTS.lock().map_err(|_| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: "todo list lock poisoned".to_owned(),
                })?;
                let list = lists.entry(session_id.clone()).or_default();
                let id = list.len() + 1;
                list.push(TodoItem {
                    id,
                    text: text.clone(),
                    status: TodoStatus::Pending,
                });

                Ok(ToolOutput {
                    content: format!("added todo #{id}: {text}"),
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("id".to_owned(), id.to_string()),
                    ]),
                })
            }
            "update" => {
                let id_str =
                    call.arguments
                        .get("id")
                        .ok_or_else(|| ToolError::MissingArgument {
                            tool: call.name.clone(),
                            argument: "id",
                        })?;
                let id: usize = id_str.parse().map_err(|_| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("invalid id `{id_str}`: expected a number"),
                })?;
                let status_str =
                    call.arguments
                        .get("status")
                        .ok_or_else(|| ToolError::MissingArgument {
                            tool: call.name.clone(),
                            argument: "status",
                        })?;
                let status = parse_status(status_str).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: e,
                })?;

                let mut lists = TODO_LISTS.lock().map_err(|_| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: "todo list lock poisoned".to_owned(),
                })?;
                let list = lists.entry(session_id.clone()).or_default();
                let item = list.iter_mut().find(|item| item.id == id).ok_or_else(|| {
                    ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: format!("no todo with id {id}"),
                    }
                })?;
                item.status = status;

                Ok(ToolOutput {
                    content: format!("todo #{id} → {}", item.status),
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("id".to_owned(), id.to_string()),
                    ]),
                })
            }
            "list" => {
                let lists = TODO_LISTS.lock().map_err(|_| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: "todo list lock poisoned".to_owned(),
                })?;
                let empty = Vec::new();
                let list = lists.get(session_id).unwrap_or(&empty);
                if list.is_empty() {
                    return Ok(ToolOutput {
                        content: "(no todos)".to_owned(),
                        metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
                    });
                }

                let lines: Vec<String> = list
                    .iter()
                    .map(|item| {
                        let marker = match item.status {
                            TodoStatus::Pending => "[ ]",
                            TodoStatus::InProgress => "[~]",
                            TodoStatus::Done => "[x]",
                        };
                        format!("#{} {} {}", item.id, marker, item.text)
                    })
                    .collect();

                let (mut pending, mut in_progress, mut done) = (0, 0, 0);
                for item in list {
                    match item.status {
                        TodoStatus::Pending => pending += 1,
                        TodoStatus::InProgress => in_progress += 1,
                        TodoStatus::Done => done += 1,
                    }
                }

                let summary =
                    format!("\n({pending} pending, {in_progress} in progress, {done} done)");

                Ok(ToolOutput {
                    content: format!("{}{summary}", lines.join("\n")),
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("count".to_owned(), list.len().to_string()),
                    ]),
                })
            }
            "clear" => {
                let mut lists = TODO_LISTS.lock().map_err(|_| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: "todo list lock poisoned".to_owned(),
                })?;
                let list = lists.entry(session_id.clone()).or_default();
                let count = list.len();
                list.clear();

                Ok(ToolOutput {
                    content: format!("cleared {count} todo(s)"),
                    metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
                })
            }
            other => Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("unknown action `{other}`: expected add, update, list, or clear"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx_destructive()
    }

    fn ctx_with_session(session_id: &str) -> ToolContext {
        ToolContext {
            session_id: session_id.to_owned(),
            ..crate::test_utils::test_ctx_destructive()
        }
    }

    fn clear_todos(session_id: &str) {
        let mut lists = TODO_LISTS.lock().unwrap();
        lists.remove(session_id);
    }

    #[test]
    fn todo_add_and_list() {
        clear_todos("test");
        let tool = TodoTool;

        // Add
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "write tests".to_owned()),
            ]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("write tests"));

        // List
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("[ ] write tests"));
        assert!(output.content.contains("1 pending"));
    }

    #[test]
    fn todo_update_status() {
        clear_todos("test");
        let tool = TodoTool;

        // Add
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "implement feature".to_owned()),
            ]),
        };
        tool.run(&call, &ctx()).unwrap();

        // Get the ID from the list
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).unwrap();
        // Extract ID from "#N [ ] text"
        let id = output
            .content
            .lines()
            .next()
            .unwrap()
            .trim_start_matches('#')
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();

        // Update to done
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "update".to_owned()),
                ("id".to_owned(), id),
                ("status".to_owned(), "done".to_owned()),
            ]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("done"));

        // Verify in list
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).unwrap();
        assert!(output.content.contains("[x]"));
        assert!(output.content.contains("1 done"));
    }

    #[test]
    fn todo_clear() {
        clear_todos("test");
        let tool = TodoTool;

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "item".to_owned()),
            ]),
        };
        tool.run(&call, &ctx()).unwrap();

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "clear".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("cleared 1"));

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).unwrap();
        assert_eq!(output.content, "(no todos)");
    }

    #[test]
    fn todo_empty_list() {
        clear_todos("test");
        let tool = TodoTool;

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.content, "(no todos)");
    }

    #[test]
    fn todo_invalid_action() {
        let tool = TodoTool;
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "delete".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("unknown action"));
            }
            _ => panic!("expected ExecutionFailed"),
        }
    }

    #[test]
    fn todo_sessions_are_isolated() {
        clear_todos("session-a");
        clear_todos("session-b");
        let tool = TodoTool;

        // Add to session A
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "task for A".to_owned()),
            ]),
        };
        tool.run(&call, &ctx_with_session("session-a")).unwrap();

        // Add to session B
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "task for B".to_owned()),
            ]),
        };
        tool.run(&call, &ctx_with_session("session-b")).unwrap();

        // Session A should only see its own todo
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output_a = tool.run(&call, &ctx_with_session("session-a")).unwrap();
        assert!(output_a.content.contains("task for A"));
        assert!(!output_a.content.contains("task for B"));

        // Session B should only see its own todo
        let output_b = tool.run(&call, &ctx_with_session("session-b")).unwrap();
        assert!(output_b.content.contains("task for B"));
        assert!(!output_b.content.contains("task for A"));

        // Clearing session A should not affect session B
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "clear".to_owned())]),
        };
        tool.run(&call, &ctx_with_session("session-a")).unwrap();

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output_b = tool.run(&call, &ctx_with_session("session-b")).unwrap();
        assert!(output_b.content.contains("task for B"));
    }

    #[test]
    fn todo_requires_action() {
        let tool = TodoTool;
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn todo_add_requires_text() {
        let tool = TodoTool;
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "add".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn todo_update_requires_id() {
        let tool = TodoTool;
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "update".to_owned()),
                ("status".to_owned(), "done".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn todo_update_requires_status() {
        let tool = TodoTool;
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "update".to_owned()),
                ("id".to_owned(), "1".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn todo_update_rejects_invalid_id() {
        let tool = TodoTool;
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "update".to_owned()),
                ("id".to_owned(), "abc".to_owned()),
                ("status".to_owned(), "done".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn todo_update_rejects_invalid_status() {
        clear_todos("test");
        let tool = TodoTool;
        // Add an item first
        let add = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "item".to_owned()),
            ]),
        };
        tool.run(&add, &ctx()).unwrap();

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "update".to_owned()),
                ("id".to_owned(), "1".to_owned()),
                ("status".to_owned(), "completed".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("invalid status"));
            }
            _ => panic!("expected ExecutionFailed"),
        }
    }

    #[test]
    fn todo_update_nonexistent_id() {
        clear_todos("test");
        let tool = TodoTool;
        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "update".to_owned()),
                ("id".to_owned(), "999".to_owned()),
                ("status".to_owned(), "done".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("no todo with id"));
            }
            _ => panic!("expected ExecutionFailed"),
        }
    }

    #[test]
    fn todo_in_progress_marker() {
        clear_todos("test");
        let tool = TodoTool;

        let add = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "working on it".to_owned()),
            ]),
        };
        tool.run(&add, &ctx()).unwrap();

        let update = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "update".to_owned()),
                ("id".to_owned(), "1".to_owned()),
                ("status".to_owned(), "in_progress".to_owned()),
            ]),
        };
        tool.run(&update, &ctx()).unwrap();

        let list = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output = tool.run(&list, &ctx()).unwrap();
        assert!(output.content.contains("[~]"));
        assert!(output.content.contains("1 in progress"));
    }

    #[test]
    fn todo_clear_empty_list() {
        clear_todos("test");
        let tool = TodoTool;

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "clear".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("cleared 0"));
    }

    #[test]
    fn todo_add_returns_id_in_metadata() {
        clear_todos("test");
        let tool = TodoTool;

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "first".to_owned()),
            ]),
        };
        let output = tool.run(&call, &ctx()).unwrap();
        assert_eq!(output.metadata.get("id").unwrap(), "1");

        let call = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([
                ("action".to_owned(), "add".to_owned()),
                ("text".to_owned(), "second".to_owned()),
            ]),
        };
        let output = tool.run(&call, &ctx()).unwrap();
        assert_eq!(output.metadata.get("id").unwrap(), "2");
    }

    #[test]
    fn todo_list_summary_counts() {
        clear_todos("test");
        let tool = TodoTool;

        // Add three items with mixed statuses
        for text in &["task a", "task b", "task c"] {
            let call = ToolCall {
                name: "todo".to_owned(),
                arguments: BTreeMap::from([
                    ("action".to_owned(), "add".to_owned()),
                    ("text".to_owned(), (*text).to_owned()),
                ]),
            };
            tool.run(&call, &ctx()).unwrap();
        }

        // Mark #1 as done, #2 as in_progress
        for (id, status) in &[("1", "done"), ("2", "in_progress")] {
            let call = ToolCall {
                name: "todo".to_owned(),
                arguments: BTreeMap::from([
                    ("action".to_owned(), "update".to_owned()),
                    ("id".to_owned(), (*id).to_owned()),
                    ("status".to_owned(), (*status).to_owned()),
                ]),
            };
            tool.run(&call, &ctx()).unwrap();
        }

        let list = ToolCall {
            name: "todo".to_owned(),
            arguments: BTreeMap::from([("action".to_owned(), "list".to_owned())]),
        };
        let output = tool.run(&list, &ctx()).unwrap();
        assert!(output.content.contains("1 pending"));
        assert!(output.content.contains("1 in progress"));
        assert!(output.content.contains("1 done"));
        assert_eq!(output.metadata.get("count").unwrap(), "3");
    }
}
