use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use genesis_storage::{bootstrap, SessionStore, SubagentStore};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

static SUBAGENT_COUNTER: AtomicU64 = AtomicU64::new(1);

fn generate_subagent_id(parent_session_id: &str) -> String {
    let seq = SUBAGENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("sub-{parent_session_id}-{seq}")
}

fn generate_child_session_id(parent_session_id: &str) -> String {
    let seq = SUBAGENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("child-{parent_session_id}-{seq}")
}

/// Tool that spawns a subagent to work on a task concurrently.
///
/// This creates the subagent record and child session in the database.
/// The actual agent loop spawning is handled by the agent loop layer,
/// which detects the `__subagent_spawn` metadata key in the output.
pub struct SpawnSubagentTool;

impl ToolHandler for SpawnSubagentTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let task = call
            .arguments
            .get("task")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "task",
            })?;

        let name = call
            .arguments
            .get("name")
            .cloned()
            .unwrap_or_else(|| "subagent".to_owned());

        let db_path = std::path::Path::new(&context.data_dir).join("genesis.db");
        let _ = bootstrap(&db_path);

        let subagent_id = generate_subagent_id(&context.session_id);
        let child_session_id = generate_child_session_id(&context.session_id);

        // Create the child session
        let session_store = SessionStore::new(&db_path);
        session_store
            .create_session(
                &child_session_id,
                "subagent",
                Some(&format!("Subagent: {name}")),
            )
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to create child session: {e}"),
            })?;

        // Create the subagent record
        let subagent_store = SubagentStore::new(&db_path);
        subagent_store
            .create(&subagent_id, &context.session_id, &child_session_id, &name, task)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to create subagent record: {e}"),
            })?;

        Ok(ToolOutput {
            content: format!(
                "Subagent `{name}` spawned with ID `{subagent_id}`. \
                 It will work on the task concurrently. Use `check_subagent` with this ID to check progress."
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("__subagent_spawn".to_owned(), "true".to_owned()),
                ("subagent_id".to_owned(), subagent_id),
                ("child_session_id".to_owned(), child_session_id),
                ("task".to_owned(), task.clone()),
            ]),
        })
    }
}

/// Tool that checks the status and result of a spawned subagent.
pub struct CheckSubagentTool;

impl ToolHandler for CheckSubagentTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let subagent_id = call
            .arguments
            .get("subagent_id")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "subagent_id",
            })?;

        let db_path = std::path::Path::new(&context.data_dir).join("genesis.db");
        let store = SubagentStore::new(&db_path);

        let subagent = store
            .get(subagent_id)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to query subagent: {e}"),
            })?
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("subagent `{subagent_id}` not found"),
            })?;

        let mut content = format!(
            "Subagent: {}\nStatus: {}\nTask: {}",
            subagent.name, subagent.status, subagent.task,
        );

        if let Some(result) = &subagent.result {
            content.push_str(&format!("\nResult: {result}"));
        }
        if let Some(error) = &subagent.error {
            content.push_str(&format!("\nError: {error}"));
        }
        if let Some(completed_at) = &subagent.completed_at {
            content.push_str(&format!("\nCompleted at: {completed_at}"));
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("status".to_owned(), subagent.status),
            ]),
        })
    }
}

/// Tool that lists all subagents for the current session.
pub struct ListSubagentsTool;

impl ToolHandler for ListSubagentsTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let db_path = std::path::Path::new(&context.data_dir).join("genesis.db");
        let store = SubagentStore::new(&db_path);

        let subagents = store
            .list_by_parent(&context.session_id)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to list subagents: {e}"),
            })?;

        if subagents.is_empty() {
            return Ok(ToolOutput {
                content: "No subagents spawned in this session.".to_owned(),
                metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
            });
        }

        let mut lines = vec![format!("{} subagent(s):", subagents.len())];
        for s in &subagents {
            let status_detail = match s.status.as_str() {
                "completed" => s
                    .result
                    .as_deref()
                    .map(|r| {
                        let truncated: String = r.chars().take(100).collect();
                        format!(" — {truncated}")
                    })
                    .unwrap_or_default(),
                "failed" => s
                    .error
                    .as_deref()
                    .map(|e| format!(" — {e}"))
                    .unwrap_or_default(),
                _ => String::new(),
            };
            lines.push(format!(
                "- [{}] {} ({}){status_detail}",
                s.status, s.name, s.id,
            ));
        }

        Ok(ToolOutput {
            content: lines.join("\n"),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("count".to_owned(), subagents.len().to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_storage::bootstrap;
    use tempfile::tempdir;

    fn ctx_with_dir(data_dir: &str) -> ToolContext {
        ToolContext {
            session_id: "parent-session".to_owned(),
            profile: "test".to_owned(),
            data_dir: data_dir.to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
        }
    }

    #[test]
    fn spawn_creates_subagent_record() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        // Create parent session first
        SessionStore::new(&db_path)
            .create_session("parent-session", "cli", None)
            .expect("parent session");

        let call = ToolCall {
            name: "spawn_subagent".to_owned(),
            arguments: BTreeMap::from([
                ("task".to_owned(), "Research Rust async patterns".to_owned()),
                ("name".to_owned(), "researcher".to_owned()),
            ]),
        };

        let output = SpawnSubagentTool.run(&call, &ctx).expect("spawn");
        assert!(output.content.contains("researcher"));
        assert_eq!(
            output.metadata.get("__subagent_spawn").map(String::as_str),
            Some("true")
        );
        assert!(output.metadata.contains_key("subagent_id"));
        assert!(output.metadata.contains_key("child_session_id"));

        // Verify the subagent record exists
        let store = SubagentStore::new(&db_path);
        let subagent_id = output.metadata.get("subagent_id").unwrap();
        let record = store.get(subagent_id).unwrap().expect("should exist");
        assert_eq!(record.name, "researcher");
        assert_eq!(record.status, "pending");
        assert_eq!(record.task, "Research Rust async patterns");
    }

    #[test]
    fn check_returns_subagent_status() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        // Create parent session and subagent
        SessionStore::new(&db_path)
            .create_session("parent-session", "cli", None)
            .expect("parent session");
        SessionStore::new(&db_path)
            .create_session("child-1", "subagent", None)
            .expect("child session");

        let store = SubagentStore::new(&db_path);
        store
            .create("sub-1", "parent-session", "child-1", "worker", "do work")
            .expect("create");
        store.set_completed("sub-1", "All done!").expect("complete");

        let call = ToolCall {
            name: "check_subagent".to_owned(),
            arguments: BTreeMap::from([("subagent_id".to_owned(), "sub-1".to_owned())]),
        };

        let output = CheckSubagentTool.run(&call, &ctx).expect("check");
        assert!(output.content.contains("completed"));
        assert!(output.content.contains("All done!"));
    }

    #[test]
    fn list_returns_all_session_subagents() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        SessionStore::new(&db_path)
            .create_session("parent-session", "cli", None)
            .expect("parent session");
        SessionStore::new(&db_path)
            .create_session("child-1", "subagent", None)
            .expect("child 1");
        SessionStore::new(&db_path)
            .create_session("child-2", "subagent", None)
            .expect("child 2");

        let store = SubagentStore::new(&db_path);
        store
            .create("sub-1", "parent-session", "child-1", "worker-a", "task a")
            .expect("create 1");
        store
            .create("sub-2", "parent-session", "child-2", "worker-b", "task b")
            .expect("create 2");

        let call = ToolCall {
            name: "list_subagents".to_owned(),
            arguments: BTreeMap::new(),
        };

        let output = ListSubagentsTool.run(&call, &ctx).expect("list");
        assert!(output.content.contains("2 subagent(s)"));
        assert!(output.content.contains("worker-a"));
        assert!(output.content.contains("worker-b"));
    }

    #[test]
    fn list_empty_returns_message() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "list_subagents".to_owned(),
            arguments: BTreeMap::new(),
        };

        let output = ListSubagentsTool.run(&call, &ctx).expect("list");
        assert!(output.content.contains("No subagents"));
    }

    #[test]
    fn spawn_requires_task() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "spawn_subagent".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = SpawnSubagentTool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }
}
