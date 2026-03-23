use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Database;
use crate::error::StorageError;
use crate::util::collect_rows;

/// A persisted conversation message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls_json: Option<String>,
    /// Whether this message is a delivery mirror (cross-platform visibility record).
    #[serde(default)]
    pub mirror: bool,
    /// Source label for mirrored messages (e.g. "cli", "telegram", "api").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_source: Option<String>,
    /// Provider-specific metadata (e.g. codex reasoning blobs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<String>,
    pub created_at: String,
}

/// Summary of a session returned from search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    pub platform: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub parent_session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A message search result — matching message with session context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageSearchResult {
    pub session_id: String,
    pub session_title: Option<String>,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

/// Aggregate token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageStats {
    pub total_sessions: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// Usage insights for a period — sessions per day, platform breakdown, etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InsightsData {
    pub period_days: u32,
    pub sessions_count: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    /// Sessions per day (date string → count).
    pub sessions_per_day: Vec<(String, u64)>,
    /// Sessions per platform (platform → count).
    pub platform_breakdown: Vec<(String, u64)>,
    /// Token usage per day (date, input_tokens, output_tokens).
    pub tokens_per_day: Vec<(String, u64, u64)>,
    /// Tool usage breakdown (tool_name → call count), sorted by frequency.
    pub tool_usage: Vec<(String, u64)>,
    /// Average input tokens per session.
    pub avg_input_tokens: u64,
    /// Average output tokens per session.
    pub avg_output_tokens: u64,
}


pub struct SessionStore {
    db: Database,
}

impl SessionStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Returns the database path this store is using.
    pub fn database_path(&self) -> &Path {
        self.db.path()
    }

    /// Map a database row to a `SessionSummary`.
    ///
    /// Expects columns in order: id, title, platform, total_input_tokens,
    /// total_output_tokens, parent_session_id, created_at, updated_at.
    fn row_to_session_summary(row: &rusqlite::Row) -> rusqlite::Result<SessionSummary> {
        Ok(SessionSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            platform: row.get(2)?,
            total_input_tokens: row.get(3)?,
            total_output_tokens: row.get(4)?,
            parent_session_id: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    /// Create a new session record.
    pub fn create_session(
        &self,
        id: &str,
        platform: &str,
        title: Option<&str>,
    ) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute(
                "INSERT INTO sessions (id, title, platform, created_at, updated_at)
                 VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
                params![id, title, platform],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Append a message to a session and index its content for search.
    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls_json: Option<&str>,
        provider_metadata: Option<&str>,
    ) -> Result<i64, StorageError> {
        let connection = self.db.conn()?;

        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls_json, provider_metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![session_id, role, content, tool_call_id, tool_calls_json, provider_metadata],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let message_id = connection.last_insert_rowid();

        // Index searchable content in FTS5
        if let Some(text) = content {
            if !text.is_empty() {
                connection
                    .execute(
                        "INSERT INTO session_search (session_id, content) VALUES (?1, ?2)",
                        params![session_id, text],
                    )
                    .map_err(|source| StorageError::Sqlite {
                        path: self.db.path().to_path_buf(),
                        source,
                    })?;
            }
        }

        // Touch session updated_at
        connection
            .execute(
                "UPDATE sessions SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        Ok(message_id)
    }

    /// Append a delivery-mirror message to a session's transcript.
    ///
    /// Mirror messages record what was sent to a platform, giving the agent
    /// cross-platform visibility into dispatched content. They are stored as
    /// assistant messages with `mirror = true` and a `mirror_source` label.
    pub fn append_mirror_message(
        &self,
        session_id: &str,
        content: &str,
        mirror_source: &str,
    ) -> Result<i64, StorageError> {
        let connection = self.db.conn()?;

        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, mirror, mirror_source, created_at)
                 VALUES (?1, 'assistant', ?2, 1, ?3, CURRENT_TIMESTAMP)",
                params![session_id, content, mirror_source],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let message_id = connection.last_insert_rowid();

        // Touch session updated_at
        connection
            .execute(
                "UPDATE sessions SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        Ok(message_id)
    }

    /// Find the most recently updated session for a given platform and chat ID.
    ///
    /// Platform handlers construct deterministic session IDs from platform + chat_id
    /// (e.g. `tg-12345`, `slack-C01ABC`, `discord-98765`, `wa-15551234`).
    /// This method looks up a session by its expected ID pattern.
    pub fn find_session_by_platform_chat_id(
        &self,
        platform: &str,
        chat_id: &str,
    ) -> Result<Option<SessionSummary>, StorageError> {
        let session_id = match platform {
            "telegram" => format!("tg-{chat_id}"),
            "slack" => format!("slack-{chat_id}"),
            "discord" => format!("discord-{chat_id}"),
            "whatsapp" => format!("wa-{chat_id}"),
            "homeassistant" => format!("ha-{chat_id}"),
            _ => format!("{platform}-{chat_id}"),
        };

        self.get_session(&session_id)
    }

    /// Atomically add token usage to a session's running totals.
    pub fn add_usage(
        &self,
        session_id: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<(), StorageError> {
        if input_tokens == 0 && output_tokens == 0 {
            return Ok(());
        }
        let connection = self.db.conn()?;
        connection
            .execute(
                "UPDATE sessions SET
                    total_input_tokens = total_input_tokens + ?2,
                    total_output_tokens = total_output_tokens + ?3,
                    updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                params![session_id, input_tokens as i64, output_tokens as i64],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Set the title on an existing session (only if currently null).
    pub fn update_title(&self, session_id: &str, title: &str) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute(
                "UPDATE sessions SET title = ?2, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1 AND title IS NULL",
                params![session_id, title],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Set the session title unconditionally (overwrites any existing title).
    pub fn set_title(&self, session_id: &str, title: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "UPDATE sessions SET title = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![session_id, title],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Get tags for a session (comma-separated string parsed into Vec).
    pub fn get_tags(&self, session_id: &str) -> Result<Vec<String>, StorageError> {
        let connection = self.db.conn()?;
        let tags: String = connection
            .query_row(
                "SELECT tags FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(if tags.is_empty() {
            Vec::new()
        } else {
            tags.split(',').map(|t| t.trim().to_owned()).collect()
        })
    }

    /// Set tags for a session (replaces existing tags).
    pub fn set_tags(&self, session_id: &str, tags: &[&str]) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let tags_str = tags.join(",");
        let rows = connection
            .execute(
                "UPDATE sessions SET tags = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![session_id, tags_str],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Add a tag to a session (no-op if already present).
    pub fn add_tag(&self, session_id: &str, tag: &str) -> Result<bool, StorageError> {
        let mut tags = self.get_tags(session_id)?;
        if tags.iter().any(|t| t == tag) {
            return Ok(false); // Already has this tag
        }
        tags.push(tag.to_owned());
        let tags_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
        self.set_tags(session_id, &tags_refs)
    }

    /// Remove a tag from a session.
    pub fn remove_tag(&self, session_id: &str, tag: &str) -> Result<bool, StorageError> {
        let tags = self.get_tags(session_id)?;
        let filtered: Vec<&str> = tags
            .iter()
            .filter(|t| t.as_str() != tag)
            .map(|t| t.as_str())
            .collect();
        if filtered.len() == tags.len() {
            return Ok(false); // Tag wasn't present
        }
        self.set_tags(session_id, &filtered)
    }

    /// List sessions that have a specific tag.
    pub fn sessions_by_tag(&self, tag: &str) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = self.db.conn()?;
        let pattern = format!("%{tag}%");
        let mut stmt = connection
            .prepare(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at
                 FROM sessions WHERE tags LIKE ?1
                 ORDER BY updated_at DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(params![pattern], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path())
    }

    /// List all sessions that were forked from a given parent session.
    pub fn list_children(&self, parent_id: &str) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens,
                        parent_session_id, created_at, updated_at
                 FROM sessions WHERE parent_session_id = ?1
                 ORDER BY created_at ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(params![parent_id], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path())
    }

    /// Delete a session and all its messages and search index entries.
    pub fn delete_session(&self, session_id: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        // Delete FTS entries (virtual table — no ON DELETE CASCADE support).
        connection
            .execute(
                "DELETE FROM session_search WHERE session_id = ?1",
                params![session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        // Delete session; ON DELETE CASCADE removes associated messages.
        let deleted = connection
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(deleted > 0)
    }

    /// Fork a session: create a new session that branches from the source
    /// session, copying all messages up to the current point. Returns the new
    /// session ID.
    pub fn fork_session(
        &self,
        source_session_id: &str,
        new_session_id: &str,
    ) -> Result<String, StorageError> {
        let connection = self.db.conn()?;

        // Get source session info
        let (platform, title): (String, Option<String>) = connection
            .query_row(
                "SELECT platform, title FROM sessions WHERE id = ?1",
                params![source_session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let fork_title = title
            .as_deref()
            .map(|t| format!("{t} (fork)"))
            .unwrap_or_else(|| "Fork".to_owned());

        // Create the new session with parent_session_id
        connection
            .execute(
                "INSERT INTO sessions (id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 0, 0, ?4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
                params![new_session_id, fork_title, platform, source_session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Copy all messages from the source session
        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls_json, mirror, mirror_source, created_at)
                 SELECT ?1, role, content, tool_call_id, tool_calls_json, mirror, mirror_source, created_at
                 FROM messages WHERE session_id = ?2
                 ORDER BY id ASC",
                params![new_session_id, source_session_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        Ok(new_session_id.to_owned())
    }

    /// Delete sessions (and their messages/FTS entries) older than `days` days.
    /// Returns the number of sessions deleted.
    pub fn purge_older_than(&self, days: u32) -> Result<u64, StorageError> {
        let connection = self.db.conn()?;
        let cutoff = format!("-{days} days");

        let tx = connection
            .unchecked_transaction()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Remove FTS entries (virtual table, no FK support).
        tx.execute(
            "DELETE FROM session_search WHERE session_id IN (
                 SELECT id FROM sessions WHERE created_at < datetime('now', ?1)
             )",
            params![cutoff],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;

        // Delete sessions; ON DELETE CASCADE handles messages.
        tx.execute(
            "DELETE FROM sessions WHERE created_at < datetime('now', ?1)",
            params![cutoff],
        )
        .map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;

        let deleted = tx.changes();

        tx.commit().map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;

        Ok(deleted)
    }

    /// Load all messages for a session in chronological order.
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>, StorageError> {
        let connection = self.db.conn()?;

        let mut stmt = connection
            .prepare(
                "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, mirror, mirror_source, provider_metadata, created_at
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY id ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    tool_call_id: row.get(4)?,
                    tool_calls_json: row.get(5)?,
                    mirror: row.get::<_, i64>(6)? != 0,
                    mirror_source: row.get(7)?,
                    provider_metadata: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Delete messages older than the N most recent for a session.
    /// Returns the number of messages deleted.
    pub fn truncate_messages(
        &self,
        session_id: &str,
        keep_recent: usize,
    ) -> Result<usize, StorageError> {
        let connection = self.db.conn()?;

        // Find the ID threshold: keep messages with the highest IDs.
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let to_delete = count as usize - keep_recent.min(count as usize);
        if to_delete == 0 {
            return Ok(0);
        }

        connection
            .execute(
                "DELETE FROM messages WHERE session_id = ?1 AND id IN (
                    SELECT id FROM messages WHERE session_id = ?1
                    ORDER BY id ASC LIMIT ?2
                )",
                params![session_id, to_delete],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        Ok(to_delete)
    }

    /// Delete the most recent `n` messages from a session (ordered by row ID).
    /// Returns the number of messages actually deleted.
    pub fn delete_last_n_messages(
        &self,
        session_id: &str,
        n: usize,
    ) -> Result<usize, StorageError> {
        if n == 0 {
            return Ok(0);
        }
        let connection = self.db.conn()?;
        let deleted = connection
            .execute(
                "DELETE FROM messages WHERE session_id = ?1 AND id IN (
                    SELECT id FROM messages WHERE session_id = ?1
                    ORDER BY id DESC LIMIT ?2
                )",
                params![session_id, n],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(deleted)
    }

    /// Full-text search across session content. Returns matching session IDs
    /// with their summaries, ordered by relevance.
    pub fn search_sessions(&self, query: &str) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = self.db.conn()?;

        let mut stmt = connection
            .prepare(
                "SELECT DISTINCT s.id, s.title, s.platform, s.total_input_tokens, s.total_output_tokens, s.parent_session_id, s.created_at, s.updated_at
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1
                 ORDER BY rank",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![query], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Search message content across all sessions using FTS5.
    ///
    /// Returns matching messages with their session context, ordered by
    /// relevance. Limited to `max_results` entries.
    pub fn search_messages(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<MessageSearchResult>, StorageError> {
        let connection = self.db.conn()?;

        let mut stmt = connection
            .prepare(
                "SELECT ss.session_id, s.title, ss.content
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![query, max_results as i64], |row| {
                Ok(MessageSearchResult {
                    session_id: row.get(0)?,
                    session_title: row.get(1)?,
                    role: String::new(), // FTS doesn't store role
                    content: row.get(2)?,
                    created_at: String::new(), // FTS doesn't store timestamp
                })
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Count total sessions.
    pub fn session_count(&self) -> Result<u64, StorageError> {
        let connection = self.db.conn()?;
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// Get a session summary by ID.
    pub fn get_session(&self, id: &str) -> Result<Option<SessionSummary>, StorageError> {
        let connection = self.db.conn()?;
        self.get_session_with_conn(&connection, id)
    }

    fn get_session_with_conn(
        &self,
        connection: &Connection,
        id: &str,
    ) -> Result<Option<SessionSummary>, StorageError> {
        connection
            .query_row(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id],
                Self::row_to_session_summary,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    pub fn list_recent_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at
                 FROM sessions
                 ORDER BY updated_at DESC, created_at DESC, id DESC
                 LIMIT ?1",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// List recent sessions with offset/limit pagination.
    pub fn list_recent_sessions_paginated(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<SessionSummary>, u64), StorageError> {
        let connection = self.db.conn()?;

        let total: i64 = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let mut stmt = connection
            .prepare(
                "SELECT id, title, platform, total_input_tokens, total_output_tokens, parent_session_id, created_at, updated_at
                 FROM sessions
                 ORDER BY updated_at DESC, created_at DESC, id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let items = collect_rows(rows, self.db.path())?;
        Ok((items, total as u64))
    }

    /// Search sessions with offset/limit pagination.
    pub fn search_sessions_paginated(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<SessionSummary>, u64), StorageError> {
        let connection = self.db.conn()?;

        let total: i64 = connection
            .query_row(
                "SELECT COUNT(DISTINCT s.id)
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1",
                params![query],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let mut stmt = connection
            .prepare(
                "SELECT DISTINCT s.id, s.title, s.platform, s.total_input_tokens, s.total_output_tokens, s.parent_session_id, s.created_at, s.updated_at
                 FROM session_search ss
                 JOIN sessions s ON s.id = ss.session_id
                 WHERE session_search MATCH ?1
                 ORDER BY rank
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![query, limit as i64, offset as i64], Self::row_to_session_summary)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let items = collect_rows(rows, self.db.path())?;
        Ok((items, total as u64))
    }

    /// Count total number of sessions.
    pub fn count_sessions(&self) -> Result<u64, StorageError> {
        let connection = self.db.conn()?;
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(count as u64)
    }

    /// Aggregate token usage across all sessions.
    pub fn usage_stats(&self) -> Result<UsageStats, StorageError> {
        let connection = self.db.conn()?;
        let (count, input, output): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(total_input_tokens), 0), COALESCE(SUM(total_output_tokens), 0) FROM sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(UsageStats {
            total_sessions: count as u64,
            total_input_tokens: input as u64,
            total_output_tokens: output as u64,
        })
    }

    /// Gather usage insights for the last N days.
    pub fn insights(&self, days: u32) -> Result<InsightsData, StorageError> {
        let connection = self.db.conn()?;
        let period = format!("-{days} days");

        // Aggregate totals for the period
        let (count, input, output): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(total_input_tokens), 0), COALESCE(SUM(total_output_tokens), 0) \
                 FROM sessions WHERE created_at >= datetime('now', ?)",
                [&period],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Sessions per day
        let mut stmt = connection
            .prepare(
                "SELECT date(created_at) as day, COUNT(*) \
                 FROM sessions WHERE created_at >= datetime('now', ?) \
                 GROUP BY day ORDER BY day",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let sessions_per_day: Vec<(String, u64)> = stmt
            .query_map([&period], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Platform breakdown
        let mut stmt = connection
            .prepare(
                "SELECT platform, COUNT(*) \
                 FROM sessions WHERE created_at >= datetime('now', ?) \
                 GROUP BY platform ORDER BY COUNT(*) DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let platform_breakdown: Vec<(String, u64)> = stmt
            .query_map([&period], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Tokens per day
        let mut stmt = connection
            .prepare(
                "SELECT date(created_at) as day, \
                 COALESCE(SUM(total_input_tokens), 0), \
                 COALESCE(SUM(total_output_tokens), 0) \
                 FROM sessions WHERE created_at >= datetime('now', ?) \
                 GROUP BY day ORDER BY day",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let tokens_per_day: Vec<(String, u64, u64)> = stmt
            .query_map([&period], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                ))
            })
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Tool usage breakdown — extract tool names from tool_calls_json
        let mut stmt = connection
            .prepare(
                "SELECT tool_calls_json FROM messages m \
                 JOIN sessions s ON m.session_id = s.id \
                 WHERE m.role = 'assistant' AND m.tool_calls_json IS NOT NULL \
                 AND s.created_at >= datetime('now', ?)",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let tool_jsons: Vec<String> = stmt
            .query_map([&period], |row| row.get::<_, String>(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let mut tool_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for json_str in &tool_jsons {
            if let Ok(calls) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                for call in calls {
                    if let Some(name) = call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .or_else(|| call.get("name").and_then(|n| n.as_str()))
                    {
                        *tool_counts.entry(name.to_owned()).or_insert(0) += 1;
                    }
                }
            }
        }
        let mut tool_usage: Vec<(String, u64)> = tool_counts.into_iter().collect();
        tool_usage.sort_by(|a, b| b.1.cmp(&a.1));

        let sessions_count = count as u64;
        let avg_input = if sessions_count > 0 {
            input as u64 / sessions_count
        } else {
            0
        };
        let avg_output = if sessions_count > 0 {
            output as u64 / sessions_count
        } else {
            0
        };

        Ok(InsightsData {
            period_days: days,
            sessions_count,
            total_input_tokens: input as u64,
            total_output_tokens: output as u64,
            sessions_per_day,
            platform_breakdown,
            tokens_per_day,
            tool_usage,
            avg_input_tokens: avg_input,
            avg_output_tokens: avg_output,
        })
    }

    /// Import a sequence of messages into a new session.
    ///
    /// Creates a session with the given ID and title, then inserts each
    /// (role, content) pair as a message. Returns the session ID.
    pub fn import_session(
        &self,
        session_id: &str,
        title: Option<&str>,
        messages: Vec<(String, String)>,
    ) -> Result<String, StorageError> {
        self.create_session(session_id, "import", title)?;

        for (role, content) in &messages {
            self.append_message(session_id, role, Some(content), None, None, None)?;
        }

        Ok(session_id.to_owned())
    }
}

#[cfg(test)]
mod session_store_tests {
    use crate::bootstrap;
    use super::SessionStore;
    use tempfile::tempdir;

    #[test]
    fn session_store_creates_and_loads_messages_in_order() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-1", "cli", None)
            .expect("session should be created");
        store
            .append_message("session-1", "user", Some("hello eve"), None, None, None)
            .expect("first message should be stored");
        store
            .append_message(
                "session-1",
                "assistant",
                Some("hello operator"),
                None,
                None,
                None,
            )
            .expect("second message should be stored");

        let messages = store
            .load_messages("session-1")
            .expect("messages should load");

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].session_id, "session-1");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content.as_deref(), Some("hello eve"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content.as_deref(), Some("hello operator"));
    }

    #[test]
    fn session_store_searches_sessions_by_indexed_message_content() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-alpha", "cli", None)
            .expect("first session should be created");
        store
            .create_session("session-beta", "slack", None)
            .expect("second session should be created");
        store
            .append_message(
                "session-alpha",
                "user",
                Some("rust migration checklist"),
                None,
                None,
                None,
            )
            .expect("first message should be stored");
        store
            .append_message(
                "session-beta",
                "user",
                Some("provider client work"),
                None,
                None,
                None,
            )
            .expect("second message should be stored");

        let matches = store
            .search_sessions("migration")
            .expect("search should succeed");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "session-alpha");
        assert_eq!(matches[0].platform, "cli");
    }

    #[test]
    fn add_usage_accumulates_token_counts() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-tok", "cli", None)
            .expect("session should be created");

        store
            .add_usage("session-tok", 100, 50)
            .expect("first add_usage");
        store
            .add_usage("session-tok", 200, 75)
            .expect("second add_usage");

        let session = store
            .get_session("session-tok")
            .expect("get should work")
            .expect("session should exist");
        assert_eq!(session.total_input_tokens, 300);
        assert_eq!(session.total_output_tokens, 125);
    }

    #[test]
    fn add_usage_noop_for_zero_tokens() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-zero", "cli", None)
            .expect("session should be created");

        // Should be a no-op
        store
            .add_usage("session-zero", 0, 0)
            .expect("zero add_usage");

        let session = store
            .get_session("session-zero")
            .expect("get should work")
            .expect("session should exist");
        assert_eq!(session.total_input_tokens, 0);
        assert_eq!(session.total_output_tokens, 0);
    }

    #[test]
    fn usage_stats_aggregates_all_sessions() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store.create_session("s1", "cli", None).expect("create s1");
        store.create_session("s2", "cli", None).expect("create s2");
        store.add_usage("s1", 1000, 500).expect("add usage s1");
        store.add_usage("s2", 2000, 800).expect("add usage s2");

        let stats = store.usage_stats().expect("stats should work");
        assert_eq!(stats.total_sessions, 2);
        assert_eq!(stats.total_input_tokens, 3000);
        assert_eq!(stats.total_output_tokens, 1300);
    }

    #[test]
    fn set_title_overwrites_existing_title() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("s1", "cli", Some("Original"))
            .expect("create");

        let session = store.get_session("s1").unwrap().unwrap();
        assert_eq!(session.title.as_deref(), Some("Original"));

        assert!(store.set_title("s1", "Renamed").unwrap());

        let session = store.get_session("s1").unwrap().unwrap();
        assert_eq!(session.title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn set_title_returns_false_for_missing_session() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        assert!(!store.set_title("nonexistent", "Title").unwrap());
    }

    #[test]
    fn purge_older_than_deletes_nothing_for_recent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store.create_session("recent", "cli", None).expect("create");
        store
            .append_message("recent", "user", Some("hello"), None, None, None)
            .expect("msg");

        let deleted = store.purge_older_than(30).unwrap();
        assert_eq!(deleted, 0);

        // Session and messages should still exist
        assert!(store.get_session("recent").unwrap().is_some());
        assert_eq!(store.load_messages("recent").unwrap().len(), 1);
    }

    #[test]
    fn append_mirror_message_stores_mirror_fields() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-mirror", "telegram", None)
            .expect("session should be created");

        let msg_id = store
            .append_mirror_message("session-mirror", "Hello from CLI", "cli")
            .expect("mirror message should be stored");

        assert!(msg_id > 0);

        let messages = store
            .load_messages("session-mirror")
            .expect("messages should load");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[0].content.as_deref(), Some("Hello from CLI"));
        assert!(messages[0].mirror);
        assert_eq!(messages[0].mirror_source.as_deref(), Some("cli"));
    }

    #[test]
    fn mirror_messages_coexist_with_regular_messages() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-mixed", "slack", None)
            .expect("session should be created");

        store
            .append_message("session-mixed", "user", Some("hello"), None, None, None)
            .expect("regular message should be stored");
        store
            .append_mirror_message("session-mixed", "scheduled reminder", "schedule")
            .expect("mirror message should be stored");
        store
            .append_message(
                "session-mixed",
                "assistant",
                Some("got it"),
                None,
                None,
                None,
            )
            .expect("regular reply should be stored");

        let messages = store
            .load_messages("session-mixed")
            .expect("messages should load");

        assert_eq!(messages.len(), 3);
        assert!(!messages[0].mirror);
        assert!(messages[0].mirror_source.is_none());
        assert!(messages[1].mirror);
        assert_eq!(messages[1].mirror_source.as_deref(), Some("schedule"));
        assert!(!messages[2].mirror);
        assert!(messages[2].mirror_source.is_none());
    }

    #[test]
    fn find_session_by_platform_chat_id_resolves_correctly() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("tg-12345", "telegram", Some("Telegram Chat"))
            .expect("session should be created");
        store
            .create_session("slack-general", "slack", Some("Slack General"))
            .expect("session should be created");

        let tg = store
            .find_session_by_platform_chat_id("telegram", "12345")
            .expect("lookup should succeed");
        assert!(tg.is_some());
        assert_eq!(tg.unwrap().id, "tg-12345");

        let slack = store
            .find_session_by_platform_chat_id("slack", "general")
            .expect("lookup should succeed");
        assert!(slack.is_some());
        assert_eq!(slack.unwrap().id, "slack-general");

        let missing = store
            .find_session_by_platform_chat_id("discord", "99999")
            .expect("lookup should succeed");
        assert!(missing.is_none());
    }

    #[test]
    fn truncate_messages_keeps_only_recent() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-trunc", "cli", None)
            .expect("session should be created");

        for i in 1..=5 {
            store
                .append_message(
                    "session-trunc",
                    "user",
                    Some(&format!("message {i}")),
                    None,
                    None,
                    None,
                )
                .expect("message should be stored");
        }

        let deleted = store
            .truncate_messages("session-trunc", 2)
            .expect("truncate should succeed");
        assert_eq!(deleted, 3);

        let messages = store
            .load_messages("session-trunc")
            .expect("messages should load");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.as_deref(), Some("message 4"));
        assert_eq!(messages[1].content.as_deref(), Some("message 5"));
    }

    #[test]
    fn truncate_messages_noop_when_fewer_than_limit() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-few", "cli", None)
            .expect("session should be created");

        for i in 1..=3 {
            store
                .append_message(
                    "session-few",
                    "user",
                    Some(&format!("message {i}")),
                    None,
                    None,
                    None,
                )
                .expect("message should be stored");
        }

        let deleted = store
            .truncate_messages("session-few", 10)
            .expect("truncate should succeed");
        assert_eq!(deleted, 0);

        let messages = store
            .load_messages("session-few")
            .expect("messages should load");
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn search_messages_finds_matching_content() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-search", "cli", Some("Search Test"))
            .expect("session should be created");

        store
            .append_message(
                "session-search",
                "user",
                Some("hello world"),
                None,
                None,
                None,
            )
            .expect("first message should be stored");
        store
            .append_message(
                "session-search",
                "assistant",
                Some("goodbye moon"),
                None,
                None,
                None,
            )
            .expect("second message should be stored");
        store
            .append_message(
                "session-search",
                "user",
                Some("hello again"),
                None,
                None,
                None,
            )
            .expect("third message should be stored");

        let results = store
            .search_messages("hello", 10)
            .expect("search should succeed");

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.session_id == "session-search"));
        assert!(results.iter().all(|r| r.content.contains("hello")));
    }

    #[test]
    fn list_recent_sessions_paginated_returns_total_and_page() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        for i in 0..5 {
            store
                .create_session(&format!("s-{i}"), "cli", None)
                .expect("session should be created");
        }

        // First page
        let (page, total) = store
            .list_recent_sessions_paginated(2, 0)
            .expect("paginated list should succeed");
        assert_eq!(total, 5);
        assert_eq!(page.len(), 2);

        // Second page
        let (page2, total2) = store
            .list_recent_sessions_paginated(2, 2)
            .expect("second page should succeed");
        assert_eq!(total2, 5);
        assert_eq!(page2.len(), 2);

        // Pages should not overlap
        assert_ne!(page[0].id, page2[0].id);

        // Beyond end
        let (page3, total3) = store
            .list_recent_sessions_paginated(2, 10)
            .expect("beyond-end page should succeed");
        assert_eq!(total3, 5);
        assert!(page3.is_empty());
    }

    #[test]
    fn search_sessions_paginated_returns_filtered_total() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("s-match-1", "cli", None)
            .expect("session should be created");
        store
            .create_session("s-match-2", "cli", None)
            .expect("session should be created");
        store
            .create_session("s-other", "cli", None)
            .expect("session should be created");
        store
            .append_message("s-match-1", "user", Some("pagination test alpha"), None, None, None)
            .expect("msg");
        store
            .append_message("s-match-2", "user", Some("pagination test beta"), None, None, None)
            .expect("msg");
        store
            .append_message("s-other", "user", Some("unrelated topic"), None, None, None)
            .expect("msg");

        let (results, total) = store
            .search_sessions_paginated("pagination", 1, 0)
            .expect("search should succeed");
        assert_eq!(total, 2);
        assert_eq!(results.len(), 1);

        let (results2, total2) = store
            .search_sessions_paginated("pagination", 1, 1)
            .expect("second page should succeed");
        assert_eq!(total2, 2);
        assert_eq!(results2.len(), 1);
    }
}
