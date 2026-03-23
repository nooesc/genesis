use std::path::Path;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::util::collect_rows;
use crate::Database;

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

/// A single audit log entry representing an agent action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub session_id: Option<String>,
    pub event_type: String,
    pub details: String,
    pub created_at: String,
}

/// Tool usage analytics derived from audit log data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAnalytics {
    pub tool_name: String,
    pub call_count: i64,
    pub success_count: i64,
    pub avg_duration_ms: f64,
}

/// LLM usage analytics derived from audit log data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmAnalytics {
    pub model: String,
    pub call_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

/// SQLite-backed audit log for tracking agent actions.
///
/// Records tool calls, LLM requests, config changes, and other security-relevant
/// events with structured JSON details. Supports querying by session, event type,
/// and time range.
pub struct AuditLogStore {
    db: Database,
}

impl AuditLogStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Map a database row to an `AuditEntry`.
    ///
    /// Expects columns in order: id, session_id, event_type, details, created_at.
    fn row_to_audit_entry(row: &rusqlite::Row) -> rusqlite::Result<AuditEntry> {
        Ok(AuditEntry {
            id: row.get(0)?,
            session_id: row.get(1)?,
            event_type: row.get(2)?,
            details: row.get(3)?,
            created_at: row.get(4)?,
        })
    }

    /// Record an audit event.
    pub fn log(
        &self,
        session_id: Option<&str>,
        event_type: &str,
        details: &serde_json::Value,
    ) -> Result<i64, StorageError> {
        let connection = self.db.conn()?;
        let details_str = details.to_string();
        connection
            .execute(
                "INSERT INTO audit_log (session_id, event_type, details)
                 VALUES (?1, ?2, ?3)",
                params![session_id, event_type, details_str],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(connection.last_insert_rowid())
    }

    /// Query audit entries for a specific session.
    pub fn by_session(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, event_type, details, created_at
                 FROM audit_log
                 WHERE session_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![session_id, limit as i64], Self::row_to_audit_entry)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Query audit entries by event type.
    pub fn by_event_type(
        &self,
        event_type: &str,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, event_type, details, created_at
                 FROM audit_log
                 WHERE event_type = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![event_type, limit as i64], Self::row_to_audit_entry)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Query recent audit entries across all sessions.
    pub fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, event_type, details, created_at
                 FROM audit_log
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64], Self::row_to_audit_entry)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Count total audit entries and entries per event type.
    pub fn stats(&self) -> Result<Vec<(String, i64)>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT event_type, COUNT(*) as cnt
                 FROM audit_log
                 GROUP BY event_type
                 ORDER BY cnt DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Aggregate tool usage analytics from tool_call_end audit events.
    ///
    /// Returns a list of (tool_name, call_count, success_count, avg_duration_ms)
    /// sorted by call count descending.
    pub fn tool_analytics(&self, days: u32) -> Result<Vec<ToolAnalytics>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT
                    json_extract(details, '$.tool') as tool_name,
                    COUNT(*) as calls,
                    SUM(CASE WHEN json_extract(details, '$.success') = 1 THEN 1 ELSE 0 END) as successes,
                    AVG(CAST(json_extract(details, '$.duration_ms') AS REAL)) as avg_dur
                 FROM audit_log
                 WHERE event_type = 'tool_call_end'
                   AND created_at >= datetime('now', '-' || ?1 || ' days')
                   AND json_extract(details, '$.tool') IS NOT NULL
                 GROUP BY tool_name
                 ORDER BY calls DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![days], |row| {
                Ok(ToolAnalytics {
                    tool_name: row.get(0)?,
                    call_count: row.get(1)?,
                    success_count: row.get(2)?,
                    avg_duration_ms: row.get(3)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Aggregate LLM usage analytics from llm_response audit events.
    ///
    /// Returns per-model token usage totals.
    pub fn llm_analytics(&self, days: u32) -> Result<Vec<LlmAnalytics>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT
                    json_extract(details, '$.model') as model,
                    COUNT(*) as calls,
                    SUM(CAST(json_extract(details, '$.input_tokens') AS INTEGER)) as total_input,
                    SUM(CAST(json_extract(details, '$.output_tokens') AS INTEGER)) as total_output
                 FROM audit_log
                 WHERE event_type = 'llm_response'
                   AND created_at >= datetime('now', '-' || ?1 || ' days')
                   AND json_extract(details, '$.model') IS NOT NULL
                 GROUP BY model
                 ORDER BY calls DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![days], |row| {
                Ok(LlmAnalytics {
                    model: row.get(0)?,
                    call_count: row.get(1)?,
                    total_input_tokens: row.get(2)?,
                    total_output_tokens: row.get(3)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Delete audit entries older than the given number of days.
    pub fn purge_older_than(&self, days: u32) -> Result<u64, StorageError> {
        let connection = self.db.conn()?;
        let deleted = connection
            .execute(
                "DELETE FROM audit_log WHERE created_at < datetime('now', '-' || ?1 || ' days')",
                params![days],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(deleted as u64)
    }
}

#[cfg(test)]
mod audit_log_store_tests {
    use super::AuditLogStore;
    use crate::bootstrap;
    use tempfile::tempdir;

    #[test]
    fn log_and_retrieve_recent_entries() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details_a = serde_json::json!({"tool": "shell", "command": "ls"});
        let details_b = serde_json::json!({"tool": "read_file", "path": "/tmp/x"});
        let details_c = serde_json::json!({"model": "gpt-4", "tokens": 100});

        store
            .log(Some("s1"), "tool_call_start", &details_a)
            .expect("first log should succeed");
        store
            .log(Some("s1"), "tool_call_end", &details_b)
            .expect("second log should succeed");
        store
            .log(Some("s2"), "llm_response", &details_c)
            .expect("third log should succeed");

        let recent = store.recent(10).expect("recent should succeed");

        assert_eq!(recent.len(), 3);
        // Most recent first (ORDER BY id DESC)
        assert_eq!(recent[0].event_type, "llm_response");
        assert_eq!(recent[1].event_type, "tool_call_end");
        assert_eq!(recent[2].event_type, "tool_call_start");
    }

    #[test]
    fn log_and_filter_by_event_type() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details = serde_json::json!({"info": "test"});

        store
            .log(Some("s1"), "tool_call_start", &details)
            .expect("log should succeed");
        store
            .log(Some("s1"), "llm_response", &details)
            .expect("log should succeed");
        store
            .log(Some("s2"), "tool_call_start", &details)
            .expect("log should succeed");
        store
            .log(Some("s2"), "config_change", &details)
            .expect("log should succeed");

        let tool_starts = store
            .by_event_type("tool_call_start", 10)
            .expect("by_event_type should succeed");

        assert_eq!(tool_starts.len(), 2);
        for entry in &tool_starts {
            assert_eq!(entry.event_type, "tool_call_start");
        }

        let llm = store
            .by_event_type("llm_response", 10)
            .expect("by_event_type should succeed");
        assert_eq!(llm.len(), 1);
        assert_eq!(llm[0].event_type, "llm_response");

        let missing = store
            .by_event_type("nonexistent", 10)
            .expect("by_event_type should succeed for missing type");
        assert!(missing.is_empty());
    }

    #[test]
    fn log_and_filter_by_session() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details = serde_json::json!({"action": "test"});

        store
            .log(Some("session-a"), "tool_call_start", &details)
            .expect("log should succeed");
        store
            .log(Some("session-a"), "tool_call_end", &details)
            .expect("log should succeed");
        store
            .log(Some("session-b"), "llm_response", &details)
            .expect("log should succeed");
        store
            .log(None, "config_change", &details)
            .expect("log with no session should succeed");

        let session_a = store
            .by_session("session-a", 10)
            .expect("by_session should succeed");
        assert_eq!(session_a.len(), 2);
        for entry in &session_a {
            assert_eq!(entry.session_id.as_deref(), Some("session-a"));
        }

        let session_b = store
            .by_session("session-b", 10)
            .expect("by_session should succeed");
        assert_eq!(session_b.len(), 1);
        assert_eq!(session_b[0].session_id.as_deref(), Some("session-b"));

        let session_c = store
            .by_session("session-c", 10)
            .expect("by_session should succeed for missing session");
        assert!(session_c.is_empty());
    }

    #[test]
    fn stats_counts_by_event_type() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details = serde_json::json!({"x": 1});

        // 3 tool_call_start, 2 llm_response, 1 config_change
        for _ in 0..3 {
            store
                .log(Some("s1"), "tool_call_start", &details)
                .expect("log should succeed");
        }
        for _ in 0..2 {
            store
                .log(Some("s1"), "llm_response", &details)
                .expect("log should succeed");
        }
        store
            .log(Some("s1"), "config_change", &details)
            .expect("log should succeed");

        let stats = store.stats().expect("stats should succeed");

        assert_eq!(stats.len(), 3);

        // Stats are ordered by count descending
        let stats_map: std::collections::HashMap<&str, i64> =
            stats.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(stats_map.get("tool_call_start"), Some(&3));
        assert_eq!(stats_map.get("llm_response"), Some(&2));
        assert_eq!(stats_map.get("config_change"), Some(&1));
    }

    #[test]
    fn purge_older_than_removes_old_entries() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = AuditLogStore::new(&database_path);

        let details = serde_json::json!({"purge": "test"});

        // Insert an entry and then manually backdate it via raw SQL
        store
            .log(Some("s1"), "old_event", &details)
            .expect("log should succeed");

        // Insert a recent entry
        store
            .log(Some("s1"), "recent_event", &details)
            .expect("log should succeed");

        // Backdate the first entry to 60 days ago
        let conn =
            rusqlite::Connection::open(&database_path).expect("open connection should succeed");
        conn.execute(
            "UPDATE audit_log SET created_at = datetime('now', '-60 days')
             WHERE event_type = 'old_event'",
            [],
        )
        .expect("backdate should succeed");

        // Purge entries older than 30 days
        let deleted = store
            .purge_older_than(30)
            .expect("purge_older_than should succeed");
        assert_eq!(deleted, 1);

        // Only the recent entry should remain
        let remaining = store.recent(10).expect("recent should succeed");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event_type, "recent_event");
    }
}
