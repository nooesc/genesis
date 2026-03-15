use std::collections::BTreeMap;

use genesis_storage::{bootstrap, SkillStore, SkillUsageStore};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that creates or updates a skill in persistent storage.
pub struct SkillCreateTool;

impl ToolHandler for SkillCreateTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let name = call
            .arguments
            .get("name")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "name",
            })?;

        let description = call
            .arguments
            .get("description")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "description",
            })?;

        let instructions = call
            .arguments
            .get("instructions")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "instructions",
            })?;

        let trigger_hint = call.arguments.get("trigger_hint").map(|s| s.as_str());

        let tags: Vec<&str> = call
            .arguments
            .get("tags")
            .map(|t| t.split(',').map(|s| s.trim()).collect())
            .unwrap_or_default();

        let db_path = context.db_path();
        bootstrap(&db_path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("database initialization failed: {e}"),
        })?;
        let store = SkillStore::new(&db_path);

        let skill = store
            .upsert(name, description, instructions, trigger_hint, &tags)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to save skill: {e}"),
            })?;

        Ok(ToolOutput {
            content: format!(
                "Skill `{}` saved (version {}). Tags: [{}]",
                skill.name,
                skill.version,
                skill.tags.join(", ")
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("skill_name".to_owned(), skill.name),
                ("version".to_owned(), skill.version.to_string()),
            ]),
        })
    }
}

/// Tool that lists all available skills.
pub struct SkillListTool;

impl ToolHandler for SkillListTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let db_path = context.db_path();
        let store = SkillStore::new(&db_path);

        let skills = store.list_all().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to list skills: {e}"),
        })?;

        if skills.is_empty() {
            return Ok(ToolOutput {
                content: "No skills saved yet.".to_owned(),
                metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
            });
        }

        let mut lines = Vec::new();
        for skill in &skills {
            let tags = if skill.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", skill.tags.join(", "))
            };
            lines.push(format!(
                "- {} (v{}){}: {}",
                skill.name, skill.version, tags, skill.description
            ));
        }

        Ok(ToolOutput {
            content: lines.join("\n"),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("count".to_owned(), skills.len().to_string()),
            ]),
        })
    }
}

/// Tool that retrieves a specific skill's full instructions.
pub struct SkillGetTool;

impl ToolHandler for SkillGetTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let name = call
            .arguments
            .get("name")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "name",
            })?;

        let db_path = context.db_path();
        let store = SkillStore::new(&db_path);

        let skill = store
            .get(name)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to get skill: {e}"),
            })?
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("skill `{name}` not found"),
            })?;

        let mut content = format!("# {}\n\n", skill.name);
        content.push_str(&format!("**Description:** {}\n", skill.description));
        if let Some(trigger) = &skill.trigger_hint {
            content.push_str(&format!("**Trigger:** {trigger}\n"));
        }
        if !skill.tags.is_empty() {
            content.push_str(&format!("**Tags:** {}\n", skill.tags.join(", ")));
        }
        content.push_str(&format!("**Version:** {}\n\n", skill.version));
        content.push_str("## Instructions\n\n");
        content.push_str(&skill.instructions);

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("skill_name".to_owned(), skill.name),
            ]),
        })
    }
}

/// Tool that deletes a skill.
pub struct SkillDeleteTool;

impl ToolHandler for SkillDeleteTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let name = call
            .arguments
            .get("name")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "name",
            })?;

        let db_path = context.db_path();
        let store = SkillStore::new(&db_path);

        let deleted = store.delete(name).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to delete skill: {e}"),
        })?;

        let content = if deleted {
            format!("Skill `{name}` deleted.")
        } else {
            format!("Skill `{name}` not found.")
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
        })
    }
}

/// Tool that records a skill usage and optional feedback for self-improvement.
///
/// After using a skill, the agent calls this to record how effective it was.
/// If the outcome reveals ways to improve, the agent can then call `skill_create`
/// to update the skill's instructions (the version auto-increments on upsert).
pub struct SkillRecordUsageTool;

impl ToolHandler for SkillRecordUsageTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let skill_name = call
            .arguments
            .get("skill_name")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "skill_name",
            })?;

        let outcome = call
            .arguments
            .get("outcome")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "outcome",
            })?;

        // Validate outcome
        if !["success", "partial", "failure", "unknown"].contains(&outcome.as_str()) {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "outcome must be one of: success, partial, failure, unknown — got `{outcome}`"
                ),
            });
        }

        let feedback = call.arguments.get("feedback").map(|s| s.as_str());

        let db_path = context.db_path();
        bootstrap(&db_path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("database initialization failed: {e}"),
        })?;

        // Verify the skill exists
        let skill_store = SkillStore::new(&db_path);
        let skill = skill_store
            .get(skill_name)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to look up skill: {e}"),
            })?
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("skill `{skill_name}` not found"),
            })?;

        let usage_store = SkillUsageStore::new(&db_path);
        let usage = usage_store
            .record_usage(skill_name, Some(&context.session_id), outcome, feedback)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to record usage: {e}"),
            })?;

        // Get current stats for the response
        let stats = usage_store
            .stats(skill_name)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to get stats: {e}"),
            })?;

        let mut content = format!(
            "Recorded usage of `{}` (v{}) — outcome: {}\n",
            skill_name, skill.version, outcome
        );
        if let Some(ref fb) = usage.feedback {
            content.push_str(&format!("Feedback: {fb}\n"));
        }
        content.push_str(&format!(
            "Lifetime stats: {} uses ({} success, {} failure, {} refined)",
            stats.total_uses, stats.successes, stats.failures, stats.times_refined
        ));

        // If outcome is not success, suggest refinement
        if outcome != "success" {
            content.push_str(
                "\n\nConsider refining this skill with `skill_create` to update its instructions based on what you learned.",
            );
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("skill_name".to_owned(), skill_name.clone()),
                ("usage_id".to_owned(), usage.id.to_string()),
                ("outcome".to_owned(), outcome.clone()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn skill_create_and_list_round_trip() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        // Create
        let create_call = ToolCall {
            name: "skill_create".to_owned(),
            arguments: BTreeMap::from([
                ("name".to_owned(), "code_review".to_owned()),
                ("description".to_owned(), "Review code for issues".to_owned()),
                ("instructions".to_owned(), "1. Read code\n2. Find bugs".to_owned()),
                ("trigger_hint".to_owned(), "review code".to_owned()),
                ("tags".to_owned(), "dev, review".to_owned()),
            ]),
        };
        let output = SkillCreateTool.run(&create_call, &ctx).expect("create should work");
        assert!(output.content.contains("code_review"));
        assert!(output.content.contains("version 1"));

        // List
        let list_call = ToolCall {
            name: "skill_list".to_owned(),
            arguments: BTreeMap::new(),
        };
        let output = SkillListTool.run(&list_call, &ctx).expect("list should work");
        assert!(output.content.contains("code_review"));
        assert!(output.content.contains("Review code for issues"));
    }

    #[test]
    fn skill_get_returns_full_instructions() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let create_call = ToolCall {
            name: "skill_create".to_owned(),
            arguments: BTreeMap::from([
                ("name".to_owned(), "deploy".to_owned()),
                ("description".to_owned(), "Deploy app".to_owned()),
                ("instructions".to_owned(), "Run kubectl apply".to_owned()),
            ]),
        };
        SkillCreateTool.run(&create_call, &ctx).expect("create");

        let get_call = ToolCall {
            name: "skill_get".to_owned(),
            arguments: BTreeMap::from([("name".to_owned(), "deploy".to_owned())]),
        };
        let output = SkillGetTool.run(&get_call, &ctx).expect("get");
        assert!(output.content.contains("# deploy"));
        assert!(output.content.contains("Run kubectl apply"));
    }

    #[test]
    fn skill_delete_removes_skill() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let create_call = ToolCall {
            name: "skill_create".to_owned(),
            arguments: BTreeMap::from([
                ("name".to_owned(), "temp_skill".to_owned()),
                ("description".to_owned(), "Temporary".to_owned()),
                ("instructions".to_owned(), "Do nothing".to_owned()),
            ]),
        };
        SkillCreateTool.run(&create_call, &ctx).expect("create");

        let delete_call = ToolCall {
            name: "skill_delete".to_owned(),
            arguments: BTreeMap::from([("name".to_owned(), "temp_skill".to_owned())]),
        };
        let output = SkillDeleteTool.run(&delete_call, &ctx).expect("delete");
        assert!(output.content.contains("deleted"));

        // Verify it's gone
        let list_call = ToolCall {
            name: "skill_list".to_owned(),
            arguments: BTreeMap::new(),
        };
        let output = SkillListTool.run(&list_call, &ctx).expect("list");
        assert!(output.content.contains("No skills saved"));
    }

    #[test]
    fn skill_create_requires_name() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "skill_create".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = SkillCreateTool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn skill_get_errors_on_missing_skill() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "skill_get".to_owned(),
            arguments: BTreeMap::from([("name".to_owned(), "nonexistent".to_owned())]),
        };
        let err = SkillGetTool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn skill_list_empty_returns_message() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "skill_list".to_owned(),
            arguments: BTreeMap::new(),
        };
        let output = SkillListTool.run(&call, &ctx).expect("list");
        assert!(output.content.contains("No skills saved"));
    }

    #[test]
    fn skill_record_usage_tracks_outcome() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        // Create a skill first
        let create_call = ToolCall {
            name: "skill_create".to_owned(),
            arguments: BTreeMap::from([
                ("name".to_owned(), "deploy".to_owned()),
                ("description".to_owned(), "Deploy app".to_owned()),
                ("instructions".to_owned(), "Run deploy".to_owned()),
            ]),
        };
        SkillCreateTool.run(&create_call, &ctx).expect("create");

        // Record usage
        let usage_call = ToolCall {
            name: "skill_record_usage".to_owned(),
            arguments: BTreeMap::from([
                ("skill_name".to_owned(), "deploy".to_owned()),
                ("outcome".to_owned(), "success".to_owned()),
                ("feedback".to_owned(), "Deployed correctly".to_owned()),
            ]),
        };
        let output = SkillRecordUsageTool.run(&usage_call, &ctx).expect("record");
        assert!(output.content.contains("deploy"));
        assert!(output.content.contains("success"));
        assert!(output.content.contains("1 uses"));
    }

    #[test]
    fn skill_record_usage_suggests_refinement_on_failure() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let create_call = ToolCall {
            name: "skill_create".to_owned(),
            arguments: BTreeMap::from([
                ("name".to_owned(), "buggy_skill".to_owned()),
                ("description".to_owned(), "Buggy".to_owned()),
                ("instructions".to_owned(), "Wrong steps".to_owned()),
            ]),
        };
        SkillCreateTool.run(&create_call, &ctx).expect("create");

        let usage_call = ToolCall {
            name: "skill_record_usage".to_owned(),
            arguments: BTreeMap::from([
                ("skill_name".to_owned(), "buggy_skill".to_owned()),
                ("outcome".to_owned(), "failure".to_owned()),
                ("feedback".to_owned(), "Steps were wrong".to_owned()),
            ]),
        };
        let output = SkillRecordUsageTool.run(&usage_call, &ctx).expect("record");
        assert!(output.content.contains("refining"));
    }

    #[test]
    fn skill_record_usage_validates_outcome() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let create_call = ToolCall {
            name: "skill_create".to_owned(),
            arguments: BTreeMap::from([
                ("name".to_owned(), "test".to_owned()),
                ("description".to_owned(), "Test".to_owned()),
                ("instructions".to_owned(), "Test".to_owned()),
            ]),
        };
        SkillCreateTool.run(&create_call, &ctx).expect("create");

        let usage_call = ToolCall {
            name: "skill_record_usage".to_owned(),
            arguments: BTreeMap::from([
                ("skill_name".to_owned(), "test".to_owned()),
                ("outcome".to_owned(), "invalid_value".to_owned()),
            ]),
        };
        let err = SkillRecordUsageTool.run(&usage_call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn skill_record_usage_errors_on_missing_skill() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let usage_call = ToolCall {
            name: "skill_record_usage".to_owned(),
            arguments: BTreeMap::from([
                ("skill_name".to_owned(), "nonexistent".to_owned()),
                ("outcome".to_owned(), "success".to_owned()),
            ]),
        };
        let err = SkillRecordUsageTool.run(&usage_call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }
}
