use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{open, StorageError};

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

pub struct SubagentStore {
    database_path: PathBuf,
}

impl SubagentStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

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
