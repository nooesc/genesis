use std::collections::BTreeMap;

use genesis_storage::{bootstrap, SkillFileStore};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

fn file_store(context: &ToolContext, tool_name: &str) -> Result<SkillFileStore, ToolError> {
    let db_path = context.db_path();
    bootstrap(&db_path).map_err(|e| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!("database initialization failed: {e}"),
    })?;
    Ok(SkillFileStore::new(&db_path))
}

/// Tool that returns the content of a supporting file attached to a skill.
pub struct SkillViewFileTool;

impl ToolHandler for SkillViewFileTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let skill_name =
            call.arguments
                .get("skill_name")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "skill_name",
                })?;

        let file_path =
            call.arguments
                .get("file_path")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "file_path",
                })?;

        let store = file_store(context, &call.name)?;

        let content = store
            .get_file(skill_name, file_path)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to load skill file: {e}"),
            })?
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("supporting file `{file_path}` for skill `{skill_name}` not found"),
            })?;

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("skill_name".to_owned(), skill_name.clone()),
                ("file_path".to_owned(), file_path.clone()),
            ]),
        })
    }
}

/// Tool that stores a supporting file for a skill.
pub struct SkillStoreFileTool;

impl ToolHandler for SkillStoreFileTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let skill_name =
            call.arguments
                .get("skill_name")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "skill_name",
                })?;

        let file_path =
            call.arguments
                .get("file_path")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "file_path",
                })?;

        let content = call
            .arguments
            .get("content")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "content",
            })?;

        let store = file_store(context, &call.name)?;

        store
            .store_file(skill_name, file_path, content)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to store skill file: {e}"),
            })?;

        Ok(ToolOutput {
            content: format!("Stored file `{file_path}` for skill `{skill_name}`."),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("skill_name".to_owned(), skill_name.clone()),
                ("file_path".to_owned(), file_path.clone()),
            ]),
        })
    }
}

/// Tool that lists all supporting files for a skill.
pub struct SkillListFilesTool;

impl ToolHandler for SkillListFilesTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let skill_name =
            call.arguments
                .get("skill_name")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "skill_name",
                })?;

        let store = file_store(context, &call.name)?;

        let files = store
            .list_files(skill_name)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to list skill files: {e}"),
            })?;

        let content = if files.is_empty() {
            format!("No supporting files found for skill `{skill_name}`.")
        } else {
            let list = files
                .iter()
                .map(|f| format!("  - {f}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("Supporting files for `{skill_name}`:\n{list}")
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("skill_name".to_owned(), skill_name.clone()),
                ("file_count".to_owned(), files.len().to_string()),
            ]),
        })
    }
}

/// Tool that deletes a supporting file from a skill.
pub struct SkillDeleteFileTool;

impl ToolHandler for SkillDeleteFileTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let skill_name =
            call.arguments
                .get("skill_name")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "skill_name",
                })?;

        let file_path =
            call.arguments
                .get("file_path")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "file_path",
                })?;

        let store = file_store(context, &call.name)?;

        let deleted =
            store
                .delete_file(skill_name, file_path)
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to delete skill file: {e}"),
                })?;

        let content = if deleted {
            format!("Deleted file `{file_path}` from skill `{skill_name}`.")
        } else {
            format!("File `{file_path}` not found for skill `{skill_name}`.")
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("skill_name".to_owned(), skill_name.clone()),
                ("file_path".to_owned(), file_path.clone()),
                ("deleted".to_owned(), deleted.to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use genesis_storage::{bootstrap, SkillFileStore, SkillStore};
    use tempfile::tempdir;

    use super::*;
    use crate::ToolHandler;

    fn context(data_dir: &str) -> ToolContext {
        ToolContext {
            session_id: "s1".to_owned(),
            profile: "default".to_owned(),
            data_dir: data_dir.to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
            embedding_service: None,
            path_validator: None,
            approval_mode: genesis_config::ApprovalMode::Auto,
        }
    }

    #[test]
    fn skill_view_file_returns_content() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = SkillStore::new(&db_path);
        skill_store
            .upsert("deploy", "Deploy app", "Do deploy", None, &[])
            .unwrap();
        let file_store = SkillFileStore::new(&db_path);
        file_store
            .store_file("deploy", "references/api.md", "api docs")
            .unwrap();

        let tool = SkillViewFileTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "skill_view_file".to_owned(),
                    arguments: BTreeMap::from([
                        ("skill_name".to_owned(), "deploy".to_owned()),
                        ("file_path".to_owned(), "references/api.md".to_owned()),
                    ]),
                },
                &context(dir.path().to_string_lossy().as_ref()),
            )
            .expect("tool should succeed");

        assert_eq!(output.content, "api docs");
        assert_eq!(output.metadata.get("skill_name").unwrap(), "deploy");
    }

    #[test]
    fn skill_view_file_requires_skill_name() {
        let tool = SkillViewFileTool;
        let err = tool
            .run(
                &ToolCall {
                    name: "skill_view_file".to_owned(),
                    arguments: BTreeMap::from([(
                        "file_path".to_owned(),
                        "references/api.md".to_owned(),
                    )]),
                },
                &context("/tmp"),
            )
            .expect_err("missing skill name should error");

        assert_eq!(
            err,
            ToolError::MissingArgument {
                tool: "skill_view_file".to_owned(),
                argument: "skill_name",
            }
        );
    }

    #[test]
    fn skill_store_file_creates_file() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = SkillStore::new(&db_path);
        skill_store
            .upsert("deploy", "Deploy app", "Do deploy", None, &[])
            .unwrap();

        let tool = SkillStoreFileTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "skill_store_file".to_owned(),
                    arguments: BTreeMap::from([
                        ("skill_name".to_owned(), "deploy".to_owned()),
                        ("file_path".to_owned(), "docs/setup.md".to_owned()),
                        ("content".to_owned(), "# Setup\nInstall things.".to_owned()),
                    ]),
                },
                &context(dir.path().to_string_lossy().as_ref()),
            )
            .expect("tool should succeed");

        assert!(output.content.contains("Stored file"));

        // Verify the file was actually stored
        let file_store = SkillFileStore::new(&db_path);
        let content = file_store
            .get_file("deploy", "docs/setup.md")
            .unwrap()
            .unwrap();
        assert_eq!(content, "# Setup\nInstall things.");
    }

    #[test]
    fn skill_store_file_requires_content() {
        let tool = SkillStoreFileTool;
        let err = tool
            .run(
                &ToolCall {
                    name: "skill_store_file".to_owned(),
                    arguments: BTreeMap::from([
                        ("skill_name".to_owned(), "deploy".to_owned()),
                        ("file_path".to_owned(), "docs/setup.md".to_owned()),
                    ]),
                },
                &context("/tmp"),
            )
            .expect_err("missing content should error");

        assert_eq!(
            err,
            ToolError::MissingArgument {
                tool: "skill_store_file".to_owned(),
                argument: "content",
            }
        );
    }

    #[test]
    fn skill_list_files_returns_file_names() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = SkillStore::new(&db_path);
        skill_store
            .upsert("deploy", "Deploy app", "Do deploy", None, &[])
            .unwrap();
        let file_store = SkillFileStore::new(&db_path);
        file_store.store_file("deploy", "refs/a.md", "a").unwrap();
        file_store.store_file("deploy", "refs/b.md", "b").unwrap();

        let tool = SkillListFilesTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "skill_list_files".to_owned(),
                    arguments: BTreeMap::from([("skill_name".to_owned(), "deploy".to_owned())]),
                },
                &context(dir.path().to_string_lossy().as_ref()),
            )
            .expect("tool should succeed");

        assert!(output.content.contains("refs/a.md"));
        assert!(output.content.contains("refs/b.md"));
        assert_eq!(output.metadata.get("file_count").unwrap(), "2");
    }

    #[test]
    fn skill_list_files_empty_skill() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let tool = SkillListFilesTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "skill_list_files".to_owned(),
                    arguments: BTreeMap::from([("skill_name".to_owned(), "noskill".to_owned())]),
                },
                &context(dir.path().to_string_lossy().as_ref()),
            )
            .expect("tool should succeed");

        assert!(output.content.contains("No supporting files"));
        assert_eq!(output.metadata.get("file_count").unwrap(), "0");
    }

    #[test]
    fn skill_delete_file_removes_file() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = SkillStore::new(&db_path);
        skill_store
            .upsert("deploy", "Deploy app", "Do deploy", None, &[])
            .unwrap();
        let file_store = SkillFileStore::new(&db_path);
        file_store.store_file("deploy", "refs/a.md", "a").unwrap();

        let tool = SkillDeleteFileTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "skill_delete_file".to_owned(),
                    arguments: BTreeMap::from([
                        ("skill_name".to_owned(), "deploy".to_owned()),
                        ("file_path".to_owned(), "refs/a.md".to_owned()),
                    ]),
                },
                &context(dir.path().to_string_lossy().as_ref()),
            )
            .expect("tool should succeed");

        assert!(output.content.contains("Deleted file"));
        assert_eq!(output.metadata.get("deleted").unwrap(), "true");

        // Verify it's gone
        assert!(file_store
            .get_file("deploy", "refs/a.md")
            .unwrap()
            .is_none());
    }

    #[test]
    fn skill_delete_file_not_found() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let tool = SkillDeleteFileTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "skill_delete_file".to_owned(),
                    arguments: BTreeMap::from([
                        ("skill_name".to_owned(), "deploy".to_owned()),
                        ("file_path".to_owned(), "refs/missing.md".to_owned()),
                    ]),
                },
                &context(dir.path().to_string_lossy().as_ref()),
            )
            .expect("tool should succeed");

        assert!(output.content.contains("not found"));
        assert_eq!(output.metadata.get("deleted").unwrap(), "false");
    }

    #[test]
    fn skill_view_file_errors_for_missing_file() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = SkillStore::new(&db_path);
        skill_store
            .upsert("deploy", "Deploy app", "Do deploy", None, &[])
            .unwrap();

        let tool = SkillViewFileTool;
        let err = tool
            .run(
                &ToolCall {
                    name: "skill_view_file".to_owned(),
                    arguments: BTreeMap::from([
                        ("skill_name".to_owned(), "deploy".to_owned()),
                        ("file_path".to_owned(), "references/missing.md".to_owned()),
                    ]),
                },
                &context(dir.path().to_string_lossy().as_ref()),
            )
            .expect_err("missing file should error");

        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("not found"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
