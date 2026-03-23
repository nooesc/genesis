pub mod cron;
pub mod error;
mod migrations;
mod stores;
mod util;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::migrations::{
    migrate_to_v10, migrate_to_v11, migrate_to_v12, migrate_to_v13, migrate_to_v14,
    migrate_to_v15, migrate_to_v2, migrate_to_v3, migrate_to_v4, migrate_to_v5, migrate_to_v6,
    migrate_to_v7, migrate_to_v8, migrate_to_v9,
};
use crate::util::{exec_migration, first_existing_dir, first_existing_file, open};

// ---------------------------------------------------------------------------
// Re-exports — keep the public API unchanged
// ---------------------------------------------------------------------------
pub use crate::error::StorageError;

pub use crate::stores::agent_bus::{AgentBusStore, AgentMessage, MessageKind};
pub use crate::stores::audit_log::{AuditEntry, AuditLogStore, LlmAnalytics, ToolAnalytics};
pub use crate::stores::channel::{CachedChannel, ChannelStore};
pub use crate::stores::embedding::EmbeddingStore;
pub use crate::stores::memory::{MemoryStore, NewMemoryNote, ScoredMemory, StoredMemory};
pub use crate::stores::pairing::{ApprovedUser, PairingStore, PendingPairing};
pub use crate::stores::response_cache::{CachedResponse, ResponseCacheStore};
pub use crate::stores::sandbox::{SandboxRow, SandboxStore};
pub use crate::stores::schedule::{ScheduleExecution, ScheduleStore, StoredSchedule};
pub use crate::stores::session::{
    InsightsData, MessageSearchResult, SessionStore, SessionSummary, StoredMessage, UsageStats,
};
pub use crate::stores::skill::{SkillStore, StoredSkill};
pub use crate::stores::skill_file::SkillFileStore;
pub use crate::stores::skill_usage::{SkillUsageStats, SkillUsageStore, StoredSkillUsage};
pub use crate::stores::sticker_cache::{CachedSticker, StickerCacheStore};
pub use crate::stores::subagent::{StoredSubagent, SubagentStore};
pub use crate::stores::user_model::{format_user_traits, StoredUserTrait, UserModelStore};

pub const SCHEMA_VERSION: i64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageBootstrap {
    pub database_path: PathBuf,
    pub schema_version: i64,
}

// ---------------------------------------------------------------------------
// Database — shared, pooled connection wrapper
// ---------------------------------------------------------------------------

/// A thread-safe, cheaply cloneable handle to a lazily-opened SQLite
/// connection.
///
/// The connection is opened on first use (with WAL mode, busy timeout, and
/// foreign-key pragmas) and then reused for every subsequent query.  Wrapping
/// it in `Arc<Mutex<…>>` means multiple stores created from the same
/// `Database` share one connection without paying the open/pragma cost on
/// every operation.
#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

struct DatabaseInner {
    conn: Mutex<Option<Connection>>,
    path: PathBuf,
}

impl Database {
    /// Create a handle for the database at `path`.  The actual SQLite
    /// connection is opened lazily on the first call to [`conn`].
    pub fn new(path: &Path) -> Self {
        Self {
            inner: Arc::new(DatabaseInner {
                conn: Mutex::new(None),
                path: path.to_path_buf(),
            }),
        }
    }

    /// Eagerly open (or create) the SQLite database.  Useful when you want
    /// to surface open-errors immediately rather than on first query.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let db = Self::new(path);
        // Force the lazy open now.
        let _ = db.conn()?;
        Ok(db)
    }

    /// Returns the filesystem path for this database.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Acquire the shared connection via a mutex guard.
    ///
    /// On the first call the connection is opened and configured.  The
    /// returned guard dereferences to `Connection` and releases the lock
    /// when dropped.  For mutable access (e.g. transactions), bind the
    /// result as `let mut conn = db.conn()?;`.
    pub fn conn(&self) -> Result<DatabaseGuard<'_>, StorageError> {
        let mut guard =
            self.inner
                .conn
                .lock()
                .map_err(|_| StorageError::ConnectionPoolPoisoned {
                    path: self.inner.path.clone(),
                })?;

        if guard.is_none() {
            *guard = Some(open(&self.inner.path)?);
        }

        Ok(DatabaseGuard { guard })
    }
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.inner.path)
            .finish()
    }
}

/// RAII guard returned by [`Database::conn`].  Dereferences to
/// [`Connection`] and releases the mutex when dropped.
pub struct DatabaseGuard<'a> {
    guard: MutexGuard<'a, Option<Connection>>,
}

impl<'a> std::ops::Deref for DatabaseGuard<'a> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.guard
            .as_ref()
            .expect("DatabaseGuard: connection must be initialised before deref")
    }
}

impl<'a> std::ops::DerefMut for DatabaseGuard<'a> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.guard
            .as_mut()
            .expect("DatabaseGuard: connection must be initialised before deref_mut")
    }
}

// ---------------------------------------------------------------------------
// Types used by bootstrap / inspect / import functions
// ---------------------------------------------------------------------------

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
                consolidated INTEGER NOT NULL DEFAULT 0,
                parent_summary_id TEXT REFERENCES memories(id),
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL
            );
            CREATE TABLE IF NOT EXISTS memory_links (
                source_id TEXT NOT NULL,
                target_id TEXT NOT NULL,
                edge_type TEXT NOT NULL DEFAULT 'semantic',
                weight REAL NOT NULL DEFAULT 1.0,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (source_id, target_id),
                FOREIGN KEY(source_id) REFERENCES memories(id) ON DELETE CASCADE,
                FOREIGN KEY(target_id) REFERENCES memories(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_memory_links_edge_type
                ON memory_links(edge_type);
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
    migrate_to_v14(&connection, database_path)?;
    migrate_to_v15(&connection, database_path)?;

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

/// Run `PRAGMA integrity_check` on the database and return the result string.
///
/// A healthy database returns `"ok"`. Any other value describes the problem.
pub fn integrity_check(database_path: &Path) -> Result<String, StorageError> {
    let connection = open(database_path)?;
    connection
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })
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

#[cfg(test)]
mod tests {
    use super::{
        bootstrap, discover_legacy_source, inspect, latest_import_run, record_import_run,
        ImportStatus, LegacyImportSource, SessionStore, SCHEMA_VERSION,
    };
    use crate::migrations::{migrate_to_v6, migrate_to_v7, migrate_to_v8, migrate_to_v9};
    use crate::util::open;
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
            .create_with_timezone(
                "tz-1",
                "0 9 * * *",
                "cli",
                "morning",
                Some("America/New_York"),
            )
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

        store
            .create("exec-test", "*/5 * * * *", "cli", "job")
            .unwrap();

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
