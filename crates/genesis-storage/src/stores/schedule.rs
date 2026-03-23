use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Database;
use crate::error::StorageError;
use crate::util::collect_rows;

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
    db: Database,
}

impl ScheduleStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
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
        let connection = self.db.conn()?;
        connection
            .execute(
                "INSERT INTO schedules (id, cron_expression, destination, prompt, enabled, created_at, timezone)
                 VALUES (?1, ?2, ?3, ?4, 1, CURRENT_TIMESTAMP, ?5)",
                params![id, cron_expression, destination, prompt, timezone],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Drop the connection guard before calling self.get() to avoid mutex
        // re-entrance deadlock.
        drop(connection);

        self.get(id)?.ok_or_else(|| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Get a schedule by ID.
    pub fn get(&self, id: &str) -> Result<Option<StoredSchedule>, StorageError> {
        let connection = self.db.conn()?;
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
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// List all enabled schedules.
    pub fn list_enabled(&self) -> Result<Vec<StoredSchedule>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, cron_expression, destination, prompt, enabled, created_at, timezone
                 FROM schedules WHERE enabled = 1
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
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
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// List all schedules (enabled and disabled).
    pub fn list_all(&self) -> Result<Vec<StoredSchedule>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, cron_expression, destination, prompt, enabled, created_at, timezone
                 FROM schedules
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
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
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
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
        let connection = self.db.conn()?;
        let db = self.db.path();
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.to_path_buf(),
            source,
        };

        let (count_sql, data_sql) = if enabled_only {
            (
                "SELECT COUNT(*) FROM schedules WHERE enabled = 1",
                "SELECT id, cron_expression, destination, prompt, enabled, created_at, timezone
                 FROM schedules WHERE enabled = 1
                 ORDER BY created_at ASC
                 LIMIT ?1 OFFSET ?2",
            )
        } else {
            (
                "SELECT COUNT(*) FROM schedules",
                "SELECT id, cron_expression, destination, prompt, enabled, created_at, timezone
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
                    timezone: row.get(6).ok().flatten(),
                })
            })
            .map_err(me)?;

        let items = collect_rows(rows, self.db.path())?;
        Ok((items, total as u64))
    }

    /// Enable or disable a schedule.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows_changed = connection
            .execute(
                "UPDATE schedules SET enabled = ?2 WHERE id = ?1",
                params![id, enabled as i64],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows_changed > 0)
    }

    /// Delete a schedule by ID.
    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows_changed = connection
            .execute("DELETE FROM schedules WHERE id = ?1", params![id])
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
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
        let connection = self.db.conn()?;
        connection
            .execute(
                "INSERT INTO schedule_executions (schedule_id, executed_at, status, error_message, duration_ms)
                 VALUES (?1, CURRENT_TIMESTAMP, ?2, ?3, ?4)",
                params![schedule_id, status, error_message, duration_ms],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
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
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, schedule_id, executed_at, status, error_message, duration_ms
                 FROM schedule_executions WHERE schedule_id = ?1
                 ORDER BY executed_at DESC LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
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
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }
}
