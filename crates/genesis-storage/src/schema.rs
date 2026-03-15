use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{open, SCHEMA_VERSION};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageBootstrap {
    pub database_path: PathBuf,
    pub schema_version: i64,
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
                mirror INTEGER NOT NULL DEFAULT 0,
                mirror_source TEXT,
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
            ",
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    migrate_to_v2(&connection, database_path)?;
    migrate_to_v3(&connection, database_path)?;
    migrate_to_v4(&connection, database_path)?;
    migrate_to_v5(&connection, database_path)?;
    migrate_to_v6(&connection, database_path)?;
    migrate_to_v7(&connection, database_path)?;
    migrate_to_v8(&connection, database_path)?;
    migrate_to_v9(&connection, database_path)?;

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

fn migrate_to_v2(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
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

fn migrate_to_v3(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    let has_column: bool = connection
        .prepare("SELECT parent_session_id FROM sessions LIMIT 0")
        .is_ok();

    if has_column {
        return Ok(());
    }

    connection
        .execute_batch("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT;")
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

fn migrate_to_v4(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    let has_column: bool = connection.prepare("SELECT tags FROM sessions LIMIT 0").is_ok();

    if has_column {
        return Ok(());
    }

    connection
        .execute_batch("ALTER TABLE sessions ADD COLUMN tags TEXT NOT NULL DEFAULT '';")
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

fn migrate_to_v5(connection: &Connection, database_path: &Path) -> Result<(), StorageError> {
    let has_column: bool = connection.prepare("SELECT mirror FROM messages LIMIT 0").is_ok();

    if has_column {
        return Ok(());
    }

    connection
        .execute_batch(
            "ALTER TABLE messages ADD COLUMN mirror INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE messages ADD COLUMN mirror_source TEXT;",
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

fn migrate_to_v6(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
    connection
        .execute_batch(
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
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

fn migrate_to_v7(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
    connection
        .execute_batch(
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
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

fn migrate_to_v8(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS sticker_cache (
                file_unique_id TEXT PRIMARY KEY,
                description TEXT NOT NULL,
                emoji TEXT NOT NULL DEFAULT '',
                sticker_set TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

fn migrate_to_v9(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
    let has_column: bool = connection
        .prepare("SELECT provider_metadata FROM messages LIMIT 0")
        .is_ok();

    if has_column {
        return Ok(());
    }

    connection
        .execute("ALTER TABLE messages ADD COLUMN provider_metadata TEXT", [])
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
    use super::*;
    use crate::open;
    use tempfile::tempdir;

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
            .execute(
                "INSERT INTO audit_log (event_type) VALUES ('test')",
                [],
            )
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
    fn migrate_to_v9_is_idempotent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        // Bootstrap creates all tables including messages, then calls migrate_to_v9 once.
        bootstrap(&database_path).expect("bootstrap should succeed");

        // Running migrate_to_v9 again should be idempotent (column already exists).
        let connection = open(&database_path).expect("open should work");
        migrate_to_v9(&connection, &database_path).expect("second v9 migration should succeed");
    }
}
