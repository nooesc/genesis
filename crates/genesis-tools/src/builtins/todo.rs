use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// In-memory task list for agent planning.
///
/// Unlike persistent storage tools, the todo list lives only for the duration
/// of the current process. It helps the agent decompose complex tasks,
/// track progress, and report completion status.

static TODO_LIST: Mutex<Vec<TodoItem>> = Mutex::new(Vec::new());

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
        other => Err(format!("invalid status `{other}`: expected pending, in_progress, or done")),
    }
}

pub struct TodoTool;

impl ToolHandler for TodoTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let action = call
            .arguments
            .get("action")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "action",
            })?;

        match action.as_str() {
            "add" => {
                let text = call
                    .arguments
                    .get("text")
                    .ok_or_else(|| ToolError::MissingArgument {
                        tool: call.name.clone(),
                        argument: "text",
                    })?;

                let mut list = TODO_LIST.lock().unwrap();
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
                let id_str = call
                    .arguments
                    .get("id")
                    .ok_or_else(|| ToolError::MissingArgument {
                        tool: call.name.clone(),
                        argument: "id",
                    })?;
                let id: usize = id_str.parse().map_err(|_| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("invalid id `{id_str}`: expected a number"),
                })?;
                let status_str = call
                    .arguments
                    .get("status")
                    .ok_or_else(|| ToolError::MissingArgument {
                        tool: call.name.clone(),
                        argument: "status",
                    })?;
                let status = parse_status(status_str).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: e,
                })?;

                let mut list = TODO_LIST.lock().unwrap();
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
                let list = TODO_LIST.lock().unwrap();
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

                let pending = list.iter().filter(|i| i.status == TodoStatus::Pending).count();
                let in_progress = list.iter().filter(|i| i.status == TodoStatus::InProgress).count();
                let done = list.iter().filter(|i| i.status == TodoStatus::Done).count();

                let summary = format!("\n({pending} pending, {in_progress} in progress, {done} done)");

                Ok(ToolOutput {
                    content: format!("{}{summary}", lines.join("\n")),
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("count".to_owned(), list.len().to_string()),
                    ]),
                })
            }
            "clear" => {
                let mut list = TODO_LIST.lock().unwrap();
                let count = list.len();
                list.clear();

                Ok(ToolOutput {
                    content: format!("cleared {count} todo(s)"),
                    metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
                })
            }
            other => Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "unknown action `{other}`: expected add, update, list, or clear"
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
            terminal_backend: None,
            default_working_dir: None,
        }
    }

    fn clear_todos() {
        TODO_LIST.lock().unwrap().clear();
    }

    #[test]
    fn todo_add_and_list() {
        clear_todos();
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
        clear_todos();
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
        let id = output.content.lines().next().unwrap()
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
        clear_todos();
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
        clear_todos();
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
}
