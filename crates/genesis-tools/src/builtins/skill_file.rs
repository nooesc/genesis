use std::collections::BTreeMap;

use genesis_storage::{bootstrap, SkillFileStore};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that returns the content of a supporting file attached to a skill.
pub struct SkillViewFileTool;

impl ToolHandler for SkillViewFileTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let skill_name = call
            .arguments
            .get("skill_name")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "skill_name",
            })?;

        let file_path = call
            .arguments
            .get("file_path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "file_path",
            })?;

        let db_path = std::path::Path::new(&context.data_dir).join("genesis.db");
        let _ = bootstrap(&db_path);
        let store = SkillFileStore::new(&db_path);

        let content = store
            .get_file(skill_name, file_path)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to load skill file: {e}"),
            })?
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "supporting file `{file_path}` for skill `{skill_name}` not found"
                ),
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
