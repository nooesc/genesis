use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::Database;

/// A cached LLM response entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// SQLite-backed response cache for LLM completions.
///
/// Caches responses keyed by a deterministic hash of the request parameters
/// (model + messages + tools + temperature). Entries expire after a configurable TTL.
pub struct ResponseCacheStore {
    db: Database,
}

impl ResponseCacheStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Look up a cached response by its cache key.
    /// Returns `None` if the entry doesn't exist or has expired.
    /// Increments the hit counter on successful lookup.
    pub fn get(&self, cache_key: &str) -> Result<Option<CachedResponse>, StorageError> {
        let connection = self.db.conn()?;

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
                path: self.db.path().to_path_buf(),
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

    /// Store a response in the cache.
    ///
    /// `ttl_seconds` controls how long the entry stays valid.
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
        let connection = self.db.conn()?;
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
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Remove expired entries from the cache.
    pub fn prune_expired(&self) -> Result<u64, StorageError> {
        let connection = self.db.conn()?;
        let deleted = connection
            .execute(
                "DELETE FROM response_cache WHERE expires_at <= datetime('now')",
                [],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(deleted as u64)
    }

    /// Clear all cache entries.
    pub fn clear(&self) -> Result<u64, StorageError> {
        let connection = self.db.conn()?;
        let deleted = connection
            .execute("DELETE FROM response_cache", [])
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(deleted as u64)
    }

    /// Return total number of cached entries and total hit count.
    pub fn stats(&self) -> Result<(u64, u64), StorageError> {
        let connection = self.db.conn()?;
        let (entries, hits) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(hit_count), 0) FROM response_cache
                 WHERE expires_at > datetime('now')",
                [],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok((entries, hits))
    }
}

#[cfg(test)]
mod response_cache_store_tests {
    use super::ResponseCacheStore;
    use crate::bootstrap;
    use tempfile::tempdir;

    #[test]
    fn set_and_get_cached_response() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ResponseCacheStore::new(&database_path);
        store
            .set("key-1", "gpt-4", "Hello world", None, 100, 20, 3600)
            .expect("set should succeed");

        let entry = store
            .get("key-1")
            .expect("get should succeed")
            .expect("entry should exist");

        assert_eq!(entry.cache_key, "key-1");
        assert_eq!(entry.model, "gpt-4");
        assert_eq!(entry.response, "Hello world");
        assert!(entry.tool_calls_json.is_none());
        assert_eq!(entry.input_tokens, 100);
        assert_eq!(entry.output_tokens, 20);
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ResponseCacheStore::new(&database_path);
        let result = store.get("nonexistent").expect("get should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn set_overwrites_existing() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ResponseCacheStore::new(&database_path);
        store
            .set("dup-key", "gpt-4", "first response", None, 50, 10, 3600)
            .expect("first set should succeed");
        store
            .set(
                "dup-key",
                "gpt-4o",
                "second response",
                Some("[{\"name\":\"tool\"}]"),
                80,
                30,
                3600,
            )
            .expect("second set should succeed");

        let entry = store
            .get("dup-key")
            .expect("get should succeed")
            .expect("entry should exist");

        assert_eq!(entry.model, "gpt-4o");
        assert_eq!(entry.response, "second response");
        assert_eq!(
            entry.tool_calls_json.as_deref(),
            Some("[{\"name\":\"tool\"}]")
        );
        assert_eq!(entry.input_tokens, 80);
        assert_eq!(entry.output_tokens, 30);
        // hit_count resets on overwrite because INSERT OR REPLACE resets to 0
        assert_eq!(entry.hit_count, 0);
    }

    #[test]
    fn clear_removes_all_entries() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ResponseCacheStore::new(&database_path);
        store
            .set("a", "m1", "r1", None, 10, 5, 3600)
            .expect("set a");
        store
            .set("b", "m2", "r2", None, 20, 10, 3600)
            .expect("set b");
        store
            .set("c", "m3", "r3", None, 30, 15, 3600)
            .expect("set c");

        let (count_before, _) = store.stats().expect("stats should succeed");
        assert_eq!(count_before, 3);

        let cleared = store.clear().expect("clear should succeed");
        assert_eq!(cleared, 3);

        let (count_after, _) = store.stats().expect("stats should succeed");
        assert_eq!(count_after, 0);

        assert!(store.get("a").expect("get should succeed").is_none());
        assert!(store.get("b").expect("get should succeed").is_none());
        assert!(store.get("c").expect("get should succeed").is_none());
    }
}
