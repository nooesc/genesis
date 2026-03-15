use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{open, StorageError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSchedule {
    pub id: String,
    pub cron_expression: String,
    pub destination: String,
    pub prompt: String,
    pub enabled: bool,
    pub created_at: String,
}

pub struct ScheduleStore {
    database_path: PathBuf,
}

impl ScheduleStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

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

        self.get(id)?.ok_or_else(|| StorageError::Sqlite {
            path: self.database_path.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

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
