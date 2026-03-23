use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::util::collect_rows;
use crate::Database;

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
    db: Database,
}

impl SandboxStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Insert or replace a sandbox record, keyed on (backend, task_id).
    pub fn upsert(
        &self,
        id: &str,
        backend: &str,
        task_id: &str,
        snapshot_data: Option<&str>,
    ) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
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
                path: self.db.path().to_path_buf(),
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
        let connection = self.db.conn()?;
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
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// Update the last_active timestamp for a sandbox by id.
    pub fn update_activity(&self, id: &str) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute(
                "UPDATE sandboxes SET last_active = datetime('now') WHERE id = ?1",
                params![id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Delete a sandbox record by id.
    pub fn delete(&self, id: &str) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute("DELETE FROM sandboxes WHERE id = ?1", params![id])
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// List all sandbox records, optionally filtered by backend.
    pub fn list(&self, backend: Option<&str>) -> Result<Vec<SandboxRow>, StorageError> {
        let connection = self.db.conn()?;

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
                path: self.db.path().to_path_buf(),
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
            path: self.db.path().to_path_buf(),
            source,
        })?;

        collect_rows(mapped_rows, self.db.path())
    }

    /// Delete sandbox records that have not been active for more than `days` days.
    /// Returns the number of records deleted.
    pub fn cleanup_older_than(&self, days: u32) -> Result<usize, StorageError> {
        let connection = self.db.conn()?;
        let deleted = connection
            .execute(
                "DELETE FROM sandboxes WHERE last_active < datetime('now', '-' || ?1 || ' days')",
                params![days],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod sandbox_store_tests {
    use super::SandboxStore;
    use crate::bootstrap;
    use crate::migrations::migrate_to_v9;
    use crate::util::open;
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
