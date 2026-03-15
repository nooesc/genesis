use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{open, StorageError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

pub struct ResponseCacheStore {
    database_path: PathBuf,
}

impl ResponseCacheStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

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
