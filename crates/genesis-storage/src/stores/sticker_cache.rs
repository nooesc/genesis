use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Database;
use crate::error::StorageError;

/// Cached sticker description from vision analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSticker {
    pub file_unique_id: String,
    pub description: String,
    pub emoji: String,
    pub sticker_set: String,
    pub created_at: String,
}

/// Persistent cache for Telegram sticker descriptions.
///
/// Keyed by `file_unique_id` (stable across messages, unlike `file_id`).
/// Stores vision-analyzed descriptions to avoid re-analyzing the same sticker.
pub struct StickerCacheStore {
    db: Database,
}

impl StickerCacheStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Look up a cached sticker description by its unique file ID.
    pub fn get(&self, file_unique_id: &str) -> Result<Option<CachedSticker>, StorageError> {
        let connection = self.db.conn()?;
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
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// Store a sticker description in the cache.
    pub fn set(
        &self,
        file_unique_id: &str,
        description: &str,
        emoji: &str,
        sticker_set: &str,
    ) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute(
                "INSERT OR REPLACE INTO sticker_cache
                 (file_unique_id, description, emoji, sticker_set, created_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                params![file_unique_id, description, emoji, sticker_set],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Delete a cached sticker entry.
    pub fn delete(&self, file_unique_id: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let deleted = connection
            .execute(
                "DELETE FROM sticker_cache WHERE file_unique_id = ?1",
                params![file_unique_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(deleted > 0)
    }

    /// Return the total number of cached sticker entries.
    pub fn count(&self) -> Result<u64, StorageError> {
        let connection = self.db.conn()?;
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM sticker_cache", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(count as u64)
    }
}

#[cfg(test)]
mod sticker_cache_tests {
    use crate::bootstrap;
    use super::StickerCacheStore;
    use tempfile::tempdir;

    #[test]
    fn sticker_cache_stores_and_retrieves_description() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        store
            .set("unique-abc", "A cat waving hello", "😺", "CatPack")
            .expect("set should succeed");

        let entry = store
            .get("unique-abc")
            .expect("get should succeed")
            .expect("entry should exist");

        assert_eq!(entry.file_unique_id, "unique-abc");
        assert_eq!(entry.description, "A cat waving hello");
        assert_eq!(entry.emoji, "😺");
        assert_eq!(entry.sticker_set, "CatPack");
        assert!(!entry.created_at.is_empty());
    }

    #[test]
    fn sticker_cache_returns_none_for_missing_entry() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        let entry = store.get("nonexistent").expect("get should succeed");
        assert!(entry.is_none());
    }

    #[test]
    fn sticker_cache_upserts_on_conflict() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        store
            .set("unique-1", "A dog barking", "🐕", "DogPack")
            .expect("first set");
        store
            .set("unique-1", "A dog sleeping", "🐕", "DogPack")
            .expect("second set (upsert)");

        let entry = store.get("unique-1").unwrap().unwrap();
        assert_eq!(entry.description, "A dog sleeping");
    }

    #[test]
    fn sticker_cache_delete_removes_entry() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        store
            .set("del-1", "A bird flying", "🐦", "BirdPack")
            .unwrap();

        assert!(store.delete("del-1").unwrap());
        assert!(store.get("del-1").unwrap().is_none());
        assert!(!store.delete("del-1").unwrap()); // already deleted
    }

    #[test]
    fn sticker_cache_count_tracks_entries() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();

        let store = StickerCacheStore::new(&db_path);
        assert_eq!(store.count().unwrap(), 0);

        store.set("c-1", "Sticker 1", "😀", "Pack1").unwrap();
        store.set("c-2", "Sticker 2", "😎", "Pack2").unwrap();
        assert_eq!(store.count().unwrap(), 2);
    }
}

