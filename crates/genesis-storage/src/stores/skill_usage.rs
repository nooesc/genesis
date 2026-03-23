use std::path::Path;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::util::collect_rows;
use crate::Database;

/// A recorded skill usage — tracks when and how a skill was applied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSkillUsage {
    pub id: i64,
    pub skill_name: String,
    pub session_id: Option<String>,
    /// "success", "partial", "failure", or "unknown"
    pub outcome: String,
    /// Agent's free-text feedback on what worked or didn't
    pub feedback: Option<String>,
    /// Whether the agent refined the skill after this usage
    pub refined: bool,
    pub created_at: String,
}

/// Aggregate stats for a skill's usage history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillUsageStats {
    pub skill_name: String,
    pub total_uses: i64,
    pub successes: i64,
    pub failures: i64,
    pub last_used: Option<String>,
    pub times_refined: i64,
}

/// Skill usage tracking layer.
pub struct SkillUsageStore {
    db: Database,
}

impl SkillUsageStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Record that a skill was used in a session.
    pub fn record_usage(
        &self,
        skill_name: &str,
        session_id: Option<&str>,
        outcome: &str,
        feedback: Option<&str>,
    ) -> Result<StoredSkillUsage, StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute(
                "INSERT INTO skill_usages (skill_name, session_id, outcome, feedback)
                 VALUES (?1, ?2, ?3, ?4)",
                params![skill_name, session_id, outcome, feedback],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let id = connection.last_insert_rowid();
        connection
            .query_row(
                "SELECT id, skill_name, session_id, outcome, feedback, refined, created_at
                 FROM skill_usages WHERE id = ?1",
                params![id],
                Self::row_to_usage,
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// Mark a usage record as having led to a skill refinement.
    pub fn mark_refined(&self, usage_id: i64) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "UPDATE skill_usages SET refined = 1 WHERE id = ?1",
                params![usage_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Get aggregate usage stats for a skill.
    pub fn stats(&self, skill_name: &str) -> Result<SkillUsageStats, StorageError> {
        let connection = self.db.conn()?;
        connection
            .query_row(
                "SELECT
                    ?1 as skill_name,
                    COUNT(*) as total_uses,
                    SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) as successes,
                    SUM(CASE WHEN outcome = 'failure' THEN 1 ELSE 0 END) as failures,
                    MAX(created_at) as last_used,
                    SUM(refined) as times_refined
                 FROM skill_usages WHERE skill_name = ?1",
                params![skill_name],
                |row| {
                    Ok(SkillUsageStats {
                        skill_name: row.get(0)?,
                        total_uses: row.get(1)?,
                        successes: row.get(2)?,
                        failures: row.get(3)?,
                        last_used: row.get(4)?,
                        times_refined: row.get(5)?,
                    })
                },
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// Get recent usage records for a skill.
    pub fn recent_usages(
        &self,
        skill_name: &str,
        limit: usize,
    ) -> Result<Vec<StoredSkillUsage>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT id, skill_name, session_id, outcome, feedback, refined, created_at
                 FROM skill_usages WHERE skill_name = ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![skill_name, limit as i64], Self::row_to_usage)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    fn row_to_usage(row: &rusqlite::Row) -> Result<StoredSkillUsage, rusqlite::Error> {
        Ok(StoredSkillUsage {
            id: row.get(0)?,
            skill_name: row.get(1)?,
            session_id: row.get(2)?,
            outcome: row.get(3)?,
            feedback: row.get(4)?,
            refined: row.get::<_, i64>(5)? != 0,
            created_at: row.get(6)?,
        })
    }
}

#[cfg(test)]
mod skill_usage_store_tests {
    use super::SkillUsageStore;
    use crate::{bootstrap, SkillStore};
    use tempfile::tempdir;

    /// Helper: create a skill so the foreign-key constraint on skill_usages is satisfied.
    fn seed_skill(store: &SkillStore, name: &str) {
        store
            .upsert(name, "test skill", "do the thing", None, &[])
            .expect("seed skill should succeed");
    }

    #[test]
    fn record_and_list_usage() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let skill_store = SkillStore::new(&database_path);
        seed_skill(&skill_store, "code-review");

        let store = SkillUsageStore::new(&database_path);
        let usage = store
            .record_usage(
                "code-review",
                Some("session-1"),
                "success",
                Some("worked well"),
            )
            .expect("record_usage should succeed");

        assert_eq!(usage.skill_name, "code-review");
        assert_eq!(usage.session_id.as_deref(), Some("session-1"));
        assert_eq!(usage.outcome, "success");
        assert_eq!(usage.feedback.as_deref(), Some("worked well"));
        assert!(!usage.refined);

        let recent = store
            .recent_usages("code-review", 10)
            .expect("recent_usages should succeed");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].skill_name, "code-review");
        assert_eq!(recent[0].outcome, "success");
    }

    #[test]
    fn aggregate_usage_stats() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let skill_store = SkillStore::new(&database_path);
        seed_skill(&skill_store, "data-extract");

        let store = SkillUsageStore::new(&database_path);

        // Record several usages with mixed outcomes
        let u1 = store
            .record_usage("data-extract", Some("s1"), "success", None)
            .expect("record_usage should succeed");
        store
            .record_usage("data-extract", Some("s2"), "success", None)
            .expect("record_usage should succeed");
        store
            .record_usage("data-extract", Some("s3"), "failure", Some("timed out"))
            .expect("record_usage should succeed");
        store
            .record_usage("data-extract", Some("s4"), "partial", None)
            .expect("record_usage should succeed");

        // Mark the first usage as refined
        store
            .mark_refined(u1.id)
            .expect("mark_refined should succeed");

        let stats = store.stats("data-extract").expect("stats should succeed");

        assert_eq!(stats.skill_name, "data-extract");
        assert_eq!(stats.total_uses, 4);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.times_refined, 1);
        assert!(stats.last_used.is_some());
    }

    #[test]
    fn list_usage_respects_limit() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let skill_store = SkillStore::new(&database_path);
        seed_skill(&skill_store, "bulk-skill");

        let store = SkillUsageStore::new(&database_path);

        // Record 5 usages
        for i in 0..5 {
            store
                .record_usage("bulk-skill", Some(&format!("session-{i}")), "success", None)
                .expect("record_usage should succeed");
        }

        // Request only 3
        let limited = store
            .recent_usages("bulk-skill", 3)
            .expect("recent_usages should succeed");
        assert_eq!(limited.len(), 3);

        // Request all
        let all = store
            .recent_usages("bulk-skill", 100)
            .expect("recent_usages should succeed");
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn usage_records_different_skills() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let skill_store = SkillStore::new(&database_path);
        seed_skill(&skill_store, "skill-alpha");
        seed_skill(&skill_store, "skill-beta");

        let store = SkillUsageStore::new(&database_path);

        // Record usages for two different skills
        store
            .record_usage("skill-alpha", Some("s1"), "success", None)
            .expect("record_usage should succeed");
        store
            .record_usage("skill-alpha", Some("s2"), "failure", None)
            .expect("record_usage should succeed");
        store
            .record_usage("skill-beta", Some("s3"), "success", None)
            .expect("record_usage should succeed");

        // Stats should be per-skill
        let alpha_stats = store.stats("skill-alpha").expect("stats should succeed");
        assert_eq!(alpha_stats.total_uses, 2);
        assert_eq!(alpha_stats.successes, 1);
        assert_eq!(alpha_stats.failures, 1);

        let beta_stats = store.stats("skill-beta").expect("stats should succeed");
        assert_eq!(beta_stats.total_uses, 1);
        assert_eq!(beta_stats.successes, 1);
        assert_eq!(beta_stats.failures, 0);

        // recent_usages should only return records for the requested skill
        let alpha_recent = store
            .recent_usages("skill-alpha", 10)
            .expect("recent_usages should succeed");
        assert_eq!(alpha_recent.len(), 2);
        assert!(alpha_recent.iter().all(|u| u.skill_name == "skill-alpha"));

        let beta_recent = store
            .recent_usages("skill-beta", 10)
            .expect("recent_usages should succeed");
        assert_eq!(beta_recent.len(), 1);
        assert_eq!(beta_recent[0].skill_name, "skill-beta");
    }
}
