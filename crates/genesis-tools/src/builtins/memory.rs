use std::collections::{BTreeMap, BTreeSet};

use genesis_storage::{MemoryStore, NewMemoryNote};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const DEFAULT_RECALL_LIMIT: usize = 5;

pub struct MemoryStoreTool;

impl ToolHandler for MemoryStoreTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let key = call
            .arguments
            .get("key")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "key",
            })?;
        let value = call
            .arguments
            .get("value")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "value",
            })?;

        let database_path = context.db_path();
        let store = MemoryStore::new(&database_path);
        store
            .create_note(NewMemoryNote {
                id: memory_id(context, key),
                session_id: Some(context.session_id.clone()),
                kind: key.clone(),
                content: value.clone(),
                keywords: extract_keywords(key, value),
                tags: Vec::new(),
                linked_ids: Vec::new(),
                importance: 0.5,
            })
            .map_err(|error| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "failed to store memory into `{}`: {error}",
                    database_path.display()
                ),
            })?;

        Ok(ToolOutput {
            content: format!("stored memory `{key}`"),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("key".to_owned(), key.clone()),
                ("session_id".to_owned(), context.session_id.clone()),
            ]),
        })
    }
}

pub struct MemoryRecallTool;

impl ToolHandler for MemoryRecallTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = call
            .arguments
            .get("query")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "query",
            })?;

        let limit = call
            .arguments
            .get("limit")
            .map(|value| {
                value
                    .parse::<usize>()
                    .map_err(|error| ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: format!("invalid `limit` value `{value}`: {error}"),
                    })
            })
            .transpose()?
            .unwrap_or(DEFAULT_RECALL_LIMIT);

        let database_path = context.db_path();
        let store = MemoryStore::new(&database_path);
        let memories =
            store
                .graph_search(query, limit)
                .map_err(|error| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!(
                        "failed to search memories in `{}`: {error}",
                        database_path.display()
                    ),
                })?;

        let content = if memories.is_empty() {
            "no memories found".to_owned()
        } else {
            memories
                .into_iter()
                .map(|memory| {
                    format!(
                        "[{}] {} ({})",
                        memory.memory.kind, memory.memory.content, memory.memory.created_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("query".to_owned(), query.clone()),
                ("limit".to_owned(), limit.to_string()),
                ("mode".to_owned(), "graph".to_owned()),
            ]),
        })
    }
}

fn memory_id(context: &ToolContext, key: &str) -> String {
    format!("{}:{}:{}", context.session_id, key, unique_suffix())
}

fn extract_keywords(key: &str, value: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    key.split(|c: char| !c.is_ascii_alphanumeric())
        .chain(value.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter_map(|token| {
            let token = token.trim().to_ascii_lowercase();
            if token.len() < 2 || !seen.insert(token.clone()) {
                None
            } else {
                Some(token)
            }
        })
        .collect()
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use genesis_storage::{bootstrap, SessionStore};
    use tempfile::tempdir;

    use super::{MemoryRecallTool, MemoryStoreTool};
    use crate::{ToolCall, ToolContext, ToolHandler};

    fn ctx(data_dir: &str) -> ToolContext {
        ToolContext {
            session_id: "session-42".to_owned(),
            data_dir: data_dir.to_owned(),
            ..crate::test_utils::test_ctx()
        }
    }

    /// Bootstrap the DB and create the session so FK constraints are satisfied.
    fn setup_db(dir: &std::path::Path) {
        let db_path = dir.join("genesis.db");
        bootstrap(&db_path).expect("bootstrap should succeed");
        let store = SessionStore::new(&db_path);
        store
            .create_session("session-42", "test", None)
            .expect("session should be created");
    }

    #[test]
    fn memory_store_persists_key_value_into_memories_table() {
        let dir = tempdir().expect("tempdir should exist");
        setup_db(dir.path());
        let tool = MemoryStoreTool;
        let db_path = dir.path().join("genesis.db");

        let output = tool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "favorite_language".to_owned()),
                        ("value".to_owned(), "rust".to_owned()),
                    ]),
                },
                &ctx(dir.path().to_string_lossy().as_ref()),
            )
            .expect("memory should store");

        assert!(output.content.contains("favorite_language"));

        let conn = rusqlite::Connection::open(&db_path).expect("db should open");
        let row = conn
            .query_row(
                "SELECT kind, content, keywords_json, tags_json, importance
                 FROM memories
                 WHERE kind = 'favorite_language'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, f64>(4)?,
                    ))
                },
            )
            .expect("structured memory should be stored");

        assert_eq!(row.0, "favorite_language");
        assert_eq!(row.1, "rust");
        assert!(row.2.contains("favorite"));
        assert_eq!(row.3, "[]");
        assert_eq!(row.4, 0.5);
    }

    #[test]
    fn memory_recall_returns_matching_memories() {
        let dir = tempdir().expect("tempdir should exist");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "project_goal".to_owned()),
                        ("value".to_owned(), "build genesis in rust".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("memory should store");

        let output = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([("query".to_owned(), "genesis".to_owned())]),
                },
                &context,
            )
            .expect("memory recall should succeed");

        assert!(output.content.contains("[project_goal]"));
        assert!(output.content.contains("build genesis in rust"));
    }

    #[test]
    fn memory_recall_returns_linked_memories() {
        let dir = tempdir().expect("tempdir should exist");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let store = genesis_storage::MemoryStore::new(&db_path);

        store
            .create_note(genesis_storage::NewMemoryNote {
                id: "linked-note".to_owned(),
                session_id: Some("session-42".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust ownership model".to_owned(),
                keywords: vec!["rust".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.7,
            })
            .expect("linked note should store");
        store
            .create_note(genesis_storage::NewMemoryNote {
                id: "primary-note".to_owned(),
                session_id: Some("session-42".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis architecture memory".to_owned(),
                keywords: vec!["genesis".to_owned(), "memory".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["linked-note".to_owned()],
                importance: 1.0,
            })
            .expect("primary note should store");

        let output = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([("query".to_owned(), "Genesis".to_owned())]),
                },
                &ctx(dir.path().to_string_lossy().as_ref()),
            )
            .expect("memory recall should succeed");

        assert!(output.content.contains("Genesis architecture memory"));
        assert!(output.content.contains("Rust ownership model"));
    }

    #[test]
    fn memory_recall_uses_data_dir_to_find_database() {
        let dir = tempdir().expect("tempdir should exist");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        assert_eq!(context.db_path(), dir.path().join("genesis.db"));
    }

    #[test]
    fn extract_keywords_skips_single_character_tokens() {
        let keywords = super::extract_keywords("i", "Rust is a systems language");

        assert!(keywords.contains(&"rust".to_owned()));
        assert!(!keywords.contains(&"i".to_owned()));
        assert!(!keywords.contains(&"a".to_owned()));
    }

    #[test]
    fn memory_store_requires_key() {
        let dir = tempdir().expect("tempdir");
        let tool = MemoryStoreTool;
        let call = ToolCall {
            name: "memory_store".to_owned(),
            arguments: BTreeMap::from([("value".to_owned(), "some value".to_owned())]),
        };
        let err = tool
            .run(&call, &ctx(dir.path().to_string_lossy().as_ref()))
            .unwrap_err();
        assert!(matches!(
            err,
            crate::ToolError::MissingArgument {
                argument: "key",
                ..
            }
        ));
    }

    #[test]
    fn memory_store_requires_value() {
        let dir = tempdir().expect("tempdir");
        let tool = MemoryStoreTool;
        let call = ToolCall {
            name: "memory_store".to_owned(),
            arguments: BTreeMap::from([("key".to_owned(), "some_key".to_owned())]),
        };
        let err = tool
            .run(&call, &ctx(dir.path().to_string_lossy().as_ref()))
            .unwrap_err();
        assert!(matches!(
            err,
            crate::ToolError::MissingArgument {
                argument: "value",
                ..
            }
        ));
    }

    #[test]
    fn memory_recall_requires_query() {
        let dir = tempdir().expect("tempdir");
        let tool = MemoryRecallTool;
        let call = ToolCall {
            name: "memory_recall".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool
            .run(&call, &ctx(dir.path().to_string_lossy().as_ref()))
            .unwrap_err();
        assert!(matches!(
            err,
            crate::ToolError::MissingArgument {
                argument: "query",
                ..
            }
        ));
    }

    #[test]
    fn memory_recall_returns_no_memories_when_empty() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let output = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([("query".to_owned(), "anything".to_owned())]),
                },
                &context,
            )
            .expect("should succeed");

        assert_eq!(output.content, "no memories found");
    }

    #[test]
    fn memory_recall_respects_custom_limit() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        // Store multiple memories
        for i in 0..5 {
            MemoryStoreTool
                .run(
                    &ToolCall {
                        name: "memory_store".to_owned(),
                        arguments: BTreeMap::from([
                            ("key".to_owned(), format!("fact_{i}")),
                            ("value".to_owned(), format!("rust fact number {i}")),
                        ]),
                    },
                    &context,
                )
                .unwrap();
        }

        let output = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([
                        ("query".to_owned(), "rust".to_owned()),
                        ("limit".to_owned(), "2".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("should succeed");

        // The result should have at most 2 memories
        let lines: Vec<&str> = output.content.lines().collect();
        assert!(
            lines.len() <= 2,
            "should respect limit of 2, got {}",
            lines.len()
        );
        assert_eq!(output.metadata.get("limit").unwrap(), "2");
    }

    #[test]
    fn memory_recall_invalid_limit_returns_error() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let err = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([
                        ("query".to_owned(), "test".to_owned()),
                        ("limit".to_owned(), "not_a_number".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap_err();

        assert!(matches!(err, crate::ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn extract_keywords_deduplicates() {
        let keywords = super::extract_keywords("rust_lang", "rust is great");
        // "rust" appears in both key and value but should only appear once
        let rust_count = keywords.iter().filter(|k| *k == "rust").count();
        assert_eq!(rust_count, 1, "keywords should be deduplicated");
    }

    #[test]
    fn memory_store_metadata_includes_session_id() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let tool = MemoryStoreTool;

        let output = tool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "test_key".to_owned()),
                        ("value".to_owned(), "test_value".to_owned()),
                    ]),
                },
                &ctx(dir.path().to_string_lossy().as_ref()),
            )
            .expect("should succeed");

        assert_eq!(output.metadata.get("session_id").unwrap(), "session-42");
        assert_eq!(output.metadata.get("key").unwrap(), "test_key");
    }
}
