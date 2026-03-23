use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::Database;
use crate::error::StorageError;
use crate::util::{
    collect_rows, detect_uniform_embedding_dimensions, ensure_memory_vec_table,
    memory_embeddings_count, memory_vec_declared_dimensions, memory_vec_table_exists,
};

/// Embedding persistence layer for vector/semantic memory search.
pub struct EmbeddingStore {
    db: Database,
}

impl EmbeddingStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Returns the database path for this store.
    pub fn database_path(&self) -> &Path {
        self.db.path()
    }

    /// Store an embedding for a memory. Replaces any existing embedding for this memory_id.
    pub fn store(
        &self,
        memory_id: &str,
        embedding: &[f32],
        model: &str,
    ) -> Result<(), StorageError> {
        let mut connection = self.db.conn()?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let memory_rowid: i64 = tx
            .query_row(
                "SELECT rowid FROM memories WHERE id = ?1",
                params![memory_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        ensure_memory_vec_table(&tx, self.db.path(), embedding.len())?;
        let blob = embedding_to_blob(embedding);
        tx.execute(
            "INSERT INTO memory_embeddings (memory_id, embedding, model, dimensions, created_at)
             VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
             ON CONFLICT(memory_id) DO UPDATE SET embedding = excluded.embedding,
                model = excluded.model, dimensions = excluded.dimensions,
                created_at = excluded.created_at",
            params![memory_id, &blob, model, embedding.len() as i64],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;
        tx.execute(
            "DELETE FROM memory_vec WHERE memory_rowid = ?1",
            params![memory_rowid],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;
        tx.execute(
            "INSERT INTO memory_vec(memory_rowid, embedding) VALUES (?1, ?2)",
            params![memory_rowid, &blob],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;
        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Retrieve all embeddings for cosine similarity search.
    /// Returns (memory_id, embedding) pairs.
    pub fn all_embeddings(&self) -> Result<Vec<(String, Vec<f32>)>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare("SELECT memory_id, embedding FROM memory_embeddings")
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map([], |row| {
                let memory_id: String = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((memory_id, blob_to_embedding(&blob)))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path())
    }

    /// Delete an embedding by memory ID.
    pub fn delete(&self, memory_id: &str) -> Result<bool, StorageError> {
        let mut connection = self.db.conn()?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let memory_rowid: Option<i64> = tx
            .query_row(
                "SELECT rowid FROM memories WHERE id = ?1",
                params![memory_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        if let Some(memory_rowid) = memory_rowid {
            if memory_vec_table_exists(&tx, self.db.path())? {
                tx.execute(
                    "DELETE FROM memory_vec WHERE memory_rowid = ?1",
                    params![memory_rowid],
                )
                .map_err(|source| StorageError::Sqlite {
                    path: self.db.path().to_path_buf(),
                    source,
                })?;
            }
        }

        let rows = tx
            .execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                params![memory_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;
        Ok(rows > 0)
    }

    /// Check if an embedding exists for a given memory ID.
    pub fn has_embedding(&self, memory_id: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE memory_id = ?1",
                params![memory_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(count > 0)
    }

    /// Count total stored embeddings.
    pub fn count(&self) -> Result<usize, StorageError> {
        let connection = self.db.conn()?;
        memory_embeddings_count(&connection, self.db.path())
    }

    /// Returns the current database-wide embedding dimensions when known.
    pub fn dimensions(&self) -> Result<Option<usize>, StorageError> {
        let connection = self.db.conn()?;
        if let Some(dimensions) = memory_vec_declared_dimensions(&connection, self.db.path())?
        {
            return Ok(Some(dimensions));
        }
        detect_uniform_embedding_dimensions(&connection, self.db.path())
    }

    /// Remove all stored embeddings and clear the vector index.
    pub fn clear(&self) -> Result<(), StorageError> {
        let mut connection = self.db.conn()?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        tx.execute("DELETE FROM memory_embeddings", [])
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        if memory_vec_table_exists(&tx, self.db.path())? {
            tx.execute("DELETE FROM memory_vec", [])
                .map_err(|source| StorageError::Sqlite {
                    path: self.db.path().to_path_buf(),
                    source,
                })?;
        }
        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

/// Serialize an f32 slice to a little-endian byte blob for SQLite storage.
pub(crate) fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        blob.extend_from_slice(&val.to_le_bytes());
    }
    blob
}

/// Deserialize a little-endian byte blob back to an f32 vector.
fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod embedding_store_tests {
    use crate::{bootstrap, SessionStore};
    use super::EmbeddingStore;
    use tempfile::tempdir;

    fn setup(dir: &std::path::Path) -> std::path::PathBuf {
        let db_path = dir.join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let store = SessionStore::new(&db_path);
        store.create_session("s1", "test", None).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem1', 's1', 'fact', 'hello world', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem2', 's1', 'preference', 'likes rust', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        drop(conn);
        db_path
    }

    #[test]
    fn store_and_retrieve_embedding() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        let embedding = vec![0.1_f32, 0.2, 0.3, 0.4, 0.5];
        store
            .store("mem1", &embedding, "text-embedding-3-small")
            .unwrap();

        assert!(store.has_embedding("mem1").unwrap());
        assert!(!store.has_embedding("mem2").unwrap());
        assert_eq!(store.count().unwrap(), 1);

        let all = store.all_embeddings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "mem1");
        for (i, &val) in embedding.iter().enumerate() {
            assert!((all[0].1[i] - val).abs() < 1e-7);
        }
    }

    #[test]
    fn upsert_replaces_existing_embedding() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 2.0], "model-v1").unwrap();
        store.store("mem1", &[3.0, 4.0], "model-v2").unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let all = store.all_embeddings().unwrap();
        assert_eq!(all[0].1.len(), 2);
        assert!((all[0].1[0] - 3.0).abs() < 1e-7);
    }

    #[test]
    fn store_rejects_dimension_mismatch_for_existing_database() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 2.0], "model-v1").unwrap();
        let error = store
            .store("mem2", &[3.0, 4.0, 5.0], "model-v2")
            .expect_err("mixed dimensions should be rejected");

        assert!(matches!(
            error,
            crate::StorageError::EmbeddingDimensionMismatch {
                expected: 2,
                actual: 3,
                ..
            }
        ));
    }

    #[test]
    fn delete_removes_embedding() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0], "test").unwrap();
        assert!(store.delete("mem1").unwrap());
        assert!(!store.has_embedding("mem1").unwrap());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn delete_removes_vec_index_entry() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 0.0], "test").unwrap();
        assert!(store.delete("mem1").unwrap());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vec", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn bootstrap_tolerates_legacy_mixed_embedding_dimensions() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (memory_id, embedding, model, dimensions, created_at)
             VALUES ('mem1', ?1, 'legacy-a', 2, CURRENT_TIMESTAMP)",
            rusqlite::params![super::embedding_to_blob(&[1.0_f32, 0.0])],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (memory_id, embedding, model, dimensions, created_at)
             VALUES ('mem2', ?1, 'legacy-b', 3, CURRENT_TIMESTAMP)",
            rusqlite::params![super::embedding_to_blob(&[0.0_f32, 1.0, 0.0])],
        )
        .unwrap();
        drop(conn);

        crate::bootstrap(&db_path).expect("legacy mixed embeddings should not brick bootstrap");
    }

    #[test]
    fn store_recreates_empty_vector_index_with_new_dimension() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 0.0], "model-a").unwrap();
        assert!(store.delete("mem1").unwrap());
        store.store("mem1", &[1.0, 0.0, 0.0], "model-b").unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memory_vec'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(sql.contains("float[3]"), "unexpected schema: {sql}");
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        assert!(!store.delete("nonexistent").unwrap());
    }

    #[test]
    fn multiple_embeddings() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store.store("mem1", &[1.0, 0.0], "test").unwrap();
        store.store("mem2", &[0.0, 1.0], "test").unwrap();

        assert_eq!(store.count().unwrap(), 2);
        let all = store.all_embeddings().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn blob_serialization_roundtrip() {
        let original = vec![0.0_f32, 1.0, -1.0, f32::MIN, f32::MAX, std::f32::consts::PI];
        let blob = super::embedding_to_blob(&original);
        let restored = super::blob_to_embedding(&blob);
        assert_eq!(original.len(), restored.len());
        for (a, b) in original.iter().zip(restored.iter()) {
            assert_eq!(a.to_bits(), b.to_bits(), "bitwise equality for {a}");
        }
    }

    #[test]
    fn database_path_accessor() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        let store = EmbeddingStore::new(&db_path);
        assert_eq!(store.database_path(), db_path);
    }

    #[test]
    fn bootstrap_registers_sqlite_vec_functions() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let conn = rusqlite::Connection::open(&db_path).expect("open connection");
        let version: String = conn
            .query_row("SELECT vec_version()", [], |row| row.get(0))
            .expect("sqlite-vec should be available after bootstrap");

        assert!(!version.is_empty());
    }

    #[test]
    fn store_populates_memory_vec_index() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = EmbeddingStore::new(&db_path);

        store
            .store("mem1", &[0.1, 0.2, 0.3, 0.4], "local-384")
            .unwrap();

        let conn = rusqlite::Connection::open(&db_path).expect("open connection");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vec", [], |row| row.get(0))
            .expect("memory_vec should exist after storing an embedding");

        assert_eq!(count, 1);
    }
}

