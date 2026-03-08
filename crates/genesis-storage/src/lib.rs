use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: i64 = 2;

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
            .append_message("session-1", "user", Some("hello eve"), None, None)
            .expect("first message should be stored");
        store
            .append_message("session-1", "assistant", Some("hello operator"), None, None)
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
            .append_message("session-alpha", "user", Some("rust migration checklist"), None, None)
            .expect("first message should be stored");
        store
            .append_message("session-beta", "user", Some("provider client work"), None, None)
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

        store.add_usage("session-tok", 100, 50).expect("first add_usage");
        store.add_usage("session-tok", 200, 75).expect("second add_usage");

        let session = store.get_session("session-tok").expect("get should work").expect("session should exist");
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
        store.add_usage("session-zero", 0, 0).expect("zero add_usage");

        let session = store.get_session("session-zero").expect("get should work").expect("session should exist");
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
        store.create_session("s1", "cli", Some("Original")).expect("create");

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
        store.append_message("recent", "user", Some("hello"), None, None).expect("msg");

        let deleted = store.purge_older_than(30).unwrap();
        assert_eq!(deleted, 0);

        // Session and messages should still exist
        assert!(store.get_session("recent").unwrap().is_some());
        assert_eq!(store.load_messages("recent").unwrap().len(), 1);
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
    #[error("unknown import status in database: {0}")]
    UnknownImportStatus(String),
}

pub fn bootstrap(database_path: &Path) -> Result<StorageBootstrap, StorageError> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent).map_err(|source| StorageError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let connection = open(database_path)?;

    connection
        .execute_batch(
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
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL
            );
            CREATE TABLE IF NOT EXISTS schedules (
                id TEXT PRIMARY KEY,
                cron_expression TEXT NOT NULL,
                destination TEXT NOT NULL,
                prompt TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls_json TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
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
            CREATE VIRTUAL TABLE IF NOT EXISTS session_search USING fts5(
                session_id UNINDEXED,
                content
            );
            ",
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    // Run migrations for existing databases.
    migrate_to_v2(&connection, database_path)?;

    connection
        .execute(
            "
            INSERT INTO metadata (key, value) VALUES ('schema_version', ?1)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            ",
            params![SCHEMA_VERSION.to_string()],
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
    // Check if columns already exist (idempotent).
    let has_column: bool = connection
        .prepare("SELECT total_input_tokens FROM sessions LIMIT 0")
        .is_ok();

    if has_column {
        return Ok(());
    }

    connection
        .execute_batch(
            "ALTER TABLE sessions ADD COLUMN total_input_tokens INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE sessions ADD COLUMN total_output_tokens INTEGER NOT NULL DEFAULT 0;",
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(())
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
    pub created_at: String,
    pub updated_at: String,
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
    ) -> Result<i64, StorageError> {
        let connection = open(&self.database_path)?;

        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, role, content, tool_call_id, tool_calls_json],
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

    /// Delete a session and all its messages and search index entries.
    pub fn delete_session(&self, session_id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        // Delete FTS entries
        connection
            .execute(
                "DELETE FROM session_search WHERE session_id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        // Delete messages
        connection
            .execute(
                "DELETE FROM messages WHERE session_id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        // Delete session
        let deleted = connection
            .execute(
                "DELETE FROM sessions WHERE id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(deleted > 0)
    }

    /// Delete sessions (and their messages/FTS entries) older than `days` days.
    /// Returns the number of sessions deleted.
    pub fn purge_older_than(&self, days: u32) -> Result<u64, StorageError> {
        let connection = open(&self.database_path)?;
        let cutoff = format!("-{days} days");
        // Gather session IDs to purge
        let mut stmt = connection
            .prepare("SELECT id FROM sessions WHERE created_at < datetime('now', ?1)")
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        let ids: Vec<String> = stmt
            .query_map(params![cutoff], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .filter_map(|r| r.ok())
            .collect();

        for id in &ids {
            let _ = connection.execute(
                "DELETE FROM session_search WHERE session_id = ?1",
                params![id],
            );
            let _ = connection.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                params![id],
            );
            let _ = connection.execute(
                "DELETE FROM sessions WHERE id = ?1",
                params![id],
            );
        }

        Ok(ids.len() as u64)
    }

    /// Load all messages for a session in chronological order.
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>, StorageError> {
        let connection = open(&self.database_path)?;

        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, created_at
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY id ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let messages = stmt
            .query_map(params![session_id], |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    tool_call_id: row.get(4)?,
                    tool_calls_json: row.get(5)?,
                    created_at: row.get(6)?,
                })
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

        Ok(messages)
    }

    /// Full-text search across session content. Returns matching session IDs
    /// with their summaries, ordered by relevance.
    pub fn search_sessions(&self, query: &str) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = open(&self.database_path)?;

        let mut stmt = connection
            .prepare(
                "SELECT DISTINCT s.id, s.title, s.platform, s.total_input_tokens, s.total_output_tokens, s.created_at, s.updated_at
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1
                 ORDER BY rank",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let summaries = stmt
            .query_map(params![query], |row| {
                Ok(SessionSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    platform: row.get(2)?,
                    total_input_tokens: row.get(3)?,
                    total_output_tokens: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
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

        Ok(summaries)
    }

    /// Get a session summary by ID.
    pub fn get_session(&self, id: &str) -> Result<Option<SessionSummary>, StorageError> {
        let connection = open(&self.database_path)?;

        connection
            .query_row(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id],
                |row| {
                    Ok(SessionSummary {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        platform: row.get(2)?,
                        total_input_tokens: row.get(3)?,
                        total_output_tokens: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                },
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
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, created_at, updated_at
                 FROM sessions
                 ORDER BY updated_at DESC, created_at DESC, id DESC
                 LIMIT ?1",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let sessions = stmt
            .query_map(params![limit as i64], |row| {
                Ok(SessionSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    platform: row.get(2)?,
                    total_input_tokens: row.get(3)?,
                    total_output_tokens: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
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

        Ok(sessions)
    }

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

        // Aggregate totals for the period
        let (count, input, output): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(total_input_tokens), 0), COALESCE(SUM(total_output_tokens), 0) \
                 FROM sessions WHERE created_at >= datetime('now', ?)",
                [format!("-{days} days")],
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
            .query_map([format!("-{days} days")], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .filter_map(|r| r.ok())
            .collect();

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
            .query_map([format!("-{days} days")], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(InsightsData {
            period_days: days,
            sessions_count: count as u64,
            total_input_tokens: input as u64,
            total_output_tokens: output as u64,
            sessions_per_day,
            platform_breakdown,
        })
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
        let connection = open(&self.database_path)?;
        connection
            .execute(
                "INSERT INTO schedules (id, cron_expression, destination, prompt, enabled, created_at)
                 VALUES (?1, ?2, ?3, ?4, 1, CURRENT_TIMESTAMP)",
                params![id, cron_expression, destination, prompt],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        self.get(id)?
            .ok_or_else(|| StorageError::Sqlite {
                path: self.database_path.clone(),
                source: rusqlite::Error::QueryReturnedNoRows,
            })
    }

    /// Get a schedule by ID.
    pub fn get(&self, id: &str) -> Result<Option<StoredSchedule>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT id, cron_expression, destination, prompt, enabled, created_at
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
                "SELECT id, cron_expression, destination, prompt, enabled, created_at
                 FROM schedules WHERE enabled = 1
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let schedules = stmt
            .query_map([], |row| {
                Ok(StoredSchedule {
                    id: row.get(0)?,
                    cron_expression: row.get(1)?,
                    destination: row.get(2)?,
                    prompt: row.get(3)?,
                    enabled: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                })
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

        Ok(schedules)
    }

    /// List all schedules (enabled and disabled).
    pub fn list_all(&self) -> Result<Vec<StoredSchedule>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT id, cron_expression, destination, prompt, enabled, created_at
                 FROM schedules
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let schedules = stmt
            .query_map([], |row| {
                Ok(StoredSchedule {
                    id: row.get(0)?,
                    cron_expression: row.get(1)?,
                    destination: row.get(2)?,
                    prompt: row.get(3)?,
                    enabled: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                })
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

        Ok(schedules)
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

        self.get(trait_key)?
            .ok_or_else(|| StorageError::Sqlite {
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

        let traits = stmt
            .query_map([], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(traits)
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

        let traits = stmt
            .query_map(params![category], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(traits)
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

        let traits = stmt
            .query_map(params![threshold], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(traits)
    }

    /// Delete a user trait.
    pub fn delete(&self, trait_key: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute("DELETE FROM user_model WHERE trait_key = ?1", params![trait_key])
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

        self.get(name)?
            .ok_or_else(|| StorageError::Sqlite {
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
                |row| {
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
                },
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

        let skills = stmt
            .query_map([], |row| {
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

        Ok(skills)
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

        let skills = stmt
            .query_map(params![pattern], |row| {
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

        Ok(skills)
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

        self.get(id)?
            .ok_or_else(|| StorageError::Sqlite {
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
    pub fn list_by_parent(&self, parent_session_id: &str) -> Result<Vec<StoredSubagent>, StorageError> {
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

        let subagents = stmt
            .query_map(params![parent_session_id], Self::row_to_subagent)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(subagents)
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

        let usages = stmt
            .query_map(params![skill_name, limit as i64], Self::row_to_usage)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(usages)
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
            .query_map(params![limit as i64], |row| {
                Ok(StoredMemory {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    kind: row.get(2)?,
                    content: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// Full-text search across stored memories.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<StoredMemory>, StorageError> {
        let connection = open(&self.database_path)?;

        // Ensure FTS index exists (memory tools also create it, but this is defensive)
        connection
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                    memory_row_id UNINDEXED, kind, content
                );",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

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
            .query_map(params![query, limit as i64], |row| {
                Ok(StoredMemory {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    kind: row.get(2)?,
                    content: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    /// Delete a memory by ID.
    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute("DELETE FROM memories WHERE id = ?1", params![id])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;
        Ok(rows > 0)
    }
}

fn open(database_path: &Path) -> Result<Connection, StorageError> {
    Connection::open(database_path).map_err(|source| StorageError::OpenDatabase {
        path: database_path.to_path_buf(),
        source,
    })
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

#[cfg(test)]
mod tests {
    use super::{
        bootstrap, discover_legacy_source, inspect, latest_import_run, record_import_run,
        ImportStatus, LegacyImportSource, SessionStore, SCHEMA_VERSION,
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
            database_path: Some(dir.path().join("legacy-hermes").join("data").join("genesis.db")),
        };

        let recorded =
            record_import_run(&database_path, &source, ImportStatus::Planned).expect("recording should work");
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
            .append_message("s-2", "user", Some("Hello Eve"), None, None)
            .expect("append user should work");
        store
            .append_message("s-2", "assistant", Some("Hi there!"), None, None)
            .expect("append assistant should work");
        store
            .append_message("s-2", "assistant", None, None, Some(r#"[{"id":"call_1","type":"function","function":{"name":"echo","arguments":"{\"message\":\"test\"}"}}]"#))
            .expect("append tool_calls should work");
        store
            .append_message("s-2", "tool", Some("test"), Some("call_1"), None)
            .expect("append tool result should work");

        let messages = store
            .load_messages("s-2")
            .expect("load should work");

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content.as_deref(), Some("Hello Eve"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].tool_calls_json.is_some(), true);
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
            .append_message("s-3", "user", Some("Tell me about quantum computing"), None, None)
            .expect("append should work");
        store
            .append_message("s-4", "user", Some("What is the weather today"), None, None)
            .expect("append should work");

        let results = store
            .search_sessions("quantum")
            .expect("search should work");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "s-3");
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
        assert_eq!(skill.trigger_hint.as_deref(), Some("when user asks to review code"));

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
            .upsert("deploy", "Deploy to production (improved)", "run deploy script v2", None, &["ops"])
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
        store.upsert("alpha", "A skill", "do A", None, &["a"]).unwrap();
        store.upsert("beta", "B skill", "do B", None, &["b"]).unwrap();

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
        store.upsert("web_scrape", "Scrape web", "use curl", None, &["web", "data"]).unwrap();
        store.upsert("deploy", "Deploy", "kubectl apply", None, &["ops", "deploy"]).unwrap();
        store.upsert("test_runner", "Run tests", "cargo test", None, &["dev", "test"]).unwrap();

        let web_skills = store.find_by_tag("web").unwrap();
        assert_eq!(web_skills.len(), 1);
        assert_eq!(web_skills[0].name, "web_scrape");

        let dev_skills = store.find_by_tag("dev").unwrap();
        assert_eq!(dev_skills.len(), 1);
        assert_eq!(dev_skills[0].name, "test_runner");
    }

    #[test]
    fn user_model_observes_and_retrieves_traits() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let store = super::UserModelStore::new(&db_path);
        let t = store
            .observe("prefers_rust", "preference", "User prefers Rust over Python", Some("s1"))
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
        store.observe("likes_concise", "communication_style", "Prefers short answers", None).unwrap();
        store.observe("likes_concise", "communication_style", "Prefers short answers", None).unwrap();
        let t = store.observe("likes_concise", "communication_style", "Prefers short answers", None).unwrap();

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
            store.observe("expert_rust", "expertise", "Expert-level Rust developer", None).unwrap();
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
        store.observe("pref_dark_mode", "preference", "Likes dark mode", None).unwrap();
        store.observe("pref_vim", "preference", "Uses vim bindings", None).unwrap();
        store.observe("style_formal", "communication_style", "Formal tone", None).unwrap();

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
        store.observe("low_conf", "preference", "Maybe likes X", None).unwrap();
        // Observe "high_conf" 4 times: confidence = 0.5 + 3*0.1 = 0.8
        for _ in 0..4 {
            store.observe("high_conf", "preference", "Definitely likes Y", None).unwrap();
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
        store.observe("temp", "preference", "Temporary", None).unwrap();
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
        store.create("sub-2", "p", "c", "runner", "build it").unwrap();

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
        session_store.create_session("c1", "subagent", None).unwrap();
        session_store.create_session("c2", "subagent", None).unwrap();
        session_store.create_session("c3", "subagent", None).unwrap();

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
        store.record_usage("test", None, "failure", Some("Flaky")).unwrap();

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
}
