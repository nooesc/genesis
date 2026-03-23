pub mod cron;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: i64 = 13;

static SQLITE_VEC_REGISTERED: OnceLock<()> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageBootstrap {
    pub database_path: PathBuf,
    pub schema_version: i64,
}

#[cfg(test)]
mod session_store_tests {
    use super::{bootstrap, SessionStore};
    use tempfile::tempdir;

    #[test]
    fn session_store_creates_and_loads_messages_in_order() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-1", "cli", None)
            .expect("session should be created");
        store
            .append_message("session-1", "user", Some("hello eve"), None, None, None)
            .expect("first message should be stored");
        store
            .append_message(
                "session-1",
                "assistant",
                Some("hello operator"),
                None,
                None,
                None,
            )
            .expect("second message should be stored");

        let messages = store
            .load_messages("session-1")
            .expect("messages should load");

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].session_id, "session-1");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content.as_deref(), Some("hello eve"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content.as_deref(), Some("hello operator"));
    }

    #[test]
    fn session_store_searches_sessions_by_indexed_message_content() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-alpha", "cli", None)
            .expect("first session should be created");
        store
            .create_session("session-beta", "slack", None)
            .expect("second session should be created");
        store
            .append_message(
                "session-alpha",
                "user",
                Some("rust migration checklist"),
                None,
                None,
                None,
            )
            .expect("first message should be stored");
        store
            .append_message(
                "session-beta",
                "user",
                Some("provider client work"),
                None,
                None,
                None,
            )
            .expect("second message should be stored");

        let matches = store
            .search_sessions("migration")
            .expect("search should succeed");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "session-alpha");
        assert_eq!(matches[0].platform, "cli");
    }

    #[test]
    fn add_usage_accumulates_token_counts() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-tok", "cli", None)
            .expect("session should be created");

        store
            .add_usage("session-tok", 100, 50)
            .expect("first add_usage");
        store
            .add_usage("session-tok", 200, 75)
            .expect("second add_usage");

        let session = store
            .get_session("session-tok")
            .expect("get should work")
            .expect("session should exist");
        assert_eq!(session.total_input_tokens, 300);
        assert_eq!(session.total_output_tokens, 125);
    }

    #[test]
    fn add_usage_noop_for_zero_tokens() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-zero", "cli", None)
            .expect("session should be created");

        // Should be a no-op
        store
            .add_usage("session-zero", 0, 0)
            .expect("zero add_usage");

        let session = store
            .get_session("session-zero")
            .expect("get should work")
            .expect("session should exist");
        assert_eq!(session.total_input_tokens, 0);
        assert_eq!(session.total_output_tokens, 0);
    }

    #[test]
    fn usage_stats_aggregates_all_sessions() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store.create_session("s1", "cli", None).expect("create s1");
        store.create_session("s2", "cli", None).expect("create s2");
        store.add_usage("s1", 1000, 500).expect("add usage s1");
        store.add_usage("s2", 2000, 800).expect("add usage s2");

        let stats = store.usage_stats().expect("stats should work");
        assert_eq!(stats.total_sessions, 2);
        assert_eq!(stats.total_input_tokens, 3000);
        assert_eq!(stats.total_output_tokens, 1300);
    }

    #[test]
    fn set_title_overwrites_existing_title() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("s1", "cli", Some("Original"))
            .expect("create");

        let session = store.get_session("s1").unwrap().unwrap();
        assert_eq!(session.title.as_deref(), Some("Original"));

        assert!(store.set_title("s1", "Renamed").unwrap());

        let session = store.get_session("s1").unwrap().unwrap();
        assert_eq!(session.title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn set_title_returns_false_for_missing_session() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        assert!(!store.set_title("nonexistent", "Title").unwrap());
    }

    #[test]
    fn purge_older_than_deletes_nothing_for_recent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store.create_session("recent", "cli", None).expect("create");
        store
            .append_message("recent", "user", Some("hello"), None, None, None)
            .expect("msg");

        let deleted = store.purge_older_than(30).unwrap();
        assert_eq!(deleted, 0);

        // Session and messages should still exist
        assert!(store.get_session("recent").unwrap().is_some());
        assert_eq!(store.load_messages("recent").unwrap().len(), 1);
    }

    #[test]
    fn append_mirror_message_stores_mirror_fields() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-mirror", "telegram", None)
            .expect("session should be created");

        let msg_id = store
            .append_mirror_message("session-mirror", "Hello from CLI", "cli")
            .expect("mirror message should be stored");

        assert!(msg_id > 0);

        let messages = store
            .load_messages("session-mirror")
            .expect("messages should load");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[0].content.as_deref(), Some("Hello from CLI"));
        assert!(messages[0].mirror);
        assert_eq!(messages[0].mirror_source.as_deref(), Some("cli"));
    }

    #[test]
    fn mirror_messages_coexist_with_regular_messages() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-mixed", "slack", None)
            .expect("session should be created");

        store
            .append_message("session-mixed", "user", Some("hello"), None, None, None)
            .expect("regular message should be stored");
        store
            .append_mirror_message("session-mixed", "scheduled reminder", "schedule")
            .expect("mirror message should be stored");
        store
            .append_message(
                "session-mixed",
                "assistant",
                Some("got it"),
                None,
                None,
                None,
            )
            .expect("regular reply should be stored");

        let messages = store
            .load_messages("session-mixed")
            .expect("messages should load");

        assert_eq!(messages.len(), 3);
        assert!(!messages[0].mirror);
        assert!(messages[0].mirror_source.is_none());
        assert!(messages[1].mirror);
        assert_eq!(messages[1].mirror_source.as_deref(), Some("schedule"));
        assert!(!messages[2].mirror);
        assert!(messages[2].mirror_source.is_none());
    }

    #[test]
    fn find_session_by_platform_chat_id_resolves_correctly() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("tg-12345", "telegram", Some("Telegram Chat"))
            .expect("session should be created");
        store
            .create_session("slack-general", "slack", Some("Slack General"))
            .expect("session should be created");

        let tg = store
            .find_session_by_platform_chat_id("telegram", "12345")
            .expect("lookup should succeed");
        assert!(tg.is_some());
        assert_eq!(tg.unwrap().id, "tg-12345");

        let slack = store
            .find_session_by_platform_chat_id("slack", "general")
            .expect("lookup should succeed");
        assert!(slack.is_some());
        assert_eq!(slack.unwrap().id, "slack-general");

        let missing = store
            .find_session_by_platform_chat_id("discord", "99999")
            .expect("lookup should succeed");
        assert!(missing.is_none());
    }

    #[test]
    fn truncate_messages_keeps_only_recent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-trunc", "cli", None)
            .expect("session should be created");

        for i in 1..=5 {
            store
                .append_message(
                    "session-trunc",
                    "user",
                    Some(&format!("message {i}")),
                    None,
                    None,
                    None,
                )
                .expect("message should be stored");
        }

        let deleted = store
            .truncate_messages("session-trunc", 2)
            .expect("truncate should succeed");
        assert_eq!(deleted, 3);

        let messages = store
            .load_messages("session-trunc")
            .expect("messages should load");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.as_deref(), Some("message 4"));
        assert_eq!(messages[1].content.as_deref(), Some("message 5"));
    }

    #[test]
    fn truncate_messages_noop_when_fewer_than_limit() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-few", "cli", None)
            .expect("session should be created");

        for i in 1..=3 {
            store
                .append_message(
                    "session-few",
                    "user",
                    Some(&format!("message {i}")),
                    None,
                    None,
                    None,
                )
                .expect("message should be stored");
        }

        let deleted = store
            .truncate_messages("session-few", 10)
            .expect("truncate should succeed");
        assert_eq!(deleted, 0);

        let messages = store
            .load_messages("session-few")
            .expect("messages should load");
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn search_messages_finds_matching_content() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-search", "cli", Some("Search Test"))
            .expect("session should be created");

        store
            .append_message(
                "session-search",
                "user",
                Some("hello world"),
                None,
                None,
                None,
            )
            .expect("first message should be stored");
        store
            .append_message(
                "session-search",
                "assistant",
                Some("goodbye moon"),
                None,
                None,
                None,
            )
            .expect("second message should be stored");
        store
            .append_message(
                "session-search",
                "user",
                Some("hello again"),
                None,
                None,
                None,
            )
            .expect("third message should be stored");

        let results = store
            .search_messages("hello", 10)
            .expect("search should succeed");

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.session_id == "session-search"));
        assert!(results.iter().all(|r| r.content.contains("hello")));
    }

    #[test]
    fn list_recent_sessions_paginated_returns_total_and_page() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        for i in 0..5 {
            store
                .create_session(&format!("s-{i}"), "cli", None)
                .expect("session should be created");
        }

        // First page
        let (page, total) = store
            .list_recent_sessions_paginated(2, 0)
            .expect("paginated list should succeed");
        assert_eq!(total, 5);
        assert_eq!(page.len(), 2);

        // Second page
        let (page2, total2) = store
            .list_recent_sessions_paginated(2, 2)
            .expect("second page should succeed");
        assert_eq!(total2, 5);
        assert_eq!(page2.len(), 2);

        // Pages should not overlap
        assert_ne!(page[0].id, page2[0].id);

        // Beyond end
        let (page3, total3) = store
            .list_recent_sessions_paginated(2, 10)
            .expect("beyond-end page should succeed");
        assert_eq!(total3, 5);
        assert!(page3.is_empty());
    }

    #[test]
    fn search_sessions_paginated_returns_filtered_total() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("s-match-1", "cli", None)
            .expect("session should be created");
        store
            .create_session("s-match-2", "cli", None)
            .expect("session should be created");
        store
            .create_session("s-other", "cli", None)
            .expect("session should be created");
        store
            .append_message("s-match-1", "user", Some("pagination test alpha"), None, None, None)
            .expect("msg");
        store
            .append_message("s-match-2", "user", Some("pagination test beta"), None, None, None)
            .expect("msg");
        store
            .append_message("s-other", "user", Some("unrelated topic"), None, None, None)
            .expect("msg");

        let (results, total) = store
            .search_sessions_paginated("pagination", 1, 0)
            .expect("search should succeed");
        assert_eq!(total, 2);
        assert_eq!(results.len(), 1);

        let (results2, total2) = store
            .search_sessions_paginated("pagination", 1, 1)
            .expect("second page should succeed");
        assert_eq!(total2, 2);
        assert_eq!(results2.len(), 1);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageHealth {
    pub database_exists: bool,
    pub schema_version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyImportSource {
    pub root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub database_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    Planned,
    Imported,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportRun {
    pub id: i64,
    pub legacy_root: PathBuf,
    pub legacy_config_path: Option<PathBuf>,
    pub legacy_data_dir: Option<PathBuf>,
    pub legacy_database_path: Option<PathBuf>,
    pub status: ImportStatus,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("failed to create storage directory for {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open sqlite database at {path}: {source}")]
    OpenDatabase {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("sqlite error at {path}: {source}")]
    Sqlite {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("database at {path} contains mixed embedding dimensions: {dimensions:?}")]
    MixedEmbeddingDimensions { path: PathBuf, dimensions: Vec<i64> },
    #[error("database at {path} uses embedding dimensions {expected}, cannot store vector with {actual}")]
    EmbeddingDimensionMismatch {
        path: PathBuf,
        expected: usize,
        actual: usize,
    },
    #[error("database at {path} has an unrecognized memory_vec schema: {sql}")]
    InvalidVectorIndexSchema { path: PathBuf, sql: String },
    #[error("invalid timestamp: {0}")]
    InvalidTimestamp(String),
    #[error("unknown import status in database: {0}")]
    UnknownImportStatus(String),
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, StorageError> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }
    if let Ok(parsed) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
        return Ok(parsed.and_utc());
    }
    Err(StorageError::InvalidTimestamp(value.to_owned()))
}

fn decayed_importance(
    importance: f32,
    accessed_at: &str,
    now: &DateTime<Utc>,
) -> Result<f32, StorageError> {
    let accessed_at = parse_timestamp(accessed_at)?;
    let days = (((*now) - accessed_at).num_seconds().max(0) as f32) / 86_400.0;
    Ok(importance * 0.99_f32.powf(days))
}

fn sql_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Collect mapped rows into a Vec, converting any SQLite error into a StorageError.
fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    database_path: &Path,
) -> Result<Vec<T>, StorageError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })
}

/// Check whether a column exists in a table (used for idempotent migrations).
fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    // All callers pass compile-time string literals. Validate identifiers
    // unconditionally (not just in debug builds) as defense-in-depth.
    assert!(
        table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "column_exists: table name must be alphanumeric, got {table:?}"
    );
    assert!(
        column
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "column_exists: column name must be alphanumeric, got {column:?}"
    );
    conn.prepare(&format!("SELECT \"{column}\" FROM \"{table}\" LIMIT 0"))
        .is_ok()
}

/// Run a batch of SQL statements as a migration step.
fn exec_migration(conn: &Connection, path: &Path, sql: &str) -> Result<(), StorageError> {
    conn.execute_batch(sql)
        .map_err(|source| StorageError::Sqlite {
            path: path.to_path_buf(),
            source,
        })
}

fn register_sqlite_vec() {
    SQLITE_VEC_REGISTERED.get_or_init(|| unsafe {
        // SAFETY: `sqlite_vec::sqlite3_vec_init` is the sqlite-vec extension entry point
        // with the exact function signature expected by `sqlite3_auto_extension`.
        #[allow(clippy::missing_transmute_annotations)]
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

fn detect_uniform_embedding_dimensions(
    conn: &Connection,
    database_path: &Path,
) -> Result<Option<usize>, StorageError> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT dimensions FROM memory_embeddings ORDER BY dimensions ASC LIMIT 2",
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    let dimensions = collect_rows(rows, database_path)?;

    match dimensions.as_slice() {
        [] => Ok(None),
        [dimension] => Ok(Some(*dimension as usize)),
        _ => Err(StorageError::MixedEmbeddingDimensions {
            path: database_path.to_path_buf(),
            dimensions,
        }),
    }
}

fn parse_memory_vec_dimensions(sql: &str) -> Option<usize> {
    let start = sql.find("float[")? + "float[".len();
    let end = sql[start..].find(']')? + start;
    sql[start..end].parse().ok()
}

fn memory_vec_declared_dimensions(
    conn: &Connection,
    database_path: &Path,
) -> Result<Option<usize>, StorageError> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memory_vec'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    match sql {
        None => Ok(None),
        Some(sql) => parse_memory_vec_dimensions(&sql)
            .ok_or(StorageError::InvalidVectorIndexSchema {
                path: database_path.to_path_buf(),
                sql,
            })
            .map(Some),
    }
}

fn create_memory_vec_table(
    conn: &Connection,
    database_path: &Path,
    dimensions: usize,
) -> Result<(), StorageError> {
    match exec_migration(
        conn,
        database_path,
        &format!(
            "CREATE VIRTUAL TABLE memory_vec USING vec0(
                memory_rowid integer primary key,
                embedding float[{dimensions}] distance_metric=cosine
            );"
        ),
    ) {
        Ok(()) => Ok(()),
        Err(StorageError::Sqlite { source, .. })
            if source.to_string().contains("already exists") =>
        {
            match memory_vec_declared_dimensions(conn, database_path)? {
                Some(existing) if existing == dimensions => Ok(()),
                Some(existing) => Err(StorageError::EmbeddingDimensionMismatch {
                    path: database_path.to_path_buf(),
                    expected: existing,
                    actual: dimensions,
                }),
                None => Err(StorageError::Sqlite {
                    path: database_path.to_path_buf(),
                    source,
                }),
            }
        }
        Err(error) => Err(error),
    }
}

fn memory_embeddings_count(conn: &Connection, database_path: &Path) -> Result<usize, StorageError> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| {
            row.get(0)
        })
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    Ok(count as usize)
}

fn ensure_memory_vec_table(
    conn: &Connection,
    database_path: &Path,
    dimensions: usize,
) -> Result<(), StorageError> {
    match memory_vec_declared_dimensions(conn, database_path)? {
        Some(existing) if existing == dimensions => Ok(()),
        Some(_existing) if memory_embeddings_count(conn, database_path)? == 0 => {
            conn.execute("DROP TABLE memory_vec", [])
                .map_err(|source| StorageError::Sqlite {
                    path: database_path.to_path_buf(),
                    source,
                })?;
            create_memory_vec_table(conn, database_path, dimensions)
        }
        Some(existing) => Err(StorageError::EmbeddingDimensionMismatch {
            path: database_path.to_path_buf(),
            expected: existing,
            actual: dimensions,
        }),
        None => {
            if let Some(existing) = detect_uniform_embedding_dimensions(conn, database_path)? {
                if existing != dimensions {
                    return Err(StorageError::EmbeddingDimensionMismatch {
                        path: database_path.to_path_buf(),
                        expected: existing,
                        actual: dimensions,
                    });
                }
            }
            create_memory_vec_table(conn, database_path, dimensions)
        }
    }
}

fn rebuild_memory_vec_index(conn: &Connection, database_path: &Path) -> Result<(), StorageError> {
    let dimensions = match detect_uniform_embedding_dimensions(conn, database_path) {
        Ok(Some(dimensions)) => dimensions,
        Ok(None) => return Ok(()),
        Err(StorageError::MixedEmbeddingDimensions { .. }) => return Ok(()),
        Err(error) => return Err(error),
    };

    ensure_memory_vec_table(conn, database_path, dimensions)?;
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         DELETE FROM memory_vec;
         INSERT INTO memory_vec(memory_rowid, embedding)
         SELECT m.rowid, me.embedding
         FROM memory_embeddings me
         JOIN memories m ON m.id = me.memory_id;
         COMMIT;",
    )
    .map_err(|source| StorageError::Sqlite {
        path: database_path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn sqlite_table_exists(
    conn: &Connection,
    database_path: &Path,
    table_name: &str,
) -> Result<bool, StorageError> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE (type = 'table' OR type = 'view') AND name = ?1
        )",
        params![table_name],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
    .map_err(|source| StorageError::Sqlite {
        path: database_path.to_path_buf(),
        source,
    })
}

fn memory_vec_table_exists(conn: &Connection, database_path: &Path) -> Result<bool, StorageError> {
    memory_vec_declared_dimensions(conn, database_path).map(|value| value.is_some())
}

pub fn bootstrap(database_path: &Path) -> Result<StorageBootstrap, StorageError> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent).map_err(|source| StorageError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let connection = open(database_path)?;

    exec_migration(
        &connection,
        database_path,
        "
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                platform TEXT NOT NULL,
                total_input_tokens INTEGER NOT NULL DEFAULT 0,
                total_output_tokens INTEGER NOT NULL DEFAULT 0,
                parent_session_id TEXT,
                tags TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL,
                keywords_json TEXT NOT NULL DEFAULT '[]',
                tags_json TEXT NOT NULL DEFAULT '[]',
                importance REAL NOT NULL DEFAULT 0.5,
                accessed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                access_count INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL
            );
            CREATE TABLE IF NOT EXISTS memory_links (
                source_id TEXT NOT NULL,
                target_id TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (source_id, target_id),
                FOREIGN KEY(source_id) REFERENCES memories(id) ON DELETE CASCADE,
                FOREIGN KEY(target_id) REFERENCES memories(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS schedules (
                id TEXT PRIMARY KEY,
                cron_expression TEXT NOT NULL,
                destination TEXT NOT NULL,
                prompt TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                timezone TEXT
            );
            CREATE TABLE IF NOT EXISTS schedule_executions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                schedule_id TEXT NOT NULL,
                executed_at TEXT NOT NULL,
                status TEXT NOT NULL,
                error_message TEXT,
                duration_ms INTEGER,
                FOREIGN KEY(schedule_id) REFERENCES schedules(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls_json TEXT,
                mirror INTEGER NOT NULL DEFAULT 0,
                mirror_source TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session_id
                ON messages(session_id);
            CREATE TABLE IF NOT EXISTS legacy_import_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                legacy_root TEXT NOT NULL,
                legacy_config_path TEXT,
                legacy_data_dir TEXT,
                legacy_database_path TEXT,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS skills (
                name TEXT PRIMARY KEY,
                description TEXT NOT NULL,
                instructions TEXT NOT NULL,
                trigger_hint TEXT,
                tags TEXT NOT NULL DEFAULT '',
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS skill_files (
                skill_name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (skill_name, file_path),
                FOREIGN KEY(skill_name) REFERENCES skills(name) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS user_model (
                trait_key TEXT PRIMARY KEY,
                category TEXT NOT NULL,
                value TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5,
                evidence_count INTEGER NOT NULL DEFAULT 1,
                source_session TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS skill_usages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                skill_name TEXT NOT NULL,
                session_id TEXT,
                outcome TEXT NOT NULL DEFAULT 'unknown',
                feedback TEXT,
                refined INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(skill_name) REFERENCES skills(name) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS subagents (
                id TEXT PRIMARY KEY,
                parent_session_id TEXT NOT NULL,
                child_session_id TEXT NOT NULL,
                name TEXT NOT NULL,
                task TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                result TEXT,
                error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                completed_at TEXT
            );
            CREATE TABLE IF NOT EXISTS pairing_approved (
                platform TEXT NOT NULL,
                user_id TEXT NOT NULL,
                user_name TEXT NOT NULL DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (platform, user_id)
            );
            CREATE TABLE IF NOT EXISTS pairing_pending (
                platform TEXT NOT NULL,
                code TEXT NOT NULL,
                user_id TEXT NOT NULL,
                user_name TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (platform, code)
            );
            CREATE TABLE IF NOT EXISTS channels (
                platform TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                channel_name TEXT NOT NULL,
                channel_type TEXT NOT NULL DEFAULT 'channel',
                is_member INTEGER NOT NULL DEFAULT 0,
                cached_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (platform, channel_id)
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS session_search USING fts5(
                session_id UNINDEXED,
                content
            );
            CREATE TABLE IF NOT EXISTS response_cache (
                cache_key TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                response TEXT NOT NULL,
                tool_calls_json TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                hit_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                expires_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                event_type TEXT NOT NULL,
                details TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE INDEX IF NOT EXISTS idx_audit_log_session
                ON audit_log(session_id);
            CREATE INDEX IF NOT EXISTS idx_audit_log_event_type
                ON audit_log(event_type);
            CREATE INDEX IF NOT EXISTS idx_audit_log_created_at
                ON audit_log(created_at);
            CREATE TABLE IF NOT EXISTS sticker_cache (
                file_unique_id TEXT PRIMARY KEY,
                description TEXT NOT NULL,
                emoji TEXT NOT NULL DEFAULT '',
                sticker_set TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS memory_embeddings (
                memory_id TEXT PRIMARY KEY,
                embedding BLOB NOT NULL,
                model TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(memory_id) REFERENCES memories(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS sandboxes (
                id            TEXT PRIMARY KEY,
                backend       TEXT NOT NULL,
                task_id       TEXT NOT NULL,
                snapshot_data TEXT,
                created_at    TEXT NOT NULL DEFAULT (datetime('now')),
                last_active   TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(backend, task_id)
            );
            ",
    )?;

    // Run migrations for existing databases.
    migrate_to_v2(&connection, database_path)?;
    migrate_to_v3(&connection, database_path)?;
    migrate_to_v4(&connection, database_path)?;
    migrate_to_v5(&connection, database_path)?;
    migrate_to_v6(&connection, database_path)?;
    migrate_to_v7(&connection, database_path)?;
    migrate_to_v8(&connection, database_path)?;
    migrate_to_v9(&connection, database_path)?;
    migrate_to_v10(&connection, database_path)?;
    migrate_to_v11(&connection, database_path)?;
    migrate_to_v12(&connection, database_path)?;
    migrate_to_v13(&connection, database_path)?;

    connection
        .execute(
            "
            INSERT INTO metadata (key, value) VALUES ('schema_version', ?1)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            ",
            params![SCHEMA_VERSION],
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(StorageBootstrap {
        database_path: database_path.to_path_buf(),
        schema_version: SCHEMA_VERSION,
    })
}

/// Migrate v1 → v2: add token tracking columns to sessions table.
fn migrate_to_v2(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    if column_exists(connection, "sessions", "total_input_tokens") {
        return Ok(());
    }

    exec_migration(
        connection,
        database_path,
        "ALTER TABLE sessions ADD COLUMN total_input_tokens INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE sessions ADD COLUMN total_output_tokens INTEGER NOT NULL DEFAULT 0;",
    )
}

/// Migrate v2 → v3: add parent_session_id for conversation forking.
fn migrate_to_v3(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    if column_exists(connection, "sessions", "parent_session_id") {
        return Ok(());
    }

    exec_migration(
        connection,
        database_path,
        "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT;",
    )
}

/// Migrate v3 → v4: add tags column to sessions for categorization.
fn migrate_to_v4(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    if column_exists(connection, "sessions", "tags") {
        return Ok(());
    }

    exec_migration(
        connection,
        database_path,
        "ALTER TABLE sessions ADD COLUMN tags TEXT NOT NULL DEFAULT '';",
    )
}

/// Migrate v4 → v5: add mirror columns to messages for delivery mirroring.
fn migrate_to_v5(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    if column_exists(connection, "messages", "mirror") {
        return Ok(());
    }

    exec_migration(
        connection,
        database_path,
        "ALTER TABLE messages ADD COLUMN mirror INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE messages ADD COLUMN mirror_source TEXT;",
    )
}

/// Migrate v5 → v6: add response_cache and audit_log tables.
fn migrate_to_v6(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    exec_migration(
        connection,
        database_path,
        "CREATE TABLE IF NOT EXISTS response_cache (
            cache_key TEXT PRIMARY KEY,
            model TEXT NOT NULL,
            response TEXT NOT NULL,
            tool_calls_json TEXT,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            hit_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            expires_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT,
            event_type TEXT NOT NULL,
            details TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_audit_log_session
            ON audit_log(session_id);
        CREATE INDEX IF NOT EXISTS idx_audit_log_event_type
            ON audit_log(event_type);
        CREATE INDEX IF NOT EXISTS idx_audit_log_created_at
            ON audit_log(created_at);",
    )
}

/// Migrate v6 → v7: add channels table for platform channel caching.
fn migrate_to_v7(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    exec_migration(
        connection,
        database_path,
        "CREATE TABLE IF NOT EXISTS channels (
            platform TEXT NOT NULL,
            channel_id TEXT NOT NULL,
            channel_name TEXT NOT NULL,
            channel_type TEXT NOT NULL DEFAULT 'channel',
            is_member INTEGER NOT NULL DEFAULT 0,
            cached_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (platform, channel_id)
        );",
    )
}

/// Migrate v7 → v8: add sticker_cache table for Telegram sticker descriptions.
fn migrate_to_v8(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    exec_migration(
        connection,
        database_path,
        "CREATE TABLE IF NOT EXISTS sticker_cache (
            file_unique_id TEXT PRIMARY KEY,
            description TEXT NOT NULL,
            emoji TEXT NOT NULL DEFAULT '',
            sticker_set TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
}

/// Migrate v8 → v9: add provider_metadata column and sandboxes table.
fn migrate_to_v9(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    // Add provider_metadata column (idempotent).
    let has_column: bool = connection
        .prepare("SELECT provider_metadata FROM messages LIMIT 0")
        .is_ok();

    if !has_column {
        connection
            .execute("ALTER TABLE messages ADD COLUMN provider_metadata TEXT", [])
            .map_err(|source| StorageError::Sqlite {
                path: database_path.to_path_buf(),
                source,
            })?;
    }

    // Add sandboxes table for sandbox terminal backend persistence.
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS sandboxes (
                id            TEXT PRIMARY KEY,
                backend       TEXT NOT NULL,
                task_id       TEXT NOT NULL,
                snapshot_data TEXT,
                created_at    TEXT NOT NULL DEFAULT (datetime('now')),
                last_active   TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(backend, task_id)
            );",
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

/// Migrate v9 → v10: create and backfill the sqlite-vec memory index when dimensions are uniform.
fn migrate_to_v10(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    rebuild_memory_vec_index(connection, database_path)
}

/// Migrate v10 → v11: add structured memory metadata columns and note-link edges.
fn migrate_to_v11(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    if !column_exists(connection, "memories", "keywords_json") {
        exec_migration(
            connection,
            database_path,
            "ALTER TABLE memories ADD COLUMN keywords_json TEXT NOT NULL DEFAULT '[]';",
        )?;
    }
    if !column_exists(connection, "memories", "tags_json") {
        exec_migration(
            connection,
            database_path,
            "ALTER TABLE memories ADD COLUMN tags_json TEXT NOT NULL DEFAULT '[]';",
        )?;
    }
    if !column_exists(connection, "memories", "importance") {
        exec_migration(
            connection,
            database_path,
            "ALTER TABLE memories ADD COLUMN importance REAL NOT NULL DEFAULT 0.5;",
        )?;
    }
    if !column_exists(connection, "memories", "accessed_at") {
        exec_migration(
            connection,
            database_path,
            "ALTER TABLE memories ADD COLUMN accessed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP;",
        )?;
    }
    if !column_exists(connection, "memories", "access_count") {
        exec_migration(
            connection,
            database_path,
            "ALTER TABLE memories ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;",
        )?;
    }

    exec_migration(
        connection,
        database_path,
        "CREATE TABLE IF NOT EXISTS memory_links (
            source_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (source_id, target_id),
            FOREIGN KEY(source_id) REFERENCES memories(id) ON DELETE CASCADE,
            FOREIGN KEY(target_id) REFERENCES memories(id) ON DELETE CASCADE
        );",
    )
}

/// Migrate v11 → v12: store unresolved memory links until both notes exist.
fn migrate_to_v12(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    exec_migration(
        connection,
        database_path,
        "CREATE TABLE IF NOT EXISTS pending_memory_links (
            source_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (source_id, target_id)
        );",
    )
}

/// Migrate v12 → v13: add timezone to schedules and schedule execution history.
fn migrate_to_v13(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    if !column_exists(connection, "schedules", "timezone") {
        exec_migration(
            connection,
            database_path,
            "ALTER TABLE schedules ADD COLUMN timezone TEXT;",
        )?;
    }

    exec_migration(
        connection,
        database_path,
        "CREATE TABLE IF NOT EXISTS schedule_executions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            schedule_id TEXT NOT NULL,
            executed_at TEXT NOT NULL,
            status TEXT NOT NULL,
            error_message TEXT,
            duration_ms INTEGER,
            FOREIGN KEY(schedule_id) REFERENCES schedules(id) ON DELETE CASCADE
        );",
    )
}

pub fn inspect(database_path: &Path) -> Result<StorageHealth, StorageError> {
    if !database_path.exists() {
        return Ok(StorageHealth {
            database_exists: false,
            schema_version: None,
        });
    }

    let connection = open(database_path)?;
    let schema_version = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?
        .and_then(|value| value.parse::<i64>().ok());

    Ok(StorageHealth {
        database_exists: true,
        schema_version,
    })
}

pub fn discover_legacy_source(root: &Path) -> LegacyImportSource {
    let config_path = first_existing_file(
        [
            root.join("cli-config.yaml"),
            root.join("cli-config.yml"),
            root.join("config.yaml"),
            root.join("config.yml"),
            root.join(".hermes").join("config.yaml"),
        ]
        .into_iter(),
    );

    let mut data_dir = first_existing_dir(
        [
            root.join("data"),
            root.join(".hermes"),
            root.join("state"),
            root.join(".local").join("share").join("hermes-agent"),
        ]
        .into_iter(),
    );

    let database_path = first_existing_file(
        [
            root.join("genesis.db"),
            root.join("data").join("genesis.db"),
            root.join(".hermes").join("genesis.db"),
            data_dir
                .as_ref()
                .map(|dir| dir.join("genesis.db"))
                .unwrap_or_else(|| root.join("__missing__")),
        ]
        .into_iter(),
    );

    if data_dir.is_none() {
        data_dir = database_path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf));
    }

    LegacyImportSource {
        root: root.to_path_buf(),
        config_path,
        data_dir,
        database_path,
    }
}

pub fn record_import_run(
    database_path: &Path,
    source: &LegacyImportSource,
    status: ImportStatus,
) -> Result<ImportRun, StorageError> {
    let connection = open(database_path)?;

    connection
        .execute(
            "
            INSERT INTO legacy_import_runs (
                legacy_root,
                legacy_config_path,
                legacy_data_dir,
                legacy_database_path,
                status
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                source.root.display().to_string(),
                source
                    .config_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                source
                    .data_dir
                    .as_ref()
                    .map(|path| path.display().to_string()),
                source
                    .database_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                status.as_str(),
            ],
        )
        .map_err(|source_error| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source: source_error,
        })?;

    let id = connection.last_insert_rowid();

    Ok(ImportRun {
        id,
        legacy_root: source.root.clone(),
        legacy_config_path: source.config_path.clone(),
        legacy_data_dir: source.data_dir.clone(),
        legacy_database_path: source.database_path.clone(),
        status,
    })
}

pub fn latest_import_run(database_path: &Path) -> Result<Option<ImportRun>, StorageError> {
    let connection = open(database_path)?;

    connection
        .query_row(
            "
            SELECT
                id,
                legacy_root,
                legacy_config_path,
                legacy_data_dir,
                legacy_database_path,
                status
            FROM legacy_import_runs
            ORDER BY id DESC
            LIMIT 1
            ",
            [],
            |row| {
                Ok(ImportRun {
                    id: row.get(0)?,
                    legacy_root: PathBuf::from(row.get::<_, String>(1)?),
                    legacy_config_path: row.get::<_, Option<String>>(2)?.map(PathBuf::from),
                    legacy_data_dir: row.get::<_, Option<String>>(3)?.map(PathBuf::from),
                    legacy_database_path: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
                    status: ImportStatus::from_db_value(&row.get::<_, String>(5)?).map_err(
                        |status_error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(status_error),
                            )
                        },
                    )?,
                })
            },
        )
        .optional()
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })
}

/// A persisted conversation message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls_json: Option<String>,
    /// Whether this message is a delivery mirror (cross-platform visibility record).
    #[serde(default)]
    pub mirror: bool,
    /// Source label for mirrored messages (e.g. "cli", "telegram", "api").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_source: Option<String>,
    /// Provider-specific metadata (e.g. codex reasoning blobs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<String>,
    pub created_at: String,
}

/// Summary of a session returned from search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    pub platform: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub parent_session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A message search result — matching message with session context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageSearchResult {
    pub session_id: String,
    pub session_title: Option<String>,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

/// Aggregate token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageStats {
    pub total_sessions: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// Usage insights for a period — sessions per day, platform breakdown, etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InsightsData {
    pub period_days: u32,
    pub sessions_count: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    /// Sessions per day (date string → count).
    pub sessions_per_day: Vec<(String, u64)>,
    /// Sessions per platform (platform → count).
    pub platform_breakdown: Vec<(String, u64)>,
    /// Token usage per day (date, input_tokens, output_tokens).
    pub tokens_per_day: Vec<(String, u64, u64)>,
    /// Tool usage breakdown (tool_name → call count), sorted by frequency.
    pub tool_usage: Vec<(String, u64)>,
    /// Average input tokens per session.
    pub avg_input_tokens: u64,
    /// Average output tokens per session.
    pub avg_output_tokens: u64,
}

/// Session persistence layer.
pub struct SessionStore {
    database_path: PathBuf,
}

impl SessionStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Returns the database path this store is using.
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Map a database row to a `SessionSummary`.
    ///
    /// Expects columns in order: id, title, platform, total_input_tokens,
    /// total_output_tokens, parent_session_id, created_at, updated_at.
    fn row_to_session_summary(row: &rusqlite::Row) -> rusqlite::Result<SessionSummary> {
        Ok(SessionSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            platform: row.get(2)?,
            total_input_tokens: row.get(3)?,
            total_output_tokens: row.get(4)?,
            parent_session_id: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    /// Create a new session record.
    pub fn create_session(
        &self,
        id: &str,
        platform: &str,
        title: Option<&str>,
    ) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT INTO sessions (id, title, platform, created_at, updated_at)
                 VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
                params![id, title, platform],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Append a message to a session and index its content for search.
    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls_json: Option<&str>,
        provider_metadata: Option<&str>,
    ) -> Result<i64, StorageError> {
        let connection = open(&self.database_path)?;

        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls_json, provider_metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![session_id, role, content, tool_call_id, tool_calls_json, provider_metadata],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let message_id = connection.last_insert_rowid();

        // Index searchable content in FTS5
        if let Some(text) = content {
            if !text.is_empty() {
                connection
                    .execute(
                        "INSERT INTO session_search (session_id, content) VALUES (?1, ?2)",
                        params![session_id, text],
                    )
                    .map_err(|source| StorageError::Sqlite {
                        path: self.database_path.clone(),
                        source,
                    })?;
            }
        }

        // Touch session updated_at
        connection
            .execute(
                "UPDATE sessions SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(message_id)
    }

    /// Append a delivery-mirror message to a session's transcript.
    ///
    /// Mirror messages record what was sent to a platform, giving the agent
    /// cross-platform visibility into dispatched content. They are stored as
    /// assistant messages with `mirror = true` and a `mirror_source` label.
    pub fn append_mirror_message(
        &self,
        session_id: &str,
        content: &str,
        mirror_source: &str,
    ) -> Result<i64, StorageError> {
        let connection = open(&self.database_path)?;

        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, mirror, mirror_source, created_at)
                 VALUES (?1, 'assistant', ?2, 1, ?3, CURRENT_TIMESTAMP)",
                params![session_id, content, mirror_source],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let message_id = connection.last_insert_rowid();

        // Touch session updated_at
        connection
            .execute(
                "UPDATE sessions SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(message_id)
    }

    /// Find the most recently updated session for a given platform and chat ID.
    ///
    /// Platform handlers construct deterministic session IDs from platform + chat_id
    /// (e.g. `tg-12345`, `slack-C01ABC`, `discord-98765`, `wa-15551234`).
    /// This method looks up a session by its expected ID pattern.
    pub fn find_session_by_platform_chat_id(
        &self,
        platform: &str,
        chat_id: &str,
    ) -> Result<Option<SessionSummary>, StorageError> {
        let session_id = match platform {
            "telegram" => format!("tg-{chat_id}"),
            "slack" => format!("slack-{chat_id}"),
            "discord" => format!("discord-{chat_id}"),
            "whatsapp" => format!("wa-{chat_id}"),
            "homeassistant" => format!("ha-{chat_id}"),
            _ => format!("{platform}-{chat_id}"),
        };

        self.get_session(&session_id)
    }

    /// Atomically add token usage to a session's running totals.
    pub fn add_usage(
        &self,
        session_id: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<(), StorageError> {
        if input_tokens == 0 && output_tokens == 0 {
            return Ok(());
        }
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "UPDATE sessions SET
                    total_input_tokens = total_input_tokens + ?2,
                    total_output_tokens = total_output_tokens + ?3,
                    updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                params![session_id, input_tokens as i64, output_tokens as i64],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Set the title on an existing session (only if currently null).
    pub fn update_title(&self, session_id: &str, title: &str) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "UPDATE sessions SET title = ?2, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1 AND title IS NULL",
                params![session_id, title],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Set the session title unconditionally (overwrites any existing title).
    pub fn set_title(&self, session_id: &str, title: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "UPDATE sessions SET title = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![session_id, title],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Get tags for a session (comma-separated string parsed into Vec).
    pub fn get_tags(&self, session_id: &str) -> Result<Vec<String>, StorageError> {
        let connection = open(&self.database_path)?;
        let tags: String = connection
            .query_row(
                "SELECT tags FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(if tags.is_empty() {
            Vec::new()
        } else {
            tags.split(',').map(|t| t.trim().to_owned()).collect()
        })
    }

    /// Set tags for a session (replaces existing tags).
    pub fn set_tags(&self, session_id: &str, tags: &[&str]) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let tags_str = tags.join(",");
        let rows = connection
            .execute(
                "UPDATE sessions SET tags = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![session_id, tags_str],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Add a tag to a session (no-op if already present).
    pub fn add_tag(&self, session_id: &str, tag: &str) -> Result<bool, StorageError> {
        let mut tags = self.get_tags(session_id)?;
        if tags.iter().any(|t| t == tag) {
            return Ok(false); // Already has this tag
        }
        tags.push(tag.to_owned());
        let tags_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
        self.set_tags(session_id, &tags_refs)
    }

    /// Remove a tag from a session.
    pub fn remove_tag(&self, session_id: &str, tag: &str) -> Result<bool, StorageError> {
        let tags = self.get_tags(session_id)?;
        let filtered: Vec<&str> = tags
            .iter()
            .filter(|t| t.as_str() != tag)
            .map(|t| t.as_str())
            .collect();
        if filtered.len() == tags.len() {
            return Ok(false); // Tag wasn't present
        }
        self.set_tags(session_id, &filtered)
    }

    /// List sessions that have a specific tag.
    pub fn sessions_by_tag(&self, tag: &str) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = open(&self.database_path)?;
        let pattern = format!("%{tag}%");
        let mut stmt = connection
            .prepare(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at
                 FROM sessions WHERE tags LIKE ?1
                 ORDER BY updated_at DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![pattern], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path)
    }

    /// List all sessions that were forked from a given parent session.
    pub fn list_children(&self, parent_id: &str) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens,
                        parent_session_id, created_at, updated_at
                 FROM sessions WHERE parent_session_id = ?1
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![parent_id], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path)
    }

    /// Delete a session and all its messages and search index entries.
    pub fn delete_session(&self, session_id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        // Delete FTS entries (virtual table — no ON DELETE CASCADE support).
        connection
            .execute(
                "DELETE FROM session_search WHERE session_id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        // Delete session; ON DELETE CASCADE removes associated messages.
        let deleted = connection
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(deleted > 0)
    }

    /// Fork a session: create a new session that branches from the source
    /// session, copying all messages up to the current point. Returns the new
    /// session ID.
    pub fn fork_session(
        &self,
        source_session_id: &str,
        new_session_id: &str,
    ) -> Result<String, StorageError> {
        let connection = open(&self.database_path)?;

        // Get source session info
        let (platform, title): (String, Option<String>) = connection
            .query_row(
                "SELECT platform, title FROM sessions WHERE id = ?1",
                params![source_session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let fork_title = title
            .as_deref()
            .map(|t| format!("{t} (fork)"))
            .unwrap_or_else(|| "Fork".to_owned());

        // Create the new session with parent_session_id
        connection
            .execute(
                "INSERT INTO sessions (id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 0, 0, ?4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
                params![new_session_id, fork_title, platform, source_session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        // Copy all messages from the source session
        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls_json, mirror, mirror_source, created_at)
                 SELECT ?1, role, content, tool_call_id, tool_calls_json, mirror, mirror_source, created_at
                 FROM messages WHERE session_id = ?2
                 ORDER BY id ASC",
                params![new_session_id, source_session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(new_session_id.to_owned())
    }

    /// Delete sessions (and their messages/FTS entries) older than `days` days.
    /// Returns the number of sessions deleted.
    pub fn purge_older_than(&self, days: u32) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        let cutoff = format!("-{days} days");

        let tx = connection
            .unchecked_transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        // Remove FTS entries (virtual table, no FK support).
        tx.execute(
            "DELETE FROM session_search WHERE session_id IN (
                 SELECT id FROM sessions WHERE created_at < datetime('now', ?1)
             )",
            params![cutoff],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;

        // Delete sessions; ON DELETE CASCADE handles messages.
        tx.execute(
            "DELETE FROM sessions WHERE created_at < datetime('now', ?1)",
            params![cutoff],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;

        let deleted = tx.changes() as u64;

        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;

        Ok(deleted)
    }

    /// Load all messages for a session in chronological order.
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>, StorageError> {
        let connection = open(&self.database_path)?;

        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, mirror, mirror_source, provider_metadata, created_at
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY id ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    tool_call_id: row.get(4)?,
                    tool_calls_json: row.get(5)?,
                    mirror: row.get::<_, i64>(6)? != 0,
                    mirror_source: row.get(7)?,
                    provider_metadata: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Delete messages older than the N most recent for a session.
    /// Returns the number of messages deleted.
    pub fn truncate_messages(
        &self,
        session_id: &str,
        keep_recent: usize,
    ) -> Result<usize, StorageError> {
        let connection = open(&self.database_path)?;

        // Find the ID threshold: keep messages with the highest IDs.
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let to_delete = count as usize - keep_recent.min(count as usize);
        if to_delete == 0 {
            return Ok(0);
        }

        connection
            .execute(
                "DELETE FROM messages WHERE session_id = ?1 AND id IN (
                    SELECT id FROM messages WHERE session_id = ?1
                    ORDER BY id ASC LIMIT ?2
                )",
                params![session_id, to_delete],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(to_delete)
    }

    /// Delete the most recent `n` messages from a session (ordered by row ID).
    /// Returns the number of messages actually deleted.
    pub fn delete_last_n_messages(
        &self,
        session_id: &str,
        n: usize,
    ) -> Result<usize, StorageError> {
        if n == 0 {
            return Ok(0);
        }
        let connection = open(&self.database_path)?;
        let deleted = connection
            .execute(
                "DELETE FROM messages WHERE session_id = ?1 AND id IN (
                    SELECT id FROM messages WHERE session_id = ?1
                    ORDER BY id DESC LIMIT ?2
                )",
                params![session_id, n],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(deleted)
    }

    /// Full-text search across session content. Returns matching session IDs
    /// with their summaries, ordered by relevance.
    pub fn search_sessions(&self, query: &str) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = open(&self.database_path)?;

        let mut stmt = connection
            .prepare(
                "SELECT DISTINCT s.id, s.title, s.platform, s.total_input_tokens, s.total_output_tokens, s.parent_session_id, s.created_at, s.updated_at
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1
                 ORDER BY rank",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![query], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Search message content across all sessions using FTS5.
    ///
    /// Returns matching messages with their session context, ordered by
    /// relevance. Limited to `max_results` entries.
    pub fn search_messages(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<MessageSearchResult>, StorageError> {
        let connection = open(&self.database_path)?;

        let mut stmt = connection
            .prepare(
                "SELECT ss.session_id, s.title, ss.content
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![query, max_results as i64], |row| {
                Ok(MessageSearchResult {
                    session_id: row.get(0)?,
                    session_title: row.get(1)?,
                    role: String::new(), // FTS doesn't store role
                    content: row.get(2)?,
                    created_at: String::new(), // FTS doesn't store timestamp
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Count total sessions.
    pub fn session_count(&self) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// Get a session summary by ID.
    pub fn get_session(&self, id: &str) -> Result<Option<SessionSummary>, StorageError> {
        let connection = open(&self.database_path)?;

        connection
            .query_row(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id],
                Self::row_to_session_summary,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    pub fn list_recent_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at
                 FROM sessions
                 ORDER BY updated_at DESC, created_at DESC, id DESC
                 LIMIT ?1",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// List recent sessions with offset/limit pagination.
    pub fn list_recent_sessions_paginated(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<SessionSummary>, u64), StorageError> {
        let connection = open(&self.database_path)?;

        let total: i64 = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let mut stmt = connection
            .prepare(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at
                 FROM sessions
                 ORDER BY updated_at DESC, created_at DESC, id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let items = collect_rows(rows, &self.database_path)?;
        Ok((items, total as u64))
    }

    /// Search sessions with offset/limit pagination.
    pub fn search_sessions_paginated(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<SessionSummary>, u64), StorageError> {
        let connection = open(&self.database_path)?;

        let total: i64 = connection
            .query_row(
                "SELECT COUNT(DISTINCT s.id)
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1",
                params![query],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let mut stmt = connection
            .prepare(
                "SELECT DISTINCT s.id, s.title, s.platform, s.total_input_tokens, s.total_output_tokens, s.parent_session_id, s.created_at, s.updated_at
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1
                 ORDER BY rank
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![query, limit as i64, offset as i64], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let items = collect_rows(rows, &self.database_path)?;
        Ok((items, total as u64))
    }

    /// Count total number of sessions.
    pub fn count_sessions(&self) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(count as u64)
    }

    /// Aggregate token usage across all sessions.
    pub fn usage_stats(&self) -> Result<UsageStats, StorageError> {
        let connection = open(&self.database_path)?;
        let (count, input, output): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(total_input_tokens), 0), COALESCE(SUM(total_output_tokens), 0) FROM sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(UsageStats {
            total_sessions: count as u64,
            total_input_tokens: input as u64,
            total_output_tokens: output as u64,
        })
    }

    /// Gather usage insights for the last N days.
    pub fn insights(&self, days: u32) -> Result<InsightsData, StorageError> {
        let connection = open(&self.database_path)?;
        let period = format!("-{days} days");

        // Aggregate totals for the period
        let (count, input, output): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(total_input_tokens), 0), COALESCE(SUM(total_output_tokens), 0) \
                 FROM sessions WHERE created_at >= datetime('now', ?)",
                [&period],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        // Sessions per day
        let mut stmt = connection
            .prepare(
                "SELECT date(created_at) as day, COUNT(*) \
                 FROM sessions WHERE created_at >= datetime('now', ?) \
                 GROUP BY day ORDER BY day",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let sessions_per_day: Vec<(String, u64)> = stmt
            .query_map([&period], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        // Platform breakdown
        let mut stmt = connection
            .prepare(
                "SELECT platform, COUNT(*) \
                 FROM sessions WHERE created_at >= datetime('now', ?) \
                 GROUP BY platform ORDER BY COUNT(*) DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let platform_breakdown: Vec<(String, u64)> = stmt
            .query_map([&period], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        // Tokens per day
        let mut stmt = connection
            .prepare(
                "SELECT date(created_at) as day, \
                 COALESCE(SUM(total_input_tokens), 0), \
                 COALESCE(SUM(total_output_tokens), 0) \
                 FROM sessions WHERE created_at >= datetime('now', ?) \
                 GROUP BY day ORDER BY day",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let tokens_per_day: Vec<(String, u64, u64)> = stmt
            .query_map([&period], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                ))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        // Tool usage breakdown — extract tool names from tool_calls_json
        let mut stmt = connection
            .prepare(
                "SELECT tool_calls_json FROM messages m \
                 JOIN sessions s ON m.session_id = s.id \
                 WHERE m.role = 'assistant' AND m.tool_calls_json IS NOT NULL \
                 AND s.created_at >= datetime('now', ?)",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let tool_jsons: Vec<String> = stmt
            .query_map([&period], |row| row.get::<_, String>(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let mut tool_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for json_str in &tool_jsons {
            if let Ok(calls) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                for call in calls {
                    if let Some(name) = call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .or_else(|| call.get("name").and_then(|n| n.as_str()))
                    {
                        *tool_counts.entry(name.to_owned()).or_insert(0) += 1;
                    }
                }
            }
        }
        let mut tool_usage: Vec<(String, u64)> = tool_counts.into_iter().collect();
        tool_usage.sort_by(|a, b| b.1.cmp(&a.1));

        let sessions_count = count as u64;
        let avg_input = if sessions_count > 0 {
            input as u64 / sessions_count
        } else {
            0
        };
        let avg_output = if sessions_count > 0 {
            output as u64 / sessions_count
        } else {
            0
        };

        Ok(InsightsData {
            period_days: days,
            sessions_count,
            total_input_tokens: input as u64,
            total_output_tokens: output as u64,
            sessions_per_day,
            platform_breakdown,
            tokens_per_day,
            tool_usage,
            avg_input_tokens: avg_input,
            avg_output_tokens: avg_output,
        })
    }

    /// Import a sequence of messages into a new session.
    ///
    /// Creates a session with the given ID and title, then inserts each
    /// (role, content) pair as a message. Returns the session ID.
    pub fn import_session(
        &self,
        session_id: &str,
        title: Option<&str>,
        messages: Vec<(String, String)>,
    ) -> Result<String, StorageError> {
        self.create_session(session_id, "import", title)?;

        for (role, content) in &messages {
            self.append_message(session_id, role, Some(content), None, None, None)?;
        }

        Ok(session_id.to_owned())
    }
}

/// A persisted scheduled job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSchedule {
    pub id: String,
    pub cron_expression: String,
    pub destination: String,
    pub prompt: String,
    pub enabled: bool,
    pub created_at: String,
    /// IANA timezone name (e.g. "America/New_York"). `None` means UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// A record of a schedule execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleExecution {
    pub id: i64,
    pub schedule_id: String,
    pub executed_at: String,
    /// "success" or "error"
    pub status: String,
    pub error_message: Option<String>,
    pub duration_ms: Option<i64>,
}

/// Schedule persistence layer.
pub struct ScheduleStore {
    database_path: PathBuf,
}

impl ScheduleStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Create a new scheduled job.
    pub fn create(
        &self,
        id: &str,
        cron_expression: &str,
        destination: &str,
        prompt: &str,
    ) -> Result<StoredSchedule, StorageError> {
        self.create_with_timezone(id, cron_expression, destination, prompt, None)
    }

    /// Create a new scheduled job with an explicit timezone.
    pub fn create_with_timezone(
        &self,
        id: &str,
        cron_expression: &str,
        destination: &str,
        prompt: &str,
        timezone: Option<&str>,
    ) -> Result<StoredSchedule, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT INTO schedules (id, cron_expression, destination, prompt, enabled, created_at, timezone)
                 VALUES (?1, ?2, ?3, ?4, 1, CURRENT_TIMESTAMP, ?5)",
                params![id, cron_expression, destination, prompt, timezone],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        self.get(id)?.ok_or_else(|| StorageError::Sqlite {
            path: self.database_path.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Get a schedule by ID.
    pub fn get(&self, id: &str) -> Result<Option<StoredSchedule>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT id, cron_expression, destination, prompt, enabled, created_at, timezone
                 FROM schedules WHERE id = ?1",
                params![id],
                |row| {
                    Ok(StoredSchedule {
                        id: row.get(0)?,
                        cron_expression: row.get(1)?,
                        destination: row.get(2)?,
                        prompt: row.get(3)?,
                        enabled: row.get::<_, i64>(4)? != 0,
                        created_at: row.get(5)?,
                        timezone: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// List all enabled schedules.
    pub fn list_enabled(&self) -> Result<Vec<StoredSchedule>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, cron_expression, destination, prompt, enabled, created_at, timezone
                 FROM schedules WHERE enabled = 1
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map([], |row| {
                Ok(StoredSchedule {
                    id: row.get(0)?,
                    cron_expression: row.get(1)?,
                    destination: row.get(2)?,
                    prompt: row.get(3)?,
                    enabled: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                    timezone: row.get(6)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// List all schedules (enabled and disabled).
    pub fn list_all(&self) -> Result<Vec<StoredSchedule>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, cron_expression, destination, prompt, enabled, created_at, timezone
                 FROM schedules
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map([], |row| {
                Ok(StoredSchedule {
                    id: row.get(0)?,
                    cron_expression: row.get(1)?,
                    destination: row.get(2)?,
                    prompt: row.get(3)?,
                    enabled: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                    timezone: row.get(6)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// List schedules with offset/limit pagination.
    ///
    /// When `enabled_only` is `true`, only enabled schedules are returned and
    /// the total count reflects the enabled subset.
    pub fn list_paginated(
        &self,
        enabled_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<StoredSchedule>, u64), StorageError> {
        let connection = open(&self.database_path)?;
        let db = &self.database_path;
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.clone(),
            source,
        };

        let (count_sql, data_sql) = if enabled_only {
            (
                "SELECT COUNT(*) FROM schedules WHERE enabled = 1",
                "SELECT id, cron_expression, destination, prompt, enabled, created_at
                 FROM schedules WHERE enabled = 1
                 ORDER BY created_at ASC
                 LIMIT ?1 OFFSET ?2",
            )
        } else {
            (
                "SELECT COUNT(*) FROM schedules",
                "SELECT id, cron_expression, destination, prompt, enabled, created_at
                 FROM schedules
                 ORDER BY created_at ASC
                 LIMIT ?1 OFFSET ?2",
            )
        };

        let total: i64 = connection
            .query_row(count_sql, [], |row| row.get(0))
            .map_err(me)?;

        let mut stmt = connection.prepare(data_sql).map_err(me)?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                Ok(StoredSchedule {
                    id: row.get(0)?,
                    cron_expression: row.get(1)?,
                    destination: row.get(2)?,
                    prompt: row.get(3)?,
                    enabled: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                })
            })
            .map_err(me)?;

        let items = collect_rows(rows, &self.database_path)?;
        Ok((items, total as u64))
    }

    /// Enable or disable a schedule.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows_changed = connection
            .execute(
                "UPDATE schedules SET enabled = ?2 WHERE id = ?1",
                params![id, enabled as i64],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows_changed > 0)
    }

    /// Delete a schedule by ID.
    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows_changed = connection
            .execute("DELETE FROM schedules WHERE id = ?1", params![id])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows_changed > 0)
    }

    /// Record an execution of a schedule.
    pub fn record_execution(
        &self,
        schedule_id: &str,
        status: &str,
        error_message: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT INTO schedule_executions (schedule_id, executed_at, status, error_message, duration_ms)
                 VALUES (?1, CURRENT_TIMESTAMP, ?2, ?3, ?4)",
                params![schedule_id, status, error_message, duration_ms],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// List recent executions for a schedule, most recent first.
    pub fn list_executions(
        &self,
        schedule_id: &str,
        limit: usize,
    ) -> Result<Vec<ScheduleExecution>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, schedule_id, executed_at, status, error_message, duration_ms
                 FROM schedule_executions WHERE schedule_id = ?1
                 ORDER BY executed_at DESC LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![schedule_id, limit as i64], |row| {
                Ok(ScheduleExecution {
                    id: row.get(0)?,
                    schedule_id: row.get(1)?,
                    executed_at: row.get(2)?,
                    status: row.get(3)?,
                    error_message: row.get(4)?,
                    duration_ms: row.get(5)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }
}

/// A stored user trait — an observation about the user's preferences, personality, or goals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredUserTrait {
    pub trait_key: String,
    pub category: String,
    pub value: String,
    pub confidence: f64,
    pub evidence_count: i64,
    pub source_session: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Format user traits as a markdown section, grouped by category.
/// Returns `None` if the list is empty.
pub fn format_user_traits(traits: &[StoredUserTrait]) -> Option<String> {
    if traits.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    let mut current_category = String::new();

    for t in traits {
        if t.category != current_category {
            if !current_category.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("## {}", t.category));
            current_category.clone_from(&t.category);
        }
        lines.push(format!(
            "- **{}**: {} (confidence: {:.0}%, {} observations)",
            t.trait_key,
            t.value,
            t.confidence * 100.0,
            t.evidence_count,
        ));
    }

    Some(lines.join("\n"))
}

/// User model persistence layer.
///
/// Stores observations about the user that the agent learns over time.
/// Categories include: preference, personality, communication_style, goal, expertise, context.
pub struct UserModelStore {
    database_path: PathBuf,
}

impl UserModelStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Record or update a user trait. If the trait already exists, its confidence
    /// is increased and evidence count bumped.
    pub fn observe(
        &self,
        trait_key: &str,
        category: &str,
        value: &str,
        source_session: Option<&str>,
    ) -> Result<StoredUserTrait, StorageError> {
        let connection = open(&self.database_path)?;

        // Clamp confidence at 1.0, increase by 0.1 per observation
        connection
            .execute(
                "INSERT INTO user_model (trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 0.5, 1, ?4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(trait_key) DO UPDATE SET
                     value = excluded.value,
                     confidence = MIN(1.0, user_model.confidence + 0.1),
                     evidence_count = user_model.evidence_count + 1,
                     source_session = excluded.source_session,
                     updated_at = CURRENT_TIMESTAMP",
                params![trait_key, category, value, source_session],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        self.get(trait_key)?.ok_or_else(|| StorageError::Sqlite {
            path: self.database_path.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Get a specific user trait by key.
    pub fn get(&self, trait_key: &str) -> Result<Option<StoredUserTrait>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE trait_key = ?1",
                params![trait_key],
                Self::row_to_trait,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// List all user traits, ordered by confidence (highest first).
    pub fn list_all(&self) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model ORDER BY confidence DESC, evidence_count DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows =
            stmt.query_map([], Self::row_to_trait)
                .map_err(|source| StorageError::Sqlite {
                    path: self.database_path.clone(),
                    source,
                })?;

        collect_rows(rows, &self.database_path)
    }

    /// List traits in a specific category.
    pub fn list_by_category(&self, category: &str) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE category = ?1 ORDER BY confidence DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![category], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Get high-confidence traits (>= threshold) for prompt injection.
    pub fn confident_traits(&self, threshold: f64) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE confidence >= ?1 ORDER BY confidence DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![threshold], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// List user traits with offset/limit pagination.
    ///
    /// Supports optional `category` and `min_confidence` filters.  The returned
    /// total count reflects the filtered subset.
    pub fn list_paginated(
        &self,
        category: Option<&str>,
        min_confidence: Option<f64>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<StoredUserTrait>, u64), StorageError> {
        let connection = open(&self.database_path)?;
        let db = &self.database_path;
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.clone(),
            source,
        };

        let (count_sql, data_sql, total): (&str, &str, i64) = if let Some(cat) = category {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM user_model WHERE category = ?1",
                    params![cat],
                    |row| row.get(0),
                )
                .map_err(me)?;
            (
                // not used below, just for symmetry
                "",
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE category = ?1 ORDER BY confidence DESC
                 LIMIT ?2 OFFSET ?3",
                t,
            )
        } else if let Some(threshold) = min_confidence {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM user_model WHERE confidence >= ?1",
                    params![threshold],
                    |row| row.get(0),
                )
                .map_err(me)?;
            (
                "",
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE confidence >= ?1 ORDER BY confidence DESC
                 LIMIT ?2 OFFSET ?3",
                t,
            )
        } else {
            let t: i64 = connection
                .query_row("SELECT COUNT(*) FROM user_model", [], |row| row.get(0))
                .map_err(me)?;
            (
                "",
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model ORDER BY confidence DESC, evidence_count DESC
                 LIMIT ?1 OFFSET ?2",
                t,
            )
        };

        // Suppress unused-variable warning for count_sql (kept for readability).
        let _ = count_sql;

        let mut stmt = connection.prepare(data_sql).map_err(me)?;
        let items = if let Some(cat) = category {
            let rows = stmt
                .query_map(params![cat, limit as i64, offset as i64], Self::row_to_trait)
                .map_err(me)?;
            collect_rows(rows, &self.database_path)?
        } else if let Some(threshold) = min_confidence {
            let rows = stmt
                .query_map(
                    params![threshold, limit as i64, offset as i64],
                    Self::row_to_trait,
                )
                .map_err(me)?;
            collect_rows(rows, &self.database_path)?
        } else {
            let rows = stmt
                .query_map(params![limit as i64, offset as i64], Self::row_to_trait)
                .map_err(me)?;
            collect_rows(rows, &self.database_path)?
        };

        Ok((items, total as u64))
    }

    /// Delete a user trait.
    pub fn delete(&self, trait_key: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "DELETE FROM user_model WHERE trait_key = ?1",
                params![trait_key],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    fn row_to_trait(row: &rusqlite::Row) -> Result<StoredUserTrait, rusqlite::Error> {
        Ok(StoredUserTrait {
            trait_key: row.get(0)?,
            category: row.get(1)?,
            value: row.get(2)?,
            confidence: row.get(3)?,
            evidence_count: row.get(4)?,
            source_session: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }
}

/// A stored agent skill — a reusable procedure the agent can invoke.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSkill {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub trigger_hint: Option<String>,
    pub tags: Vec<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Skill persistence layer.
pub struct SkillStore {
    database_path: PathBuf,
}

impl SkillStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    fn row_to_skill(row: &rusqlite::Row) -> Result<StoredSkill, rusqlite::Error> {
        let tags_str: String = row.get(4)?;
        let tags = if tags_str.is_empty() {
            Vec::new()
        } else {
            tags_str.split(',').map(|s| s.to_owned()).collect()
        };

        Ok(StoredSkill {
            name: row.get(0)?,
            description: row.get(1)?,
            instructions: row.get(2)?,
            trigger_hint: row.get(3)?,
            tags,
            version: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    /// Create or update a skill. If the skill already exists, its version is bumped.
    pub fn upsert(
        &self,
        name: &str,
        description: &str,
        instructions: &str,
        trigger_hint: Option<&str>,
        tags: &[&str],
    ) -> Result<StoredSkill, StorageError> {
        let connection = open(&self.database_path)?;
        let tags_str = tags.join(",");

        connection
            .execute(
                "INSERT INTO skills (name, description, instructions, trigger_hint, tags, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(name) DO UPDATE SET
                     description = excluded.description,
                     instructions = excluded.instructions,
                     trigger_hint = excluded.trigger_hint,
                     tags = excluded.tags,
                     version = skills.version + 1,
                     updated_at = CURRENT_TIMESTAMP",
                params![name, description, instructions, trigger_hint, tags_str],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        self.get(name)?.ok_or_else(|| StorageError::Sqlite {
            path: self.database_path.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Get a skill by name.
    pub fn get(&self, name: &str) -> Result<Option<StoredSkill>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT name, description, instructions, trigger_hint, tags, version, created_at, updated_at
                 FROM skills WHERE name = ?1",
                params![name],
                Self::row_to_skill,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// List all skills, ordered by name.
    pub fn list_all(&self) -> Result<Vec<StoredSkill>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT name, description, instructions, trigger_hint, tags, version, created_at, updated_at
                 FROM skills ORDER BY name ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows =
            stmt.query_map([], Self::row_to_skill)
                .map_err(|source| StorageError::Sqlite {
                    path: self.database_path.clone(),
                    source,
                })?;

        collect_rows(rows, &self.database_path)
    }

    /// List all skills with offset/limit pagination.
    pub fn list_all_paginated(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<StoredSkill>, u64), StorageError> {
        let connection = open(&self.database_path)?;

        let total: i64 = connection
            .query_row("SELECT COUNT(*) FROM skills", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let mut stmt = connection
            .prepare(
                "SELECT name, description, instructions, trigger_hint, tags, version, created_at, updated_at
                 FROM skills ORDER BY name ASC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64], Self::row_to_skill)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let items = collect_rows(rows, &self.database_path)?;
        Ok((items, total as u64))
    }

    /// Find skills matching any of the given tags.
    pub fn find_by_tag(&self, tag: &str) -> Result<Vec<StoredSkill>, StorageError> {
        let connection = open(&self.database_path)?;
        // SQLite LIKE with comma-separated tags
        let pattern = format!("%{tag}%");
        let mut stmt = connection
            .prepare(
                "SELECT name, description, instructions, trigger_hint, tags, version, created_at, updated_at
                 FROM skills WHERE tags LIKE ?1 ORDER BY name ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![pattern], Self::row_to_skill)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Find skills whose trigger hints, names, descriptions, or tags match the
    /// given user prompt. Uses simple keyword overlap scoring to rank results.
    /// Returns up to `limit` matching skills, ordered by relevance score.
    pub fn find_matching(
        &self,
        prompt: &str,
        limit: usize,
    ) -> Result<Vec<StoredSkill>, StorageError> {
        let all_skills = self.list_all()?;
        if all_skills.is_empty() {
            return Ok(Vec::new());
        }

        // Tokenize the prompt into lowercase words.
        let prompt_words: Vec<String> = prompt
            .split_whitespace()
            .map(|w| {
                w.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_owned()
            })
            .filter(|w| w.len() >= 2)
            .collect();

        if prompt_words.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(f64, StoredSkill)> = all_skills
            .into_iter()
            .filter_map(|skill| {
                let score = skill_match_score(&skill, &prompt_words);
                if score > 0.0 {
                    Some((score, skill))
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending.
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored.into_iter().map(|(_, skill)| skill).collect())
    }

    /// Delete a skill by name. Returns true if a skill was deleted.
    pub fn delete(&self, name: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows_changed = connection
            .execute("DELETE FROM skills WHERE name = ?1", params![name])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows_changed > 0)
    }
}

/// Supporting files associated with a skill, stored in SQLite.
pub struct SkillFileStore {
    database_path: PathBuf,
}

impl SkillFileStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    pub fn store_file(
        &self,
        skill_name: &str,
        file_path: &str,
        content: &str,
    ) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT INTO skill_files (skill_name, file_path, content, created_at)
                 VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
                 ON CONFLICT(skill_name, file_path) DO UPDATE SET
                    content = excluded.content",
                params![skill_name, file_path, content],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    pub fn get_file(
        &self,
        skill_name: &str,
        file_path: &str,
    ) -> Result<Option<String>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT content FROM skill_files WHERE skill_name = ?1 AND file_path = ?2",
                params![skill_name, file_path],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    pub fn list_files(&self, skill_name: &str) -> Result<Vec<String>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT file_path FROM skill_files WHERE skill_name = ?1 ORDER BY file_path ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![skill_name], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path)
    }

    pub fn delete_file(&self, skill_name: &str, file_path: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "DELETE FROM skill_files WHERE skill_name = ?1 AND file_path = ?2",
                params![skill_name, file_path],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    pub fn delete_all_files(&self, skill_name: &str) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "DELETE FROM skill_files WHERE skill_name = ?1",
                params![skill_name],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows as u64)
    }
}

/// Compute a relevance score for a skill against tokenized prompt words.
/// Higher score = more relevant. Returns 0.0 if no match.
fn skill_match_score(skill: &StoredSkill, prompt_words: &[String]) -> f64 {
    let mut score = 0.0;

    // Build searchable text from the skill's fields.
    let trigger_words: Vec<String> = skill
        .trigger_hint
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();

    let name_words: Vec<String> = skill
        .name
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .collect();

    let desc_words: Vec<String> = skill
        .description
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();

    let tag_words: Vec<String> = skill.tags.iter().map(|t| t.to_lowercase()).collect();

    for word in prompt_words {
        // Trigger hint matches are weighted highest (3x).
        if trigger_words.iter().any(|tw| tw.contains(word.as_str())) {
            score += 3.0;
        }
        // Skill name matches get 2x weight.
        if name_words.iter().any(|nw| nw.contains(word.as_str())) {
            score += 2.0;
        }
        // Tag matches get 2x weight.
        if tag_words.iter().any(|tw| tw.contains(word.as_str())) {
            score += 2.0;
        }
        // Description matches get 1x weight.
        if desc_words.iter().any(|dw| dw.contains(word.as_str())) {
            score += 1.0;
        }
    }

    score
}

/// A stored subagent — a child agent loop spawned by a parent session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSubagent {
    pub id: String,
    pub parent_session_id: String,
    pub child_session_id: String,
    pub name: String,
    pub task: String,
    pub status: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Subagent persistence layer.
pub struct SubagentStore {
    database_path: PathBuf,
}

impl SubagentStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Create a new subagent record with status "pending".
    pub fn create(
        &self,
        id: &str,
        parent_session_id: &str,
        child_session_id: &str,
        name: &str,
        task: &str,
    ) -> Result<StoredSubagent, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT INTO subagents (id, parent_session_id, child_session_id, name, task, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                params![id, parent_session_id, child_session_id, name, task],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        self.get(id)?.ok_or_else(|| StorageError::Sqlite {
            path: self.database_path.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Get a subagent by ID.
    pub fn get(&self, id: &str) -> Result<Option<StoredSubagent>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT id, parent_session_id, child_session_id, name, task, status, result, error, created_at, completed_at
                 FROM subagents WHERE id = ?1",
                params![id],
                Self::row_to_subagent,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// List all subagents for a parent session.
    pub fn list_by_parent(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<StoredSubagent>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, parent_session_id, child_session_id, name, task, status, result, error, created_at, completed_at
                 FROM subagents WHERE parent_session_id = ?1
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![parent_session_id], Self::row_to_subagent)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Mark a subagent as running.
    pub fn set_running(&self, id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "UPDATE subagents SET status = 'running' WHERE id = ?1",
                params![id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Mark a subagent as completed with its result.
    pub fn set_completed(&self, id: &str, result: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "UPDATE subagents SET status = 'completed', result = ?2, completed_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![id, result],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Mark a subagent as failed with an error message.
    pub fn set_failed(&self, id: &str, error: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "UPDATE subagents SET status = 'failed', error = ?2, completed_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![id, error],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    fn row_to_subagent(row: &rusqlite::Row) -> Result<StoredSubagent, rusqlite::Error> {
        Ok(StoredSubagent {
            id: row.get(0)?,
            parent_session_id: row.get(1)?,
            child_session_id: row.get(2)?,
            name: row.get(3)?,
            task: row.get(4)?,
            status: row.get(5)?,
            result: row.get(6)?,
            error: row.get(7)?,
            created_at: row.get(8)?,
            completed_at: row.get(9)?,
        })
    }
}

/// A recorded skill usage — tracks when and how a skill was applied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSkillUsage {
    pub id: i64,
    pub skill_name: String,
    pub session_id: Option<String>,
    /// "success", "partial", "failure", or "unknown"
    pub outcome: String,
    /// Agent's free-text feedback on what worked or didn't
    pub feedback: Option<String>,
    /// Whether the agent refined the skill after this usage
    pub refined: bool,
    pub created_at: String,
}

/// Aggregate stats for a skill's usage history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillUsageStats {
    pub skill_name: String,
    pub total_uses: i64,
    pub successes: i64,
    pub failures: i64,
    pub last_used: Option<String>,
    pub times_refined: i64,
}

/// Skill usage tracking layer.
pub struct SkillUsageStore {
    database_path: PathBuf,
}

impl SkillUsageStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Record that a skill was used in a session.
    pub fn record_usage(
        &self,
        skill_name: &str,
        session_id: Option<&str>,
        outcome: &str,
        feedback: Option<&str>,
    ) -> Result<StoredSkillUsage, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT INTO skill_usages (skill_name, session_id, outcome, feedback)
                 VALUES (?1, ?2, ?3, ?4)",
                params![skill_name, session_id, outcome, feedback],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let id = connection.last_insert_rowid();
        connection
            .query_row(
                "SELECT id, skill_name, session_id, outcome, feedback, refined, created_at
                 FROM skill_usages WHERE id = ?1",
                params![id],
                Self::row_to_usage,
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// Mark a usage record as having led to a skill refinement.
    pub fn mark_refined(&self, usage_id: i64) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "UPDATE skill_usages SET refined = 1 WHERE id = ?1",
                params![usage_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Get aggregate usage stats for a skill.
    pub fn stats(&self, skill_name: &str) -> Result<SkillUsageStats, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT
                    ?1 as skill_name,
                    COUNT(*) as total_uses,
                    SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) as successes,
                    SUM(CASE WHEN outcome = 'failure' THEN 1 ELSE 0 END) as failures,
                    MAX(created_at) as last_used,
                    SUM(refined) as times_refined
                 FROM skill_usages WHERE skill_name = ?1",
                params![skill_name],
                |row| {
                    Ok(SkillUsageStats {
                        skill_name: row.get(0)?,
                        total_uses: row.get(1)?,
                        successes: row.get(2)?,
                        failures: row.get(3)?,
                        last_used: row.get(4)?,
                        times_refined: row.get(5)?,
                    })
                },
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// Get recent usage records for a skill.
    pub fn recent_usages(
        &self,
        skill_name: &str,
        limit: usize,
    ) -> Result<Vec<StoredSkillUsage>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, skill_name, session_id, outcome, feedback, refined, created_at
                 FROM skill_usages WHERE skill_name = ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![skill_name, limit as i64], Self::row_to_usage)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    fn row_to_usage(row: &rusqlite::Row) -> Result<StoredSkillUsage, rusqlite::Error> {
        Ok(StoredSkillUsage {
            id: row.get(0)?,
            skill_name: row.get(1)?,
            session_id: row.get(2)?,
            outcome: row.get(3)?,
            feedback: row.get(4)?,
            refined: row.get::<_, i64>(5)? != 0,
            created_at: row.get(6)?,
        })
    }
}

/// A stored memory (key-value note persisted by the agent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMemory {
    pub id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub content: String,
    pub created_at: String,
}

/// A memory search result with its combined score.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoredMemory {
    pub memory: StoredMemory,
    /// Combined score from hybrid search (higher is better).
    pub score: f64,
    /// Source of the match: "fts", "vector", or "hybrid".
    pub source: String,
}

#[derive(Debug, Clone)]
struct GraphAttributes {
    importance: f32,
    accessed_at: String,
}

#[derive(Debug, Clone)]
struct GraphCandidate {
    memory: StoredMemory,
    attributes: GraphAttributes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewMemoryNote {
    pub id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub content: String,
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    pub linked_ids: Vec<String>,
    pub importance: f32,
}

/// Memory persistence layer.
pub struct MemoryStore {
    database_path: PathBuf,
}

impl MemoryStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Map a database row to a `StoredMemory`.
    ///
    /// Expects columns in order: id, session_id, kind, content, created_at.
    fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<StoredMemory> {
        Ok(StoredMemory {
            id: row.get(0)?,
            session_id: row.get(1)?,
            kind: row.get(2)?,
            content: row.get(3)?,
            created_at: row.get(4)?,
        })
    }

    /// List all stored memories, most recent first.
    pub fn list(&self, limit: usize) -> Result<Vec<StoredMemory>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, kind, content, created_at
                 FROM memories ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![limit as i64], Self::row_to_memory)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path)
    }

    /// List stored memories with offset/limit pagination.
    pub fn list_paginated(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<StoredMemory>, u64), StorageError> {
        let connection = open(&self.database_path)?;

        let total: i64 = connection
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, kind, content, created_at
                 FROM memories ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64], Self::row_to_memory)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let items = collect_rows(rows, &self.database_path)?;
        Ok((items, total as u64))
    }

    /// Get a single memory by ID. Returns `None` if not found.
    pub fn get(&self, id: &str) -> Result<Option<StoredMemory>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT id, session_id, kind, content, created_at
                 FROM memories WHERE id = ?1",
                params![id],
                Self::row_to_memory,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// Full-text search across stored memories.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<StoredMemory>, StorageError> {
        let connection = open(&self.database_path)?;

        // Ensure FTS index exists (memory tools also create it, but this is defensive)
        exec_migration(
            &connection,
            &self.database_path,
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )?;

        let mut stmt = connection
            .prepare(
                "SELECT m.id, m.session_id, m.kind, m.content, m.created_at
                 FROM memory_search ms
                 JOIN memories m ON m.rowid = ms.memory_row_id
                 WHERE memory_search MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![query, limit as i64], Self::row_to_memory)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path)
    }

    /// Vector similarity search across stored memory embeddings.
    pub fn vector_search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<ScoredMemory>, StorageError> {
        if limit == 0 || query_embedding.is_empty() {
            return Ok(Vec::new());
        }

        let connection = open(&self.database_path)?;
        let Some(expected_dimensions) =
            memory_vec_declared_dimensions(&connection, &self.database_path)?
        else {
            return Ok(Vec::new());
        };

        if expected_dimensions != query_embedding.len() {
            return Ok(Vec::new());
        }

        let query_blob = embedding_to_blob(query_embedding);
        let mut stmt = connection
            .prepare(
                "SELECT m.id, m.session_id, m.kind, m.content, m.created_at, distance
                 FROM memory_vec mv
                 JOIN memories m ON m.rowid = mv.memory_rowid
                 WHERE embedding MATCH ?1 AND k = ?2
                 ORDER BY distance",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![query_blob, limit as i64], |row| {
                let distance: f64 = row.get(5)?;
                Ok(ScoredMemory {
                    memory: StoredMemory {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        kind: row.get(2)?,
                        content: row.get(3)?,
                        created_at: row.get(4)?,
                    },
                    score: 1.0 / (1.0 + distance),
                    source: "vector".to_owned(),
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path)
    }

    /// Hybrid search combining FTS results with vector similarity via reciprocal rank fusion.
    pub fn hybrid_search(
        &self,
        query: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<ScoredMemory>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let fts_results = self.search(query, limit * 2)?;
        let vector_results = self.vector_search(query_embedding, limit * 2)?;
        Ok(reciprocal_rank_fusion(&fts_results, &vector_results, limit))
    }

    pub fn graph_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ScoredMemory>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let primary = self.search(query, limit)?;
        if primary.is_empty() {
            return Ok(Vec::new());
        }

        let connection = open(&self.database_path)?;
        let primary_ids: Vec<String> = primary.iter().map(|memory| memory.id.clone()).collect();
        let attributes = self.graph_attributes_for_ids(&connection, &primary_ids)?;
        let linked_memories = self.linked_memories_for_sources(&connection, &primary_ids)?;
        let now = Utc::now();
        let mut seen = HashSet::new();
        let mut expanded = Vec::new();

        for (rank, memory) in primary.iter().enumerate() {
            let base_score = 1.0 / (60.0 + rank as f64);
            if seen.insert(memory.id.clone()) {
                if let Some(attributes) = attributes.get(&memory.id) {
                    expanded.push(Self::score_graph_memory(
                        memory, attributes, base_score, &now,
                    )?);
                }
            }

            for (link_rank, linked) in linked_memories
                .get(&memory.id)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if seen.insert(linked.memory.id.clone()) {
                    let link_score = base_score * (0.5 / (link_rank as f64 + 1.0));
                    expanded.push(Self::score_graph_memory(
                        &linked.memory,
                        &linked.attributes,
                        link_score,
                        &now,
                    )?);
                }
            }
        }

        expanded.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        expanded.truncate(limit);
        self.bump_access_metrics(
            &expanded
                .iter()
                .map(|item| item.memory.id.clone())
                .collect::<Vec<_>>(),
        )?;
        Ok(expanded)
    }

    pub fn create_note(&self, note: NewMemoryNote) -> Result<StoredMemory, StorageError> {
        let mut connection = open(&self.database_path)?;
        exec_migration(
            &connection,
            &self.database_path,
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )?;

        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let note_id = note.id.clone();
        let session_id = note.session_id.clone();
        let kind = note.kind.clone();
        let content = note.content.clone();
        let linked_ids = note.linked_ids.clone();
        // Vec<String> serialization is infallible in practice; fall back to
        // empty array and log to stderr if it somehow fails.
        let keywords_json = serde_json::to_string(&note.keywords).unwrap_or_else(|e| {
            eprintln!("warning: failed to serialize memory keywords: {e}");
            "[]".to_owned()
        });
        let tags_json = serde_json::to_string(&note.tags).unwrap_or_else(|e| {
            eprintln!("warning: failed to serialize memory tags: {e}");
            "[]".to_owned()
        });

        tx.execute(
            "INSERT INTO memories (
                id, session_id, kind, content, created_at, keywords_json, tags_json,
                importance, accessed_at, access_count
             ) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP, ?5, ?6, ?7, CURRENT_TIMESTAMP, 0)",
            params![
                &note_id,
                session_id,
                &kind,
                &content,
                keywords_json,
                tags_json,
                note.importance,
            ],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;

        let memory_row_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, ?2, ?3)",
            params![memory_row_id, &kind, &content],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;

        for linked_id in linked_ids
            .iter()
            .filter(|linked_id| linked_id.as_str() != note_id.as_str())
        {
            insert_pending_memory_link(&tx, &self.database_path, &note_id, linked_id)?;
            insert_pending_memory_link(&tx, &self.database_path, linked_id, &note_id)?;
        }
        resolve_pending_memory_links(&tx, &self.database_path, &note_id)?;

        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;

        self.get(&note_id)?.ok_or_else(|| StorageError::Sqlite {
            path: self.database_path.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Create and index a new memory entry.
    pub fn create(
        &self,
        session_id: Option<&str>,
        kind: &str,
        content: &str,
    ) -> Result<StoredMemory, StorageError> {
        self.create_note(NewMemoryNote {
            id: format!("memory-{}", memory_unique_suffix()),
            session_id: session_id.map(str::to_owned),
            kind: kind.to_owned(),
            content: content.to_owned(),
            keywords: Vec::new(),
            tags: Vec::new(),
            linked_ids: Vec::new(),
            importance: 0.5,
        })
    }

    pub fn links_for(&self, id: &str) -> Result<Vec<String>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT target_id
                 FROM memory_links
                 WHERE source_id = ?1
                 ORDER BY target_id ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![id], |row| row.get::<_, String>(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path)
    }

    fn graph_attributes_for_ids(
        &self,
        connection: &Connection,
        memory_ids: &[String],
    ) -> Result<HashMap<String, GraphAttributes>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut stmt = connection
            .prepare(&format!(
                "SELECT id, importance, accessed_at
                 FROM memories
                 WHERE id IN ({})",
                sql_placeholders(memory_ids.len()),
            ))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(
                params_from_iter(memory_ids.iter().map(|id| id.as_str())),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        GraphAttributes {
                            importance: row.get::<_, f32>(1)?,
                            accessed_at: row.get::<_, String>(2)?,
                        },
                    ))
                },
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path).map(|pairs| pairs.into_iter().collect())
    }

    fn linked_memories_for_sources(
        &self,
        connection: &Connection,
        source_ids: &[String],
    ) -> Result<HashMap<String, Vec<GraphCandidate>>, StorageError> {
        if source_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut stmt = connection
            .prepare(&format!(
                "SELECT ml.source_id, m.id, m.session_id, m.kind, m.content, m.created_at,
                        m.importance, m.accessed_at
                 FROM memory_links ml
                 JOIN memories m ON m.id = ml.target_id
                 WHERE ml.source_id IN ({})
                 ORDER BY ml.source_id ASC, m.created_at ASC",
                sql_placeholders(source_ids.len()),
            ))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(
                params_from_iter(source_ids.iter().map(|id| id.as_str())),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        GraphCandidate {
                            memory: StoredMemory {
                                id: row.get(1)?,
                                session_id: row.get(2)?,
                                kind: row.get(3)?,
                                content: row.get(4)?,
                                created_at: row.get(5)?,
                            },
                            attributes: GraphAttributes {
                                importance: row.get::<_, f32>(6)?,
                                accessed_at: row.get::<_, String>(7)?,
                            },
                        },
                    ))
                },
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let mut grouped = HashMap::new();
        for (source_id, candidate) in collect_rows(rows, &self.database_path)? {
            grouped
                .entry(source_id)
                .or_insert_with(Vec::new)
                .push(candidate);
        }
        Ok(grouped)
    }

    fn score_graph_memory(
        memory: &StoredMemory,
        attributes: &GraphAttributes,
        base_score: f64,
        now: &DateTime<Utc>,
    ) -> Result<ScoredMemory, StorageError> {
        let score = base_score
            * decayed_importance(attributes.importance, &attributes.accessed_at, now)? as f64;
        Ok(ScoredMemory {
            memory: memory.clone(),
            score,
            source: "graph".to_owned(),
        })
    }

    fn bump_access_metrics(&self, memory_ids: &[String]) -> Result<(), StorageError> {
        if memory_ids.is_empty() {
            return Ok(());
        }

        let mut connection = open(&self.database_path)?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        for memory_id in memory_ids {
            tx.execute(
                "UPDATE memories
                 SET accessed_at = CURRENT_TIMESTAMP,
                     access_count = access_count + 1
                 WHERE id = ?1",
                params![memory_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        }

        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;
        Ok(())
    }

    /// Delete a memory by ID.
    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let mut connection = open(&self.database_path)?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let memory_rowid: Option<i64> = tx
            .query_row(
                "SELECT rowid FROM memories WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let Some(memory_rowid) = memory_rowid else {
            return Ok(false);
        };

        if memory_vec_table_exists(&tx, &self.database_path)? {
            tx.execute(
                "DELETE FROM memory_vec WHERE memory_rowid = ?1",
                params![memory_rowid],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        }

        if sqlite_table_exists(&tx, &self.database_path, "memory_search")? {
            tx.execute(
                "DELETE FROM memory_search WHERE memory_row_id = ?1",
                params![memory_rowid],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        }

        let rows = tx
            .execute("DELETE FROM memories WHERE id = ?1", params![id])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;
        Ok(rows > 0)
    }
}

fn insert_pending_memory_link(
    connection: &Connection,
    database_path: &Path,
    source_id: &str,
    target_id: &str,
) -> Result<(), StorageError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO pending_memory_links (source_id, target_id) VALUES (?1, ?2)",
            params![source_id, target_id],
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn resolve_pending_memory_links(
    connection: &Connection,
    database_path: &Path,
    memory_id: &str,
) -> Result<(), StorageError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO memory_links (source_id, target_id)
             SELECT pending.source_id, pending.target_id
             FROM pending_memory_links pending
             JOIN memories source ON source.id = pending.source_id
             JOIN memories target ON target.id = pending.target_id
             WHERE pending.source_id = ?1 OR pending.target_id = ?1",
            params![memory_id],
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    connection
        .execute(
            "DELETE FROM pending_memory_links
             WHERE rowid IN (
                 SELECT pending.rowid
                 FROM pending_memory_links pending
                 JOIN memories source ON source.id = pending.source_id
                 JOIN memories target ON target.id = pending.target_id
                 WHERE pending.source_id = ?1 OR pending.target_id = ?1
             )",
            params![memory_id],
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn memory_unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn reciprocal_rank_fusion(
    fts_results: &[StoredMemory],
    vector_results: &[ScoredMemory],
    limit: usize,
) -> Vec<ScoredMemory> {
    const K: f64 = 60.0;

    let mut scores: std::collections::HashMap<String, (f64, Option<StoredMemory>)> =
        std::collections::HashMap::new();

    for (rank, memory) in fts_results.iter().enumerate() {
        let entry = scores.entry(memory.id.clone()).or_insert((0.0, None));
        entry.0 += 1.0 / (K + rank as f64);
        if entry.1.is_none() {
            entry.1 = Some(memory.clone());
        }
    }

    for (rank, scored) in vector_results.iter().enumerate() {
        let entry = scores
            .entry(scored.memory.id.clone())
            .or_insert((0.0, None));
        entry.0 += 1.0 / (K + rank as f64);
        if entry.1.is_none() {
            entry.1 = Some(scored.memory.clone());
        }
    }

    let mut merged: Vec<ScoredMemory> = scores
        .into_iter()
        .filter_map(|(_, (score, memory))| {
            memory.map(|memory| ScoredMemory {
                memory,
                score,
                source: "hybrid".to_owned(),
            })
        })
        .collect();

    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(limit);
    merged
}

/// Embedding persistence layer for vector/semantic memory search.
pub struct EmbeddingStore {
    database_path: PathBuf,
}

impl EmbeddingStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Returns the database path for this store.
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Store an embedding for a memory. Replaces any existing embedding for this memory_id.
    pub fn store(
        &self,
        memory_id: &str,
        embedding: &[f32],
        model: &str,
    ) -> Result<(), StorageError> {
        let mut connection = open(&self.database_path)?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let memory_rowid: i64 = tx
            .query_row(
                "SELECT rowid FROM memories WHERE id = ?1",
                params![memory_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        ensure_memory_vec_table(&tx, &self.database_path, embedding.len())?;
        let blob = embedding_to_blob(embedding);
        tx.execute(
            "INSERT INTO memory_embeddings (memory_id, embedding, model, dimensions, created_at)
             VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
             ON CONFLICT(memory_id) DO UPDATE SET embedding = excluded.embedding,
                model = excluded.model, dimensions = excluded.dimensions,
                created_at = excluded.created_at",
            params![memory_id, &blob, model, embedding.len() as i64],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;
        tx.execute(
            "DELETE FROM memory_vec WHERE memory_rowid = ?1",
            params![memory_rowid],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;
        tx.execute(
            "INSERT INTO memory_vec(memory_rowid, embedding) VALUES (?1, ?2)",
            params![memory_rowid, &blob],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;
        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;
        Ok(())
    }

    /// Retrieve all embeddings for cosine similarity search.
    /// Returns (memory_id, embedding) pairs.
    pub fn all_embeddings(&self) -> Result<Vec<(String, Vec<f32>)>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare("SELECT memory_id, embedding FROM memory_embeddings")
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([], |row| {
                let memory_id: String = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((memory_id, blob_to_embedding(&blob)))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        collect_rows(rows, &self.database_path)
    }

    /// Delete an embedding by memory ID.
    pub fn delete(&self, memory_id: &str) -> Result<bool, StorageError> {
        let mut connection = open(&self.database_path)?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let memory_rowid: Option<i64> = tx
            .query_row(
                "SELECT rowid FROM memories WHERE id = ?1",
                params![memory_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        if let Some(memory_rowid) = memory_rowid {
            if memory_vec_table_exists(&tx, &self.database_path)? {
                tx.execute(
                    "DELETE FROM memory_vec WHERE memory_rowid = ?1",
                    params![memory_rowid],
                )
                .map_err(|source| StorageError::Sqlite {
                    path: self.database_path.clone(),
                    source,
                })?;
            }
        }

        let rows = tx
            .execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                params![memory_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;
        Ok(rows > 0)
    }

    /// Check if an embedding exists for a given memory ID.
    pub fn has_embedding(&self, memory_id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE memory_id = ?1",
                params![memory_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(count > 0)
    }

    /// Count total stored embeddings.
    pub fn count(&self) -> Result<usize, StorageError> {
        let connection = open(&self.database_path)?;
        memory_embeddings_count(&connection, &self.database_path)
    }

    /// Returns the current database-wide embedding dimensions when known.
    pub fn dimensions(&self) -> Result<Option<usize>, StorageError> {
        let connection = open(&self.database_path)?;
        if let Some(dimensions) = memory_vec_declared_dimensions(&connection, &self.database_path)?
        {
            return Ok(Some(dimensions));
        }
        detect_uniform_embedding_dimensions(&connection, &self.database_path)
    }

    /// Remove all stored embeddings and clear the vector index.
    pub fn clear(&self) -> Result<(), StorageError> {
        let mut connection = open(&self.database_path)?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        tx.execute("DELETE FROM memory_embeddings", [])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        if memory_vec_table_exists(&tx, &self.database_path)? {
            tx.execute("DELETE FROM memory_vec", [])
                .map_err(|source| StorageError::Sqlite {
                    path: self.database_path.clone(),
                    source,
                })?;
        }
        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;
        Ok(())
    }
}

/// Serialize an f32 slice to a little-endian byte blob for SQLite storage.
fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        blob.extend_from_slice(&val.to_le_bytes());
    }
    blob
}

/// Deserialize a little-endian byte blob back to an f32 vector.
fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn open(database_path: &Path) -> Result<Connection, StorageError> {
    register_sqlite_vec();
    let conn = Connection::open(database_path).map_err(|source| StorageError::OpenDatabase {
        path: database_path.to_path_buf(),
        source,
    })?;

    // Performance PRAGMAs — WAL mode enables concurrent readers + single writer,
    // NORMAL sync is crash-safe in WAL mode, busy_timeout prevents SQLITE_BUSY
    // under concurrent access from gateway + CLI.
    // foreign_keys enables ON DELETE CASCADE for all connections (required per-connection).
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;
         PRAGMA foreign_keys = ON;",
    )
    .map_err(|source| StorageError::OpenDatabase {
        path: database_path.to_path_buf(),
        source,
    })?;

    Ok(conn)
}

impl ImportStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Imported => "imported",
            Self::Failed => "failed",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, StorageError> {
        match value {
            "planned" => Ok(Self::Planned),
            "imported" => Ok(Self::Imported),
            "failed" => Ok(Self::Failed),
            other => Err(StorageError::UnknownImportStatus(other.to_owned())),
        }
    }
}

fn first_existing_file<I>(mut candidates: I) -> Option<PathBuf>
where
    I: Iterator<Item = PathBuf>,
{
    candidates.find(|path| path.is_file())
}

fn first_existing_dir<I>(mut candidates: I) -> Option<PathBuf>
where
    I: Iterator<Item = PathBuf>,
{
    candidates.find(|path| path.is_dir())
}

// ===========================================================================
// PairingStore — DM pairing system for messaging platform authorization
// ===========================================================================

/// An approved (paired) user on a messaging platform.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApprovedUser {
    pub platform: String,
    pub user_id: String,
    pub user_name: String,
    pub approved_at: String,
}

/// A pending pairing request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingPairing {
    pub platform: String,
    pub code: String,
    pub user_id: String,
    pub user_name: String,
    pub created_at: String,
}

/// Code-based approval flow for authorizing users on messaging platforms.
///
/// Instead of static allowlists, unknown users receive a one-time pairing
/// code that the bot owner approves via the CLI or API.
pub struct PairingStore {
    database_path: PathBuf,
}

/// Unambiguous alphabet for pairing codes (no 0/O, 1/I).
const PAIRING_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const PAIRING_CODE_LENGTH: usize = 8;
/// Codes expire after 1 hour.
const PAIRING_CODE_TTL_SECS: i64 = 3600;
/// Max pending codes per platform.
const MAX_PENDING_PER_PLATFORM: usize = 3;

fn generate_pairing_code() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Use a combination of time-based entropy and process-level randomness.
    // Not cryptographic, but adequate for pairing codes with short TTL.
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let mut state = seed ^ (std::process::id() as u128) ^ 0xDEAD_BEEF_CAFE_BABE;
    let mut code = String::with_capacity(PAIRING_CODE_LENGTH);
    for _ in 0..PAIRING_CODE_LENGTH {
        // xorshift-style mixing
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let idx = (state as usize) % PAIRING_ALPHABET.len();
        code.push(PAIRING_ALPHABET[idx] as char);
    }
    code
}

impl PairingStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Delete pending pairing codes that have exceeded their TTL.
    fn cleanup_expired_codes(
        connection: &Connection,
        database_path: &Path,
        expiry: &str,
    ) -> Result<(), StorageError> {
        connection
            .execute(
                "DELETE FROM pairing_pending
                 WHERE created_at < datetime('now', ?1)",
                params![expiry],
            )
            .map_err(|source| StorageError::Sqlite {
                path: database_path.to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Check if a user is approved (paired) on a platform.
    pub fn is_approved(&self, platform: &str, user_id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pairing_approved
                 WHERE platform = ?1 AND user_id = ?2",
                params![platform, user_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(count > 0)
    }

    /// List all approved users, optionally filtered by platform.
    pub fn list_approved(&self, platform: Option<&str>) -> Result<Vec<ApprovedUser>, StorageError> {
        let connection = open(&self.database_path)?;
        let db = &self.database_path;
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.clone(),
            source,
        };

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<ApprovedUser> {
            Ok(ApprovedUser {
                platform: row.get(0)?,
                user_id: row.get(1)?,
                user_name: row.get(2)?,
                approved_at: row.get(3)?,
            })
        };

        let users = if let Some(p) = platform {
            let mut stmt = connection
                .prepare(
                    "SELECT platform, user_id, user_name, approved_at
                     FROM pairing_approved WHERE platform = ?1
                     ORDER BY approved_at DESC",
                )
                .map_err(me)?;
            let rows = stmt.query_map(params![p], map_row).map_err(me)?;
            collect_rows(rows, &self.database_path)?
        } else {
            let mut stmt = connection
                .prepare(
                    "SELECT platform, user_id, user_name, approved_at
                     FROM pairing_approved
                     ORDER BY platform, approved_at DESC",
                )
                .map_err(me)?;
            let rows = stmt.query_map([], map_row).map_err(me)?;
            collect_rows(rows, &self.database_path)?
        };

        Ok(users)
    }

    /// Generate a pairing code for a new user.
    ///
    /// Returns `None` if the platform already has the max number of pending
    /// codes, or if the user is already approved.
    pub fn generate_code(
        &self,
        platform: &str,
        user_id: &str,
        user_name: &str,
    ) -> Result<Option<String>, StorageError> {
        // Don't generate if already approved
        if self.is_approved(platform, user_id)? {
            return Ok(None);
        }

        let connection = open(&self.database_path)?;

        // Clean up expired codes
        Self::cleanup_expired_codes(
            &connection,
            &self.database_path,
            &format!("-{PAIRING_CODE_TTL_SECS} seconds"),
        )?;

        // Check if we've hit the max pending for this platform
        let pending_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pairing_pending WHERE platform = ?1",
                params![platform],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        if pending_count as usize >= MAX_PENDING_PER_PLATFORM {
            return Ok(None);
        }

        let code = generate_pairing_code();

        connection
            .execute(
                "INSERT OR REPLACE INTO pairing_pending
                 (platform, code, user_id, user_name, created_at)
                 VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
                params![platform, &code, user_id, user_name],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(Some(code))
    }

    /// Approve a pairing code, moving the user to the approved list.
    ///
    /// Returns the approved user info, or `None` if the code is invalid/expired.
    pub fn approve_code(
        &self,
        platform: &str,
        code: &str,
    ) -> Result<Option<ApprovedUser>, StorageError> {
        let code = code.to_uppercase();
        let connection = open(&self.database_path)?;

        // Clean up expired codes first
        Self::cleanup_expired_codes(
            &connection,
            &self.database_path,
            &format!("-{PAIRING_CODE_TTL_SECS} seconds"),
        )?;

        // Find the pending code
        let pending = connection
            .query_row(
                "SELECT user_id, user_name FROM pairing_pending
                 WHERE platform = ?1 AND code = ?2",
                params![platform, &code],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let Some((user_id, user_name)) = pending else {
            return Ok(None);
        };

        // Remove the pending code
        connection
            .execute(
                "DELETE FROM pairing_pending WHERE platform = ?1 AND code = ?2",
                params![platform, &code],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        // Add to approved users
        connection
            .execute(
                "INSERT OR REPLACE INTO pairing_approved
                 (platform, user_id, user_name, approved_at)
                 VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)",
                params![platform, &user_id, &user_name],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        // Retrieve the approved user for the return value
        let approved = connection
            .query_row(
                "SELECT platform, user_id, user_name, approved_at
                 FROM pairing_approved WHERE platform = ?1 AND user_id = ?2",
                params![platform, &user_id],
                |row| {
                    Ok(ApprovedUser {
                        platform: row.get(0)?,
                        user_id: row.get(1)?,
                        user_name: row.get(2)?,
                        approved_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(approved)
    }

    /// List pending pairing requests, optionally filtered by platform.
    pub fn list_pending(
        &self,
        platform: Option<&str>,
    ) -> Result<Vec<PendingPairing>, StorageError> {
        let connection = open(&self.database_path)?;
        let db = &self.database_path;
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.clone(),
            source,
        };

        // Clean up expired first
        Self::cleanup_expired_codes(
            &connection,
            &self.database_path,
            &format!("-{PAIRING_CODE_TTL_SECS} seconds"),
        )?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<PendingPairing> {
            Ok(PendingPairing {
                platform: row.get(0)?,
                code: row.get(1)?,
                user_id: row.get(2)?,
                user_name: row.get(3)?,
                created_at: row.get(4)?,
            })
        };

        let pending = if let Some(p) = platform {
            let mut stmt = connection
                .prepare(
                    "SELECT platform, code, user_id, user_name, created_at
                     FROM pairing_pending WHERE platform = ?1
                     ORDER BY created_at DESC",
                )
                .map_err(me)?;
            let rows = stmt.query_map(params![p], map_row).map_err(me)?;
            collect_rows(rows, &self.database_path)?
        } else {
            let mut stmt = connection
                .prepare(
                    "SELECT platform, code, user_id, user_name, created_at
                     FROM pairing_pending
                     ORDER BY platform, created_at DESC",
                )
                .map_err(me)?;
            let rows = stmt.query_map([], map_row).map_err(me)?;
            collect_rows(rows, &self.database_path)?
        };

        Ok(pending)
    }

    /// Revoke an approved user's access.
    pub fn revoke(&self, platform: &str, user_id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute(
                "DELETE FROM pairing_approved WHERE platform = ?1 AND user_id = ?2",
                params![platform, user_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Clear all pending codes, optionally filtered by platform.
    pub fn clear_pending(&self, platform: Option<&str>) -> Result<usize, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = if let Some(p) = platform {
            connection
                .execute(
                    "DELETE FROM pairing_pending WHERE platform = ?1",
                    params![p],
                )
                .map_err(|source| StorageError::Sqlite {
                    path: self.database_path.clone(),
                    source,
                })?
        } else {
            connection
                .execute("DELETE FROM pairing_pending", [])
                .map_err(|source| StorageError::Sqlite {
                    path: self.database_path.clone(),
                    source,
                })?
        };
        Ok(rows)
    }

    /// List approved users with offset/limit pagination.
    pub fn list_approved_paginated(
        &self,
        platform: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ApprovedUser>, u64), StorageError> {
        let connection = open(&self.database_path)?;
        let db = &self.database_path;
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.clone(),
            source,
        };

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<ApprovedUser> {
            Ok(ApprovedUser {
                platform: row.get(0)?,
                user_id: row.get(1)?,
                user_name: row.get(2)?,
                approved_at: row.get(3)?,
            })
        };

        let (total, items) = if let Some(p) = platform {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pairing_approved WHERE platform = ?1",
                    params![p],
                    |row| row.get(0),
                )
                .map_err(me)?;
            let mut stmt = connection
                .prepare(
                    "SELECT platform, user_id, user_name, approved_at
                     FROM pairing_approved WHERE platform = ?1
                     ORDER BY approved_at DESC
                     LIMIT ?2 OFFSET ?3",
                )
                .map_err(me)?;
            let rows = stmt
                .query_map(params![p, limit as i64, offset as i64], map_row)
                .map_err(me)?;
            (t, collect_rows(rows, &self.database_path)?)
        } else {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pairing_approved",
                    [],
                    |row| row.get(0),
                )
                .map_err(me)?;
            let mut stmt = connection
                .prepare(
                    "SELECT platform, user_id, user_name, approved_at
                     FROM pairing_approved
                     ORDER BY platform, approved_at DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(me)?;
            let rows = stmt
                .query_map(params![limit as i64, offset as i64], map_row)
                .map_err(me)?;
            (t, collect_rows(rows, &self.database_path)?)
        };

        Ok((items, total as u64))
    }

    /// List pending pairings with offset/limit pagination.
    pub fn list_pending_paginated(
        &self,
        platform: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<PendingPairing>, u64), StorageError> {
        let connection = open(&self.database_path)?;
        let db = &self.database_path;
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.clone(),
            source,
        };

        // Clean up expired first
        Self::cleanup_expired_codes(
            &connection,
            &self.database_path,
            &format!("-{PAIRING_CODE_TTL_SECS} seconds"),
        )?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<PendingPairing> {
            Ok(PendingPairing {
                platform: row.get(0)?,
                code: row.get(1)?,
                user_id: row.get(2)?,
                user_name: row.get(3)?,
                created_at: row.get(4)?,
            })
        };

        let (total, items) = if let Some(p) = platform {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pairing_pending WHERE platform = ?1",
                    params![p],
                    |row| row.get(0),
                )
                .map_err(me)?;
            let mut stmt = connection
                .prepare(
                    "SELECT platform, code, user_id, user_name, created_at
                     FROM pairing_pending WHERE platform = ?1
                     ORDER BY created_at DESC
                     LIMIT ?2 OFFSET ?3",
                )
                .map_err(me)?;
            let rows = stmt
                .query_map(params![p, limit as i64, offset as i64], map_row)
                .map_err(me)?;
            (t, collect_rows(rows, &self.database_path)?)
        } else {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pairing_pending",
                    [],
                    |row| row.get(0),
                )
                .map_err(me)?;
            let mut stmt = connection
                .prepare(
                    "SELECT platform, code, user_id, user_name, created_at
                     FROM pairing_pending
                     ORDER BY platform, created_at DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(me)?;
            let rows = stmt
                .query_map(params![limit as i64, offset as i64], map_row)
                .map_err(me)?;
            (t, collect_rows(rows, &self.database_path)?)
        };

        Ok((items, total as u64))
    }
}

// ChannelStore — cached platform channel directory for send_message discovery
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedChannel {
    pub platform: String,
    pub channel_id: String,
    pub channel_name: String,
    pub channel_type: String,
    pub is_member: bool,
    pub cached_at: String,
}

pub struct ChannelStore {
    database_path: PathBuf,
}

impl ChannelStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// List cached channels, optionally filtered by platform.
    pub fn list(&self, platform: Option<&str>) -> Result<Vec<CachedChannel>, StorageError> {
        let connection = open(&self.database_path)?;

        let (sql, param): (&str, Option<&str>) = if platform.is_some() {
            (
                "SELECT platform, channel_id, channel_name, channel_type, is_member, cached_at
                 FROM channels WHERE platform = ?1
                 ORDER BY channel_name",
                platform,
            )
        } else {
            (
                "SELECT platform, channel_id, channel_name, channel_type, is_member, cached_at
                 FROM channels ORDER BY platform, channel_name",
                None,
            )
        };

        let mut stmt = connection
            .prepare(sql)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let row_mapper = |row: &rusqlite::Row| {
            Ok(CachedChannel {
                platform: row.get(0)?,
                channel_id: row.get(1)?,
                channel_name: row.get(2)?,
                channel_type: row.get(3)?,
                is_member: row.get::<_, i64>(4)? != 0,
                cached_at: row.get(5)?,
            })
        };

        let mapped_rows = if let Some(p) = param {
            stmt.query_map(params![p], row_mapper)
        } else {
            stmt.query_map([], row_mapper)
        }
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;

        collect_rows(mapped_rows, &self.database_path)
    }

    /// Upsert a batch of channels for a platform, replacing stale entries.
    pub fn upsert_channels(
        &self,
        platform: &str,
        channels: &[CachedChannel],
    ) -> Result<usize, StorageError> {
        let connection = open(&self.database_path)?;

        // Clear old entries for this platform before inserting fresh data.
        connection
            .execute(
                "DELETE FROM channels WHERE platform = ?1",
                params![platform],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        for ch in channels {
            connection
                .execute(
                    "INSERT INTO channels (platform, channel_id, channel_name, channel_type, is_member, cached_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)",
                    params![
                        platform,
                        ch.channel_id,
                        ch.channel_name,
                        ch.channel_type,
                        ch.is_member as i64,
                    ],
                )
                .map_err(|source| StorageError::Sqlite {
                    path: self.database_path.clone(),
                    source,
                })?;
        }

        Ok(channels.len())
    }

    /// Check if channels for a platform are cached and fresh (within max_age_secs).
    pub fn is_fresh(&self, platform: &str, max_age_secs: i64) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let fresh: bool = connection
            .query_row(
                "SELECT COUNT(*) FROM channels
                 WHERE platform = ?1
                   AND CAST((julianday('now') - julianday(cached_at)) * 86400 AS INTEGER) < ?2",
                params![platform, max_age_secs],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            > 0;
        Ok(fresh)
    }
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

/// A single audit log entry representing an agent action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub session_id: Option<String>,
    pub event_type: String,
    pub details: String,
    pub created_at: String,
}

/// Tool usage analytics derived from audit log data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAnalytics {
    pub tool_name: String,
    pub call_count: i64,
    pub success_count: i64,
    pub avg_duration_ms: f64,
}

/// LLM usage analytics derived from audit log data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmAnalytics {
    pub model: String,
    pub call_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

/// SQLite-backed audit log for tracking agent actions.
///
/// Records tool calls, LLM requests, config changes, and other security-relevant
/// events with structured JSON details. Supports querying by session, event type,
/// and time range.
pub struct AuditLogStore {
    database_path: PathBuf,
}

impl AuditLogStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Map a database row to an `AuditEntry`.
    ///
    /// Expects columns in order: id, session_id, event_type, details, created_at.
    fn row_to_audit_entry(row: &rusqlite::Row) -> rusqlite::Result<AuditEntry> {
        Ok(AuditEntry {
            id: row.get(0)?,
            session_id: row.get(1)?,
            event_type: row.get(2)?,
            details: row.get(3)?,
            created_at: row.get(4)?,
        })
    }

    /// Record an audit event.
    pub fn log(
        &self,
        session_id: Option<&str>,
        event_type: &str,
        details: &serde_json::Value,
    ) -> Result<i64, StorageError> {
        let connection = open(&self.database_path)?;
        let details_str = details.to_string();
        connection
            .execute(
                "INSERT INTO audit_log (session_id, event_type, details)
                 VALUES (?1, ?2, ?3)",
                params![session_id, event_type, details_str],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(connection.last_insert_rowid())
    }

    /// Query audit entries for a specific session.
    pub fn by_session(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, event_type, details, created_at
                 FROM audit_log
                 WHERE session_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![session_id, limit as i64], Self::row_to_audit_entry)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Query audit entries by event type.
    pub fn by_event_type(
        &self,
        event_type: &str,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, event_type, details, created_at
                 FROM audit_log
                 WHERE event_type = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![event_type, limit as i64], Self::row_to_audit_entry)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Query recent audit entries across all sessions.
    pub fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, event_type, details, created_at
                 FROM audit_log
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64], Self::row_to_audit_entry)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Count total audit entries and entries per event type.
    pub fn stats(&self) -> Result<Vec<(String, i64)>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT event_type, COUNT(*) as cnt
                 FROM audit_log
                 GROUP BY event_type
                 ORDER BY cnt DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Aggregate tool usage analytics from tool_call_end audit events.
    ///
    /// Returns a list of (tool_name, call_count, success_count, avg_duration_ms)
    /// sorted by call count descending.
    pub fn tool_analytics(&self, days: u32) -> Result<Vec<ToolAnalytics>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT
                    json_extract(details, '$.tool') as tool_name,
                    COUNT(*) as calls,
                    SUM(CASE WHEN json_extract(details, '$.success') = 1 THEN 1 ELSE 0 END) as successes,
                    AVG(CAST(json_extract(details, '$.duration_ms') AS REAL)) as avg_dur
                 FROM audit_log
                 WHERE event_type = 'tool_call_end'
                   AND created_at >= datetime('now', '-' || ?1 || ' days')
                   AND json_extract(details, '$.tool') IS NOT NULL
                 GROUP BY tool_name
                 ORDER BY calls DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![days], |row| {
                Ok(ToolAnalytics {
                    tool_name: row.get(0)?,
                    call_count: row.get(1)?,
                    success_count: row.get(2)?,
                    avg_duration_ms: row.get(3)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Aggregate LLM usage analytics from llm_response audit events.
    ///
    /// Returns per-model token usage totals.
    pub fn llm_analytics(&self, days: u32) -> Result<Vec<LlmAnalytics>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT
                    json_extract(details, '$.model') as model,
                    COUNT(*) as calls,
                    SUM(CAST(json_extract(details, '$.input_tokens') AS INTEGER)) as total_input,
                    SUM(CAST(json_extract(details, '$.output_tokens') AS INTEGER)) as total_output
                 FROM audit_log
                 WHERE event_type = 'llm_response'
                   AND created_at >= datetime('now', '-' || ?1 || ' days')
                   AND json_extract(details, '$.model') IS NOT NULL
                 GROUP BY model
                 ORDER BY calls DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map(params![days], |row| {
                Ok(LlmAnalytics {
                    model: row.get(0)?,
                    call_count: row.get(1)?,
                    total_input_tokens: row.get(2)?,
                    total_output_tokens: row.get(3)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        collect_rows(rows, &self.database_path)
    }

    /// Delete audit entries older than the given number of days.
    pub fn purge_older_than(&self, days: u32) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        let deleted = connection
            .execute(
                "DELETE FROM audit_log WHERE created_at < datetime('now', '-' || ?1 || ' days')",
                params![days],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(deleted as u64)
    }
}

/// A cached LLM response entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResponse {
    pub cache_key: String,
    pub model: String,
    pub response: String,
    pub tool_calls_json: Option<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub hit_count: u32,
    pub created_at: String,
    pub expires_at: String,
}

/// SQLite-backed response cache for LLM completions.
///
/// Caches responses keyed by a deterministic hash of the request parameters
/// (model + messages + tools + temperature). Entries expire after a configurable TTL.
pub struct ResponseCacheStore {
    database_path: PathBuf,
}

impl ResponseCacheStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Look up a cached response by its cache key.
    /// Returns `None` if the entry doesn't exist or has expired.
    /// Increments the hit counter on successful lookup.
    pub fn get(&self, cache_key: &str) -> Result<Option<CachedResponse>, StorageError> {
        let connection = open(&self.database_path)?;

        let entry: Option<CachedResponse> = connection
            .query_row(
                "SELECT cache_key, model, response, tool_calls_json, input_tokens,
                        output_tokens, hit_count, created_at, expires_at
                 FROM response_cache
                 WHERE cache_key = ?1 AND expires_at > datetime('now')",
                params![cache_key],
                |row| {
                    Ok(CachedResponse {
                        cache_key: row.get(0)?,
                        model: row.get(1)?,
                        response: row.get(2)?,
                        tool_calls_json: row.get(3)?,
                        input_tokens: row.get(4)?,
                        output_tokens: row.get(5)?,
                        hit_count: row.get(6)?,
                        created_at: row.get(7)?,
                        expires_at: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        if entry.is_some() {
            let _ = connection.execute(
                "UPDATE response_cache SET hit_count = hit_count + 1 WHERE cache_key = ?1",
                params![cache_key],
            );
        }

        Ok(entry)
    }

    /// Store a response in the cache.
    ///
    /// `ttl_seconds` controls how long the entry stays valid.
    #[allow(clippy::too_many_arguments)]
    pub fn set(
        &self,
        cache_key: &str,
        model: &str,
        response: &str,
        tool_calls_json: Option<&str>,
        input_tokens: u32,
        output_tokens: u32,
        ttl_seconds: u32,
    ) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT OR REPLACE INTO response_cache
                 (cache_key, model, response, tool_calls_json, input_tokens, output_tokens,
                  hit_count, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, datetime('now'),
                         datetime('now', '+' || ?7 || ' seconds'))",
                params![
                    cache_key,
                    model,
                    response,
                    tool_calls_json,
                    input_tokens,
                    output_tokens,
                    ttl_seconds
                ],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Remove expired entries from the cache.
    pub fn prune_expired(&self) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        let deleted = connection
            .execute(
                "DELETE FROM response_cache WHERE expires_at <= datetime('now')",
                [],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(deleted as u64)
    }

    /// Clear all cache entries.
    pub fn clear(&self) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        let deleted = connection
            .execute("DELETE FROM response_cache", [])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(deleted as u64)
    }

    /// Return total number of cached entries and total hit count.
    pub fn stats(&self) -> Result<(u64, u64), StorageError> {
        let connection = open(&self.database_path)?;
        let (entries, hits) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(hit_count), 0) FROM response_cache
                 WHERE expires_at > datetime('now')",
                [],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok((entries, hits))
    }
}

/// Cached sticker description from vision analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSticker {
    pub file_unique_id: String,
    pub description: String,
    pub emoji: String,
    pub sticker_set: String,
    pub created_at: String,
}

/// Persistent cache for Telegram sticker descriptions.
///
/// Keyed by `file_unique_id` (stable across messages, unlike `file_id`).
/// Stores vision-analyzed descriptions to avoid re-analyzing the same sticker.
pub struct StickerCacheStore {
    database_path: PathBuf,
}

impl StickerCacheStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Look up a cached sticker description by its unique file ID.
    pub fn get(&self, file_unique_id: &str) -> Result<Option<CachedSticker>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT file_unique_id, description, emoji, sticker_set, created_at
                 FROM sticker_cache WHERE file_unique_id = ?1",
                params![file_unique_id],
                |row| {
                    Ok(CachedSticker {
                        file_unique_id: row.get(0)?,
                        description: row.get(1)?,
                        emoji: row.get(2)?,
                        sticker_set: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// Store a sticker description in the cache.
    pub fn set(
        &self,
        file_unique_id: &str,
        description: &str,
        emoji: &str,
        sticker_set: &str,
    ) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT OR REPLACE INTO sticker_cache
                 (file_unique_id, description, emoji, sticker_set, created_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                params![file_unique_id, description, emoji, sticker_set],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Delete a cached sticker entry.
    pub fn delete(&self, file_unique_id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let deleted = connection
            .execute(
                "DELETE FROM sticker_cache WHERE file_unique_id = ?1",
                params![file_unique_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(deleted > 0)
    }

    /// Return the total number of cached sticker entries.
    pub fn count(&self) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM sticker_cache", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(count as u64)
    }
}

/// A sandbox instance record persisted to SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxRow {
    pub id: String,
    pub backend: String,
    pub task_id: String,
    pub snapshot_data: Option<String>,
    pub created_at: String,
    pub last_active: String,
}

/// SQLite-backed store for sandbox terminal backend records.
pub struct SandboxStore {
    database_path: PathBuf,
}

impl SandboxStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    /// Insert or replace a sandbox record, keyed on (backend, task_id).
    pub fn upsert(
        &self,
        id: &str,
        backend: &str,
        task_id: &str,
        snapshot_data: Option<&str>,
    ) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT INTO sandboxes (id, backend, task_id, snapshot_data, created_at, last_active)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'), datetime('now'))
                 ON CONFLICT(backend, task_id) DO UPDATE SET
                     id = excluded.id,
                     snapshot_data = excluded.snapshot_data,
                     last_active = datetime('now')",
                params![id, backend, task_id, snapshot_data],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Find a sandbox by (backend, task_id).
    pub fn find_by_task(
        &self,
        backend: &str,
        task_id: &str,
    ) -> Result<Option<SandboxRow>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT id, backend, task_id, snapshot_data, created_at, last_active
                 FROM sandboxes WHERE backend = ?1 AND task_id = ?2",
                params![backend, task_id],
                |row| {
                    Ok(SandboxRow {
                        id: row.get(0)?,
                        backend: row.get(1)?,
                        task_id: row.get(2)?,
                        snapshot_data: row.get(3)?,
                        created_at: row.get(4)?,
                        last_active: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// Update the last_active timestamp for a sandbox by id.
    pub fn update_activity(&self, id: &str) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "UPDATE sandboxes SET last_active = datetime('now') WHERE id = ?1",
                params![id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Delete a sandbox record by id.
    pub fn delete(&self, id: &str) -> Result<(), StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .execute("DELETE FROM sandboxes WHERE id = ?1", params![id])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(())
    }

    /// List all sandbox records, optionally filtered by backend.
    pub fn list(&self, backend: Option<&str>) -> Result<Vec<SandboxRow>, StorageError> {
        let connection = open(&self.database_path)?;

        let (sql, param): (&str, Option<&str>) = if backend.is_some() {
            (
                "SELECT id, backend, task_id, snapshot_data, created_at, last_active
                 FROM sandboxes WHERE backend = ?1
                 ORDER BY last_active DESC",
                backend,
            )
        } else {
            (
                "SELECT id, backend, task_id, snapshot_data, created_at, last_active
                 FROM sandboxes ORDER BY last_active DESC",
                None,
            )
        };

        let mut stmt = connection
            .prepare(sql)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let row_mapper = |row: &rusqlite::Row| {
            Ok(SandboxRow {
                id: row.get(0)?,
                backend: row.get(1)?,
                task_id: row.get(2)?,
                snapshot_data: row.get(3)?,
                created_at: row.get(4)?,
                last_active: row.get(5)?,
            })
        };

        let mapped_rows = if let Some(b) = param {
            stmt.query_map(params![b], row_mapper)
        } else {
            stmt.query_map([], row_mapper)
        }
        .map_err(|source| StorageError::Sqlite {
            path: self.database_path.clone(),
            source,
        })?;

        collect_rows(mapped_rows, &self.database_path)
    }

    /// Delete sandbox records that have not been active for more than `days` days.
    /// Returns the number of records deleted.
    pub fn cleanup_older_than(&self, days: u32) -> Result<usize, StorageError> {
        let connection = open(&self.database_path)?;
        let deleted = connection
            .execute(
                "DELETE FROM sandboxes WHERE last_active < datetime('now', '-' || ?1 || ' days')",
                params![days],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod sandbox_store_tests {
    use super::{bootstrap, migrate_to_v9, open, SandboxStore};
    use rusqlite::params;
    use tempfile::tempdir;

    #[test]
    fn sandbox_store_upserts_and_finds_by_task() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap should succeed");

        let store = SandboxStore::new(&db_path);
        store
            .upsert("sb-1", "singularity", "task-abc", Some(r#"{"key":"val"}"#))
            .expect("upsert should succeed");

        let row = store
            .find_by_task("singularity", "task-abc")
            .expect("find_by_task should succeed")
            .expect("row should exist");

        assert_eq!(row.id, "sb-1");
        assert_eq!(row.backend, "singularity");
        assert_eq!(row.task_id, "task-abc");
        assert_eq!(row.snapshot_data.as_deref(), Some(r#"{"key":"val"}"#));
        assert!(!row.created_at.is_empty());
        assert!(!row.last_active.is_empty());
    }

    #[test]
    fn sandbox_store_upsert_replaces_existing() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap should succeed");

        let store = SandboxStore::new(&db_path);
        store
            .upsert("sb-old", "modal", "task-1", None)
            .expect("first upsert");
        store
            .upsert("sb-new", "modal", "task-1", Some("updated"))
            .expect("second upsert replaces by UNIQUE(backend, task_id)");

        let row = store
            .find_by_task("modal", "task-1")
            .expect("find_by_task should succeed")
            .expect("row should exist");

        assert_eq!(row.id, "sb-new");
        assert_eq!(row.snapshot_data.as_deref(), Some("updated"));
    }

    #[test]
    fn sandbox_store_delete_removes_record() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap should succeed");

        let store = SandboxStore::new(&db_path);
        store
            .upsert("sb-del", "daytona", "task-del", None)
            .expect("upsert should succeed");

        store.delete("sb-del").expect("delete should succeed");

        let row = store
            .find_by_task("daytona", "task-del")
            .expect("find_by_task should succeed");
        assert!(row.is_none());
    }

    #[test]
    fn sandbox_store_list_filters_by_backend() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap should succeed");

        let store = SandboxStore::new(&db_path);
        store
            .upsert("sb-a1", "singularity", "task-a1", None)
            .expect("upsert a1");
        store
            .upsert("sb-a2", "singularity", "task-a2", None)
            .expect("upsert a2");
        store
            .upsert("sb-b1", "modal", "task-b1", None)
            .expect("upsert b1");

        let singularity_rows = store
            .list(Some("singularity"))
            .expect("list singularity should succeed");
        assert_eq!(singularity_rows.len(), 2);
        assert!(singularity_rows.iter().all(|r| r.backend == "singularity"));

        let all_rows = store.list(None).expect("list all should succeed");
        assert_eq!(all_rows.len(), 3);
    }

    #[test]
    fn sandbox_store_cleanup_older_than_removes_stale() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap should succeed");

        let store = SandboxStore::new(&db_path);
        store
            .upsert("sb-fresh", "singularity", "task-fresh", None)
            .expect("upsert fresh");
        store
            .upsert("sb-stale", "singularity", "task-stale", None)
            .expect("upsert stale");

        // Backdate the stale record's last_active by 10 days.
        let connection = open(&db_path).expect("open should succeed");
        connection
            .execute(
                "UPDATE sandboxes SET last_active = datetime('now', '-10 days') WHERE id = ?1",
                params!["sb-stale"],
            )
            .expect("backdate should succeed");
        drop(connection);

        let deleted = store.cleanup_older_than(5).expect("cleanup should succeed");
        assert_eq!(deleted, 1);

        let remaining = store.list(None).expect("list should succeed");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "sb-fresh");
    }

    #[test]
    fn migrate_to_v9_creates_sandboxes_table() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap should succeed");

        // Running migrate_to_v9 again must be idempotent (CREATE TABLE IF NOT EXISTS).
        let connection = open(&db_path).expect("open should succeed");
        migrate_to_v9(&connection, &db_path).expect("migrate_to_v9 should be idempotent");

        // Confirm the table exists by querying it.
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM sandboxes", [], |row| row.get(0))
            .expect("sandboxes table should exist");
        assert_eq!(count, 0);
    }
}

#[cfg(test)]
mod sticker_cache_tests {
    use super::{bootstrap, StickerCacheStore};
    use tempfile::tempdir;

    #[test]
    fn sticker_cache_stores_and_retrieves_description() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        store
            .set("unique-abc", "A cat waving hello", "😺", "CatPack")
            .expect("set should succeed");

        let entry = store
            .get("unique-abc")
            .expect("get should succeed")
            .expect("entry should exist");

        assert_eq!(entry.file_unique_id, "unique-abc");
        assert_eq!(entry.description, "A cat waving hello");
        assert_eq!(entry.emoji, "😺");
        assert_eq!(entry.sticker_set, "CatPack");
        assert!(!entry.created_at.is_empty());
    }

    #[test]
    fn sticker_cache_returns_none_for_missing_entry() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        let entry = store.get("nonexistent").expect("get should succeed");
        assert!(entry.is_none());
    }

    #[test]
    fn sticker_cache_upserts_on_conflict() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        store
            .set("unique-1", "A dog barking", "🐕", "DogPack")
            .expect("first set");
        store
            .set("unique-1", "A dog sleeping", "🐕", "DogPack")
            .expect("second set (upsert)");

        let entry = store.get("unique-1").unwrap().unwrap();
        assert_eq!(entry.description, "A dog sleeping");
    }

    #[test]
    fn sticker_cache_delete_removes_entry() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        store
            .set("del-1", "A bird flying", "🐦", "BirdPack")
            .unwrap();

        assert!(store.delete("del-1").unwrap());
        assert!(store.get("del-1").unwrap().is_none());
        assert!(!store.delete("del-1").unwrap()); // already deleted
    }

    #[test]
    fn sticker_cache_count_tracks_entries() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        assert_eq!(store.count().unwrap(), 0);

        store.set("c-1", "Sticker 1", "😀", "Pack1").unwrap();
        store.set("c-2", "Sticker 2", "😎", "Pack2").unwrap();
        assert_eq!(store.count().unwrap(), 2);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bootstrap, discover_legacy_source, inspect, latest_import_run, migrate_to_v6,
        migrate_to_v7, migrate_to_v8, migrate_to_v9, open, record_import_run, ImportStatus,
        LegacyImportSource, SessionStore, SCHEMA_VERSION,
    };
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn bootstrapping_creates_the_expected_schema_version() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");

        let bootstrap_result = bootstrap(&database_path).expect("bootstrap should succeed");
        let health = inspect(&database_path).expect("inspect should succeed");

        assert_eq!(bootstrap_result.schema_version, SCHEMA_VERSION);
        assert!(health.database_exists);
        assert_eq!(health.schema_version, Some(SCHEMA_VERSION));
    }

    #[test]
    fn legacy_source_discovery_prefers_known_legacy_layouts() {
        let dir = tempdir().expect("tempdir should exist");
        let root = dir.path();
        let data_dir = root.join("data");
        let config_path = root.join("cli-config.yaml");
        let database_path = data_dir.join("genesis.db");

        fs::create_dir_all(&data_dir).expect("data dir should be created");
        fs::write(&config_path, "profile: operator").expect("config file should exist");
        fs::write(&database_path, "").expect("database placeholder should exist");

        let source = discover_legacy_source(root);

        assert_eq!(source.config_path, Some(config_path));
        assert_eq!(source.data_dir, Some(data_dir));
        assert_eq!(source.database_path, Some(database_path));
    }

    #[test]
    fn import_runs_can_be_recorded_and_loaded() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let source = LegacyImportSource {
            root: dir.path().join("legacy-hermes"),
            config_path: Some(dir.path().join("legacy-hermes").join("cli-config.yaml")),
            data_dir: Some(dir.path().join("legacy-hermes").join("data")),
            database_path: Some(
                dir.path()
                    .join("legacy-hermes")
                    .join("data")
                    .join("genesis.db"),
            ),
        };

        let recorded = record_import_run(&database_path, &source, ImportStatus::Planned)
            .expect("recording should work");
        let latest = latest_import_run(&database_path)
            .expect("query should work")
            .expect("latest import run should exist");

        assert_eq!(recorded.id, latest.id);
        assert_eq!(latest.legacy_root, source.root);
        assert_eq!(latest.status, ImportStatus::Planned);
    }

    fn bootstrapped_store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        let store = SessionStore::new(&database_path);
        (dir, store)
    }

    #[test]
    fn session_store_creates_and_retrieves_session() {
        let (_dir, store) = bootstrapped_store();

        store
            .create_session("s-1", "cli", Some("test session"))
            .expect("create should work");

        let session = store
            .get_session("s-1")
            .expect("get should work")
            .expect("session should exist");

        assert_eq!(session.id, "s-1");
        assert_eq!(session.title.as_deref(), Some("test session"));
        assert_eq!(session.platform, "cli");
    }

    #[test]
    fn session_store_appends_and_loads_messages() {
        let (_dir, store) = bootstrapped_store();

        store
            .create_session("s-2", "cli", None)
            .expect("create should work");

        store
            .append_message("s-2", "user", Some("Hello Eve"), None, None, None)
            .expect("append user should work");
        store
            .append_message("s-2", "assistant", Some("Hi there!"), None, None, None)
            .expect("append assistant should work");
        store
            .append_message("s-2", "assistant", None, None, Some(r#"[{"id":"call_1","type":"function","function":{"name":"echo","arguments":"{\"message\":\"test\"}"}}]"#), None)
            .expect("append tool_calls should work");
        store
            .append_message("s-2", "tool", Some("test"), Some("call_1"), None, None)
            .expect("append tool result should work");

        let messages = store.load_messages("s-2").expect("load should work");

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content.as_deref(), Some("Hello Eve"));
        assert_eq!(messages[1].role, "assistant");
        assert!(messages[2].tool_calls_json.is_some());
        assert_eq!(messages[3].role, "tool");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn session_store_search_finds_matching_sessions() {
        let (_dir, store) = bootstrapped_store();

        store
            .create_session("s-3", "cli", None)
            .expect("create should work");
        store
            .create_session("s-4", "telegram", None)
            .expect("create should work");

        store
            .append_message(
                "s-3",
                "user",
                Some("Tell me about quantum computing"),
                None,
                None,
                None,
            )
            .expect("append should work");
        store
            .append_message(
                "s-4",
                "user",
                Some("What is the weather today"),
                None,
                None,
                None,
            )
            .expect("append should work");

        let results = store
            .search_sessions("quantum")
            .expect("search should work");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "s-3");
    }

    #[test]
    fn session_store_count_sessions() {
        let (_dir, store) = bootstrapped_store();
        assert_eq!(store.count_sessions().unwrap(), 0);

        store.create_session("s-count-1", "cli", None).unwrap();
        assert_eq!(store.count_sessions().unwrap(), 1);

        store.create_session("s-count-2", "api", None).unwrap();
        assert_eq!(store.count_sessions().unwrap(), 2);
    }

    #[test]
    fn schedule_store_creates_and_retrieves_schedule() {
        let (_dir, _session_store) = bootstrapped_store();
        let schedule_store = super::ScheduleStore::new(&_dir.path().join("genesis.db"));

        let schedule = schedule_store
            .create("sched-1", "*/5 * * * *", "cli", "run diagnostics")
            .expect("create should work");

        assert_eq!(schedule.id, "sched-1");
        assert_eq!(schedule.cron_expression, "*/5 * * * *");
        assert_eq!(schedule.destination, "cli");
        assert_eq!(schedule.prompt, "run diagnostics");
        assert!(schedule.enabled);

        let fetched = schedule_store
            .get("sched-1")
            .expect("get should work")
            .expect("schedule should exist");
        assert_eq!(fetched.id, "sched-1");
    }

    #[test]
    fn schedule_store_lists_enabled_only() {
        let (_dir, _session_store) = bootstrapped_store();
        let store = super::ScheduleStore::new(&_dir.path().join("genesis.db"));

        store.create("s1", "*/5 * * * *", "cli", "job1").unwrap();
        store.create("s2", "0 * * * *", "cli", "job2").unwrap();
        store.set_enabled("s2", false).unwrap();

        let enabled = store.list_enabled().expect("list should work");
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].id, "s1");

        let all = store.list_all().expect("list_all should work");
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn schedule_store_deletes_schedule() {
        let (_dir, _session_store) = bootstrapped_store();
        let store = super::ScheduleStore::new(&_dir.path().join("genesis.db"));

        store.create("s1", "*/5 * * * *", "cli", "job1").unwrap();
        assert!(store.delete("s1").unwrap());
        assert!(store.get("s1").unwrap().is_none());
        assert!(!store.delete("s1").unwrap()); // already deleted
    }

    #[test]
    fn schedule_store_creates_with_timezone() {
        let (_dir, _session_store) = bootstrapped_store();
        let store = super::ScheduleStore::new(&_dir.path().join("genesis.db"));

        let schedule = store
            .create_with_timezone("tz-1", "0 9 * * *", "cli", "morning", Some("America/New_York"))
            .expect("create should work");

        assert_eq!(schedule.id, "tz-1");
        assert_eq!(schedule.timezone.as_deref(), Some("America/New_York"));

        let fetched = store.get("tz-1").unwrap().unwrap();
        assert_eq!(fetched.timezone.as_deref(), Some("America/New_York"));
    }

    #[test]
    fn schedule_store_timezone_defaults_to_none() {
        let (_dir, _session_store) = bootstrapped_store();
        let store = super::ScheduleStore::new(&_dir.path().join("genesis.db"));

        let schedule = store
            .create("no-tz", "*/5 * * * *", "cli", "job")
            .expect("create should work");

        assert_eq!(schedule.timezone, None);
    }

    #[test]
    fn schedule_store_records_and_lists_executions() {
        let (_dir, _session_store) = bootstrapped_store();
        let store = super::ScheduleStore::new(&_dir.path().join("genesis.db"));

        store.create("exec-test", "*/5 * * * *", "cli", "job").unwrap();

        store
            .record_execution("exec-test", "success", None, Some(150))
            .expect("record should work");
        store
            .record_execution("exec-test", "error", Some("timeout"), Some(30000))
            .expect("record should work");

        let execs = store
            .list_executions("exec-test", 10)
            .expect("list should work");
        assert_eq!(execs.len(), 2);

        let statuses: Vec<&str> = execs.iter().map(|e| e.status.as_str()).collect();
        assert!(statuses.contains(&"success"));
        assert!(statuses.contains(&"error"));

        let error_exec = execs.iter().find(|e| e.status == "error").unwrap();
        assert_eq!(error_exec.error_message.as_deref(), Some("timeout"));
        assert_eq!(error_exec.duration_ms, Some(30000));

        let success_exec = execs.iter().find(|e| e.status == "success").unwrap();
        assert_eq!(success_exec.error_message, None);
        assert_eq!(success_exec.duration_ms, Some(150));
    }

    #[test]
    fn schedule_store_execution_history_empty_for_unknown_schedule() {
        let (_dir, _session_store) = bootstrapped_store();
        let store = super::ScheduleStore::new(&_dir.path().join("genesis.db"));

        let execs = store
            .list_executions("nonexistent", 10)
            .expect("list should work");
        assert!(execs.is_empty());
    }

    #[test]
    fn skill_store_creates_and_retrieves_skill() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::SkillStore::new(&db_path);
        let skill = store
            .upsert(
                "code_review",
                "Reviews code for bugs and style issues",
                "1. Read the file\n2. Check for common issues\n3. Report findings",
                Some("when user asks to review code"),
                &["dev", "review"],
            )
            .expect("upsert should work");

        assert_eq!(skill.name, "code_review");
        assert_eq!(skill.description, "Reviews code for bugs and style issues");
        assert_eq!(skill.version, 1);
        assert_eq!(skill.tags, vec!["dev", "review"]);
        assert_eq!(
            skill.trigger_hint.as_deref(),
            Some("when user asks to review code")
        );

        let fetched = store.get("code_review").unwrap().expect("should exist");
        assert_eq!(fetched.name, "code_review");
    }

    #[test]
    fn skill_store_upsert_bumps_version() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::SkillStore::new(&db_path);
        let v1 = store
            .upsert("deploy", "Deploy to prod", "run deploy script", None, &[])
            .expect("v1");
        assert_eq!(v1.version, 1);

        let v2 = store
            .upsert(
                "deploy",
                "Deploy to production (improved)",
                "run deploy script v2",
                None,
                &["ops"],
            )
            .expect("v2");
        assert_eq!(v2.version, 2);
        assert_eq!(v2.description, "Deploy to production (improved)");
        assert_eq!(v2.tags, vec!["ops"]);
    }

    #[test]
    fn skill_store_lists_and_deletes() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::SkillStore::new(&db_path);
        store
            .upsert("alpha", "A skill", "do A", None, &["a"])
            .unwrap();
        store
            .upsert("beta", "B skill", "do B", None, &["b"])
            .unwrap();

        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "alpha");
        assert_eq!(all[1].name, "beta");

        assert!(store.delete("alpha").unwrap());
        assert!(!store.delete("alpha").unwrap()); // already gone

        let remaining = store.list_all().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].name, "beta");
    }

    #[test]
    fn skill_store_finds_by_tag() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::SkillStore::new(&db_path);
        store
            .upsert(
                "web_scrape",
                "Scrape web",
                "use curl",
                None,
                &["web", "data"],
            )
            .unwrap();
        store
            .upsert(
                "deploy",
                "Deploy",
                "kubectl apply",
                None,
                &["ops", "deploy"],
            )
            .unwrap();
        store
            .upsert(
                "test_runner",
                "Run tests",
                "cargo test",
                None,
                &["dev", "test"],
            )
            .unwrap();

        let web_skills = store.find_by_tag("web").unwrap();
        assert_eq!(web_skills.len(), 1);
        assert_eq!(web_skills[0].name, "web_scrape");

        let dev_skills = store.find_by_tag("dev").unwrap();
        assert_eq!(dev_skills.len(), 1);
        assert_eq!(dev_skills[0].name, "test_runner");
    }

    #[test]
    fn skill_store_find_matching_scores_by_relevance() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::SkillStore::new(&db_path);
        store
            .upsert(
                "deploy",
                "Deploy app to production",
                "kubectl apply",
                Some("deploy production"),
                &["ops", "deploy"],
            )
            .unwrap();
        store
            .upsert(
                "review",
                "Review code for bugs",
                "check bugs",
                Some("review code"),
                &["dev", "review"],
            )
            .unwrap();
        store
            .upsert(
                "unrelated",
                "Unrelated skill",
                "do nothing",
                Some("bake cookies"),
                &["cooking"],
            )
            .unwrap();

        let matches = store
            .find_matching("please deploy to production", 5)
            .unwrap();
        assert!(!matches.is_empty(), "should find deploy skill");
        assert_eq!(matches[0].name, "deploy");
    }

    #[test]
    fn skill_store_find_matching_returns_empty_for_no_match() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::SkillStore::new(&db_path);
        store
            .upsert(
                "deploy",
                "Deploy app",
                "run deploy",
                Some("deploy"),
                &["ops"],
            )
            .unwrap();

        let matches = store.find_matching("quantum physics lecture", 5).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn skill_store_find_matching_respects_limit() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::SkillStore::new(&db_path);
        for i in 0..10 {
            let name = format!("deploy_{i}");
            store
                .upsert(
                    &name,
                    "Deploy variant",
                    "run it",
                    Some("deploy app"),
                    &["deploy"],
                )
                .unwrap();
        }

        let matches = store.find_matching("deploy my app", 3).unwrap();
        assert!(matches.len() <= 3, "should respect limit");
    }

    #[test]
    fn user_model_observes_and_retrieves_traits() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::UserModelStore::new(&db_path);
        let t = store
            .observe(
                "prefers_rust",
                "preference",
                "User prefers Rust over Python",
                Some("s1"),
            )
            .expect("observe");

        assert_eq!(t.trait_key, "prefers_rust");
        assert_eq!(t.category, "preference");
        assert_eq!(t.confidence, 0.5);
        assert_eq!(t.evidence_count, 1);
    }

    #[test]
    fn user_model_increases_confidence_on_repeat_observations() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::UserModelStore::new(&db_path);
        store
            .observe(
                "likes_concise",
                "communication_style",
                "Prefers short answers",
                None,
            )
            .unwrap();
        store
            .observe(
                "likes_concise",
                "communication_style",
                "Prefers short answers",
                None,
            )
            .unwrap();
        let t = store
            .observe(
                "likes_concise",
                "communication_style",
                "Prefers short answers",
                None,
            )
            .unwrap();

        assert_eq!(t.evidence_count, 3);
        assert!((t.confidence - 0.7).abs() < 0.01); // 0.5 + 0.1 + 0.1 = 0.7
    }

    #[test]
    fn user_model_confidence_caps_at_one() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::UserModelStore::new(&db_path);
        // Observe 10 times: 0.5 + 9*0.1 = 1.4 -> capped at 1.0
        for _ in 0..10 {
            store
                .observe(
                    "expert_rust",
                    "expertise",
                    "Expert-level Rust developer",
                    None,
                )
                .unwrap();
        }
        let t = store.get("expert_rust").unwrap().expect("should exist");
        assert!(t.confidence <= 1.0);
        assert_eq!(t.evidence_count, 10);
    }

    #[test]
    fn user_model_lists_by_category() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::UserModelStore::new(&db_path);
        store
            .observe("pref_dark_mode", "preference", "Likes dark mode", None)
            .unwrap();
        store
            .observe("pref_vim", "preference", "Uses vim bindings", None)
            .unwrap();
        store
            .observe("style_formal", "communication_style", "Formal tone", None)
            .unwrap();

        let prefs = store.list_by_category("preference").unwrap();
        assert_eq!(prefs.len(), 2);

        let styles = store.list_by_category("communication_style").unwrap();
        assert_eq!(styles.len(), 1);
    }

    #[test]
    fn user_model_filters_by_confidence_threshold() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::UserModelStore::new(&db_path);
        store
            .observe("low_conf", "preference", "Maybe likes X", None)
            .unwrap();
        // Observe "high_conf" 4 times: confidence = 0.5 + 3*0.1 = 0.8
        for _ in 0..4 {
            store
                .observe("high_conf", "preference", "Definitely likes Y", None)
                .unwrap();
        }

        let confident = store.confident_traits(0.7).unwrap();
        assert_eq!(confident.len(), 1);
        assert_eq!(confident[0].trait_key, "high_conf");
    }

    #[test]
    fn user_model_deletes_trait() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::UserModelStore::new(&db_path);
        store
            .observe("temp", "preference", "Temporary", None)
            .unwrap();
        assert!(store.delete("temp").unwrap());
        assert!(!store.delete("temp").unwrap());
        assert!(store.get("temp").unwrap().is_none());
    }

    #[test]
    fn subagent_store_creates_and_retrieves() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let session_store = SessionStore::new(&db_path);
        session_store
            .create_session("parent-1", "cli", None)
            .expect("parent session");
        session_store
            .create_session("child-1", "subagent", Some("Subagent: worker"))
            .expect("child session");

        let store = super::SubagentStore::new(&db_path);
        let record = store
            .create("sub-1", "parent-1", "child-1", "worker", "do the thing")
            .expect("create");

        assert_eq!(record.id, "sub-1");
        assert_eq!(record.parent_session_id, "parent-1");
        assert_eq!(record.child_session_id, "child-1");
        assert_eq!(record.name, "worker");
        assert_eq!(record.task, "do the thing");
        assert_eq!(record.status, "pending");
        assert!(record.result.is_none());
        assert!(record.error.is_none());

        let fetched = store.get("sub-1").unwrap().expect("should exist");
        assert_eq!(fetched.id, "sub-1");
    }

    #[test]
    fn subagent_store_status_transitions() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let session_store = SessionStore::new(&db_path);
        session_store.create_session("p", "cli", None).unwrap();
        session_store.create_session("c", "subagent", None).unwrap();

        let store = super::SubagentStore::new(&db_path);
        store
            .create("sub-2", "p", "c", "runner", "build it")
            .unwrap();

        // pending -> running
        assert!(store.set_running("sub-2").unwrap());
        let r = store.get("sub-2").unwrap().unwrap();
        assert_eq!(r.status, "running");

        // running -> completed
        assert!(store.set_completed("sub-2", "Built successfully!").unwrap());
        let r = store.get("sub-2").unwrap().unwrap();
        assert_eq!(r.status, "completed");
        assert_eq!(r.result.as_deref(), Some("Built successfully!"));
        assert!(r.completed_at.is_some());
    }

    #[test]
    fn subagent_store_lists_by_parent() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let session_store = SessionStore::new(&db_path);
        session_store.create_session("p1", "cli", None).unwrap();
        session_store.create_session("p2", "cli", None).unwrap();
        session_store
            .create_session("c1", "subagent", None)
            .unwrap();
        session_store
            .create_session("c2", "subagent", None)
            .unwrap();
        session_store
            .create_session("c3", "subagent", None)
            .unwrap();

        let store = super::SubagentStore::new(&db_path);
        store.create("s1", "p1", "c1", "a", "task a").unwrap();
        store.create("s2", "p1", "c2", "b", "task b").unwrap();
        store.create("s3", "p2", "c3", "c", "task c").unwrap();

        let p1_subs = store.list_by_parent("p1").unwrap();
        assert_eq!(p1_subs.len(), 2);

        let p2_subs = store.list_by_parent("p2").unwrap();
        assert_eq!(p2_subs.len(), 1);
        assert_eq!(p2_subs[0].name, "c");
    }

    #[test]
    fn subagent_store_set_failed() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let session_store = SessionStore::new(&db_path);
        session_store.create_session("p", "cli", None).unwrap();
        session_store.create_session("c", "subagent", None).unwrap();

        let store = super::SubagentStore::new(&db_path);
        store.create("sub-f", "p", "c", "failing", "crash").unwrap();
        store.set_running("sub-f").unwrap();
        store.set_failed("sub-f", "out of tokens").unwrap();

        let r = store.get("sub-f").unwrap().unwrap();
        assert_eq!(r.status, "failed");
        assert_eq!(r.error.as_deref(), Some("out of tokens"));
        assert!(r.completed_at.is_some());
    }

    #[test]
    fn skill_usage_store_records_and_retrieves() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = super::SkillStore::new(&db_path);
        skill_store
            .upsert("deploy", "Deploy app", "Run deploy", None, &[])
            .unwrap();

        let store = super::SkillUsageStore::new(&db_path);
        let usage = store
            .record_usage("deploy", Some("s-1"), "success", Some("Worked well"))
            .unwrap();

        assert_eq!(usage.skill_name, "deploy");
        assert_eq!(usage.outcome, "success");
        assert_eq!(usage.feedback.as_deref(), Some("Worked well"));
        assert!(!usage.refined);
    }

    #[test]
    fn skill_usage_store_stats_aggregates() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = super::SkillStore::new(&db_path);
        skill_store
            .upsert("test", "Test skill", "Run tests", None, &[])
            .unwrap();

        let store = super::SkillUsageStore::new(&db_path);
        store.record_usage("test", None, "success", None).unwrap();
        store.record_usage("test", None, "success", None).unwrap();
        store
            .record_usage("test", None, "failure", Some("Flaky"))
            .unwrap();

        let stats = store.stats("test").unwrap();
        assert_eq!(stats.total_uses, 3);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.failures, 1);
        assert!(stats.last_used.is_some());
    }

    #[test]
    fn skill_usage_store_mark_refined() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = super::SkillStore::new(&db_path);
        skill_store
            .upsert("refine_me", "Refine test", "v1 instructions", None, &[])
            .unwrap();

        let store = super::SkillUsageStore::new(&db_path);
        let usage = store
            .record_usage("refine_me", None, "partial", Some("Needs improvement"))
            .unwrap();

        store.mark_refined(usage.id).unwrap();

        let recent = store.recent_usages("refine_me", 5).unwrap();
        assert_eq!(recent.len(), 1);
        assert!(recent[0].refined);
    }

    #[test]
    fn skill_usage_store_recent_respects_limit() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = super::SkillStore::new(&db_path);
        skill_store
            .upsert("many", "Many uses", "Instructions", None, &[])
            .unwrap();

        let store = super::SkillUsageStore::new(&db_path);
        for _ in 0..5 {
            store.record_usage("many", None, "success", None).unwrap();
        }

        let recent = store.recent_usages("many", 3).unwrap();
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn skill_file_store_round_trip_and_list() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = super::SkillStore::new(&db_path);
        skill_store
            .upsert("deploy", "Deploy app", "Run deploy", None, &[])
            .unwrap();

        let store = super::SkillFileStore::new(&db_path);
        store
            .store_file("deploy", "references/api.md", "# API\n...")
            .unwrap();
        store
            .store_file("deploy", "examples/example.txt", "hello")
            .unwrap();

        let content = store
            .get_file("deploy", "references/api.md")
            .unwrap()
            .expect("content");
        assert_eq!(content, "# API\n...");

        let files = store.list_files("deploy").unwrap();
        assert_eq!(
            files,
            vec![
                "examples/example.txt".to_owned(),
                "references/api.md".to_owned()
            ]
        );
    }

    #[test]
    fn skill_file_store_delete_one_and_all() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = super::SkillStore::new(&db_path);
        skill_store
            .upsert("review", "Review code", "Read file", None, &[])
            .unwrap();

        let store = super::SkillFileStore::new(&db_path);
        store.store_file("review", "refs/a.md", "a").unwrap();
        store.store_file("review", "refs/b.md", "b").unwrap();

        assert!(store.delete_file("review", "refs/a.md").unwrap());
        assert_eq!(store.get_file("review", "refs/a.md").unwrap(), None);

        let deleted = store.delete_all_files("review").unwrap();
        assert_eq!(deleted, 1);
        assert!(store.list_files("review").unwrap().is_empty());
    }

    #[test]
    fn skill_file_store_updates_existing_file() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let skill_store = super::SkillStore::new(&db_path);
        skill_store
            .upsert("test", "Test skill", "Do test", None, &[])
            .unwrap();

        let store = super::SkillFileStore::new(&db_path);
        store.store_file("test", "refs/doc.md", "v1").unwrap();
        store.store_file("test", "refs/doc.md", "v2").unwrap();

        let content = store
            .get_file("test", "refs/doc.md")
            .unwrap()
            .expect("content");
        assert_eq!(content, "v2");
        assert_eq!(store.list_files("test").unwrap().len(), 1);
    }

    #[test]
    fn session_store_load_returns_empty_for_unknown_session() {
        let (_dir, store) = bootstrapped_store();

        let messages = store
            .load_messages("nonexistent")
            .expect("load should work");

        assert!(messages.is_empty());
    }

    #[test]
    fn insights_returns_data_for_recent_sessions() {
        let (_dir, store) = bootstrapped_store();
        store.create_session("s1", "cli", None).unwrap();
        store.create_session("s2", "api", None).unwrap();
        store.add_usage("s1", 100, 50).unwrap();
        store.add_usage("s2", 200, 100).unwrap();

        let data = store.insights(30).unwrap();
        assert_eq!(data.period_days, 30);
        assert_eq!(data.sessions_count, 2);
        assert_eq!(data.total_input_tokens, 300);
        assert_eq!(data.total_output_tokens, 150);
        assert!(!data.sessions_per_day.is_empty());
        assert!(data.platform_breakdown.iter().any(|(p, _)| p == "cli"));
        assert!(data.platform_breakdown.iter().any(|(p, _)| p == "api"));
    }

    #[test]
    fn fork_session_copies_messages_and_sets_parent() {
        let (_dir, store) = bootstrapped_store();
        store
            .create_session("s-orig", "cli", Some("Original"))
            .unwrap();
        store
            .append_message("s-orig", "system", Some("sys"), None, None, None)
            .unwrap();
        store
            .append_message("s-orig", "user", Some("hello"), None, None, None)
            .unwrap();
        store
            .append_message("s-orig", "assistant", Some("hi"), None, None, None)
            .unwrap();

        let forked_id = store.fork_session("s-orig", "s-fork").unwrap();
        assert_eq!(forked_id, "s-fork");

        // Check the forked session exists
        let session = store
            .get_session("s-fork")
            .unwrap()
            .expect("forked session should exist");
        assert_eq!(session.title.as_deref(), Some("Original (fork)"));
        assert_eq!(session.platform, "cli");
        assert_eq!(session.parent_session_id.as_deref(), Some("s-orig"));

        // Check messages were copied
        let messages = store.load_messages("s-fork").unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
    }

    #[test]
    fn fork_session_without_title() {
        let (_dir, store) = bootstrapped_store();
        store.create_session("s-notitle", "api", None).unwrap();
        store
            .append_message("s-notitle", "user", Some("test"), None, None, None)
            .unwrap();

        store.fork_session("s-notitle", "s-fork2").unwrap();

        let session = store.get_session("s-fork2").unwrap().expect("exists");
        assert_eq!(session.title.as_deref(), Some("Fork"));
        assert_eq!(session.parent_session_id.as_deref(), Some("s-notitle"));
    }

    #[test]
    fn session_summary_has_no_parent_by_default() {
        let (_dir, store) = bootstrapped_store();
        store.create_session("s-nop", "cli", None).unwrap();

        let session = store.get_session("s-nop").unwrap().expect("exists");
        assert!(session.parent_session_id.is_none());
    }

    #[test]
    fn delete_last_n_messages_removes_most_recent() {
        let (_dir, store) = bootstrapped_store();
        store.create_session("s-del", "cli", None).unwrap();
        store
            .append_message("s-del", "system", Some("sys"), None, None, None)
            .unwrap();
        store
            .append_message("s-del", "user", Some("msg1"), None, None, None)
            .unwrap();
        store
            .append_message("s-del", "assistant", Some("resp1"), None, None, None)
            .unwrap();
        store
            .append_message("s-del", "user", Some("msg2"), None, None, None)
            .unwrap();
        store
            .append_message("s-del", "assistant", Some("resp2"), None, None, None)
            .unwrap();

        let deleted = store.delete_last_n_messages("s-del", 2).unwrap();
        assert_eq!(deleted, 2);

        let remaining = store.load_messages("s-del").unwrap();
        assert_eq!(remaining.len(), 3);
        assert_eq!(remaining[0].role, "system");
        assert_eq!(remaining[1].role, "user");
        assert_eq!(remaining[2].role, "assistant");
        assert_eq!(remaining[2].content.as_deref(), Some("resp1"));
    }

    #[test]
    fn delete_last_n_messages_zero_is_noop() {
        let (_dir, store) = bootstrapped_store();
        store.create_session("s-noop", "cli", None).unwrap();
        store
            .append_message("s-noop", "user", Some("hello"), None, None, None)
            .unwrap();

        let deleted = store.delete_last_n_messages("s-noop", 0).unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(store.load_messages("s-noop").unwrap().len(), 1);
    }

    #[test]
    fn delete_last_n_messages_more_than_exists() {
        let (_dir, store) = bootstrapped_store();
        store.create_session("s-over", "cli", None).unwrap();
        store
            .append_message("s-over", "user", Some("hello"), None, None, None)
            .unwrap();

        let deleted = store.delete_last_n_messages("s-over", 100).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(store.load_messages("s-over").unwrap().len(), 0);
    }

    #[test]
    fn import_session_creates_session_and_messages() {
        let (_dir, store) = bootstrapped_store();

        let messages = vec![
            ("user".to_owned(), "hello".to_owned()),
            ("assistant".to_owned(), "hi there".to_owned()),
            ("user".to_owned(), "how are you?".to_owned()),
        ];

        let id = store
            .import_session("import-1", Some("Test Import"), messages)
            .expect("import should succeed");

        assert_eq!(id, "import-1");

        let session = store
            .get_session("import-1")
            .expect("get should work")
            .expect("session should exist");
        assert_eq!(session.platform, "import");
        assert_eq!(session.title.as_deref(), Some("Test Import"));

        let stored = store.load_messages("import-1").expect("load should work");
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[0].role, "user");
        assert_eq!(stored[0].content.as_deref(), Some("hello"));
        assert_eq!(stored[1].role, "assistant");
        assert_eq!(stored[1].content.as_deref(), Some("hi there"));
        assert_eq!(stored[2].role, "user");
        assert_eq!(stored[2].content.as_deref(), Some("how are you?"));
    }

    #[test]
    fn import_session_with_no_title() {
        let (_dir, store) = bootstrapped_store();

        let messages = vec![("user".to_owned(), "test".to_owned())];

        let id = store
            .import_session("import-notitle", None, messages)
            .expect("import should succeed");
        assert_eq!(id, "import-notitle");

        let session = store
            .get_session("import-notitle")
            .expect("get should work")
            .expect("session should exist");
        assert!(session.title.is_none());
    }

    #[test]
    fn import_session_empty_messages() {
        let (_dir, store) = bootstrapped_store();

        let id = store
            .import_session("import-empty", Some("Empty"), vec![])
            .expect("import should succeed");
        assert_eq!(id, "import-empty");

        let stored = store
            .load_messages("import-empty")
            .expect("load should work");
        assert_eq!(stored.len(), 0);
    }

    // -----------------------------------------------------------------------
    // PairingStore
    // -----------------------------------------------------------------------

    #[test]
    fn pairing_store_basic_flow() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::PairingStore::new(&db_path);

        // Initially no one is approved
        assert!(!store.is_approved("telegram", "user123").unwrap());
        assert!(store.list_approved(None).unwrap().is_empty());

        // Generate a code
        let code = store
            .generate_code("telegram", "user123", "Alice")
            .unwrap()
            .expect("should get a code");
        assert_eq!(code.len(), 8);

        // Check pending list
        let pending = store.list_pending(None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].platform, "telegram");
        assert_eq!(pending[0].user_id, "user123");
        assert_eq!(pending[0].user_name, "Alice");

        // Approve the code
        let approved = store
            .approve_code("telegram", &code)
            .unwrap()
            .expect("should approve");
        assert_eq!(approved.user_id, "user123");
        assert_eq!(approved.user_name, "Alice");

        // Now the user is approved
        assert!(store.is_approved("telegram", "user123").unwrap());

        // Pending should be empty
        assert!(store.list_pending(None).unwrap().is_empty());

        // Approved list should have one entry
        let approved_list = store.list_approved(None).unwrap();
        assert_eq!(approved_list.len(), 1);
    }

    #[test]
    fn pairing_store_approve_wrong_code() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::PairingStore::new(&db_path);
        store.generate_code("discord", "u1", "Bob").unwrap();

        // Wrong code returns None
        let result = store.approve_code("discord", "WRONGCODE").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn pairing_store_revoke() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::PairingStore::new(&db_path);
        let code = store
            .generate_code("slack", "u2", "Carol")
            .unwrap()
            .unwrap();
        store.approve_code("slack", &code).unwrap();

        assert!(store.is_approved("slack", "u2").unwrap());
        assert!(store.revoke("slack", "u2").unwrap());
        assert!(!store.is_approved("slack", "u2").unwrap());

        // Revoking again returns false
        assert!(!store.revoke("slack", "u2").unwrap());
    }

    #[test]
    fn pairing_store_max_pending() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::PairingStore::new(&db_path);

        // Generate max codes
        for i in 0..3 {
            store
                .generate_code("telegram", &format!("user{i}"), "")
                .unwrap()
                .expect("should generate");
        }

        // Fourth should fail (max pending reached)
        let result = store.generate_code("telegram", "user99", "").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn pairing_store_clear_pending() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::PairingStore::new(&db_path);
        store.generate_code("telegram", "u1", "").unwrap();
        store.generate_code("discord", "u2", "").unwrap();

        let cleared = store.clear_pending(Some("telegram")).unwrap();
        assert_eq!(cleared, 1);

        // Discord pending still exists
        let pending = store.list_pending(None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].platform, "discord");
    }

    #[test]
    fn pairing_store_no_code_for_approved_user() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::PairingStore::new(&db_path);
        let code = store.generate_code("telegram", "u1", "").unwrap().unwrap();
        store.approve_code("telegram", &code).unwrap();

        // Generating a code for an already-approved user returns None
        let result = store.generate_code("telegram", "u1", "").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn pairing_store_platform_isolation() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::PairingStore::new(&db_path);

        // Approve on telegram
        let code = store.generate_code("telegram", "u1", "").unwrap().unwrap();
        store.approve_code("telegram", &code).unwrap();

        // Not approved on discord
        assert!(store.is_approved("telegram", "u1").unwrap());
        assert!(!store.is_approved("discord", "u1").unwrap());

        // Platform-filtered list
        let tg = store.list_approved(Some("telegram")).unwrap();
        assert_eq!(tg.len(), 1);
        let dc = store.list_approved(Some("discord")).unwrap();
        assert_eq!(dc.len(), 0);
    }

    #[test]
    fn response_cache_store_set_and_get() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let cache = super::ResponseCacheStore::new(&db_path);
        cache
            .set("key-1", "gpt-4", "Hello world", None, 100, 20, 3600)
            .unwrap();

        let entry = cache
            .get("key-1")
            .unwrap()
            .expect("should find cached entry");
        assert_eq!(entry.model, "gpt-4");
        assert_eq!(entry.response, "Hello world");
        assert_eq!(entry.input_tokens, 100);
        assert_eq!(entry.output_tokens, 20);
        assert!(entry.tool_calls_json.is_none());
    }

    #[test]
    fn response_cache_miss_returns_none() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let cache = super::ResponseCacheStore::new(&db_path);
        let result = cache.get("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn response_cache_hit_increments_counter() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let cache = super::ResponseCacheStore::new(&db_path);
        cache
            .set("key-2", "gpt-4", "cached", None, 50, 10, 3600)
            .unwrap();

        let _ = cache.get("key-2").unwrap();
        let _ = cache.get("key-2").unwrap();
        let entry = cache.get("key-2").unwrap().expect("should exist");
        // Each get reads then increments, so after 3 gets the stored count is 3
        // but the returned entry from the 3rd get shows 2 (read before increment)
        assert!(entry.hit_count >= 2, "hit_count should track accesses");
    }

    #[test]
    fn response_cache_expired_entry_not_returned() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let cache = super::ResponseCacheStore::new(&db_path);
        // TTL of 0 seconds = immediately expired
        cache
            .set("expired", "gpt-4", "old", None, 10, 5, 0)
            .unwrap();

        let result = cache.get("expired").unwrap();
        assert!(result.is_none(), "expired entry should not be returned");
    }

    #[test]
    fn response_cache_clear_removes_all() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let cache = super::ResponseCacheStore::new(&db_path);
        cache.set("a", "gpt-4", "1", None, 10, 5, 3600).unwrap();
        cache.set("b", "gpt-4", "2", None, 10, 5, 3600).unwrap();

        let (entries, _) = cache.stats().unwrap();
        assert_eq!(entries, 2);

        let deleted = cache.clear().unwrap();
        assert_eq!(deleted, 2);

        let (entries, _) = cache.stats().unwrap();
        assert_eq!(entries, 0);
    }

    #[test]
    fn response_cache_stats_tracks_entries_and_hits() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let cache = super::ResponseCacheStore::new(&db_path);
        cache.set("s1", "gpt-4", "resp", None, 10, 5, 3600).unwrap();
        let _ = cache.get("s1").unwrap(); // 1 hit
        let _ = cache.get("s1").unwrap(); // 2 hits

        let (entries, hits) = cache.stats().unwrap();
        assert_eq!(entries, 1);
        assert_eq!(hits, 2);
    }

    #[test]
    fn response_cache_with_tool_calls() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let cache = super::ResponseCacheStore::new(&db_path);
        let tc = r#"[{"id":"tc-1","type":"function","function":{"name":"echo","arguments":"{}"}}]"#;
        cache
            .set("tc-key", "gpt-4", "Using tool", Some(tc), 100, 30, 3600)
            .unwrap();

        let entry = cache.get("tc-key").unwrap().expect("should exist");
        assert_eq!(entry.tool_calls_json.as_deref(), Some(tc));
    }

    #[test]
    fn audit_log_records_and_queries_by_session() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        super::bootstrap(&db_path).unwrap();

        let store = super::AuditLogStore::new(&db_path);
        let details = serde_json::json!({"tool": "shell_execute", "args": "ls"});
        store.log(Some("s1"), "tool_call", &details).unwrap();
        store
            .log(
                Some("s1"),
                "llm_request",
                &serde_json::json!({"model": "gpt-4"}),
            )
            .unwrap();
        store
            .log(
                Some("s2"),
                "tool_call",
                &serde_json::json!({"tool": "memory_create"}),
            )
            .unwrap();

        let entries = store.by_session("s1", 100).unwrap();
        assert_eq!(entries.len(), 2);
        // Most recent first
        assert_eq!(entries[0].event_type, "llm_request");
        assert_eq!(entries[1].event_type, "tool_call");
    }

    #[test]
    fn audit_log_queries_by_event_type() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        super::bootstrap(&db_path).unwrap();

        let store = super::AuditLogStore::new(&db_path);
        store
            .log(Some("s1"), "tool_call", &serde_json::json!({"tool": "a"}))
            .unwrap();
        store
            .log(Some("s2"), "tool_call", &serde_json::json!({"tool": "b"}))
            .unwrap();
        store
            .log(Some("s1"), "llm_request", &serde_json::json!({}))
            .unwrap();

        let entries = store.by_event_type("tool_call", 100).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn audit_log_recent_returns_all() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        super::bootstrap(&db_path).unwrap();

        let store = super::AuditLogStore::new(&db_path);
        store
            .log(None, "config_change", &serde_json::json!({"key": "model"}))
            .unwrap();
        store
            .log(Some("s1"), "tool_call", &serde_json::json!({}))
            .unwrap();

        let entries = store.recent(100).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn audit_log_stats_groups_by_type() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        super::bootstrap(&db_path).unwrap();

        let store = super::AuditLogStore::new(&db_path);
        store
            .log(None, "tool_call", &serde_json::json!({}))
            .unwrap();
        store
            .log(None, "tool_call", &serde_json::json!({}))
            .unwrap();
        store
            .log(None, "llm_request", &serde_json::json!({}))
            .unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].0, "tool_call");
        assert_eq!(stats[0].1, 2);
        assert_eq!(stats[1].0, "llm_request");
        assert_eq!(stats[1].1, 1);
    }

    #[test]
    fn audit_log_purge_deletes_nothing_for_recent() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        super::bootstrap(&db_path).unwrap();

        let store = super::AuditLogStore::new(&db_path);
        store.log(None, "test", &serde_json::json!({})).unwrap();

        let deleted = store.purge_older_than(30).unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(store.recent(100).unwrap().len(), 1);
    }

    #[test]
    fn audit_log_limit_works() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        super::bootstrap(&db_path).unwrap();

        let store = super::AuditLogStore::new(&db_path);
        for i in 0..10 {
            store
                .log(Some("s1"), "tool_call", &serde_json::json!({"i": i}))
                .unwrap();
        }

        let entries = store.by_session("s1", 3).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn tool_analytics_aggregates_by_tool() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        super::bootstrap(&db_path).unwrap();

        let store = super::AuditLogStore::new(&db_path);
        store
            .log(
                Some("s1"),
                "tool_call_end",
                &serde_json::json!({"tool": "shell_execute", "success": true, "duration_ms": 100}),
            )
            .unwrap();
        store
            .log(
                Some("s1"),
                "tool_call_end",
                &serde_json::json!({"tool": "shell_execute", "success": true, "duration_ms": 200}),
            )
            .unwrap();
        store
            .log(
                Some("s1"),
                "tool_call_end",
                &serde_json::json!({"tool": "file_read", "success": false, "duration_ms": 50}),
            )
            .unwrap();

        let analytics = store.tool_analytics(30).unwrap();
        assert_eq!(analytics.len(), 2);
        // shell_execute has more calls, should be first
        assert_eq!(analytics[0].tool_name, "shell_execute");
        assert_eq!(analytics[0].call_count, 2);
        assert_eq!(analytics[0].success_count, 2);
        assert!((analytics[0].avg_duration_ms - 150.0).abs() < 1.0);

        assert_eq!(analytics[1].tool_name, "file_read");
        assert_eq!(analytics[1].call_count, 1);
        assert_eq!(analytics[1].success_count, 0);
    }

    #[test]
    fn llm_analytics_aggregates_by_model() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        super::bootstrap(&db_path).unwrap();

        let store = super::AuditLogStore::new(&db_path);
        store
            .log(
                Some("s1"),
                "llm_response",
                &serde_json::json!({"model": "gpt-4", "input_tokens": 100, "output_tokens": 50}),
            )
            .unwrap();
        store
            .log(
                Some("s1"),
                "llm_response",
                &serde_json::json!({"model": "gpt-4", "input_tokens": 200, "output_tokens": 80}),
            )
            .unwrap();
        store.log(Some("s1"), "llm_response", &serde_json::json!({"model": "claude-3", "input_tokens": 300, "output_tokens": 100})).unwrap();

        let analytics = store.llm_analytics(30).unwrap();
        assert_eq!(analytics.len(), 2);
        assert_eq!(analytics[0].model, "gpt-4");
        assert_eq!(analytics[0].call_count, 2);
        assert_eq!(analytics[0].total_input_tokens, 300);
        assert_eq!(analytics[0].total_output_tokens, 130);
    }

    #[test]
    fn migrations_v6_v7_v8_are_idempotent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");

        // First bootstrap creates all tables
        bootstrap(&database_path).expect("bootstrap should succeed");

        // Running bootstrap again (which re-runs all migrations) should not fail
        // because all migrations use CREATE TABLE IF NOT EXISTS
        bootstrap(&database_path).expect("second bootstrap should succeed");

        let health = inspect(&database_path).expect("inspect should succeed");
        assert_eq!(health.schema_version, Some(SCHEMA_VERSION));
    }

    #[test]
    fn v6_migration_creates_response_cache_and_audit_log() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        let connection = open(&database_path).expect("open should work");

        // Create minimal schema without response_cache/audit_log
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .unwrap();

        // Run v6 migration
        migrate_to_v6(&connection, &database_path).expect("v6 migration should succeed");

        // Verify tables exist by querying them
        connection
            .execute(
                "INSERT INTO response_cache (cache_key, model, response, expires_at) VALUES ('k', 'm', 'r', '2099-01-01')",
                [],
            )
            .expect("response_cache should exist");
        connection
            .execute("INSERT INTO audit_log (event_type) VALUES ('test')", [])
            .expect("audit_log should exist");
    }

    #[test]
    fn v7_migration_creates_channels_table() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        let connection = open(&database_path).expect("open should work");

        // Run v7 migration
        migrate_to_v7(&connection, &database_path).expect("v7 migration should succeed");

        // Verify table exists
        connection
            .execute(
                "INSERT INTO channels (platform, channel_id, channel_name) VALUES ('slack', 'C1', 'general')",
                [],
            )
            .expect("channels table should exist");
    }

    #[test]
    fn v8_migration_creates_sticker_cache_table() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        let connection = open(&database_path).expect("open should work");

        // Run v8 migration
        migrate_to_v8(&connection, &database_path).expect("v8 migration should succeed");

        // Verify table exists
        connection
            .execute(
                "INSERT INTO sticker_cache (file_unique_id, description) VALUES ('abc', 'a cat')",
                [],
            )
            .expect("sticker_cache table should exist");
    }

    #[test]
    fn v9_migration_adds_provider_metadata_column() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store.create_session("s-meta", "cli", None).expect("create");
        store
            .append_message(
                "s-meta",
                "assistant",
                Some("hello"),
                None,
                None,
                Some(r#"{"codex_reasoning_items":[]}"#),
            )
            .expect("append with metadata");

        let messages = store.load_messages("s-meta").expect("load");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].provider_metadata.as_deref(),
            Some(r#"{"codex_reasoning_items":[]}"#)
        );
    }

    #[test]
    fn migrate_to_v9_is_idempotent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        // Bootstrap creates all tables including messages, then calls migrate_to_v9 once.
        bootstrap(&database_path).expect("bootstrap should succeed");

        // Running migrate_to_v9 again should be idempotent (column already exists).
        let connection = open(&database_path).expect("open should work");
        migrate_to_v9(&connection, &database_path).expect("second v9 migration should succeed");
    }

    #[test]
    fn provider_metadata_defaults_to_none_in_messages() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("s-no-meta", "cli", None)
            .expect("create");
        store
            .append_message("s-no-meta", "user", Some("hello"), None, None, None)
            .expect("append");

        let messages = store.load_messages("s-no-meta").expect("load");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].provider_metadata.is_none());
    }
}

#[cfg(test)]
mod memory_store_tests {
    use super::{bootstrap, EmbeddingStore, MemoryStore, NewMemoryNote, SessionStore};
    use tempfile::tempdir;

    fn setup(dir: &std::path::Path) -> std::path::PathBuf {
        let db_path = dir.join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let store = SessionStore::new(&db_path);
        store.create_session("s1", "test", None).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem1', 's1', 'fact', 'hello world', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem2', 's1', 'preference', 'likes rust', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        drop(conn);
        db_path
    }

    #[test]
    fn get_returns_existing_memory() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = MemoryStore::new(&db_path);

        let memory = store.get("mem1").unwrap();
        assert!(memory.is_some());
        let memory = memory.unwrap();
        assert_eq!(memory.id, "mem1");
        assert_eq!(memory.kind, "fact");
        assert_eq!(memory.content, "hello world");
    }

    #[test]
    fn get_returns_none_for_nonexistent() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = MemoryStore::new(&db_path);

        let memory = store.get("nonexistent").unwrap();
        assert!(memory.is_none());
    }

    #[test]
    fn create_note_persists_bidirectional_links() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        store
            .create_note(NewMemoryNote {
                id: "note-b".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust powers Genesis".to_owned(),
                keywords: vec!["rust".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.6,
            })
            .unwrap();

        store
            .create_note(NewMemoryNote {
                id: "note-a".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis uses Rust".to_owned(),
                keywords: vec!["genesis".to_owned(), "rust".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["note-b".to_owned()],
                importance: 0.8,
            })
            .unwrap();

        let a_links = store.links_for("note-a").unwrap();
        let b_links = store.links_for("note-b").unwrap();
        assert_eq!(a_links, vec!["note-b".to_owned()]);
        assert_eq!(b_links, vec!["note-a".to_owned()]);
    }

    #[test]
    fn importance_decays_with_time_since_access() {
        let original = 1.0_f32;
        let decayed = super::decayed_importance(
            original,
            "2026-03-01T00:00:00Z",
            &super::parse_timestamp("2026-03-11T00:00:00Z").unwrap(),
        )
        .expect("timestamps should parse");
        assert!(decayed < original);
    }

    #[test]
    fn create_note_allows_links_to_notes_inserted_later() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        store
            .create_note(NewMemoryNote {
                id: "note-a".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis uses Rust".to_owned(),
                keywords: vec!["genesis".to_owned(), "rust".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["note-b".to_owned()],
                importance: 0.8,
            })
            .unwrap();

        store
            .create_note(NewMemoryNote {
                id: "note-b".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust powers Genesis".to_owned(),
                keywords: vec!["rust".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.6,
            })
            .unwrap();

        let a_links = store.links_for("note-a").unwrap();
        let b_links = store.links_for("note-b").unwrap();
        assert_eq!(a_links, vec!["note-b".to_owned()]);
        assert_eq!(b_links, vec!["note-a".to_owned()]);
    }

    fn seed_linked_memory_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let db_path = dir.join("linked.db");
        bootstrap(&db_path).expect("bootstrap");
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        store
            .create_note(NewMemoryNote {
                id: "linked-note".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust ownership keeps Genesis safe".to_owned(),
                keywords: vec!["rust".to_owned(), "ownership".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.7,
            })
            .unwrap();

        store
            .create_note(NewMemoryNote {
                id: "primary-note".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis memory architecture".to_owned(),
                keywords: vec!["genesis".to_owned(), "memory".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["linked-note".to_owned()],
                importance: 1.0,
            })
            .unwrap();

        db_path
    }

    #[test]
    fn recall_returns_linked_notes_with_primary_result() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_linked_memory_fixture(dir.path());
        let store = MemoryStore::new(&db_path);

        let results = store.graph_search("Genesis", 5).unwrap();
        let ids: Vec<String> = results.into_iter().map(|item| item.memory.id).collect();
        assert!(ids.contains(&"primary-note".to_owned()));
        assert!(ids.contains(&"linked-note".to_owned()));
    }

    fn seed_hybrid_search_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let db_path = dir.join("hybrid.db");
        bootstrap(&db_path).expect("bootstrap");
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )
        .unwrap();

        let rows = [
            (
                "mem-hybrid-best",
                "fact",
                "genesis memory handbook",
                [1.0_f32, 0.0, 0.0, 0.0],
            ),
            (
                "mem-fts-only",
                "fact",
                "genesis memory archive",
                [0.0_f32, 1.0, 0.0, 0.0],
            ),
            (
                "mem-vector-only",
                "fact",
                "semantic retrieval note",
                [0.95_f32, 0.05, 0.0, 0.0],
            ),
        ];

        for (id, kind, content, _) in rows {
            conn.execute(
                "INSERT INTO memories (id, session_id, kind, content, created_at)
                 VALUES (?1, 's1', ?2, ?3, CURRENT_TIMESTAMP)",
                rusqlite::params![id, kind, content],
            )
            .unwrap();
            let rowid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, ?2, ?3)",
                rusqlite::params![rowid, kind, content],
            )
            .unwrap();
        }
        drop(conn);

        let embeddings = EmbeddingStore::new(&db_path);
        embeddings
            .store("mem-hybrid-best", &[1.0, 0.0, 0.0, 0.0], "local-384")
            .unwrap();
        embeddings
            .store("mem-fts-only", &[0.0, 1.0, 0.0, 0.0], "local-384")
            .unwrap();
        embeddings
            .store("mem-vector-only", &[0.95, 0.05, 0.0, 0.0], "local-384")
            .unwrap();

        db_path
    }

    #[test]
    fn hybrid_search_prefers_multi_signal_matches() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_hybrid_search_fixture(dir.path());
        let store = MemoryStore::new(&db_path);

        let results = store
            .hybrid_search("genesis memory", &[1.0, 0.0, 0.0, 0.0], 3)
            .unwrap();

        assert_eq!(results[0].memory.id, "mem-hybrid-best");
        assert_eq!(results[0].source, "hybrid");
    }

    #[test]
    fn vector_search_returns_empty_when_query_dimensions_drift() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_hybrid_search_fixture(dir.path());
        let store = MemoryStore::new(&db_path);

        let results = store
            .vector_search(&[1.0, 0.0, 0.0], 3)
            .expect("dimension drift should degrade to empty results");

        assert!(results.is_empty());
    }

    #[test]
    fn delete_removes_memory_from_vector_index() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_hybrid_search_fixture(dir.path());
        let store = MemoryStore::new(&db_path);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM memories WHERE id = 'mem-vector-only'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        assert!(store.delete("mem-vector-only").unwrap());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_vec WHERE memory_rowid = ?1",
                rusqlite::params![rowid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn delete_removes_memory_from_fts_index() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_hybrid_search_fixture(dir.path());
        let store = MemoryStore::new(&db_path);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM memories WHERE id = 'mem-vector-only'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        assert!(store.delete("mem-vector-only").unwrap());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_search WHERE memory_row_id = ?1",
                rusqlite::params![rowid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    #[ignore = "benchmark harness for local performance checks"]
    fn hybrid_search_benchmark_10k_memories() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("benchmark.db");
        bootstrap(&db_path).expect("bootstrap");

        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();

        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )
        .unwrap();

        let tx = conn.transaction().unwrap();
        tx.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vec USING vec0(
                memory_rowid integer primary key,
                embedding float[4] distance_metric=cosine
            );",
            [],
        )
        .unwrap();

        for i in 0..10_000 {
            let id = format!("mem-{i}");
            let content = if i % 200 == 0 {
                format!("genesis memory note {i}")
            } else {
                format!("background semantic note {i}")
            };
            tx.execute(
                "INSERT INTO memories (id, session_id, kind, content, created_at)
                 VALUES (?1, 's1', 'fact', ?2, CURRENT_TIMESTAMP)",
                rusqlite::params![id, content],
            )
            .unwrap();
            let rowid = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, 'fact', ?2)",
                rusqlite::params![rowid, content],
            )
            .unwrap();

            let embedding = if i % 200 == 0 {
                [1.0_f32, 0.0, 0.0, 0.0]
            } else {
                [0.0_f32, 1.0, 0.0, 0.0]
            };
            let blob = super::embedding_to_blob(&embedding);
            tx.execute(
                "INSERT INTO memory_embeddings (memory_id, embedding, model, dimensions, created_at)
                 VALUES (?1, ?2, 'benchmark-model', 4, CURRENT_TIMESTAMP)",
                rusqlite::params![id, &blob],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO memory_vec (memory_rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![rowid, &blob],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let store = MemoryStore::new(&db_path);
        let started = std::time::Instant::now();
        let results = store
            .hybrid_search("genesis memory", &[1.0, 0.0, 0.0, 0.0], 10)
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(results.len(), 10);
        println!(
            "hybrid_search_benchmark_10k_memories: {:?} for {} results",
            elapsed,
            results.len()
        );
    }

    #[test]
    fn list_paginated_returns_total_and_windowed_results() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        for i in 0..5 {
            store
                .create(Some("s1"), "fact", &format!("memory number {i}"))
                .unwrap();
        }

        let (page, total) = store.list_paginated(2, 0).expect("first page");
        assert_eq!(total, 5);
        assert_eq!(page.len(), 2);

        let (page2, total2) = store.list_paginated(2, 2).expect("second page");
        assert_eq!(total2, 5);
        assert_eq!(page2.len(), 2);
        assert_ne!(page[0].id, page2[0].id);

        let (page3, _) = store.list_paginated(10, 5).expect("beyond end");
        assert!(page3.is_empty());
    }
}

#[cfg(test)]
mod embedding_store_tests {
    use super::{bootstrap, EmbeddingStore, SessionStore};
    use tempfile::tempdir;

    fn setup(dir: &std::path::Path) -> std::path::PathBuf {
        let db_path = dir.join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let store = SessionStore::new(&db_path);
        store.create_session("s1", "test", None).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem1', 's1', 'fact', 'hello world', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem2', 's1', 'preference', 'likes rust', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        drop(conn);
        db_path
    }

    #[test]
    fn store_and_retrieve_embedding() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        let embedding = vec![0.1_f32, 0.2, 0.3, 0.4, 0.5];
        store
            .store("mem1", &embedding, "text-embedding-3-small")
            .unwrap();

        assert!(store.has_embedding("mem1").unwrap());
        assert!(!store.has_embedding("mem2").unwrap());
        assert_eq!(store.count().unwrap(), 1);

        let all = store.all_embeddings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "mem1");
        for (i, &val) in embedding.iter().enumerate() {
            assert!((all[0].1[i] - val).abs() < 1e-7);
        }
    }

    #[test]
    fn upsert_replaces_existing_embedding() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 2.0], "model-v1").unwrap();
        store.store("mem1", &[3.0, 4.0], "model-v2").unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let all = store.all_embeddings().unwrap();
        assert_eq!(all[0].1.len(), 2);
        assert!((all[0].1[0] - 3.0).abs() < 1e-7);
    }

    #[test]
    fn store_rejects_dimension_mismatch_for_existing_database() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 2.0], "model-v1").unwrap();
        let error = store
            .store("mem2", &[3.0, 4.0, 5.0], "model-v2")
            .expect_err("mixed dimensions should be rejected");

        assert!(matches!(
            error,
            super::StorageError::EmbeddingDimensionMismatch {
                expected: 2,
                actual: 3,
                ..
            }
        ));
    }

    #[test]
    fn delete_removes_embedding() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0], "test").unwrap();
        assert!(store.delete("mem1").unwrap());
        assert!(!store.has_embedding("mem1").unwrap());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn delete_removes_vec_index_entry() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 0.0], "test").unwrap();
        assert!(store.delete("mem1").unwrap());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vec", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn bootstrap_tolerates_legacy_mixed_embedding_dimensions() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (memory_id, embedding, model, dimensions, created_at)
             VALUES ('mem1', ?1, 'legacy-a', 2, CURRENT_TIMESTAMP)",
            rusqlite::params![super::embedding_to_blob(&[1.0_f32, 0.0])],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (memory_id, embedding, model, dimensions, created_at)
             VALUES ('mem2', ?1, 'legacy-b', 3, CURRENT_TIMESTAMP)",
            rusqlite::params![super::embedding_to_blob(&[0.0_f32, 1.0, 0.0])],
        )
        .unwrap();
        drop(conn);

        super::bootstrap(&db_path).expect("legacy mixed embeddings should not brick bootstrap");
    }

    #[test]
    fn store_recreates_empty_vector_index_with_new_dimension() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 0.0], "model-a").unwrap();
        assert!(store.delete("mem1").unwrap());
        store.store("mem1", &[1.0, 0.0, 0.0], "model-b").unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memory_vec'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(sql.contains("float[3]"), "unexpected schema: {sql}");
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        assert!(!store.delete("nonexistent").unwrap());
    }

    #[test]
    fn multiple_embeddings() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 0.0], "test").unwrap();
        store.store("mem2", &[0.0, 1.0], "test").unwrap();

        assert_eq!(store.count().unwrap(), 2);
        let all = store.all_embeddings().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn blob_serialization_roundtrip() {
        let original = vec![0.0_f32, 1.0, -1.0, f32::MIN, f32::MAX, std::f32::consts::PI];
        let blob = super::embedding_to_blob(&original);
        let restored = super::blob_to_embedding(&blob);
        assert_eq!(original.len(), restored.len());
        for (a, b) in original.iter().zip(restored.iter()) {
            assert_eq!(a.to_bits(), b.to_bits(), "bitwise equality for {a}");
        }
    }

    #[test]
    fn database_path_accessor() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        let store = EmbeddingStore::new(&db_path);
        assert_eq!(store.database_path(), db_path);
    }

    #[test]
    fn bootstrap_registers_sqlite_vec_functions() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let conn = rusqlite::Connection::open(&db_path).expect("open connection");
        let version: String = conn
            .query_row("SELECT vec_version()", [], |row| row.get(0))
            .expect("sqlite-vec should be available after bootstrap");

        assert!(!version.is_empty());
    }

    #[test]
    fn store_populates_memory_vec_index() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store
            .store("mem1", &[0.1, 0.2, 0.3, 0.4], "local-384")
            .unwrap();

        let conn = rusqlite::Connection::open(&db_path).expect("open connection");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vec", [], |row| row.get(0))
            .expect("memory_vec should exist after storing an embedding");

        assert_eq!(count, 1);
    }
}

#[cfg(test)]
mod audit_log_store_tests {
    use super::{bootstrap, AuditLogStore};
    use tempfile::tempdir;

    #[test]
    fn log_and_retrieve_recent_entries() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details_a = serde_json::json!({"tool": "shell", "command": "ls"});
        let details_b = serde_json::json!({"tool": "read_file", "path": "/tmp/x"});
        let details_c = serde_json::json!({"model": "gpt-4", "tokens": 100});

        store
            .log(Some("s1"), "tool_call_start", &details_a)
            .expect("first log should succeed");
        store
            .log(Some("s1"), "tool_call_end", &details_b)
            .expect("second log should succeed");
        store
            .log(Some("s2"), "llm_response", &details_c)
            .expect("third log should succeed");

        let recent = store.recent(10).expect("recent should succeed");

        assert_eq!(recent.len(), 3);
        // Most recent first (ORDER BY id DESC)
        assert_eq!(recent[0].event_type, "llm_response");
        assert_eq!(recent[1].event_type, "tool_call_end");
        assert_eq!(recent[2].event_type, "tool_call_start");
    }

    #[test]
    fn log_and_filter_by_event_type() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details = serde_json::json!({"info": "test"});

        store
            .log(Some("s1"), "tool_call_start", &details)
            .expect("log should succeed");
        store
            .log(Some("s1"), "llm_response", &details)
            .expect("log should succeed");
        store
            .log(Some("s2"), "tool_call_start", &details)
            .expect("log should succeed");
        store
            .log(Some("s2"), "config_change", &details)
            .expect("log should succeed");

        let tool_starts = store
            .by_event_type("tool_call_start", 10)
            .expect("by_event_type should succeed");

        assert_eq!(tool_starts.len(), 2);
        for entry in &tool_starts {
            assert_eq!(entry.event_type, "tool_call_start");
        }

        let llm = store
            .by_event_type("llm_response", 10)
            .expect("by_event_type should succeed");
        assert_eq!(llm.len(), 1);
        assert_eq!(llm[0].event_type, "llm_response");

        let missing = store
            .by_event_type("nonexistent", 10)
            .expect("by_event_type should succeed for missing type");
        assert!(missing.is_empty());
    }

    #[test]
    fn log_and_filter_by_session() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details = serde_json::json!({"action": "test"});

        store
            .log(Some("session-a"), "tool_call_start", &details)
            .expect("log should succeed");
        store
            .log(Some("session-a"), "tool_call_end", &details)
            .expect("log should succeed");
        store
            .log(Some("session-b"), "llm_response", &details)
            .expect("log should succeed");
        store
            .log(None, "config_change", &details)
            .expect("log with no session should succeed");

        let session_a = store
            .by_session("session-a", 10)
            .expect("by_session should succeed");
        assert_eq!(session_a.len(), 2);
        for entry in &session_a {
            assert_eq!(entry.session_id.as_deref(), Some("session-a"));
        }

        let session_b = store
            .by_session("session-b", 10)
            .expect("by_session should succeed");
        assert_eq!(session_b.len(), 1);
        assert_eq!(session_b[0].session_id.as_deref(), Some("session-b"));

        let session_c = store
            .by_session("session-c", 10)
            .expect("by_session should succeed for missing session");
        assert!(session_c.is_empty());
    }

    #[test]
    fn stats_counts_by_event_type() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details = serde_json::json!({"x": 1});

        // 3 tool_call_start, 2 llm_response, 1 config_change
        for _ in 0..3 {
            store
                .log(Some("s1"), "tool_call_start", &details)
                .expect("log should succeed");
        }
        for _ in 0..2 {
            store
                .log(Some("s1"), "llm_response", &details)
                .expect("log should succeed");
        }
        store
            .log(Some("s1"), "config_change", &details)
            .expect("log should succeed");

        let stats = store.stats().expect("stats should succeed");

        assert_eq!(stats.len(), 3);

        // Stats are ordered by count descending
        let stats_map: std::collections::HashMap<&str, i64> =
            stats.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(stats_map.get("tool_call_start"), Some(&3));
        assert_eq!(stats_map.get("llm_response"), Some(&2));
        assert_eq!(stats_map.get("config_change"), Some(&1));
    }

    #[test]
    fn purge_older_than_removes_old_entries() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details = serde_json::json!({"purge": "test"});

        // Insert an entry and then manually backdate it via raw SQL
        store
            .log(Some("s1"), "old_event", &details)
            .expect("log should succeed");

        // Insert a recent entry
        store
            .log(Some("s1"), "recent_event", &details)
            .expect("log should succeed");

        // Backdate the first entry to 60 days ago
        let conn =
            rusqlite::Connection::open(&database_path).expect("open connection should succeed");
        conn.execute(
            "UPDATE audit_log SET created_at = datetime('now', '-60 days')
             WHERE event_type = 'old_event'",
            [],
        )
        .expect("backdate should succeed");

        // Purge entries older than 30 days
        let deleted = store
            .purge_older_than(30)
            .expect("purge_older_than should succeed");
        assert_eq!(deleted, 1);

        // Only the recent entry should remain
        let remaining = store.recent(10).expect("recent should succeed");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event_type, "recent_event");
    }
}

#[cfg(test)]
mod pairing_store_tests {
    use super::{bootstrap, PairingStore};
    use tempfile::tempdir;

    #[test]
    fn generate_and_verify_pairing_code() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        let code = store
            .generate_code("telegram", "user123", "Alice")
            .expect("generate_code should succeed");
        assert!(code.is_some(), "code should be generated");

        let pending = store
            .list_pending(Some("telegram"))
            .expect("list_pending should succeed");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].code, code.unwrap());
        assert_eq!(pending[0].user_id, "user123");
        assert_eq!(pending[0].user_name, "Alice");
        assert_eq!(pending[0].platform, "telegram");
    }

    #[test]
    fn approve_code_moves_to_approved() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        let code = store
            .generate_code("discord", "user456", "Bob")
            .expect("generate_code should succeed")
            .expect("code should be generated");

        let approved = store
            .approve_code("discord", &code)
            .expect("approve_code should succeed");
        assert!(approved.is_some(), "approved user should be returned");

        let approved_user = approved.unwrap();
        assert_eq!(approved_user.platform, "discord");
        assert_eq!(approved_user.user_id, "user456");
        assert_eq!(approved_user.user_name, "Bob");

        // Verify it appears in the approved list
        let approved_list = store
            .list_approved(Some("discord"))
            .expect("list_approved should succeed");
        assert_eq!(approved_list.len(), 1);
        assert_eq!(approved_list[0].user_id, "user456");

        // Verify it's no longer in the pending list
        let pending = store
            .list_pending(Some("discord"))
            .expect("list_pending should succeed");
        assert!(
            pending.is_empty(),
            "pending list should be empty after approval"
        );
    }

    #[test]
    fn revoke_user_removes_from_approved() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        // Generate and approve
        let code = store
            .generate_code("slack", "user789", "Charlie")
            .expect("generate_code should succeed")
            .expect("code should be generated");
        store
            .approve_code("slack", &code)
            .expect("approve_code should succeed");

        // Confirm approved
        assert!(
            store
                .is_approved("slack", "user789")
                .expect("is_approved should succeed"),
            "user should be approved"
        );

        // Revoke
        let revoked = store
            .revoke("slack", "user789")
            .expect("revoke should succeed");
        assert!(revoked, "revoke should return true for existing user");

        // Confirm no longer approved
        assert!(
            !store
                .is_approved("slack", "user789")
                .expect("is_approved should succeed"),
            "user should no longer be approved after revocation"
        );

        let approved_list = store
            .list_approved(Some("slack"))
            .expect("list_approved should succeed");
        assert!(
            approved_list.is_empty(),
            "approved list should be empty after revocation"
        );
    }

    #[test]
    fn expired_codes_not_returned() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        let code = store
            .generate_code("telegram", "user_exp", "Expired User")
            .expect("generate_code should succeed")
            .expect("code should be generated");

        // Verify it's in pending
        let pending = store
            .list_pending(Some("telegram"))
            .expect("list_pending should succeed");
        assert_eq!(pending.len(), 1);

        // Backdate the code to 2 hours ago (beyond the 1-hour TTL)
        let conn =
            rusqlite::Connection::open(&database_path).expect("open connection should succeed");
        conn.execute(
            "UPDATE pairing_pending SET created_at = datetime('now', '-7200 seconds')
             WHERE code = ?1",
            rusqlite::params![code],
        )
        .expect("backdate should succeed");

        // list_pending cleans up expired codes, so the expired code should not be returned
        let pending_after = store
            .list_pending(Some("telegram"))
            .expect("list_pending should succeed");
        assert!(
            pending_after.is_empty(),
            "expired codes should not appear in pending list"
        );

        // Trying to approve an expired code should also fail
        let approved = store
            .approve_code("telegram", &code)
            .expect("approve_code should succeed");
        assert!(approved.is_none(), "expired code should not be approvable");
    }

    #[test]
    fn is_approved_returns_correct_status() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        // Unknown user should not be approved
        assert!(
            !store
                .is_approved("telegram", "unknown_user")
                .expect("is_approved should succeed"),
            "unknown user should not be approved"
        );

        // Generate and approve a user
        let code = store
            .generate_code("telegram", "known_user", "Known")
            .expect("generate_code should succeed")
            .expect("code should be generated");
        store
            .approve_code("telegram", &code)
            .expect("approve_code should succeed");

        // Approved user should return true
        assert!(
            store
                .is_approved("telegram", "known_user")
                .expect("is_approved should succeed"),
            "approved user should return true"
        );

        // Different platform should return false
        assert!(
            !store
                .is_approved("discord", "known_user")
                .expect("is_approved should succeed"),
            "user approved on different platform should return false"
        );
    }

    #[test]
    fn clear_pending_removes_codes() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        // Generate codes on two platforms. Sleep between calls to avoid
        // XORShift seed collisions from identical nanosecond timestamps.
        store
            .generate_code("telegram", "user_a", "A")
            .expect("generate_code should succeed");
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .generate_code("telegram", "user_b", "B")
            .expect("generate_code should succeed");
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .generate_code("discord", "user_c", "C")
            .expect("generate_code should succeed");

        // Clear only telegram pending codes
        let cleared = store
            .clear_pending(Some("telegram"))
            .expect("clear_pending should succeed");
        assert_eq!(cleared, 2);

        // Telegram pending should be empty
        let tg_pending = store
            .list_pending(Some("telegram"))
            .expect("list_pending should succeed");
        assert!(
            tg_pending.is_empty(),
            "telegram pending should be empty after clear"
        );

        // Discord pending should still exist
        let dc_pending = store
            .list_pending(Some("discord"))
            .expect("list_pending should succeed");
        assert_eq!(dc_pending.len(), 1);

        // Clear all remaining
        let cleared_all = store
            .clear_pending(None)
            .expect("clear_pending should succeed");
        assert_eq!(cleared_all, 1);

        let all_pending = store
            .list_pending(None)
            .expect("list_pending should succeed");
        assert!(
            all_pending.is_empty(),
            "all pending should be empty after clear_pending(None)"
        );
    }
}

#[cfg(test)]
mod subagent_store_tests {
    use super::{bootstrap, SubagentStore};
    use tempfile::tempdir;

    #[test]
    fn create_and_get_subagent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SubagentStore::new(&database_path);
        let sub = store
            .create(
                "sub-1",
                "parent-session-1",
                "child-session-1",
                "researcher",
                "find relevant papers",
            )
            .expect("create should succeed");

        assert_eq!(sub.id, "sub-1");
        assert_eq!(sub.parent_session_id, "parent-session-1");
        assert_eq!(sub.child_session_id, "child-session-1");
        assert_eq!(sub.name, "researcher");
        assert_eq!(sub.task, "find relevant papers");
        assert_eq!(sub.status, "pending");
        assert!(sub.result.is_none());
        assert!(sub.error.is_none());
        assert!(sub.completed_at.is_none());

        let fetched = store
            .get("sub-1")
            .expect("get should succeed")
            .expect("subagent should exist");
        assert_eq!(fetched.id, "sub-1");
        assert_eq!(fetched.name, "researcher");
        assert_eq!(fetched.status, "pending");
    }

    #[test]
    fn update_subagent_status() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SubagentStore::new(&database_path);
        store
            .create(
                "sub-2",
                "parent-1",
                "child-2",
                "coder",
                "implement feature X",
            )
            .expect("create should succeed");

        // Transition to running
        let updated = store
            .set_running("sub-2")
            .expect("set_running should succeed");
        assert!(updated);
        let sub = store
            .get("sub-2")
            .expect("get should succeed")
            .expect("subagent should exist");
        assert_eq!(sub.status, "running");
        assert!(sub.completed_at.is_none());

        // Transition to completed
        let completed = store
            .set_completed("sub-2", "feature X implemented successfully")
            .expect("set_completed should succeed");
        assert!(completed);
        let sub = store
            .get("sub-2")
            .expect("get should succeed")
            .expect("subagent should exist");
        assert_eq!(sub.status, "completed");
        assert_eq!(
            sub.result.as_deref(),
            Some("feature X implemented successfully")
        );
        assert!(sub.completed_at.is_some());

        // Create another subagent and transition to failed
        store
            .create("sub-3", "parent-1", "child-3", "tester", "run tests")
            .expect("create should succeed");
        let failed = store
            .set_failed("sub-3", "tests timed out")
            .expect("set_failed should succeed");
        assert!(failed);
        let sub = store
            .get("sub-3")
            .expect("get should succeed")
            .expect("subagent should exist");
        assert_eq!(sub.status, "failed");
        assert_eq!(sub.error.as_deref(), Some("tests timed out"));
        assert!(sub.completed_at.is_some());
    }

    #[test]
    fn list_by_parent_isolates_sessions() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SubagentStore::new(&database_path);

        // Create subagents under different parent sessions
        store
            .create("sub-a1", "parent-A", "child-a1", "worker-1", "task alpha-1")
            .expect("create should succeed");
        store
            .create("sub-a2", "parent-A", "child-a2", "worker-2", "task alpha-2")
            .expect("create should succeed");
        store
            .create("sub-b1", "parent-B", "child-b1", "worker-3", "task beta-1")
            .expect("create should succeed");

        // List for parent-A
        let parent_a_subs = store
            .list_by_parent("parent-A")
            .expect("list_by_parent should succeed");
        assert_eq!(parent_a_subs.len(), 2);
        assert!(parent_a_subs
            .iter()
            .all(|s| s.parent_session_id == "parent-A"));
        let names: Vec<&str> = parent_a_subs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"worker-1"));
        assert!(names.contains(&"worker-2"));

        // List for parent-B — should be isolated
        let parent_b_subs = store
            .list_by_parent("parent-B")
            .expect("list_by_parent should succeed");
        assert_eq!(parent_b_subs.len(), 1);
        assert_eq!(parent_b_subs[0].name, "worker-3");

        // List for non-existent parent — should be empty
        let empty = store
            .list_by_parent("parent-C")
            .expect("list_by_parent should succeed");
        assert!(empty.is_empty());
    }

    #[test]
    fn list_by_parent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SubagentStore::new(&database_path);

        // Same parent, different child sessions
        store
            .create(
                "sub-x1",
                "shared-parent",
                "child-x1",
                "analyzer",
                "analyze data",
            )
            .expect("create should succeed");
        store
            .create(
                "sub-x2",
                "shared-parent",
                "child-x2",
                "summarizer",
                "summarize results",
            )
            .expect("create should succeed");
        store
            .create(
                "sub-x3",
                "shared-parent",
                "child-x3",
                "reviewer",
                "review output",
            )
            .expect("create should succeed");

        let subs = store
            .list_by_parent("shared-parent")
            .expect("list_by_parent should succeed");
        assert_eq!(subs.len(), 3);
        assert!(subs.iter().all(|s| s.parent_session_id == "shared-parent"));

        // Verify each has a distinct child_session_id
        let child_ids: Vec<&str> = subs.iter().map(|s| s.child_session_id.as_str()).collect();
        assert!(child_ids.contains(&"child-x1"));
        assert!(child_ids.contains(&"child-x2"));
        assert!(child_ids.contains(&"child-x3"));

        // Verify all names are present (order may vary on fast machines
        // where created_at timestamps coincide).
        let names: Vec<&str> = subs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"analyzer"));
        assert!(names.contains(&"summarizer"));
        assert!(names.contains(&"reviewer"));
    }
}

#[cfg(test)]
mod skill_usage_store_tests {
    use super::{bootstrap, SkillStore, SkillUsageStore};
    use tempfile::tempdir;

    /// Helper: create a skill so the foreign-key constraint on skill_usages is satisfied.
    fn seed_skill(store: &SkillStore, name: &str) {
        store
            .upsert(name, "test skill", "do the thing", None, &[])
            .expect("seed skill should succeed");
    }

    #[test]
    fn record_and_list_usage() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let skill_store = SkillStore::new(&database_path);
        seed_skill(&skill_store, "code-review");

        let store = SkillUsageStore::new(&database_path);
        let usage = store
            .record_usage(
                "code-review",
                Some("session-1"),
                "success",
                Some("worked well"),
            )
            .expect("record_usage should succeed");

        assert_eq!(usage.skill_name, "code-review");
        assert_eq!(usage.session_id.as_deref(), Some("session-1"));
        assert_eq!(usage.outcome, "success");
        assert_eq!(usage.feedback.as_deref(), Some("worked well"));
        assert!(!usage.refined);

        let recent = store
            .recent_usages("code-review", 10)
            .expect("recent_usages should succeed");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].skill_name, "code-review");
        assert_eq!(recent[0].outcome, "success");
    }

    #[test]
    fn aggregate_usage_stats() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let skill_store = SkillStore::new(&database_path);
        seed_skill(&skill_store, "data-extract");

        let store = SkillUsageStore::new(&database_path);

        // Record several usages with mixed outcomes
        let u1 = store
            .record_usage("data-extract", Some("s1"), "success", None)
            .expect("record_usage should succeed");
        store
            .record_usage("data-extract", Some("s2"), "success", None)
            .expect("record_usage should succeed");
        store
            .record_usage("data-extract", Some("s3"), "failure", Some("timed out"))
            .expect("record_usage should succeed");
        store
            .record_usage("data-extract", Some("s4"), "partial", None)
            .expect("record_usage should succeed");

        // Mark the first usage as refined
        store
            .mark_refined(u1.id)
            .expect("mark_refined should succeed");

        let stats = store.stats("data-extract").expect("stats should succeed");

        assert_eq!(stats.skill_name, "data-extract");
        assert_eq!(stats.total_uses, 4);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.times_refined, 1);
        assert!(stats.last_used.is_some());
    }

    #[test]
    fn list_usage_respects_limit() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let skill_store = SkillStore::new(&database_path);
        seed_skill(&skill_store, "bulk-skill");

        let store = SkillUsageStore::new(&database_path);

        // Record 5 usages
        for i in 0..5 {
            store
                .record_usage("bulk-skill", Some(&format!("session-{i}")), "success", None)
                .expect("record_usage should succeed");
        }

        // Request only 3
        let limited = store
            .recent_usages("bulk-skill", 3)
            .expect("recent_usages should succeed");
        assert_eq!(limited.len(), 3);

        // Request all
        let all = store
            .recent_usages("bulk-skill", 100)
            .expect("recent_usages should succeed");
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn usage_records_different_skills() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let skill_store = SkillStore::new(&database_path);
        seed_skill(&skill_store, "skill-alpha");
        seed_skill(&skill_store, "skill-beta");

        let store = SkillUsageStore::new(&database_path);

        // Record usages for two different skills
        store
            .record_usage("skill-alpha", Some("s1"), "success", None)
            .expect("record_usage should succeed");
        store
            .record_usage("skill-alpha", Some("s2"), "failure", None)
            .expect("record_usage should succeed");
        store
            .record_usage("skill-beta", Some("s3"), "success", None)
            .expect("record_usage should succeed");

        // Stats should be per-skill
        let alpha_stats = store.stats("skill-alpha").expect("stats should succeed");
        assert_eq!(alpha_stats.total_uses, 2);
        assert_eq!(alpha_stats.successes, 1);
        assert_eq!(alpha_stats.failures, 1);

        let beta_stats = store.stats("skill-beta").expect("stats should succeed");
        assert_eq!(beta_stats.total_uses, 1);
        assert_eq!(beta_stats.successes, 1);
        assert_eq!(beta_stats.failures, 0);

        // recent_usages should only return records for the requested skill
        let alpha_recent = store
            .recent_usages("skill-alpha", 10)
            .expect("recent_usages should succeed");
        assert_eq!(alpha_recent.len(), 2);
        assert!(alpha_recent.iter().all(|u| u.skill_name == "skill-alpha"));

        let beta_recent = store
            .recent_usages("skill-beta", 10)
            .expect("recent_usages should succeed");
        assert_eq!(beta_recent.len(), 1);
        assert_eq!(beta_recent[0].skill_name, "skill-beta");
    }
}

#[cfg(test)]
mod skill_store_tests {
    use super::{bootstrap, SkillStore};
    use tempfile::tempdir;

    #[test]
    fn upsert_and_get_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        let skill = store
            .upsert(
                "greet_user",
                "Greet the user warmly",
                "Say hello and ask how they are doing",
                Some("when user says hi"),
                &["greeting", "social"],
            )
            .expect("upsert should succeed");

        assert_eq!(skill.name, "greet_user");
        assert_eq!(skill.description, "Greet the user warmly");
        assert_eq!(skill.instructions, "Say hello and ask how they are doing");
        assert_eq!(skill.trigger_hint.as_deref(), Some("when user says hi"));
        assert_eq!(skill.tags, vec!["greeting", "social"]);
        assert_eq!(skill.version, 1);

        let fetched = store
            .get("greet_user")
            .expect("get should succeed")
            .expect("skill should exist");

        assert_eq!(fetched.name, "greet_user");
        assert_eq!(fetched.description, "Greet the user warmly");
        assert_eq!(fetched.instructions, "Say hello and ask how they are doing");
        assert_eq!(fetched.trigger_hint.as_deref(), Some("when user says hi"));
        assert_eq!(fetched.tags, vec!["greeting", "social"]);
        assert_eq!(fetched.version, 1);
    }

    #[test]
    fn upsert_updates_existing_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        store
            .upsert(
                "summarize",
                "Summarize text",
                "Provide a brief summary",
                None,
                &["text"],
            )
            .expect("first upsert should succeed");

        let updated = store
            .upsert(
                "summarize",
                "Summarize any content",
                "Provide a concise summary with key points",
                Some("when asked to summarize"),
                &["text", "analysis"],
            )
            .expect("second upsert should succeed");

        assert_eq!(updated.name, "summarize");
        assert_eq!(updated.description, "Summarize any content");
        assert_eq!(
            updated.instructions,
            "Provide a concise summary with key points"
        );
        assert_eq!(
            updated.trigger_hint.as_deref(),
            Some("when asked to summarize")
        );
        assert_eq!(updated.tags, vec!["text", "analysis"]);
        assert_eq!(updated.version, 2);
    }

    #[test]
    fn list_all_skills() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        store
            .upsert("alpha_skill", "Alpha", "Do alpha things", None, &[])
            .expect("upsert alpha should succeed");
        store
            .upsert("beta_skill", "Beta", "Do beta things", None, &[])
            .expect("upsert beta should succeed");
        store
            .upsert("gamma_skill", "Gamma", "Do gamma things", None, &[])
            .expect("upsert gamma should succeed");

        let all = store.list_all().expect("list_all should succeed");
        assert_eq!(all.len(), 3);

        // list_all orders by name ASC
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha_skill", "beta_skill", "gamma_skill"]);
    }

    #[test]
    fn delete_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        store
            .upsert("to_delete", "Temporary skill", "Will be removed", None, &[])
            .expect("upsert should succeed");

        assert!(
            store.get("to_delete").unwrap().is_some(),
            "skill should exist before delete"
        );

        let deleted = store.delete("to_delete").expect("delete should succeed");
        assert!(deleted, "delete should return true for existing skill");

        assert!(
            store.get("to_delete").unwrap().is_none(),
            "skill should not exist after delete"
        );

        let deleted_again = store
            .delete("to_delete")
            .expect("delete of missing should succeed");
        assert!(
            !deleted_again,
            "delete should return false for nonexistent skill"
        );
    }

    #[test]
    fn search_by_tag() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        store
            .upsert(
                "code_review",
                "Review code",
                "Analyze code for issues",
                None,
                &["code", "review"],
            )
            .expect("upsert code_review should succeed");
        store
            .upsert(
                "write_tests",
                "Write tests",
                "Generate test cases",
                None,
                &["code", "testing"],
            )
            .expect("upsert write_tests should succeed");
        store
            .upsert(
                "draft_email",
                "Draft an email",
                "Compose a professional email",
                None,
                &["writing", "communication"],
            )
            .expect("upsert draft_email should succeed");

        let code_skills = store
            .find_by_tag("code")
            .expect("find_by_tag should succeed");
        assert_eq!(code_skills.len(), 2);
        let code_names: Vec<&str> = code_skills.iter().map(|s| s.name.as_str()).collect();
        assert!(code_names.contains(&"code_review"));
        assert!(code_names.contains(&"write_tests"));

        let review_skills = store
            .find_by_tag("review")
            .expect("find_by_tag should succeed");
        assert_eq!(review_skills.len(), 1);
        assert_eq!(review_skills[0].name, "code_review");

        let writing_skills = store
            .find_by_tag("writing")
            .expect("find_by_tag should succeed");
        assert_eq!(writing_skills.len(), 1);
        assert_eq!(writing_skills[0].name, "draft_email");

        let no_match = store
            .find_by_tag("nonexistent")
            .expect("find_by_tag should succeed");
        assert!(no_match.is_empty());
    }

    #[test]
    fn list_all_paginated_returns_total_and_page() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        for i in 0..4 {
            store
                .upsert(&format!("skill_{i}"), "desc", "instr", None, &[])
                .expect("upsert should succeed");
        }

        let (page, total) = store.list_all_paginated(2, 0).expect("first page");
        assert_eq!(total, 4);
        assert_eq!(page.len(), 2);

        let (page2, total2) = store.list_all_paginated(2, 2).expect("second page");
        assert_eq!(total2, 4);
        assert_eq!(page2.len(), 2);
        assert_ne!(page[0].name, page2[0].name);
    }
}

#[cfg(test)]
mod user_model_store_tests {
    use super::{bootstrap, UserModelStore};
    use tempfile::tempdir;

    #[test]
    fn observe_and_get_trait() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        let observed = store
            .observe(
                "preferred_language",
                "preference",
                "Rust",
                Some("session-1"),
            )
            .expect("observe should succeed");

        assert_eq!(observed.trait_key, "preferred_language");
        assert_eq!(observed.category, "preference");
        assert_eq!(observed.value, "Rust");
        assert!((observed.confidence - 0.5).abs() < f64::EPSILON);
        assert_eq!(observed.evidence_count, 1);
        assert_eq!(observed.source_session.as_deref(), Some("session-1"));

        let fetched = store
            .get("preferred_language")
            .expect("get should succeed")
            .expect("trait should exist");

        assert_eq!(fetched.trait_key, "preferred_language");
        assert_eq!(fetched.category, "preference");
        assert_eq!(fetched.value, "Rust");
        assert!((fetched.confidence - 0.5).abs() < f64::EPSILON);
        assert_eq!(fetched.evidence_count, 1);
    }

    #[test]
    fn observe_updates_existing_trait() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store
            .observe("editor", "preference", "vim", Some("s1"))
            .expect("first observe should succeed");

        let updated = store
            .observe("editor", "preference", "neovim", Some("s2"))
            .expect("second observe should succeed");

        assert_eq!(updated.trait_key, "editor");
        assert_eq!(updated.value, "neovim");
        // Confidence should increase by 0.1 from 0.5 to 0.6
        assert!((updated.confidence - 0.6).abs() < f64::EPSILON);
        assert_eq!(updated.evidence_count, 2);
        assert_eq!(updated.source_session.as_deref(), Some("s2"));
    }

    #[test]
    fn list_traits() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store
            .observe("lang", "preference", "Rust", None)
            .expect("observe lang");
        store
            .observe("tone", "communication_style", "casual", None)
            .expect("observe tone");
        store
            .observe("goal", "goal", "build an AI agent", None)
            .expect("observe goal");

        let all = store.list_all().expect("list_all should succeed");
        assert_eq!(all.len(), 3);

        let keys: Vec<&str> = all.iter().map(|t| t.trait_key.as_str()).collect();
        assert!(keys.contains(&"lang"));
        assert!(keys.contains(&"tone"));
        assert!(keys.contains(&"goal"));
    }

    #[test]
    fn list_traits_filters_by_category() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store
            .observe("lang", "preference", "Rust", None)
            .expect("observe lang");
        store
            .observe("theme", "preference", "dark mode", None)
            .expect("observe theme");
        store
            .observe("tone", "communication_style", "formal", None)
            .expect("observe tone");
        store
            .observe("expertise", "expertise", "systems programming", None)
            .expect("observe expertise");

        let preferences = store
            .list_by_category("preference")
            .expect("list_by_category should succeed");
        assert_eq!(preferences.len(), 2);
        let pref_keys: Vec<&str> = preferences.iter().map(|t| t.trait_key.as_str()).collect();
        assert!(pref_keys.contains(&"lang"));
        assert!(pref_keys.contains(&"theme"));

        let comm = store
            .list_by_category("communication_style")
            .expect("list_by_category should succeed");
        assert_eq!(comm.len(), 1);
        assert_eq!(comm[0].trait_key, "tone");

        let empty = store
            .list_by_category("nonexistent_category")
            .expect("list_by_category should succeed");
        assert!(empty.is_empty());
    }

    #[test]
    fn list_traits_filters_by_min_confidence() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);

        // First trait: observed once -> confidence 0.5
        store
            .observe("low_conf", "preference", "tabs", None)
            .expect("observe low_conf");

        // Second trait: observed 4 times -> confidence 0.5 + 0.3 = 0.8
        store
            .observe("high_conf", "preference", "dark mode", None)
            .expect("observe high_conf 1");
        store
            .observe("high_conf", "preference", "dark mode", None)
            .expect("observe high_conf 2");
        store
            .observe("high_conf", "preference", "dark mode", None)
            .expect("observe high_conf 3");
        store
            .observe("high_conf", "preference", "dark mode", None)
            .expect("observe high_conf 4");

        // Filter by confidence >= 0.7 should only return high_conf
        let confident = store
            .confident_traits(0.7)
            .expect("confident_traits should succeed");
        assert_eq!(confident.len(), 1);
        assert_eq!(confident[0].trait_key, "high_conf");
        assert!(confident[0].confidence >= 0.7);

        // Filter by confidence >= 0.5 should return both
        let all_confident = store
            .confident_traits(0.5)
            .expect("confident_traits should succeed");
        assert_eq!(all_confident.len(), 2);
    }

    #[test]
    fn delete_trait() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store
            .observe("to_remove", "preference", "light mode", None)
            .expect("observe should succeed");

        assert!(
            store.get("to_remove").unwrap().is_some(),
            "trait should exist before delete"
        );

        let deleted = store.delete("to_remove").expect("delete should succeed");
        assert!(deleted, "delete should return true for existing trait");

        assert!(
            store.get("to_remove").unwrap().is_none(),
            "trait should not exist after delete"
        );

        let deleted_again = store
            .delete("to_remove")
            .expect("delete of missing should succeed");
        assert!(
            !deleted_again,
            "delete should return false for nonexistent trait"
        );
    }

    #[test]
    fn list_paginated_returns_total_and_page() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        for i in 0..4 {
            store
                .observe(&format!("trait_{i}"), "category_a", &format!("value_{i}"), None)
                .expect("observe should succeed");
        }

        let (page, total) = store.list_paginated(None, None, 2, 0).expect("first page");
        assert_eq!(total, 4);
        assert_eq!(page.len(), 2);

        let (page2, _) = store.list_paginated(None, None, 2, 2).expect("second page");
        assert_eq!(page2.len(), 2);
        assert_ne!(page[0].trait_key, page2[0].trait_key);
    }

    #[test]
    fn list_paginated_with_category_filter() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store.observe("t1", "pref", "v1", None).unwrap();
        store.observe("t2", "pref", "v2", None).unwrap();
        store.observe("t3", "skill", "v3", None).unwrap();

        let (page, total) = store.list_paginated(Some("pref"), None, 50, 0).expect("filter by category");
        assert_eq!(total, 2);
        assert_eq!(page.len(), 2);
    }
}

#[cfg(test)]
mod skill_file_store_tests {
    use super::{bootstrap, SkillFileStore, SkillStore};
    use tempfile::tempdir;

    /// Helper: create a skill so the foreign key constraint is satisfied.
    fn create_skill(database_path: &std::path::Path, name: &str) {
        let store = SkillStore::new(database_path);
        store
            .upsert(name, "test skill", "do stuff", None, &[])
            .expect("skill upsert should succeed");
    }

    #[test]
    fn store_and_get_file() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        create_skill(&database_path, "my_skill");

        let store = SkillFileStore::new(&database_path);
        store
            .store_file("my_skill", "config.yaml", "key: value")
            .expect("store_file should succeed");

        let content = store
            .get_file("my_skill", "config.yaml")
            .expect("get_file should succeed")
            .expect("file should exist");

        assert_eq!(content, "key: value");
    }

    #[test]
    fn list_files_for_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        create_skill(&database_path, "multi_file_skill");

        let store = SkillFileStore::new(&database_path);
        store
            .store_file("multi_file_skill", "b_second.txt", "two")
            .expect("store second file");
        store
            .store_file("multi_file_skill", "a_first.txt", "one")
            .expect("store first file");
        store
            .store_file("multi_file_skill", "c_third.txt", "three")
            .expect("store third file");

        let files = store
            .list_files("multi_file_skill")
            .expect("list_files should succeed");

        assert_eq!(files.len(), 3);
        // list_files returns file_path ordered ASC
        assert_eq!(files[0], "a_first.txt");
        assert_eq!(files[1], "b_second.txt");
        assert_eq!(files[2], "c_third.txt");
    }

    #[test]
    fn delete_file() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        create_skill(&database_path, "del_skill");

        let store = SkillFileStore::new(&database_path);
        store
            .store_file("del_skill", "to_remove.txt", "ephemeral")
            .expect("store_file should succeed");

        let deleted = store
            .delete_file("del_skill", "to_remove.txt")
            .expect("delete_file should succeed");
        assert!(deleted, "delete should return true for existing file");

        let gone = store
            .get_file("del_skill", "to_remove.txt")
            .expect("get_file should succeed");
        assert!(gone.is_none(), "file should no longer exist after deletion");

        let deleted_again = store
            .delete_file("del_skill", "to_remove.txt")
            .expect("delete of missing file should succeed");
        assert!(
            !deleted_again,
            "delete should return false for nonexistent file"
        );
    }

    #[test]
    fn files_isolated_per_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        create_skill(&database_path, "skill_a");
        create_skill(&database_path, "skill_b");

        let store = SkillFileStore::new(&database_path);
        store
            .store_file("skill_a", "shared_name.txt", "content from A")
            .expect("store file for skill_a");
        store
            .store_file("skill_b", "shared_name.txt", "content from B")
            .expect("store file for skill_b");
        store
            .store_file("skill_b", "extra.txt", "only in B")
            .expect("store extra file for skill_b");

        let files_a = store.list_files("skill_a").expect("list_files for skill_a");
        let files_b = store.list_files("skill_b").expect("list_files for skill_b");

        assert_eq!(files_a.len(), 1);
        assert_eq!(files_a[0], "shared_name.txt");
        assert_eq!(files_b.len(), 2);

        let content_a = store
            .get_file("skill_a", "shared_name.txt")
            .expect("get_file skill_a")
            .expect("file should exist for skill_a");
        let content_b = store
            .get_file("skill_b", "shared_name.txt")
            .expect("get_file skill_b")
            .expect("file should exist for skill_b");

        assert_eq!(content_a, "content from A");
        assert_eq!(content_b, "content from B");
    }
}

#[cfg(test)]
mod response_cache_store_tests {
    use super::{bootstrap, ResponseCacheStore};
    use tempfile::tempdir;

    #[test]
    fn set_and_get_cached_response() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ResponseCacheStore::new(&database_path);
        store
            .set("key-1", "gpt-4", "Hello world", None, 100, 20, 3600)
            .expect("set should succeed");

        let entry = store
            .get("key-1")
            .expect("get should succeed")
            .expect("entry should exist");

        assert_eq!(entry.cache_key, "key-1");
        assert_eq!(entry.model, "gpt-4");
        assert_eq!(entry.response, "Hello world");
        assert!(entry.tool_calls_json.is_none());
        assert_eq!(entry.input_tokens, 100);
        assert_eq!(entry.output_tokens, 20);
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ResponseCacheStore::new(&database_path);
        let result = store.get("nonexistent").expect("get should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn set_overwrites_existing() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ResponseCacheStore::new(&database_path);
        store
            .set("dup-key", "gpt-4", "first response", None, 50, 10, 3600)
            .expect("first set should succeed");
        store
            .set(
                "dup-key",
                "gpt-4o",
                "second response",
                Some("[{\"name\":\"tool\"}]"),
                80,
                30,
                3600,
            )
            .expect("second set should succeed");

        let entry = store
            .get("dup-key")
            .expect("get should succeed")
            .expect("entry should exist");

        assert_eq!(entry.model, "gpt-4o");
        assert_eq!(entry.response, "second response");
        assert_eq!(
            entry.tool_calls_json.as_deref(),
            Some("[{\"name\":\"tool\"}]")
        );
        assert_eq!(entry.input_tokens, 80);
        assert_eq!(entry.output_tokens, 30);
        // hit_count resets on overwrite because INSERT OR REPLACE resets to 0
        assert_eq!(entry.hit_count, 0);
    }

    #[test]
    fn clear_removes_all_entries() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ResponseCacheStore::new(&database_path);
        store
            .set("a", "m1", "r1", None, 10, 5, 3600)
            .expect("set a");
        store
            .set("b", "m2", "r2", None, 20, 10, 3600)
            .expect("set b");
        store
            .set("c", "m3", "r3", None, 30, 15, 3600)
            .expect("set c");

        let (count_before, _) = store.stats().expect("stats should succeed");
        assert_eq!(count_before, 3);

        let cleared = store.clear().expect("clear should succeed");
        assert_eq!(cleared, 3);

        let (count_after, _) = store.stats().expect("stats should succeed");
        assert_eq!(count_after, 0);

        assert!(store.get("a").expect("get should succeed").is_none());
        assert!(store.get("b").expect("get should succeed").is_none());
        assert!(store.get("c").expect("get should succeed").is_none());
    }
}

#[cfg(test)]
mod channel_store_tests {
    use super::{bootstrap, CachedChannel, ChannelStore};
    use tempfile::tempdir;

    fn make_channel(
        platform: &str,
        id: &str,
        name: &str,
        ch_type: &str,
        is_member: bool,
    ) -> CachedChannel {
        CachedChannel {
            platform: platform.to_string(),
            channel_id: id.to_string(),
            channel_name: name.to_string(),
            channel_type: ch_type.to_string(),
            is_member,
            cached_at: String::new(), // ignored on insert; DB sets CURRENT_TIMESTAMP
        }
    }

    #[test]
    fn cache_and_get_channel() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ChannelStore::new(&database_path);
        let channels = vec![
            make_channel("slack", "C001", "general", "channel", true),
            make_channel("slack", "C002", "random", "channel", false),
        ];

        let inserted = store
            .upsert_channels("slack", &channels)
            .expect("upsert_channels should succeed");
        assert_eq!(inserted, 2);

        let listed = store.list(Some("slack")).expect("list should succeed");
        assert_eq!(listed.len(), 2);

        // list is ordered by channel_name ASC
        assert_eq!(listed[0].channel_id, "C001");
        assert_eq!(listed[0].channel_name, "general");
        assert_eq!(listed[0].channel_type, "channel");
        assert!(listed[0].is_member);
        assert_eq!(listed[0].platform, "slack");

        assert_eq!(listed[1].channel_id, "C002");
        assert_eq!(listed[1].channel_name, "random");
        assert!(!listed[1].is_member);
    }

    #[test]
    fn list_returns_empty_for_unknown_platform() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ChannelStore::new(&database_path);
        let listed = store.list(Some("discord")).expect("list should succeed");
        assert!(listed.is_empty());
    }

    #[test]
    fn update_channel() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ChannelStore::new(&database_path);

        // Initial insert
        let initial = vec![
            make_channel("slack", "C001", "general", "channel", false),
            make_channel("slack", "C002", "random", "channel", true),
        ];
        store
            .upsert_channels("slack", &initial)
            .expect("initial upsert");

        // Update: change membership and add a new channel
        let updated = vec![
            make_channel("slack", "C001", "general", "channel", true),
            make_channel("slack", "C003", "engineering", "channel", true),
        ];
        store
            .upsert_channels("slack", &updated)
            .expect("update upsert");

        let listed = store.list(Some("slack")).expect("list should succeed");

        // upsert_channels deletes old entries for the platform, then inserts fresh ones
        assert_eq!(listed.len(), 2);

        let names: Vec<&str> = listed.iter().map(|c| c.channel_name.as_str()).collect();
        assert!(names.contains(&"general"));
        assert!(names.contains(&"engineering"));
        // "random" (C002) should be gone — replaced by the new batch
        assert!(!names.contains(&"random"));

        // Verify the updated membership flag
        let general = listed.iter().find(|c| c.channel_id == "C001").unwrap();
        assert!(general.is_member, "general should now be a member");
    }
}
