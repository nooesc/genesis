use std::collections::HashMap;
use std::path::Path;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::util::collect_rows;
use crate::Database;

/// A message on the agent bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Unique message ID.
    pub id: String,
    /// Channel this message was published to.
    pub channel: String,
    /// Sender agent/session ID.
    pub sender: String,
    /// Message type for routing and filtering.
    pub kind: MessageKind,
    /// The message payload.
    pub payload: String,
    /// Optional structured metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

/// Type of message on the bus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Free-form text message.
    Text,
    /// Structured JSON data.
    Json,
    /// A task delegation request.
    TaskRequest,
    /// Result of a completed task.
    TaskResult,
    /// Error or failure notification.
    Error,
    /// Agent lifecycle event (started, stopped, etc.)
    Lifecycle,
}

/// Persistent storage for agent bus messages.
///
/// The table is created lazily via `CREATE TABLE IF NOT EXISTS` on first
/// access, so it works regardless of whether the main schema bootstrap has
/// been run.
pub struct AgentBusStore {
    db: Database,
}

impl AgentBusStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Ensure the agent_bus_messages table and its indexes exist.
    ///
    /// Uses `CREATE TABLE IF NOT EXISTS`, so it is safe to call repeatedly.
    /// Callers should invoke this once before first use.
    pub fn ensure_table(&self) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS agent_bus_messages (
                    id TEXT PRIMARY KEY,
                    channel TEXT NOT NULL,
                    sender TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    metadata TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE INDEX IF NOT EXISTS idx_bus_messages_channel
                    ON agent_bus_messages(channel);
                CREATE INDEX IF NOT EXISTS idx_bus_messages_sender
                    ON agent_bus_messages(sender);
                CREATE INDEX IF NOT EXISTS idx_bus_messages_created_at
                    ON agent_bus_messages(created_at);",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<AgentMessage> {
        let kind_str: String = row.get(3)?;
        let metadata_str: String = row.get(5)?;
        Ok(AgentMessage {
            id: row.get(0)?,
            channel: row.get(1)?,
            sender: row.get(2)?,
            kind: serde_json::from_value(serde_json::Value::String(kind_str))
                .unwrap_or(MessageKind::Text),
            payload: row.get(4)?,
            metadata: serde_json::from_str(&metadata_str).unwrap_or_default(),
            timestamp: row.get(6)?,
        })
    }

    /// Store a message.
    pub fn store_message(&self, message: &AgentMessage) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        let metadata_json =
            serde_json::to_string(&message.metadata).unwrap_or_else(|_| "{}".to_owned());
        let kind_str = serde_json::to_string(&message.kind)
            .unwrap_or_else(|_| "\"text\"".to_owned())
            .trim_matches('"')
            .to_owned();
        connection
            .execute(
                "INSERT OR REPLACE INTO agent_bus_messages (id, channel, sender, kind, payload, metadata, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    message.id,
                    message.channel,
                    message.sender,
                    kind_str,
                    message.payload,
                    metadata_json,
                    message.timestamp,
                ],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Retrieve recent messages from a channel.
    pub fn channel_messages(
        &self,
        channel: &str,
        limit: usize,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, channel, sender, kind, payload, metadata, created_at
                 FROM agent_bus_messages
                 WHERE channel = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![channel, limit], Self::row_to_message)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Count messages per channel.
    pub fn channel_stats(&self) -> Result<Vec<(String, i64)>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT channel, COUNT(*) as cnt
                 FROM agent_bus_messages
                 GROUP BY channel
                 ORDER BY cnt DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// Purge messages older than N days.
    pub fn purge_older_than(&self, days: u32) -> Result<usize, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "DELETE FROM agent_bus_messages
                 WHERE created_at < datetime('now', ?1)",
                params![format!("-{days} days")],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows)
    }
}

#[cfg(test)]
impl AgentBusStore {
    /// Get all messages from a sender (test-only helper).
    pub fn sender_messages(
        &self,
        sender: &str,
        limit: usize,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, channel, sender, kind, payload, metadata, created_at
                 FROM agent_bus_messages
                 WHERE sender = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![sender, limit], Self::row_to_message)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a store with the table bootstrapped for testing.
    fn test_store(db_path: &Path) -> AgentBusStore {
        let store = AgentBusStore::new(db_path);
        store.ensure_table().expect("ensure_table");
        store
    }

    #[test]
    fn store_and_retrieve_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("bus.db");

        let store = test_store(&db_path);

        let msg = AgentMessage {
            id: "persist-1".into(),
            channel: "logs".into(),
            sender: "agent-x".into(),
            kind: MessageKind::Text,
            payload: "important log".into(),
            metadata: HashMap::new(),
            timestamp: "2025-06-01T12:00:00Z".into(),
        };
        store.store_message(&msg).unwrap();

        let messages = store.channel_messages("logs", 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload, "important log");
        assert_eq!(messages[0].sender, "agent-x");
    }

    #[test]
    fn channel_stats_counts_by_channel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("bus.db");

        let store = test_store(&db_path);

        for i in 0..5 {
            store
                .store_message(&AgentMessage {
                    id: format!("a-{i}"),
                    channel: "alpha".into(),
                    sender: "s".into(),
                    kind: MessageKind::Text,
                    payload: "msg".into(),
                    metadata: HashMap::new(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                })
                .unwrap();
        }
        for i in 0..3 {
            store
                .store_message(&AgentMessage {
                    id: format!("b-{i}"),
                    channel: "beta".into(),
                    sender: "s".into(),
                    kind: MessageKind::Text,
                    payload: "msg".into(),
                    metadata: HashMap::new(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                })
                .unwrap();
        }

        let stats = store.channel_stats().unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0], ("alpha".to_owned(), 5));
        assert_eq!(stats[1], ("beta".to_owned(), 3));
    }

    #[test]
    fn sender_messages_filters_by_sender() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("bus.db");

        let store = test_store(&db_path);

        store
            .store_message(&AgentMessage {
                id: "s1".into(),
                channel: "ch".into(),
                sender: "agent-a".into(),
                kind: MessageKind::Text,
                payload: "from a".into(),
                metadata: HashMap::new(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            })
            .unwrap();
        store
            .store_message(&AgentMessage {
                id: "s2".into(),
                channel: "ch".into(),
                sender: "agent-b".into(),
                kind: MessageKind::TaskResult,
                payload: "from b".into(),
                metadata: HashMap::new(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            })
            .unwrap();

        let msgs = store.sender_messages("agent-a", 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, "from a");
    }

    #[test]
    fn message_kind_roundtrip() {
        let kinds = vec![
            MessageKind::Text,
            MessageKind::Json,
            MessageKind::TaskRequest,
            MessageKind::TaskResult,
            MessageKind::Error,
            MessageKind::Lifecycle,
        ];
        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            let parsed: MessageKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, parsed);
        }
    }
}
