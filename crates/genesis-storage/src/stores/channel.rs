use std::path::Path;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::util::collect_rows;
use crate::Database;

// ChannelStore — cached platform channel directory for send_message discovery
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedChannel {
    pub platform: String,
    pub channel_id: String,
    pub channel_name: String,
    pub channel_type: String,
    pub is_member: bool,
    pub cached_at: String,
}

pub struct ChannelStore {
    db: Database,
}

impl ChannelStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// List cached channels, optionally filtered by platform.
    pub fn list(&self, platform: Option<&str>) -> Result<Vec<CachedChannel>, StorageError> {
        let connection = self.db.conn()?;

        let (sql, param): (&str, Option<&str>) = if platform.is_some() {
            (
                "SELECT platform, channel_id, channel_name, channel_type, is_member, cached_at
                 FROM channels WHERE platform = ?1
                 ORDER BY channel_name",
                platform,
            )
        } else {
            (
                "SELECT platform, channel_id, channel_name, channel_type, is_member, cached_at
                 FROM channels ORDER BY platform, channel_name",
                None,
            )
        };

        let mut stmt = connection
            .prepare(sql)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let row_mapper = |row: &rusqlite::Row| {
            Ok(CachedChannel {
                platform: row.get(0)?,
                channel_id: row.get(1)?,
                channel_name: row.get(2)?,
                channel_type: row.get(3)?,
                is_member: row.get::<_, i64>(4)? != 0,
                cached_at: row.get(5)?,
            })
        };

        let mapped_rows = if let Some(p) = param {
            stmt.query_map(params![p], row_mapper)
        } else {
            stmt.query_map([], row_mapper)
        }
        .map_err(|source| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source,
        })?;

        collect_rows(mapped_rows, self.db.path())
    }

    /// Upsert a batch of channels for a platform, replacing stale entries.
    pub fn upsert_channels(
        &self,
        platform: &str,
        channels: &[CachedChannel],
    ) -> Result<usize, StorageError> {
        let connection = self.db.conn()?;

        // Clear old entries for this platform before inserting fresh data.
        connection
            .execute(
                "DELETE FROM channels WHERE platform = ?1",
                params![platform],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        for ch in channels {
            connection
                .execute(
                    "INSERT INTO channels (platform, channel_id, channel_name, channel_type, is_member, cached_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)",
                    params![
                        platform,
                        ch.channel_id,
                        ch.channel_name,
                        ch.channel_type,
                        ch.is_member as i64,
                    ],
                )
                .map_err(|source| StorageError::Sqlite {
                    path: self.db.path().to_path_buf(),
                    source,
                })?;
        }

        Ok(channels.len())
    }

    /// Check if channels for a platform are cached and fresh (within max_age_secs).
    pub fn is_fresh(&self, platform: &str, max_age_secs: i64) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let fresh: bool = connection
            .query_row(
                "SELECT COUNT(*) FROM channels
                 WHERE platform = ?1
                   AND CAST((julianday('now') - julianday(cached_at)) * 86400 AS INTEGER) < ?2",
                params![platform, max_age_secs],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?
            > 0;
        Ok(fresh)
    }
}

#[cfg(test)]
mod channel_store_tests {
    use super::{CachedChannel, ChannelStore};
    use crate::bootstrap;
    use tempfile::tempdir;

    fn make_channel(
        platform: &str,
        id: &str,
        name: &str,
        ch_type: &str,
        is_member: bool,
    ) -> CachedChannel {
        CachedChannel {
            platform: platform.to_string(),
            channel_id: id.to_string(),
            channel_name: name.to_string(),
            channel_type: ch_type.to_string(),
            is_member,
            cached_at: String::new(), // ignored on insert; DB sets CURRENT_TIMESTAMP
        }
    }

    #[test]
    fn cache_and_get_channel() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ChannelStore::new(&database_path);
        let channels = vec![
            make_channel("slack", "C001", "general", "channel", true),
            make_channel("slack", "C002", "random", "channel", false),
        ];

        let inserted = store
            .upsert_channels("slack", &channels)
            .expect("upsert_channels should succeed");
        assert_eq!(inserted, 2);

        let listed = store.list(Some("slack")).expect("list should succeed");
        assert_eq!(listed.len(), 2);

        // list is ordered by channel_name ASC
        assert_eq!(listed[0].channel_id, "C001");
        assert_eq!(listed[0].channel_name, "general");
        assert_eq!(listed[0].channel_type, "channel");
        assert!(listed[0].is_member);
        assert_eq!(listed[0].platform, "slack");

        assert_eq!(listed[1].channel_id, "C002");
        assert_eq!(listed[1].channel_name, "random");
        assert!(!listed[1].is_member);
    }

    #[test]
    fn list_returns_empty_for_unknown_platform() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ChannelStore::new(&database_path);
        let listed = store.list(Some("discord")).expect("list should succeed");
        assert!(listed.is_empty());
    }

    #[test]
    fn update_channel() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = ChannelStore::new(&database_path);

        // Initial insert
        let initial = vec![
            make_channel("slack", "C001", "general", "channel", false),
            make_channel("slack", "C002", "random", "channel", true),
        ];
        store
            .upsert_channels("slack", &initial)
            .expect("initial upsert");

        // Update: change membership and add a new channel
        let updated = vec![
            make_channel("slack", "C001", "general", "channel", true),
            make_channel("slack", "C003", "engineering", "channel", true),
        ];
        store
            .upsert_channels("slack", &updated)
            .expect("update upsert");

        let listed = store.list(Some("slack")).expect("list should succeed");

        // upsert_channels deletes old entries for the platform, then inserts fresh ones
        assert_eq!(listed.len(), 2);

        let names: Vec<&str> = listed.iter().map(|c| c.channel_name.as_str()).collect();
        assert!(names.contains(&"general"));
        assert!(names.contains(&"engineering"));
        // "random" (C002) should be gone — replaced by the new batch
        assert!(!names.contains(&"random"));

        // Verify the updated membership flag
        let general = listed.iter().find(|c| c.channel_id == "C001").unwrap();
        assert!(general.is_member, "general should now be a member");
    }
}
