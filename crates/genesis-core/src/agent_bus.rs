//! Agent-to-agent message bus for multi-agent communication.
//!
//! Provides a publish-subscribe message bus that enables agents to send
//! messages, subscribe to topics, and communicate results across agent
//! boundaries. Built on top of the existing subagent infrastructure.
//!
//! ## Architecture
//!
//! - **Channels**: Named message queues that agents can publish to and subscribe from
//! - **Messages**: Typed payloads (text, JSON, result, error) with metadata
//! - **Subscriptions**: Agents register interest in specific channels
//! - **Persistence**: All messages stored in SQLite for durability and replay

pub use genesis_storage::{AgentBusStore, StoredBusMessage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::broadcast;

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

impl AgentMessage {
    /// Convert to storage representation.
    fn to_stored(&self) -> StoredBusMessage {
        let kind_str = serde_json::to_string(&self.kind)
            .unwrap_or_else(|_| "\"text\"".to_owned())
            .trim_matches('"')
            .to_owned();
        StoredBusMessage {
            id: self.id.clone(),
            channel: self.channel.clone(),
            sender: self.sender.clone(),
            kind: kind_str,
            payload: self.payload.clone(),
            metadata: self.metadata.clone(),
            timestamp: self.timestamp.clone(),
        }
    }

    /// Convert from storage representation.
    fn from_stored(stored: StoredBusMessage) -> Self {
        Self {
            id: stored.id,
            channel: stored.channel,
            sender: stored.sender,
            kind: serde_json::from_value(serde_json::Value::String(stored.kind))
                .unwrap_or(MessageKind::Text),
            payload: stored.payload,
            metadata: stored.metadata,
            timestamp: stored.timestamp,
        }
    }
}

/// In-memory agent message bus with broadcast channels.
pub struct AgentBus {
    channels: tokio::sync::RwLock<HashMap<String, broadcast::Sender<AgentMessage>>>,
    /// SQLite path for persistence (optional).
    database_path: Option<std::path::PathBuf>,
}

impl AgentBus {
    /// Create a new agent bus.
    pub fn new() -> Self {
        Self {
            channels: tokio::sync::RwLock::new(HashMap::new()),
            database_path: None,
        }
    }

    /// Create a bus with SQLite persistence.
    pub fn with_persistence(database_path: impl Into<std::path::PathBuf>) -> Self {
        let db_path = database_path.into();
        let store = AgentBusStore::new(&db_path);
        if let Err(e) = store.ensure_table() {
            tracing::warn!(error = %e, "failed to initialize agent_bus_messages table");
        }
        Self {
            channels: tokio::sync::RwLock::new(HashMap::new()),
            database_path: Some(db_path),
        }
    }

    /// Publish a message to a channel.
    ///
    /// Creates the channel if it doesn't exist. Returns the number of
    /// active subscribers that received the message.
    pub async fn publish(&self, message: AgentMessage) -> usize {
        // Persist to SQLite if configured
        if let Some(ref db_path) = self.database_path {
            let store = AgentBusStore::new(db_path);
            if let Err(e) = store.store_message(&message.to_stored()) {
                tracing::warn!(error = %e, channel = %message.channel, "failed to persist bus message");
            }
        }

        let channels = self.channels.read().await;
        if let Some(sender) = channels.get(&message.channel) {
            sender.send(message).unwrap_or(0)
        } else {
            drop(channels);
            // Create the channel and send
            let mut channels = self.channels.write().await;
            let (sender, _) = broadcast::channel(256);
            let count = sender.send(message.clone()).unwrap_or(0);
            channels.insert(message.channel.clone(), sender);
            count
        }
    }

    /// Subscribe to a channel. Returns a receiver for incoming messages.
    pub async fn subscribe(&self, channel: &str) -> broadcast::Receiver<AgentMessage> {
        let channels = self.channels.read().await;
        if let Some(sender) = channels.get(channel) {
            return sender.subscribe();
        }
        drop(channels);

        let mut channels = self.channels.write().await;
        // Double-check after acquiring write lock
        if let Some(sender) = channels.get(channel) {
            return sender.subscribe();
        }
        let (sender, receiver) = broadcast::channel(256);
        channels.insert(channel.to_owned(), sender);
        receiver
    }

    /// List active channels.
    pub async fn channels(&self) -> Vec<String> {
        self.channels.read().await.keys().cloned().collect()
    }

    /// Get recent messages from a channel (from persistence).
    pub fn history(&self, channel: &str, limit: usize) -> Vec<AgentMessage> {
        if let Some(ref db_path) = self.database_path {
            let store = AgentBusStore::new(db_path);
            store
                .channel_messages(channel, limit)
                .unwrap_or_default()
                .into_iter()
                .map(AgentMessage::from_stored)
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get message counts per channel (from persistence).
    pub fn stats(&self) -> Vec<(String, i64)> {
        if let Some(ref db_path) = self.database_path {
            let store = AgentBusStore::new(db_path);
            store.channel_stats().unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// Send a task delegation request to a channel and return immediately.
    pub async fn delegate_task(
        &self,
        channel: &str,
        sender: &str,
        task: &str,
        metadata: HashMap<String, String>,
    ) -> String {
        let id = format!("task-{}", uuid_v4());
        let message = AgentMessage {
            id: id.clone(),
            channel: channel.to_owned(),
            sender: sender.to_owned(),
            kind: MessageKind::TaskRequest,
            payload: task.to_owned(),
            metadata,
            timestamp: now_iso8601(),
        };
        self.publish(message).await;
        id
    }

    /// Publish a task result back to the channel.
    pub async fn complete_task(
        &self,
        channel: &str,
        sender: &str,
        task_id: &str,
        result: &str,
    ) -> usize {
        let message = AgentMessage {
            id: format!("result-{}", uuid_v4()),
            channel: channel.to_owned(),
            sender: sender.to_owned(),
            kind: MessageKind::TaskResult,
            payload: result.to_owned(),
            metadata: HashMap::from([("task_id".to_owned(), task_id.to_owned())]),
            timestamp: now_iso8601(),
        };
        self.publish(message).await
    }
}

impl Default for AgentBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Simple UUID v4 generator (no dependency needed).
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let random = nanos ^ (std::process::id() as u128) ^ (rand_bits() as u128);
    format!("{:032x}", random)
}

fn rand_bits() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish()
}

fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn agent_message_serializes() {
        let msg = AgentMessage {
            id: "msg-1".into(),
            channel: "tasks".into(),
            sender: "agent-a".into(),
            kind: MessageKind::TaskRequest,
            payload: "build the feature".into(),
            metadata: HashMap::from([("priority".into(), "high".into())]),
            timestamp: "2025-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["channel"], "tasks");
        assert_eq!(json["kind"], "task_request");
        assert_eq!(json["metadata"]["priority"], "high");
    }

    #[tokio::test]
    async fn bus_publish_subscribe() {
        let bus = AgentBus::new();
        let mut rx = bus.subscribe("test-channel").await;

        let msg = AgentMessage {
            id: "m1".into(),
            channel: "test-channel".into(),
            sender: "agent-1".into(),
            kind: MessageKind::Text,
            payload: "hello".into(),
            metadata: HashMap::new(),
            timestamp: now_iso8601(),
        };

        let count = bus.publish(msg.clone()).await;
        assert_eq!(count, 1);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.id, "m1");
        assert_eq!(received.payload, "hello");
    }

    #[tokio::test]
    async fn bus_multiple_subscribers() {
        let bus = AgentBus::new();
        let mut rx1 = bus.subscribe("multi").await;
        let mut rx2 = bus.subscribe("multi").await;

        let msg = AgentMessage {
            id: "m2".into(),
            channel: "multi".into(),
            sender: "sender".into(),
            kind: MessageKind::Json,
            payload: r#"{"key": "value"}"#.into(),
            metadata: HashMap::new(),
            timestamp: now_iso8601(),
        };

        let count = bus.publish(msg).await;
        assert_eq!(count, 2);

        let r1 = rx1.recv().await.unwrap();
        let r2 = rx2.recv().await.unwrap();
        assert_eq!(r1.id, r2.id);
    }

    #[tokio::test]
    async fn bus_delegate_and_complete() {
        let bus = AgentBus::new();
        let mut rx = bus.subscribe("workers").await;

        let task_id = bus
            .delegate_task(
                "workers",
                "manager",
                "compile the code",
                HashMap::from([("repo".into(), "genesis".into())]),
            )
            .await;
        assert!(task_id.starts_with("task-"));

        let request = rx.recv().await.unwrap();
        assert_eq!(request.kind, MessageKind::TaskRequest);
        assert_eq!(request.payload, "compile the code");
        assert_eq!(request.metadata["repo"], "genesis");

        // Worker completes the task
        bus.complete_task("workers", "worker-1", &task_id, "compilation succeeded")
            .await;

        let result = rx.recv().await.unwrap();
        assert_eq!(result.kind, MessageKind::TaskResult);
        assert_eq!(result.payload, "compilation succeeded");
        assert_eq!(result.metadata["task_id"], task_id);
    }

    #[tokio::test]
    async fn bus_channels_list() {
        let bus = AgentBus::new();
        bus.subscribe("alpha").await;
        bus.subscribe("beta").await;

        let mut channels = bus.channels().await;
        channels.sort();
        assert_eq!(channels, vec!["alpha", "beta"]);
    }

    /// Helper to create a test store with table initialized.
    fn test_store(db_path: &std::path::Path) -> AgentBusStore {
        let store = AgentBusStore::new(db_path);
        store.ensure_table().expect("ensure_table");
        store
    }

    /// Helper to create a `StoredBusMessage` from an `AgentMessage`.
    fn to_stored(msg: &AgentMessage) -> StoredBusMessage {
        msg.to_stored()
    }

    #[test]
    fn bus_store_persistence() {
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
        store.store_message(&to_stored(&msg)).unwrap();

        let messages = store.channel_messages("logs", 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload, "important log");
        assert_eq!(messages[0].sender, "agent-x");
    }

    #[test]
    fn bus_store_channel_stats() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("bus.db");

        let store = test_store(&db_path);

        for i in 0..5 {
            let msg = AgentMessage {
                id: format!("a-{i}"),
                channel: "alpha".into(),
                sender: "s".into(),
                kind: MessageKind::Text,
                payload: "msg".into(),
                metadata: HashMap::new(),
                timestamp: now_iso8601(),
            };
            store.store_message(&to_stored(&msg)).unwrap();
        }
        for i in 0..3 {
            let msg = AgentMessage {
                id: format!("b-{i}"),
                channel: "beta".into(),
                sender: "s".into(),
                kind: MessageKind::Text,
                payload: "msg".into(),
                metadata: HashMap::new(),
                timestamp: now_iso8601(),
            };
            store.store_message(&to_stored(&msg)).unwrap();
        }

        let stats = store.channel_stats().unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0], ("alpha".to_owned(), 5));
        assert_eq!(stats[1], ("beta".to_owned(), 3));
    }

    #[test]
    fn bus_store_sender_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("bus.db");

        let store = test_store(&db_path);

        let msg1 = AgentMessage {
            id: "s1".into(),
            channel: "ch".into(),
            sender: "agent-a".into(),
            kind: MessageKind::Text,
            payload: "from a".into(),
            metadata: HashMap::new(),
            timestamp: now_iso8601(),
        };
        store.store_message(&to_stored(&msg1)).unwrap();

        let msg2 = AgentMessage {
            id: "s2".into(),
            channel: "ch".into(),
            sender: "agent-b".into(),
            kind: MessageKind::TaskResult,
            payload: "from b".into(),
            metadata: HashMap::new(),
            timestamp: now_iso8601(),
        };
        store.store_message(&to_stored(&msg2)).unwrap();

        let msgs = store.sender_messages("agent-a", 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, "from a");
    }

    #[tokio::test]
    async fn bus_with_persistence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("bus.db");

        let bus = AgentBus::with_persistence(&db_path);
        let mut rx = bus.subscribe("persisted").await;

        let msg = AgentMessage {
            id: "pm-1".into(),
            channel: "persisted".into(),
            sender: "agent".into(),
            kind: MessageKind::Json,
            payload: r#"{"data": 42}"#.into(),
            metadata: HashMap::new(),
            timestamp: now_iso8601(),
        };

        bus.publish(msg).await;
        let received = rx.recv().await.unwrap();
        assert_eq!(received.id, "pm-1");

        // Verify persistence
        let history = bus.history("persisted", 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, "pm-1");
    }

    #[test]
    fn message_kind_serde_from_string() {
        let parse = |s: &str| -> MessageKind {
            serde_json::from_value(serde_json::Value::String(s.to_owned()))
                .unwrap_or(MessageKind::Text)
        };
        assert_eq!(parse("text"), MessageKind::Text);
        assert_eq!(parse("json"), MessageKind::Json);
        assert_eq!(parse("task_request"), MessageKind::TaskRequest);
        assert_eq!(parse("task_result"), MessageKind::TaskResult);
        assert_eq!(parse("error"), MessageKind::Error);
        assert_eq!(parse("lifecycle"), MessageKind::Lifecycle);
        assert_eq!(parse("unknown"), MessageKind::Text);
    }
}
