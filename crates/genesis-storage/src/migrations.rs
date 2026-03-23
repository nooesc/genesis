use std::path::Path;

use rusqlite::Connection;

use crate::error::StorageError;
use crate::util::{column_exists, exec_migration, rebuild_memory_vec_index};

/// Migrate v1 → v2: add token tracking columns to sessions table.
pub(crate) fn migrate_to_v2(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v3(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v4(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v5(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v6(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v7(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v8(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v9(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v10(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
    rebuild_memory_vec_index(connection, database_path)
}

/// Migrate v10 → v11: add structured memory metadata columns and note-link edges.
pub(crate) fn migrate_to_v11(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v12(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
pub(crate) fn migrate_to_v13(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
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
