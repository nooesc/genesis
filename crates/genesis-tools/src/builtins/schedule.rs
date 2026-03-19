use std::collections::BTreeMap;

use genesis_storage::ScheduleStore;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Creates a new scheduled prompt.
pub struct ScheduleCreateTool;

impl ToolHandler for ScheduleCreateTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let cron = call
            .arguments
            .get("cron")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "cron",
            })?;

        let prompt = call
            .arguments
            .get("prompt")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "prompt",
            })?;

        let destination = call
            .arguments
            .get("destination")
            .map(|s| s.as_str())
            .unwrap_or("cli");

        // Validate the cron expression before creating
        if cron.split_whitespace().count() != 5 {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "invalid cron expression '{cron}': expected 5 fields (minute hour day month weekday)"
                ),
            });
        }

        let id = format!("sched-{}", &uuid_v4()[..8]);
        let store = ScheduleStore::new(&context.db_path());
        let schedule = store.create(&id, cron, destination, prompt).map_err(|e| {
            ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to create schedule: {e}"),
            }
        })?;

        Ok(ToolOutput {
            content: format!(
                "Created schedule '{}': runs '{}' with cron '{}'",
                schedule.id, schedule.prompt, schedule.cron_expression
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("schedule_id".to_owned(), schedule.id),
            ]),
        })
    }
}

/// Lists all scheduled prompts.
pub struct ScheduleListTool;

impl ToolHandler for ScheduleListTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let store = ScheduleStore::new(&context.db_path());
        let schedules = store.list_all().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to list schedules: {e}"),
        })?;

        if schedules.is_empty() {
            return Ok(ToolOutput {
                content: "No schedules configured.".to_owned(),
                metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
            });
        }

        let mut content = format!("{} schedule(s):\n", schedules.len());
        for s in &schedules {
            let status = if s.enabled { "enabled" } else { "disabled" };
            content.push_str(&format!(
                "\n- [{}] {} | cron: {} | dest: {} | prompt: {}",
                status, s.id, s.cron_expression, s.destination, s.prompt
            ));
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("count".to_owned(), schedules.len().to_string()),
            ]),
        })
    }
}

/// Deletes a scheduled prompt by ID.
pub struct ScheduleDeleteTool;

impl ToolHandler for ScheduleDeleteTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = call
            .arguments
            .get("id")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "id",
            })?;

        let store = ScheduleStore::new(&context.db_path());
        let deleted = store.delete(id).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to delete schedule: {e}"),
        })?;

        if deleted {
            Ok(ToolOutput {
                content: format!("Deleted schedule '{id}'."),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), call.name.clone()),
                    ("deleted_id".to_owned(), id.clone()),
                ]),
            })
        } else {
            Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("schedule '{id}' not found"),
            })
        }
    }
}

/// Generate a simple UUID-like string for schedule IDs.
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    format!("{:016x}{:08x}", nanos, pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use tempfile::tempdir;

    fn ctx_with_dir(dir: &std::path::Path) -> ToolContext {
        // Bootstrap the database
        genesis_storage::bootstrap(&dir.join("genesis.db")).expect("bootstrap");
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: dir.to_string_lossy().to_string(),
            allow_destructive_tools: true,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
            approval_mode: genesis_config::ApprovalMode::Auto,
        }
    }

    #[test]
    fn create_schedule_works() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(dir.path());

        let tool = ScheduleCreateTool;
        let call = ToolCall {
            name: "schedule_create".to_owned(),
            arguments: BTreeMap::from([
                ("cron".to_owned(), "*/5 * * * *".to_owned()),
                ("prompt".to_owned(), "check status".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx).expect("should succeed");
        assert!(output.content.contains("Created schedule"));
        assert!(output.content.contains("check status"));
        assert!(output.metadata.contains_key("schedule_id"));
    }

    #[test]
    fn create_schedule_validates_cron() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(dir.path());

        let tool = ScheduleCreateTool;
        let call = ToolCall {
            name: "schedule_create".to_owned(),
            arguments: BTreeMap::from([
                ("cron".to_owned(), "bad".to_owned()),
                ("prompt".to_owned(), "test".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn list_schedules_empty() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(dir.path());

        let tool = ScheduleListTool;
        let call = ToolCall {
            name: "schedule_list".to_owned(),
            arguments: BTreeMap::new(),
        };

        let output = tool.run(&call, &ctx).expect("should succeed");
        assert!(output.content.contains("No schedules"));
    }

    #[test]
    fn create_then_list_then_delete() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(dir.path());

        // Create
        let create = ScheduleCreateTool;
        let call = ToolCall {
            name: "schedule_create".to_owned(),
            arguments: BTreeMap::from([
                ("cron".to_owned(), "0 9 * * *".to_owned()),
                ("prompt".to_owned(), "morning report".to_owned()),
            ]),
        };
        let output = create.run(&call, &ctx).expect("create should succeed");
        let schedule_id = output.metadata.get("schedule_id").unwrap().clone();

        // List
        let list = ScheduleListTool;
        let call = ToolCall {
            name: "schedule_list".to_owned(),
            arguments: BTreeMap::new(),
        };
        let output = list.run(&call, &ctx).expect("list should succeed");
        assert!(output.content.contains("1 schedule(s)"));
        assert!(output.content.contains("morning report"));

        // Delete
        let delete = ScheduleDeleteTool;
        let call = ToolCall {
            name: "schedule_delete".to_owned(),
            arguments: BTreeMap::from([("id".to_owned(), schedule_id)]),
        };
        let output = delete.run(&call, &ctx).expect("delete should succeed");
        assert!(output.content.contains("Deleted"));

        // Verify empty
        let call = ToolCall {
            name: "schedule_list".to_owned(),
            arguments: BTreeMap::new(),
        };
        let output = list.run(&call, &ctx).expect("list should succeed");
        assert!(output.content.contains("No schedules"));
    }

    #[test]
    fn delete_nonexistent_schedule_errors() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(dir.path());

        let tool = ScheduleDeleteTool;
        let call = ToolCall {
            name: "schedule_delete".to_owned(),
            arguments: BTreeMap::from([("id".to_owned(), "nonexistent".to_owned())]),
        };

        let err = tool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn create_with_custom_destination() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(dir.path());

        let tool = ScheduleCreateTool;
        let call = ToolCall {
            name: "schedule_create".to_owned(),
            arguments: BTreeMap::from([
                ("cron".to_owned(), "*/10 * * * *".to_owned()),
                ("prompt".to_owned(), "heartbeat".to_owned()),
                ("destination".to_owned(), "telegram".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx).expect("should succeed");
        assert!(output.content.contains("Created schedule"));
    }
}
