use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{open, StorageError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSticker {
    pub file_unique_id: String,
    pub description: String,
    pub emoji: String,
    pub sticker_set: String,
    pub created_at: String,
}

pub struct StickerCacheStore {
    database_path: PathBuf,
}

impl StickerCacheStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

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
