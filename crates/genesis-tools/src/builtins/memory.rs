use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

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

        let database_path = database_path_from_context(context);
        let connection = open_database(&call.name, &database_path)?;
        ensure_memory_search_index(&call.name, &connection)?;

        connection
            .execute(
                "INSERT INTO memories (id, session_id, kind, content, created_at)
                 VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
                params![memory_id(context, key), context.session_id, key, value],
            )
            .map_err(|error| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to insert memory into `{}`: {error}", database_path.display()),
            })?;

        let memory_row_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, ?2, ?3)",
                params![memory_row_id, key, value],
            )
            .map_err(|error| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "failed to index memory in `{}`: {error}",
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
                value.parse::<usize>().map_err(|error| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("invalid `limit` value `{value}`: {error}"),
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_RECALL_LIMIT);

        let database_path = database_path_from_context(context);
        let connection = open_database(&call.name, &database_path)?;
        ensure_memory_search_index(&call.name, &connection)?;

        let mut statement = connection
            .prepare(
                "SELECT m.kind, m.content, m.created_at
                 FROM memory_search ms
                 JOIN memories m ON m.rowid = ms.memory_row_id
                 WHERE memory_search MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|error| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to prepare recall query: {error}"),
            })?;

        let rows = statement
            .query_map(params![query, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to search memories: {error}"),
            })?;

        let memories = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to collect memory search results: {error}"),
            })?;

        let content = if memories.is_empty() {
            "no memories found".to_owned()
        } else {
            memories
                .into_iter()
                .map(|(kind, content, created_at)| format!("[{kind}] {content} ({created_at})"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("query".to_owned(), query.clone()),
                ("limit".to_owned(), limit.to_string()),
            ]),
        })
    }
}

fn database_path_from_context(context: &ToolContext) -> PathBuf {
    Path::new(&context.data_dir).join("genesis.db")
}

fn open_database(tool_name: &str, database_path: &Path) -> Result<Connection, ToolError> {
    Connection::open(database_path).map_err(|error| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!("failed to open `{}`: {error}", database_path.display()),
    })
}

fn ensure_memory_search_index(tool_name: &str, connection: &Connection) -> Result<(), ToolError> {
    connection
        .execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED,
                kind,
                content
            );",
        )
        .map_err(|error| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to ensure memory search index: {error}"),
        })
}

fn memory_id(context: &ToolContext, key: &str) -> String {
    format!("{}:{}:{}", context.session_id, key, unique_suffix())
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

    use super::{database_path_from_context, MemoryRecallTool, MemoryStoreTool};
    use crate::{ToolCall, ToolContext, ToolHandler};

    fn ctx(data_dir: &str) -> ToolContext {
        ToolContext {
            session_id: "session-42".to_owned(),
            profile: "operator".to_owned(),
            data_dir: data_dir.to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
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
    fn memory_recall_uses_data_dir_to_find_database() {
        let dir = tempdir().expect("tempdir should exist");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        assert_eq!(database_path_from_context(&context), dir.path().join("genesis.db"));
    }
}
