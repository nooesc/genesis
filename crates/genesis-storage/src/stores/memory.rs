use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::stores::embedding::embedding_to_blob;
use crate::util::{
    collect_rows, decayed_importance, edge_type_weight, exec_migration,
    memory_vec_declared_dimensions, memory_vec_table_exists, sql_placeholders,
    sqlite_table_exists,
};
use crate::Database;

/// A stored memory (key-value note persisted by the agent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMemory {
    pub id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub content: String,
    pub created_at: String,
}

/// A memory search result with its combined score.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoredMemory {
    pub memory: StoredMemory,
    /// Combined score from hybrid search (higher is better).
    pub score: f64,
    /// Source of the match: "fts", "vector", or "hybrid".
    pub source: String,
}

#[derive(Debug, Clone)]
struct GraphAttributes {
    importance: f32,
    accessed_at: String,
}

#[derive(Debug, Clone)]
struct GraphCandidate {
    memory: StoredMemory,
    attributes: GraphAttributes,
    edge_type: String,
    edge_weight: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewMemoryNote {
    pub id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub content: String,
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    pub linked_ids: Vec<String>,
    pub importance: f32,
}

/// Memory persistence layer.
pub struct MemoryStore {
    db: Database,
}

impl MemoryStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Map a database row to a `StoredMemory`.
    ///
    /// Expects columns in order: id, session_id, kind, content, created_at.
    fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<StoredMemory> {
        Ok(StoredMemory {
            id: row.get(0)?,
            session_id: row.get(1)?,
            kind: row.get(2)?,
            content: row.get(3)?,
            created_at: row.get(4)?,
        })
    }

    /// List all stored memories, most recent first.
    pub fn list(&self, limit: usize) -> Result<Vec<StoredMemory>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, kind, content, created_at
                 FROM memories ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(params![limit as i64], Self::row_to_memory)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path())
    }

    /// List stored memories with offset/limit pagination.
    pub fn list_paginated(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<StoredMemory>, u64), StorageError> {
        let connection = self.db.conn()?;

        let total: i64 = connection
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, kind, content, created_at
                 FROM memories ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64], Self::row_to_memory)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let items = collect_rows(rows, self.db.path())?;
        Ok((items, total as u64))
    }

    /// Get a single memory by ID. Returns `None` if not found.
    pub fn get(&self, id: &str) -> Result<Option<StoredMemory>, StorageError> {
        let connection = self.db.conn()?;
        connection
            .query_row(
                "SELECT id, session_id, kind, content, created_at
                 FROM memories WHERE id = ?1",
                params![id],
                Self::row_to_memory,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// Full-text search across stored memories.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<StoredMemory>, StorageError> {
        let connection = self.db.conn()?;

        // Ensure FTS index exists (memory tools also create it, but this is defensive)
        exec_migration(
            &connection,
            self.db.path(),
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )?;

        let mut stmt = connection
            .prepare(
                "SELECT m.id, m.session_id, m.kind, m.content, m.created_at
                 FROM memory_search ms
                 JOIN memories m ON m.rowid = ms.memory_row_id
                 WHERE memory_search MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(params![query, limit as i64], Self::row_to_memory)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path())
    }

    /// Vector similarity search across stored memory embeddings.
    pub fn vector_search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<ScoredMemory>, StorageError> {
        if limit == 0 || query_embedding.is_empty() {
            return Ok(Vec::new());
        }

        let connection = self.db.conn()?;
        let Some(expected_dimensions) =
            memory_vec_declared_dimensions(&connection, self.db.path())?
        else {
            return Ok(Vec::new());
        };

        if expected_dimensions != query_embedding.len() {
            return Ok(Vec::new());
        }

        let query_blob = embedding_to_blob(query_embedding);
        let mut stmt = connection
            .prepare(
                "SELECT m.id, m.session_id, m.kind, m.content, m.created_at, distance
                 FROM memory_vec mv
                 JOIN memories m ON m.rowid = mv.memory_rowid
                 WHERE embedding MATCH ?1 AND k = ?2
                 ORDER BY distance",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(params![query_blob, limit as i64], |row| {
                let distance: f64 = row.get(5)?;
                Ok(ScoredMemory {
                    memory: StoredMemory {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        kind: row.get(2)?,
                        content: row.get(3)?,
                        created_at: row.get(4)?,
                    },
                    score: 1.0 / (1.0 + distance),
                    source: "vector".to_owned(),
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path())
    }

    /// Hybrid search combining FTS results with vector similarity via reciprocal rank fusion.
    pub fn hybrid_search(
        &self,
        query: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<ScoredMemory>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let fts_results = self.search(query, limit * 2)?;
        let vector_results = self.vector_search(query_embedding, limit * 2)?;
        Ok(reciprocal_rank_fusion(&fts_results, &vector_results, limit))
    }

    pub fn graph_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ScoredMemory>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let primary = self.search(query, limit)?;
        if primary.is_empty() {
            return Ok(Vec::new());
        }

        let connection = self.db.conn()?;
        let primary_ids: Vec<String> = primary.iter().map(|memory| memory.id.clone()).collect();
        let attributes = self.graph_attributes_for_ids(&connection, &primary_ids)?;
        let linked_memories = self.linked_memories_for_sources(&connection, &primary_ids)?;
        let now = Utc::now();
        let mut seen = HashSet::new();
        let mut expanded = Vec::new();

        for (rank, memory) in primary.iter().enumerate() {
            let base_score = 1.0 / (60.0 + rank as f64);
            if seen.insert(memory.id.clone()) {
                if let Some(attributes) = attributes.get(&memory.id) {
                    expanded.push(Self::score_graph_memory(
                        memory, attributes, base_score, &now,
                    )?);
                }
            }

            for (link_rank, linked) in linked_memories
                .get(&memory.id)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if seen.insert(linked.memory.id.clone()) {
                    let link_score = base_score
                        * (0.5 / (link_rank as f64 + 1.0))
                        * edge_type_weight(&linked.edge_type)
                        * linked.edge_weight;
                    expanded.push(Self::score_graph_memory(
                        &linked.memory,
                        &linked.attributes,
                        link_score,
                        &now,
                    )?);
                }
            }
        }

        expanded.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        expanded.truncate(limit);

        // Drop the connection guard before calling self.bump_access_metrics()
        // to avoid mutex re-entrance deadlock.
        drop(connection);

        self.bump_access_metrics(
            &expanded
                .iter()
                .map(|item| item.memory.id.clone())
                .collect::<Vec<_>>(),
        )?;
        Ok(expanded)
    }

    pub fn create_note(&self, note: NewMemoryNote) -> Result<StoredMemory, StorageError> {
        let mut connection = self.db.conn()?;
        exec_migration(
            &connection,
            self.db.path(),
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )?;

        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let note_id = note.id.clone();
        let session_id = note.session_id.clone();
        let kind = note.kind.clone();
        let content = note.content.clone();
        let linked_ids = note.linked_ids.clone();
        // Vec<String> serialization is infallible in practice; fall back to
        // empty array and log to stderr if it somehow fails.
        let keywords_json = serde_json::to_string(&note.keywords).unwrap_or_else(|e| {
            eprintln!("warning: failed to serialize memory keywords: {e}");
            "[]".to_owned()
        });
        let tags_json = serde_json::to_string(&note.tags).unwrap_or_else(|e| {
            eprintln!("warning: failed to serialize memory tags: {e}");
            "[]".to_owned()
        });

        tx.execute(
            "INSERT INTO memories (
                id, session_id, kind, content, created_at, keywords_json, tags_json,
                importance, accessed_at, access_count
             ) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP, ?5, ?6, ?7, CURRENT_TIMESTAMP, 0)",
            params![
                &note_id,
                session_id,
                &kind,
                &content,
                keywords_json,
                tags_json,
                note.importance,
            ],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;

        let memory_row_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, ?2, ?3)",
            params![memory_row_id, &kind, &content],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;

        for linked_id in linked_ids
            .iter()
            .filter(|linked_id| linked_id.as_str() != note_id.as_str())
        {
            insert_pending_memory_link(&tx, self.db.path(), &note_id, linked_id)?;
            insert_pending_memory_link(&tx, self.db.path(), linked_id, &note_id)?;
        }
        resolve_pending_memory_links(&tx, self.db.path(), &note_id)?;

        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;

        // Drop the connection guard before calling self.get() to avoid mutex
        // re-entrance deadlock.
        drop(connection);

        self.get(&note_id)?.ok_or_else(|| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Create and index a new memory entry.
    pub fn create(
        &self,
        session_id: Option<&str>,
        kind: &str,
        content: &str,
    ) -> Result<StoredMemory, StorageError> {
        self.create_note(NewMemoryNote {
            id: format!("memory-{}", memory_unique_suffix()),
            session_id: session_id.map(str::to_owned),
            kind: kind.to_owned(),
            content: content.to_owned(),
            keywords: Vec::new(),
            tags: Vec::new(),
            linked_ids: Vec::new(),
            importance: 0.5,
        })
    }

    pub fn links_for(&self, id: &str) -> Result<Vec<String>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT target_id
                 FROM memory_links
                 WHERE source_id = ?1
                 ORDER BY target_id ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(params![id], |row| row.get::<_, String>(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path())
    }

    /// Insert or update a typed, weighted edge between two memories.
    pub fn create_link(
        &self,
        source_id: &str,
        target_id: &str,
        edge_type: &str,
        weight: f64,
    ) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute(
                "INSERT INTO memory_links (source_id, target_id, edge_type, weight)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(source_id, target_id) DO UPDATE
                 SET edge_type = excluded.edge_type, weight = excluded.weight",
                params![source_id, target_id, edge_type, weight],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    fn graph_attributes_for_ids(
        &self,
        connection: &Connection,
        memory_ids: &[String],
    ) -> Result<HashMap<String, GraphAttributes>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut stmt = connection
            .prepare(&format!(
                "SELECT id, importance, accessed_at
                 FROM memories
                 WHERE id IN ({})",
                sql_placeholders(memory_ids.len()),
            ))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(
                params_from_iter(memory_ids.iter().map(|id| id.as_str())),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        GraphAttributes {
                            importance: row.get::<_, f32>(1)?,
                            accessed_at: row.get::<_, String>(2)?,
                        },
                    ))
                },
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path()).map(|pairs| pairs.into_iter().collect())
    }

    fn linked_memories_for_sources(
        &self,
        connection: &Connection,
        source_ids: &[String],
    ) -> Result<HashMap<String, Vec<GraphCandidate>>, StorageError> {
        if source_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut stmt = connection
            .prepare(&format!(
                "SELECT ml.source_id, m.id, m.session_id, m.kind, m.content, m.created_at,
                        m.importance, m.accessed_at, ml.edge_type, ml.weight
                 FROM memory_links ml
                 JOIN memories m ON m.id = ml.target_id
                 WHERE ml.source_id IN ({})
                 ORDER BY ml.source_id ASC, m.created_at ASC",
                sql_placeholders(source_ids.len()),
            ))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(
                params_from_iter(source_ids.iter().map(|id| id.as_str())),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        GraphCandidate {
                            memory: StoredMemory {
                                id: row.get(1)?,
                                session_id: row.get(2)?,
                                kind: row.get(3)?,
                                content: row.get(4)?,
                                created_at: row.get(5)?,
                            },
                            attributes: GraphAttributes {
                                importance: row.get::<_, f32>(6)?,
                                accessed_at: row.get::<_, String>(7)?,
                            },
                            edge_type: row.get::<_, String>(8)?,
                            edge_weight: row.get::<_, f64>(9)?,
                        },
                    ))
                },
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let mut grouped = HashMap::new();
        for (source_id, candidate) in collect_rows(rows, self.db.path())? {
            grouped
                .entry(source_id)
                .or_insert_with(Vec::new)
                .push(candidate);
        }
        Ok(grouped)
    }

    fn score_graph_memory(
        memory: &StoredMemory,
        attributes: &GraphAttributes,
        base_score: f64,
        now: &DateTime<Utc>,
    ) -> Result<ScoredMemory, StorageError> {
        let score = base_score
            * decayed_importance(attributes.importance, &attributes.accessed_at, now)? as f64;
        Ok(ScoredMemory {
            memory: memory.clone(),
            score,
            source: "graph".to_owned(),
        })
    }

    fn bump_access_metrics(&self, memory_ids: &[String]) -> Result<(), StorageError> {
        if memory_ids.is_empty() {
            return Ok(());
        }

        let mut connection = self.db.conn()?;
        let tx = connection
            .transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        for memory_id in memory_ids {
            tx.execute(
                "UPDATE memories
                 SET accessed_at = CURRENT_TIMESTAMP,
                     access_count = access_count + 1
                 WHERE id = ?1",
                params![memory_id],
            )
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

    /// Delete a memory by ID.
    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
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
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let Some(memory_rowid) = memory_rowid else {
            return Ok(false);
        };

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

        if sqlite_table_exists(&tx, self.db.path(), "memory_search")? {
            tx.execute(
                "DELETE FROM memory_search WHERE memory_row_id = ?1",
                params![memory_rowid],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        }

        let rows = tx
            .execute("DELETE FROM memories WHERE id = ?1", params![id])
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
}

fn insert_pending_memory_link(
    connection: &Connection,
    database_path: &Path,
    source_id: &str,
    target_id: &str,
) -> Result<(), StorageError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO pending_memory_links (source_id, target_id) VALUES (?1, ?2)",
            params![source_id, target_id],
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn resolve_pending_memory_links(
    connection: &Connection,
    database_path: &Path,
    memory_id: &str,
) -> Result<(), StorageError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO memory_links (source_id, target_id)
             SELECT pending.source_id, pending.target_id
             FROM pending_memory_links pending
             JOIN memories source ON source.id = pending.source_id
             JOIN memories target ON target.id = pending.target_id
             WHERE pending.source_id = ?1 OR pending.target_id = ?1",
            params![memory_id],
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    connection
        .execute(
            "DELETE FROM pending_memory_links
             WHERE rowid IN (
                 SELECT pending.rowid
                 FROM pending_memory_links pending
                 JOIN memories source ON source.id = pending.source_id
                 JOIN memories target ON target.id = pending.target_id
                 WHERE pending.source_id = ?1 OR pending.target_id = ?1
             )",
            params![memory_id],
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn memory_unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn reciprocal_rank_fusion(
    fts_results: &[StoredMemory],
    vector_results: &[ScoredMemory],
    limit: usize,
) -> Vec<ScoredMemory> {
    const K: f64 = 60.0;

    let mut scores: std::collections::HashMap<String, (f64, Option<StoredMemory>)> =
        std::collections::HashMap::new();

    for (rank, memory) in fts_results.iter().enumerate() {
        let entry = scores.entry(memory.id.clone()).or_insert((0.0, None));
        entry.0 += 1.0 / (K + rank as f64);
        if entry.1.is_none() {
            entry.1 = Some(memory.clone());
        }
    }

    for (rank, scored) in vector_results.iter().enumerate() {
        let entry = scores
            .entry(scored.memory.id.clone())
            .or_insert((0.0, None));
        entry.0 += 1.0 / (K + rank as f64);
        if entry.1.is_none() {
            entry.1 = Some(scored.memory.clone());
        }
    }

    let mut merged: Vec<ScoredMemory> = scores
        .into_iter()
        .filter_map(|(_, (score, memory))| {
            memory.map(|memory| ScoredMemory {
                memory,
                score,
                source: "hybrid".to_owned(),
            })
        })
        .collect();

    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(limit);
    merged
}

#[cfg(test)]
mod memory_store_tests {
    use super::{MemoryStore, NewMemoryNote};
    use crate::{bootstrap, EmbeddingStore, SessionStore};
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
    fn get_returns_existing_memory() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = MemoryStore::new(&db_path);

        let memory = store.get("mem1").unwrap();
        assert!(memory.is_some());
        let memory = memory.unwrap();
        assert_eq!(memory.id, "mem1");
        assert_eq!(memory.kind, "fact");
        assert_eq!(memory.content, "hello world");
    }

    #[test]
    fn get_returns_none_for_nonexistent() {
        let dir = tempdir().expect("tempdir");
        let db_path = setup(dir.path());
        let store = MemoryStore::new(&db_path);

        let memory = store.get("nonexistent").unwrap();
        assert!(memory.is_none());
    }

    #[test]
    fn create_note_persists_bidirectional_links() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        store
            .create_note(NewMemoryNote {
                id: "note-b".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust powers Genesis".to_owned(),
                keywords: vec!["rust".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.6,
            })
            .unwrap();

        store
            .create_note(NewMemoryNote {
                id: "note-a".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis uses Rust".to_owned(),
                keywords: vec!["genesis".to_owned(), "rust".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["note-b".to_owned()],
                importance: 0.8,
            })
            .unwrap();

        let a_links = store.links_for("note-a").unwrap();
        let b_links = store.links_for("note-b").unwrap();
        assert_eq!(a_links, vec!["note-b".to_owned()]);
        assert_eq!(b_links, vec!["note-a".to_owned()]);
    }

    #[test]
    fn importance_decays_with_time_since_access() {
        let original = 1.0_f32;
        let decayed = crate::util::decayed_importance(
            original,
            "2026-03-01T00:00:00Z",
            &crate::util::parse_timestamp("2026-03-11T00:00:00Z").unwrap(),
        )
        .expect("timestamps should parse");
        assert!(decayed < original);
    }

    #[test]
    fn create_note_allows_links_to_notes_inserted_later() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).unwrap();
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        store
            .create_note(NewMemoryNote {
                id: "note-a".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis uses Rust".to_owned(),
                keywords: vec!["genesis".to_owned(), "rust".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["note-b".to_owned()],
                importance: 0.8,
            })
            .unwrap();

        store
            .create_note(NewMemoryNote {
                id: "note-b".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust powers Genesis".to_owned(),
                keywords: vec!["rust".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.6,
            })
            .unwrap();

        let a_links = store.links_for("note-a").unwrap();
        let b_links = store.links_for("note-b").unwrap();
        assert_eq!(a_links, vec!["note-b".to_owned()]);
        assert_eq!(b_links, vec!["note-a".to_owned()]);
    }

    fn seed_linked_memory_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let db_path = dir.join("linked.db");
        bootstrap(&db_path).expect("bootstrap");
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        store
            .create_note(NewMemoryNote {
                id: "linked-note".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust ownership keeps Genesis safe".to_owned(),
                keywords: vec!["rust".to_owned(), "ownership".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.7,
            })
            .unwrap();

        store
            .create_note(NewMemoryNote {
                id: "primary-note".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis memory architecture".to_owned(),
                keywords: vec!["genesis".to_owned(), "memory".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["linked-note".to_owned()],
                importance: 1.0,
            })
            .unwrap();

        db_path
    }

    #[test]
    fn recall_returns_linked_notes_with_primary_result() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_linked_memory_fixture(dir.path());
        let store = MemoryStore::new(&db_path);

        let results = store.graph_search("Genesis", 5).unwrap();
        let ids: Vec<String> = results.into_iter().map(|item| item.memory.id).collect();
        assert!(ids.contains(&"primary-note".to_owned()));
        assert!(ids.contains(&"linked-note".to_owned()));
    }

    fn seed_hybrid_search_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let db_path = dir.join("hybrid.db");
        bootstrap(&db_path).expect("bootstrap");
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )
        .unwrap();

        let rows = [
            (
                "mem-hybrid-best",
                "fact",
                "genesis memory handbook",
                [1.0_f32, 0.0, 0.0, 0.0],
            ),
            (
                "mem-fts-only",
                "fact",
                "genesis memory archive",
                [0.0_f32, 1.0, 0.0, 0.0],
            ),
            (
                "mem-vector-only",
                "fact",
                "semantic retrieval note",
                [0.95_f32, 0.05, 0.0, 0.0],
            ),
        ];

        for (id, kind, content, _) in rows {
            conn.execute(
                "INSERT INTO memories (id, session_id, kind, content, created_at)
                 VALUES (?1, 's1', ?2, ?3, CURRENT_TIMESTAMP)",
                rusqlite::params![id, kind, content],
            )
            .unwrap();
            let rowid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, ?2, ?3)",
                rusqlite::params![rowid, kind, content],
            )
            .unwrap();
        }
        drop(conn);

        let embeddings = EmbeddingStore::new(&db_path);
        embeddings
            .store("mem-hybrid-best", &[1.0, 0.0, 0.0, 0.0], "local-384")
            .unwrap();
        embeddings
            .store("mem-fts-only", &[0.0, 1.0, 0.0, 0.0], "local-384")
            .unwrap();
        embeddings
            .store("mem-vector-only", &[0.95, 0.05, 0.0, 0.0], "local-384")
            .unwrap();

        db_path
    }

    #[test]
    fn hybrid_search_prefers_multi_signal_matches() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_hybrid_search_fixture(dir.path());
        let store = MemoryStore::new(&db_path);

        let results = store
            .hybrid_search("genesis memory", &[1.0, 0.0, 0.0, 0.0], 3)
            .unwrap();

        assert_eq!(results[0].memory.id, "mem-hybrid-best");
        assert_eq!(results[0].source, "hybrid");
    }

    #[test]
    fn vector_search_returns_empty_when_query_dimensions_drift() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_hybrid_search_fixture(dir.path());
        let store = MemoryStore::new(&db_path);

        let results = store
            .vector_search(&[1.0, 0.0, 0.0], 3)
            .expect("dimension drift should degrade to empty results");

        assert!(results.is_empty());
    }

    #[test]
    fn delete_removes_memory_from_vector_index() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_hybrid_search_fixture(dir.path());
        let store = MemoryStore::new(&db_path);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM memories WHERE id = 'mem-vector-only'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        assert!(store.delete("mem-vector-only").unwrap());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_vec WHERE memory_rowid = ?1",
                rusqlite::params![rowid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn delete_removes_memory_from_fts_index() {
        let dir = tempdir().expect("tempdir");
        let db_path = seed_hybrid_search_fixture(dir.path());
        let store = MemoryStore::new(&db_path);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM memories WHERE id = 'mem-vector-only'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        assert!(store.delete("mem-vector-only").unwrap());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_search WHERE memory_row_id = ?1",
                rusqlite::params![rowid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    #[ignore = "benchmark harness for local performance checks"]
    fn hybrid_search_benchmark_10k_memories() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("benchmark.db");
        bootstrap(&db_path).expect("bootstrap");

        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();

        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )
        .unwrap();

        let tx = conn.transaction().unwrap();
        tx.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vec USING vec0(
                memory_rowid integer primary key,
                embedding float[4] distance_metric=cosine
            );",
            [],
        )
        .unwrap();

        for i in 0..10_000 {
            let id = format!("mem-{i}");
            let content = if i % 200 == 0 {
                format!("genesis memory note {i}")
            } else {
                format!("background semantic note {i}")
            };
            tx.execute(
                "INSERT INTO memories (id, session_id, kind, content, created_at)
                 VALUES (?1, 's1', 'fact', ?2, CURRENT_TIMESTAMP)",
                rusqlite::params![id, content],
            )
            .unwrap();
            let rowid = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, 'fact', ?2)",
                rusqlite::params![rowid, content],
            )
            .unwrap();

            let embedding = if i % 200 == 0 {
                [1.0_f32, 0.0, 0.0, 0.0]
            } else {
                [0.0_f32, 1.0, 0.0, 0.0]
            };
            let blob = crate::stores::embedding::embedding_to_blob(&embedding);
            tx.execute(
                "INSERT INTO memory_embeddings (memory_id, embedding, model, dimensions, created_at)
                 VALUES (?1, ?2, 'benchmark-model', 4, CURRENT_TIMESTAMP)",
                rusqlite::params![id, &blob],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO memory_vec (memory_rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![rowid, &blob],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let store = MemoryStore::new(&db_path);
        let started = std::time::Instant::now();
        let results = store
            .hybrid_search("genesis memory", &[1.0, 0.0, 0.0, 0.0], 10)
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(results.len(), 10);
        println!(
            "hybrid_search_benchmark_10k_memories: {:?} for {} results",
            elapsed,
            results.len()
        );
    }

    #[test]
    fn list_paginated_returns_total_and_windowed_results() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        for i in 0..5 {
            store
                .create(Some("s1"), "fact", &format!("memory number {i}"))
                .unwrap();
        }

        let (page, total) = store.list_paginated(2, 0).expect("first page");
        assert_eq!(total, 5);
        assert_eq!(page.len(), 2);

        let (page2, total2) = store.list_paginated(2, 2).expect("second page");
        assert_eq!(total2, 5);
        assert_eq!(page2.len(), 2);
        assert_ne!(page[0].id, page2[0].id);

        let (page3, _) = store.list_paginated(10, 5).expect("beyond end");
        assert!(page3.is_empty());
    }

    #[test]
    fn create_link_with_edge_type_stores_type_and_weight() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        store
            .create_note(NewMemoryNote {
                id: "src".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "source note".to_owned(),
                keywords: vec![],
                tags: vec![],
                linked_ids: vec![],
                importance: 0.5,
            })
            .unwrap();
        store
            .create_note(NewMemoryNote {
                id: "tgt".to_owned(),
                session_id: Some("s1".to_owned()),
                kind: "fact".to_owned(),
                content: "target note".to_owned(),
                keywords: vec![],
                tags: vec![],
                linked_ids: vec![],
                importance: 0.5,
            })
            .unwrap();

        store
            .create_link("src", "tgt", "causal", 0.9)
            .expect("create_link should succeed");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let (edge_type, weight): (String, f64) = conn
            .query_row(
                "SELECT edge_type, weight FROM memory_links
                 WHERE source_id = 'src' AND target_id = 'tgt'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("link row should exist");
        assert_eq!(edge_type, "causal");
        assert!((weight - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn graph_search_weights_causal_edges_higher_than_entity() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");
        let sessions = SessionStore::new(&db_path);
        sessions.create_session("s1", "test", None).unwrap();
        let store = MemoryStore::new(&db_path);

        // Create FTS index so search works.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )
        .unwrap();

        // Primary memory — will be the FTS hit.
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at, importance, accessed_at)
             VALUES ('primary', 's1', 'fact', 'genesis architecture overview', CURRENT_TIMESTAMP, 1.0, CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        let primary_rowid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, 'fact', 'genesis architecture overview')",
            rusqlite::params![primary_rowid],
        )
        .unwrap();

        // Causal linked memory.
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at, importance, accessed_at)
             VALUES ('causal-linked', 's1', 'fact', 'causal consequence', CURRENT_TIMESTAMP, 1.0, CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();

        // Entity linked memory.
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at, importance, accessed_at)
             VALUES ('entity-linked', 's1', 'fact', 'entity relation', CURRENT_TIMESTAMP, 1.0, CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();

        // Insert links with typed edges.
        conn.execute(
            "INSERT INTO memory_links (source_id, target_id, edge_type, weight)
             VALUES ('primary', 'causal-linked', 'causal', 1.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_links (source_id, target_id, edge_type, weight)
             VALUES ('primary', 'entity-linked', 'entity', 1.0)",
            [],
        )
        .unwrap();
        drop(conn);

        let results = store.graph_search("genesis architecture", 10).unwrap();
        let ids: Vec<&str> = results.iter().map(|r| r.memory.id.as_str()).collect();
        assert!(ids.contains(&"causal-linked"), "causal-linked should appear in results");
        assert!(ids.contains(&"entity-linked"), "entity-linked should appear in results");

        // Causal (0.9 multiplier) should score higher than entity (0.6 multiplier).
        let causal_score = results
            .iter()
            .find(|r| r.memory.id == "causal-linked")
            .unwrap()
            .score;
        let entity_score = results
            .iter()
            .find(|r| r.memory.id == "entity-linked")
            .unwrap()
            .score;
        assert!(
            causal_score > entity_score,
            "causal ({causal_score}) should score higher than entity ({entity_score})"
        );
    }
}
