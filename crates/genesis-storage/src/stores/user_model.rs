use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::util::collect_rows;
use crate::Database;

/// A stored user trait — an observation about the user's preferences, personality, or goals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredUserTrait {
    pub trait_key: String,
    pub category: String,
    pub value: String,
    pub confidence: f64,
    pub evidence_count: i64,
    pub source_session: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Format user traits as a markdown section, grouped by category.
/// Returns `None` if the list is empty.
pub fn format_user_traits(traits: &[StoredUserTrait]) -> Option<String> {
    if traits.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    let mut current_category = String::new();

    for t in traits {
        if t.category != current_category {
            if !current_category.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("## {}", t.category));
            current_category.clone_from(&t.category);
        }
        lines.push(format!(
            "- **{}**: {} (confidence: {:.0}%, {} observations)",
            t.trait_key,
            t.value,
            t.confidence * 100.0,
            t.evidence_count,
        ));
    }

    Some(lines.join("\n"))
}

/// User model persistence layer.
///
/// Stores observations about the user that the agent learns over time.
/// Categories include: preference, personality, communication_style, goal, expertise, context.
pub struct UserModelStore {
    db: Database,
}

impl UserModelStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Record or update a user trait. If the trait already exists, its confidence
    /// is increased and evidence count bumped.
    pub fn observe(
        &self,
        trait_key: &str,
        category: &str,
        value: &str,
        source_session: Option<&str>,
    ) -> Result<StoredUserTrait, StorageError> {
        let connection = self.db.conn()?;

        // Clamp confidence at 1.0, increase by 0.1 per observation
        connection
            .execute(
                "INSERT INTO user_model (trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 0.5, 1, ?4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(trait_key) DO UPDATE SET
                     value = excluded.value,
                     confidence = MIN(1.0, user_model.confidence + 0.1),
                     evidence_count = user_model.evidence_count + 1,
                     source_session = excluded.source_session,
                     updated_at = CURRENT_TIMESTAMP",
                params![trait_key, category, value, source_session],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Drop the connection guard before calling self.get() to avoid mutex
        // re-entrance deadlock.
        drop(connection);

        self.get(trait_key)?.ok_or_else(|| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Get a specific user trait by key.
    pub fn get(&self, trait_key: &str) -> Result<Option<StoredUserTrait>, StorageError> {
        let connection = self.db.conn()?;
        connection
            .query_row(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE trait_key = ?1",
                params![trait_key],
                Self::row_to_trait,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// List all user traits, ordered by confidence (highest first).
    pub fn list_all(&self) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model ORDER BY confidence DESC, evidence_count DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows =
            stmt.query_map([], Self::row_to_trait)
                .map_err(|source| StorageError::Sqlite {
                    path: self.db.path().to_path_buf(),
                    source,
                })?;

        collect_rows(rows, self.db.path())
    }

    /// List traits in a specific category.
    pub fn list_by_category(&self, category: &str) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE category = ?1 ORDER BY confidence DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![category], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Get high-confidence traits (>= threshold) for prompt injection.
    pub fn confident_traits(&self, threshold: f64) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE confidence >= ?1 ORDER BY confidence DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![threshold], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// List user traits with offset/limit pagination.
    ///
    /// Supports optional `category` and `min_confidence` filters.  The returned
    /// total count reflects the filtered subset.
    pub fn list_paginated(
        &self,
        category: Option<&str>,
        min_confidence: Option<f64>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<StoredUserTrait>, u64), StorageError> {
        let connection = self.db.conn()?;
        let db = self.db.path();
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.to_path_buf(),
            source,
        };

        let (count_sql, data_sql, total): (&str, &str, i64) = if let Some(cat) = category {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM user_model WHERE category = ?1",
                    params![cat],
                    |row| row.get(0),
                )
                .map_err(me)?;
            (
                // not used below, just for symmetry
                "",
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE category = ?1 ORDER BY confidence DESC
                 LIMIT ?2 OFFSET ?3",
                t,
            )
        } else if let Some(threshold) = min_confidence {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM user_model WHERE confidence >= ?1",
                    params![threshold],
                    |row| row.get(0),
                )
                .map_err(me)?;
            (
                "",
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE confidence >= ?1 ORDER BY confidence DESC
                 LIMIT ?2 OFFSET ?3",
                t,
            )
        } else {
            let t: i64 = connection
                .query_row("SELECT COUNT(*) FROM user_model", [], |row| row.get(0))
                .map_err(me)?;
            (
                "",
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model ORDER BY confidence DESC, evidence_count DESC
                 LIMIT ?1 OFFSET ?2",
                t,
            )
        };

        // Suppress unused-variable warning for count_sql (kept for readability).
        let _ = count_sql;

        let mut stmt = connection.prepare(data_sql).map_err(me)?;
        let items = if let Some(cat) = category {
            let rows = stmt
                .query_map(
                    params![cat, limit as i64, offset as i64],
                    Self::row_to_trait,
                )
                .map_err(me)?;
            collect_rows(rows, self.db.path())?
        } else if let Some(threshold) = min_confidence {
            let rows = stmt
                .query_map(
                    params![threshold, limit as i64, offset as i64],
                    Self::row_to_trait,
                )
                .map_err(me)?;
            collect_rows(rows, self.db.path())?
        } else {
            let rows = stmt
                .query_map(params![limit as i64, offset as i64], Self::row_to_trait)
                .map_err(me)?;
            collect_rows(rows, self.db.path())?
        };

        Ok((items, total as u64))
    }

    /// Delete a user trait.
    pub fn delete(&self, trait_key: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "DELETE FROM user_model WHERE trait_key = ?1",
                params![trait_key],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows > 0)
    }

    fn row_to_trait(row: &rusqlite::Row) -> Result<StoredUserTrait, rusqlite::Error> {
        Ok(StoredUserTrait {
            trait_key: row.get(0)?,
            category: row.get(1)?,
            value: row.get(2)?,
            confidence: row.get(3)?,
            evidence_count: row.get(4)?,
            source_session: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }
}

#[cfg(test)]
mod user_model_store_tests {
    use super::UserModelStore;
    use crate::bootstrap;
    use tempfile::tempdir;

    #[test]
    fn observe_and_get_trait() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        let observed = store
            .observe(
                "preferred_language",
                "preference",
                "Rust",
                Some("session-1"),
            )
            .expect("observe should succeed");

        assert_eq!(observed.trait_key, "preferred_language");
        assert_eq!(observed.category, "preference");
        assert_eq!(observed.value, "Rust");
        assert!((observed.confidence - 0.5).abs() < f64::EPSILON);
        assert_eq!(observed.evidence_count, 1);
        assert_eq!(observed.source_session.as_deref(), Some("session-1"));

        let fetched = store
            .get("preferred_language")
            .expect("get should succeed")
            .expect("trait should exist");

        assert_eq!(fetched.trait_key, "preferred_language");
        assert_eq!(fetched.category, "preference");
        assert_eq!(fetched.value, "Rust");
        assert!((fetched.confidence - 0.5).abs() < f64::EPSILON);
        assert_eq!(fetched.evidence_count, 1);
    }

    #[test]
    fn observe_updates_existing_trait() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store
            .observe("editor", "preference", "vim", Some("s1"))
            .expect("first observe should succeed");

        let updated = store
            .observe("editor", "preference", "neovim", Some("s2"))
            .expect("second observe should succeed");

        assert_eq!(updated.trait_key, "editor");
        assert_eq!(updated.value, "neovim");
        // Confidence should increase by 0.1 from 0.5 to 0.6
        assert!((updated.confidence - 0.6).abs() < f64::EPSILON);
        assert_eq!(updated.evidence_count, 2);
        assert_eq!(updated.source_session.as_deref(), Some("s2"));
    }

    #[test]
    fn list_traits() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store
            .observe("lang", "preference", "Rust", None)
            .expect("observe lang");
        store
            .observe("tone", "communication_style", "casual", None)
            .expect("observe tone");
        store
            .observe("goal", "goal", "build an AI agent", None)
            .expect("observe goal");

        let all = store.list_all().expect("list_all should succeed");
        assert_eq!(all.len(), 3);

        let keys: Vec<&str> = all.iter().map(|t| t.trait_key.as_str()).collect();
        assert!(keys.contains(&"lang"));
        assert!(keys.contains(&"tone"));
        assert!(keys.contains(&"goal"));
    }

    #[test]
    fn list_traits_filters_by_category() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store
            .observe("lang", "preference", "Rust", None)
            .expect("observe lang");
        store
            .observe("theme", "preference", "dark mode", None)
            .expect("observe theme");
        store
            .observe("tone", "communication_style", "formal", None)
            .expect("observe tone");
        store
            .observe("expertise", "expertise", "systems programming", None)
            .expect("observe expertise");

        let preferences = store
            .list_by_category("preference")
            .expect("list_by_category should succeed");
        assert_eq!(preferences.len(), 2);
        let pref_keys: Vec<&str> = preferences.iter().map(|t| t.trait_key.as_str()).collect();
        assert!(pref_keys.contains(&"lang"));
        assert!(pref_keys.contains(&"theme"));

        let comm = store
            .list_by_category("communication_style")
            .expect("list_by_category should succeed");
        assert_eq!(comm.len(), 1);
        assert_eq!(comm[0].trait_key, "tone");

        let empty = store
            .list_by_category("nonexistent_category")
            .expect("list_by_category should succeed");
        assert!(empty.is_empty());
    }

    #[test]
    fn list_traits_filters_by_min_confidence() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);

        // First trait: observed once -> confidence 0.5
        store
            .observe("low_conf", "preference", "tabs", None)
            .expect("observe low_conf");

        // Second trait: observed 4 times -> confidence 0.5 + 0.3 = 0.8
        store
            .observe("high_conf", "preference", "dark mode", None)
            .expect("observe high_conf 1");
        store
            .observe("high_conf", "preference", "dark mode", None)
            .expect("observe high_conf 2");
        store
            .observe("high_conf", "preference", "dark mode", None)
            .expect("observe high_conf 3");
        store
            .observe("high_conf", "preference", "dark mode", None)
            .expect("observe high_conf 4");

        // Filter by confidence >= 0.7 should only return high_conf
        let confident = store
            .confident_traits(0.7)
            .expect("confident_traits should succeed");
        assert_eq!(confident.len(), 1);
        assert_eq!(confident[0].trait_key, "high_conf");
        assert!(confident[0].confidence >= 0.7);

        // Filter by confidence >= 0.5 should return both
        let all_confident = store
            .confident_traits(0.5)
            .expect("confident_traits should succeed");
        assert_eq!(all_confident.len(), 2);
    }

    #[test]
    fn delete_trait() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store
            .observe("to_remove", "preference", "light mode", None)
            .expect("observe should succeed");

        assert!(
            store.get("to_remove").unwrap().is_some(),
            "trait should exist before delete"
        );

        let deleted = store.delete("to_remove").expect("delete should succeed");
        assert!(deleted, "delete should return true for existing trait");

        assert!(
            store.get("to_remove").unwrap().is_none(),
            "trait should not exist after delete"
        );

        let deleted_again = store
            .delete("to_remove")
            .expect("delete of missing should succeed");
        assert!(
            !deleted_again,
            "delete should return false for nonexistent trait"
        );
    }

    #[test]
    fn list_paginated_returns_total_and_page() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        for i in 0..4 {
            store
                .observe(
                    &format!("trait_{i}"),
                    "category_a",
                    &format!("value_{i}"),
                    None,
                )
                .expect("observe should succeed");
        }

        let (page, total) = store.list_paginated(None, None, 2, 0).expect("first page");
        assert_eq!(total, 4);
        assert_eq!(page.len(), 2);

        let (page2, _) = store.list_paginated(None, None, 2, 2).expect("second page");
        assert_eq!(page2.len(), 2);
        assert_ne!(page[0].trait_key, page2[0].trait_key);
    }

    #[test]
    fn list_paginated_with_category_filter() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = UserModelStore::new(&database_path);
        store.observe("t1", "pref", "v1", None).unwrap();
        store.observe("t2", "pref", "v2", None).unwrap();
        store.observe("t3", "skill", "v3", None).unwrap();

        let (page, total) = store
            .list_paginated(Some("pref"), None, 50, 0)
            .expect("filter by category");
        assert_eq!(total, 2);
        assert_eq!(page.len(), 2);
    }
}
