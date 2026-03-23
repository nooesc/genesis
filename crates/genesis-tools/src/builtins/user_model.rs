use std::collections::BTreeMap;

use genesis_storage::{bootstrap, format_user_traits, UserModelStore};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that records an observation about the user.
pub struct UserObserveTool;

impl ToolHandler for UserObserveTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let trait_key =
            call.arguments
                .get("trait_key")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "trait_key",
                })?;

        let category =
            call.arguments
                .get("category")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "category",
                })?;

        let value = call
            .arguments
            .get("value")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "value",
            })?;

        let db_path = context.db_path();
        bootstrap(&db_path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("database initialization failed: {e}"),
        })?;
        let store = UserModelStore::new(&db_path);

        let trait_record = store
            .observe(trait_key, category, value, Some(&context.session_id))
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to record observation: {e}"),
            })?;

        Ok(ToolOutput {
            content: format!(
                "Recorded: `{}` = \"{}\" (category: {}, confidence: {:.0}%, observations: {})",
                trait_record.trait_key,
                trait_record.value,
                trait_record.category,
                trait_record.confidence * 100.0,
                trait_record.evidence_count,
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("trait_key".to_owned(), trait_record.trait_key),
                (
                    "confidence".to_owned(),
                    format!("{:.2}", trait_record.confidence),
                ),
            ]),
        })
    }
}

/// Tool that retrieves the user model — what the agent knows about the user.
pub struct UserModelTool;

impl ToolHandler for UserModelTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let db_path = context.db_path();
        let store = UserModelStore::new(&db_path);

        let category = call.arguments.get("category");
        let min_confidence: f64 = call
            .arguments
            .get("min_confidence")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);

        let traits = if let Some(cat) = category {
            store.list_by_category(cat)
        } else if min_confidence > 0.0 {
            store.confident_traits(min_confidence)
        } else {
            store.list_all()
        }
        .map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read user model: {e}"),
        })?;

        if traits.is_empty() {
            return Ok(ToolOutput {
                content: "No user model data recorded yet.".to_owned(),
                metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
            });
        }

        // format_user_traits returns None only for empty slices, which we already handled above.
        let formatted = format_user_traits(&traits).unwrap_or_default();

        Ok(ToolOutput {
            content: formatted,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("count".to_owned(), traits.len().to_string()),
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
            session_id: "test-session".to_owned(),
            profile: "test".to_owned(),
            data_dir: data_dir.to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
            embedding_service: None,
            path_validator: None,
            recalled_memory_ids: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            approval_mode: genesis_config::ApprovalMode::Auto,
        }
    }

    #[test]
    fn observe_and_recall_user_trait() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        // Observe
        let call = ToolCall {
            name: "user_observe".to_owned(),
            arguments: BTreeMap::from([
                ("trait_key".to_owned(), "prefers_rust".to_owned()),
                ("category".to_owned(), "preference".to_owned()),
                ("value".to_owned(), "User strongly prefers Rust".to_owned()),
            ]),
        };
        let output = UserObserveTool.run(&call, &ctx).expect("observe");
        assert!(output.content.contains("prefers_rust"));
        assert!(output.content.contains("50%"));

        // Recall
        let recall = ToolCall {
            name: "user_model".to_owned(),
            arguments: BTreeMap::new(),
        };
        let output = UserModelTool.run(&recall, &ctx).expect("recall");
        assert!(output.content.contains("prefers_rust"));
        assert!(output.content.contains("preference"));
    }

    #[test]
    fn user_model_empty_returns_message() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "user_model".to_owned(),
            arguments: BTreeMap::new(),
        };
        let output = UserModelTool.run(&call, &ctx).expect("recall");
        assert!(output.content.contains("No user model data"));
    }

    #[test]
    fn user_model_filters_by_category() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        // Add two categories
        UserObserveTool
            .run(
                &ToolCall {
                    name: "user_observe".to_owned(),
                    arguments: BTreeMap::from([
                        ("trait_key".to_owned(), "likes_dark".to_owned()),
                        ("category".to_owned(), "preference".to_owned()),
                        ("value".to_owned(), "Dark mode".to_owned()),
                    ]),
                },
                &ctx,
            )
            .unwrap();

        UserObserveTool
            .run(
                &ToolCall {
                    name: "user_observe".to_owned(),
                    arguments: BTreeMap::from([
                        ("trait_key".to_owned(), "tone_formal".to_owned()),
                        ("category".to_owned(), "communication_style".to_owned()),
                        ("value".to_owned(), "Formal".to_owned()),
                    ]),
                },
                &ctx,
            )
            .unwrap();

        // Filter by category
        let call = ToolCall {
            name: "user_model".to_owned(),
            arguments: BTreeMap::from([("category".to_owned(), "preference".to_owned())]),
        };
        let output = UserModelTool.run(&call, &ctx).expect("recall");
        assert!(output.content.contains("likes_dark"));
        assert!(!output.content.contains("tone_formal"));
    }

    #[test]
    fn observe_requires_trait_key() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "user_observe".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = UserObserveTool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn observe_requires_category() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "user_observe".to_owned(),
            arguments: BTreeMap::from([("trait_key".to_owned(), "likes_rust".to_owned())]),
        };
        let err = UserObserveTool.run(&call, &ctx).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "category",
                ..
            }
        ));
    }

    #[test]
    fn observe_requires_value() {
        let dir = tempdir().expect("tempdir");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "user_observe".to_owned(),
            arguments: BTreeMap::from([
                ("trait_key".to_owned(), "likes_rust".to_owned()),
                ("category".to_owned(), "preference".to_owned()),
            ]),
        };
        let err = UserObserveTool.run(&call, &ctx).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "value",
                ..
            }
        ));
    }

    #[test]
    fn observe_multiple_times_increases_confidence() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let make_call = || ToolCall {
            name: "user_observe".to_owned(),
            arguments: BTreeMap::from([
                ("trait_key".to_owned(), "prefers_tabs".to_owned()),
                ("category".to_owned(), "preference".to_owned()),
                ("value".to_owned(), "Uses tabs over spaces".to_owned()),
            ]),
        };

        let output1 = UserObserveTool
            .run(&make_call(), &ctx)
            .expect("first observe");
        let output2 = UserObserveTool
            .run(&make_call(), &ctx)
            .expect("second observe");

        // Confidence should increase with more observations
        let conf1: f64 = output1.metadata.get("confidence").unwrap().parse().unwrap();
        let conf2: f64 = output2.metadata.get("confidence").unwrap().parse().unwrap();

        assert!(
            conf2 >= conf1,
            "confidence should increase: {conf1} -> {conf2}"
        );
    }

    #[test]
    fn user_model_min_confidence_filter() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        // Add a single observation (low confidence)
        UserObserveTool
            .run(
                &ToolCall {
                    name: "user_observe".to_owned(),
                    arguments: BTreeMap::from([
                        ("trait_key".to_owned(), "low_conf".to_owned()),
                        ("category".to_owned(), "preference".to_owned()),
                        ("value".to_owned(), "Some preference".to_owned()),
                    ]),
                },
                &ctx,
            )
            .unwrap();

        // Query with high min_confidence
        let call = ToolCall {
            name: "user_model".to_owned(),
            arguments: BTreeMap::from([("min_confidence".to_owned(), "0.99".to_owned())]),
        };
        let output = UserModelTool.run(&call, &ctx).expect("recall");
        // A single observation starts at 50% confidence, so filtering at 99% should be empty
        assert!(
            output.content.contains("No user model data"),
            "high min_confidence should filter out low-confidence traits"
        );
    }

    #[test]
    fn observe_metadata_includes_confidence() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let ctx = ctx_with_dir(&dir.path().display().to_string());

        let call = ToolCall {
            name: "user_observe".to_owned(),
            arguments: BTreeMap::from([
                ("trait_key".to_owned(), "test_key".to_owned()),
                ("category".to_owned(), "test".to_owned()),
                ("value".to_owned(), "test_value".to_owned()),
            ]),
        };
        let output = UserObserveTool.run(&call, &ctx).expect("observe");
        assert!(output.metadata.contains_key("confidence"));
        assert!(output.metadata.contains_key("trait_key"));
    }
}
