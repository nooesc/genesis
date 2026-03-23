use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::util::collect_rows;
use crate::Database;

/// A stored subagent — a child agent loop spawned by a parent session.
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

/// Subagent persistence layer.
pub struct SubagentStore {
    db: Database,
}

impl SubagentStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Create a new subagent record with status "pending".
    pub fn create(
        &self,
        id: &str,
        parent_session_id: &str,
        child_session_id: &str,
        name: &str,
        task: &str,
    ) -> Result<StoredSubagent, StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute(
                "INSERT INTO subagents (id, parent_session_id, child_session_id, name, task, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                params![id, parent_session_id, child_session_id, name, task],
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

    /// Get a subagent by ID.
    pub fn get(&self, id: &str) -> Result<Option<StoredSubagent>, StorageError> {
        let connection = self.db.conn()?;
        connection
            .query_row(
                "SELECT id, parent_session_id, child_session_id, name, task, status, result, error, created_at, completed_at
                 FROM subagents WHERE id = ?1",
                params![id],
                Self::row_to_subagent,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// List all subagents for a parent session.
    pub fn list_by_parent(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<StoredSubagent>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, parent_session_id, child_session_id, name, task, status, result, error, created_at, completed_at
                 FROM subagents WHERE parent_session_id = ?1
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![parent_session_id], Self::row_to_subagent)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Mark a subagent as running.
    pub fn set_running(&self, id: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "UPDATE subagents SET status = 'running' WHERE id = ?1",
                params![id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Mark a subagent as completed with its result.
    pub fn set_completed(&self, id: &str, result: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "UPDATE subagents SET status = 'completed', result = ?2, completed_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![id, result],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Mark a subagent as failed with an error message.
    pub fn set_failed(&self, id: &str, error: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "UPDATE subagents SET status = 'failed', error = ?2, completed_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![id, error],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
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

#[cfg(test)]
mod subagent_store_tests {
    use super::SubagentStore;
    use crate::bootstrap;
    use tempfile::tempdir;

    #[test]
    fn create_and_get_subagent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SubagentStore::new(&database_path);
        let sub = store
            .create(
                "sub-1",
                "parent-session-1",
                "child-session-1",
                "researcher",
                "find relevant papers",
            )
            .expect("create should succeed");

        assert_eq!(sub.id, "sub-1");
        assert_eq!(sub.parent_session_id, "parent-session-1");
        assert_eq!(sub.child_session_id, "child-session-1");
        assert_eq!(sub.name, "researcher");
        assert_eq!(sub.task, "find relevant papers");
        assert_eq!(sub.status, "pending");
        assert!(sub.result.is_none());
        assert!(sub.error.is_none());
        assert!(sub.completed_at.is_none());

        let fetched = store
            .get("sub-1")
            .expect("get should succeed")
            .expect("subagent should exist");
        assert_eq!(fetched.id, "sub-1");
        assert_eq!(fetched.name, "researcher");
        assert_eq!(fetched.status, "pending");
    }

    #[test]
    fn update_subagent_status() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SubagentStore::new(&database_path);
        store
            .create(
                "sub-2",
                "parent-1",
                "child-2",
                "coder",
                "implement feature X",
            )
            .expect("create should succeed");

        // Transition to running
        let updated = store
            .set_running("sub-2")
            .expect("set_running should succeed");
        assert!(updated);
        let sub = store
            .get("sub-2")
            .expect("get should succeed")
            .expect("subagent should exist");
        assert_eq!(sub.status, "running");
        assert!(sub.completed_at.is_none());

        // Transition to completed
        let completed = store
            .set_completed("sub-2", "feature X implemented successfully")
            .expect("set_completed should succeed");
        assert!(completed);
        let sub = store
            .get("sub-2")
            .expect("get should succeed")
            .expect("subagent should exist");
        assert_eq!(sub.status, "completed");
        assert_eq!(
            sub.result.as_deref(),
            Some("feature X implemented successfully")
        );
        assert!(sub.completed_at.is_some());

        // Create another subagent and transition to failed
        store
            .create("sub-3", "parent-1", "child-3", "tester", "run tests")
            .expect("create should succeed");
        let failed = store
            .set_failed("sub-3", "tests timed out")
            .expect("set_failed should succeed");
        assert!(failed);
        let sub = store
            .get("sub-3")
            .expect("get should succeed")
            .expect("subagent should exist");
        assert_eq!(sub.status, "failed");
        assert_eq!(sub.error.as_deref(), Some("tests timed out"));
        assert!(sub.completed_at.is_some());
    }

    #[test]
    fn list_by_parent_isolates_sessions() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SubagentStore::new(&database_path);

        // Create subagents under different parent sessions
        store
            .create("sub-a1", "parent-A", "child-a1", "worker-1", "task alpha-1")
            .expect("create should succeed");
        store
            .create("sub-a2", "parent-A", "child-a2", "worker-2", "task alpha-2")
            .expect("create should succeed");
        store
            .create("sub-b1", "parent-B", "child-b1", "worker-3", "task beta-1")
            .expect("create should succeed");

        // List for parent-A
        let parent_a_subs = store
            .list_by_parent("parent-A")
            .expect("list_by_parent should succeed");
        assert_eq!(parent_a_subs.len(), 2);
        assert!(parent_a_subs
            .iter()
            .all(|s| s.parent_session_id == "parent-A"));
        let names: Vec<&str> = parent_a_subs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"worker-1"));
        assert!(names.contains(&"worker-2"));

        // List for parent-B — should be isolated
        let parent_b_subs = store
            .list_by_parent("parent-B")
            .expect("list_by_parent should succeed");
        assert_eq!(parent_b_subs.len(), 1);
        assert_eq!(parent_b_subs[0].name, "worker-3");

        // List for non-existent parent — should be empty
        let empty = store
            .list_by_parent("parent-C")
            .expect("list_by_parent should succeed");
        assert!(empty.is_empty());
    }

    #[test]
    fn list_by_parent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SubagentStore::new(&database_path);

        // Same parent, different child sessions
        store
            .create(
                "sub-x1",
                "shared-parent",
                "child-x1",
                "analyzer",
                "analyze data",
            )
            .expect("create should succeed");
        store
            .create(
                "sub-x2",
                "shared-parent",
                "child-x2",
                "summarizer",
                "summarize results",
            )
            .expect("create should succeed");
        store
            .create(
                "sub-x3",
                "shared-parent",
                "child-x3",
                "reviewer",
                "review output",
            )
            .expect("create should succeed");

        let subs = store
            .list_by_parent("shared-parent")
            .expect("list_by_parent should succeed");
        assert_eq!(subs.len(), 3);
        assert!(subs.iter().all(|s| s.parent_session_id == "shared-parent"));

        // Verify each has a distinct child_session_id
        let child_ids: Vec<&str> = subs.iter().map(|s| s.child_session_id.as_str()).collect();
        assert!(child_ids.contains(&"child-x1"));
        assert!(child_ids.contains(&"child-x2"));
        assert!(child_ids.contains(&"child-x3"));

        // Verify all names are present (order may vary on fast machines
        // where created_at timestamps coincide).
        let names: Vec<&str> = subs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"analyzer"));
        assert!(names.contains(&"summarizer"));
        assert!(names.contains(&"reviewer"));
    }
}
