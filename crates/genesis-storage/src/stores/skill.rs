use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Database;
use crate::error::StorageError;
use crate::util::collect_rows;

/// A stored agent skill — a reusable procedure the agent can invoke.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSkill {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub trigger_hint: Option<String>,
    pub tags: Vec<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Skill persistence layer.
pub struct SkillStore {
    db: Database,
}

impl SkillStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    fn row_to_skill(row: &rusqlite::Row) -> Result<StoredSkill, rusqlite::Error> {
        let tags_str: String = row.get(4)?;
        let tags = if tags_str.is_empty() {
            Vec::new()
        } else {
            tags_str.split(',').map(|s| s.to_owned()).collect()
        };

        Ok(StoredSkill {
            name: row.get(0)?,
            description: row.get(1)?,
            instructions: row.get(2)?,
            trigger_hint: row.get(3)?,
            tags,
            version: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    /// Create or update a skill. If the skill already exists, its version is bumped.
    pub fn upsert(
        &self,
        name: &str,
        description: &str,
        instructions: &str,
        trigger_hint: Option<&str>,
        tags: &[&str],
    ) -> Result<StoredSkill, StorageError> {
        let connection = self.db.conn()?;
        let tags_str = tags.join(",");

        connection
            .execute(
                "INSERT INTO skills (name, description, instructions, trigger_hint, tags, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(name) DO UPDATE SET
                     description = excluded.description,
                     instructions = excluded.instructions,
                     trigger_hint = excluded.trigger_hint,
                     tags = excluded.tags,
                     version = skills.version + 1,
                     updated_at = CURRENT_TIMESTAMP",
                params![name, description, instructions, trigger_hint, tags_str],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Drop the connection guard before calling self.get() to avoid mutex
        // re-entrance deadlock.
        drop(connection);

        self.get(name)?.ok_or_else(|| StorageError::Sqlite {
            path: self.db.path().to_path_buf(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    /// Get a skill by name.
    pub fn get(&self, name: &str) -> Result<Option<StoredSkill>, StorageError> {
        let connection = self.db.conn()?;
        connection
            .query_row(
                "SELECT name, description, instructions, trigger_hint, tags, version, created_at, updated_at
                 FROM skills WHERE name = ?1",
                params![name],
                Self::row_to_skill,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    /// List all skills, ordered by name.
    pub fn list_all(&self) -> Result<Vec<StoredSkill>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT name, description, instructions, trigger_hint, tags, version, created_at, updated_at
                 FROM skills ORDER BY name ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows =
            stmt.query_map([], Self::row_to_skill)
                .map_err(|source| StorageError::Sqlite {
                    path: self.db.path().to_path_buf(),
                    source,
                })?;

        collect_rows(rows, self.db.path())
    }

    /// List all skills with offset/limit pagination.
    pub fn list_all_paginated(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<StoredSkill>, u64), StorageError> {
        let connection = self.db.conn()?;

        let total: i64 = connection
            .query_row("SELECT COUNT(*) FROM skills", [], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let mut stmt = connection
            .prepare(
                "SELECT name, description, instructions, trigger_hint, tags, version, created_at, updated_at
                 FROM skills ORDER BY name ASC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64], Self::row_to_skill)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let items = collect_rows(rows, self.db.path())?;
        Ok((items, total as u64))
    }

    /// Find skills matching any of the given tags.
    pub fn find_by_tag(&self, tag: &str) -> Result<Vec<StoredSkill>, StorageError> {
        let connection = self.db.conn()?;
        // SQLite LIKE with comma-separated tags
        let pattern = format!("%{tag}%");
        let mut stmt = connection
            .prepare(
                "SELECT name, description, instructions, trigger_hint, tags, version, created_at, updated_at
                 FROM skills WHERE tags LIKE ?1 ORDER BY name ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let rows = stmt
            .query_map(params![pattern], Self::row_to_skill)
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        collect_rows(rows, self.db.path())
    }

    /// Find skills whose trigger hints, names, descriptions, or tags match the
    /// given user prompt. Uses simple keyword overlap scoring to rank results.
    /// Returns up to `limit` matching skills, ordered by relevance score.
    pub fn find_matching(
        &self,
        prompt: &str,
        limit: usize,
    ) -> Result<Vec<StoredSkill>, StorageError> {
        let all_skills = self.list_all()?;
        if all_skills.is_empty() {
            return Ok(Vec::new());
        }

        // Tokenize the prompt into lowercase words.
        let prompt_words: Vec<String> = prompt
            .split_whitespace()
            .map(|w| {
                w.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_owned()
            })
            .filter(|w| w.len() >= 2)
            .collect();

        if prompt_words.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(f64, StoredSkill)> = all_skills
            .into_iter()
            .filter_map(|skill| {
                let score = skill_match_score(&skill, &prompt_words);
                if score > 0.0 {
                    Some((score, skill))
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending.
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored.into_iter().map(|(_, skill)| skill).collect())
    }

    /// Delete a skill by name. Returns true if a skill was deleted.
    pub fn delete(&self, name: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows_changed = connection
            .execute("DELETE FROM skills WHERE name = ?1", params![name])
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows_changed > 0)
    }
}

/// Compute a relevance score for a skill against tokenized prompt words.
/// Higher score = more relevant. Returns 0.0 if no match.
fn skill_match_score(skill: &StoredSkill, prompt_words: &[String]) -> f64 {
    let mut score = 0.0;

    // Build searchable text from the skill's fields.
    let trigger_words: Vec<String> = skill
        .trigger_hint
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();

    let name_words: Vec<String> = skill
        .name
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .collect();

    let desc_words: Vec<String> = skill
        .description
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();

    let tag_words: Vec<String> = skill.tags.iter().map(|t| t.to_lowercase()).collect();

    for word in prompt_words {
        // Trigger hint matches are weighted highest (3x).
        if trigger_words.iter().any(|tw| tw.contains(word.as_str())) {
            score += 3.0;
        }
        // Skill name matches get 2x weight.
        if name_words.iter().any(|nw| nw.contains(word.as_str())) {
            score += 2.0;
        }
        // Tag matches get 2x weight.
        if tag_words.iter().any(|tw| tw.contains(word.as_str())) {
            score += 2.0;
        }
        // Description matches get 1x weight.
        if desc_words.iter().any(|dw| dw.contains(word.as_str())) {
            score += 1.0;
        }
    }

    score
}

#[cfg(test)]
mod skill_store_tests {
    use crate::bootstrap;
    use super::SkillStore;
    use tempfile::tempdir;

    #[test]
    fn upsert_and_get_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        let skill = store
            .upsert(
                "greet_user",
                "Greet the user warmly",
                "Say hello and ask how they are doing",
                Some("when user says hi"),
                &["greeting", "social"],
            )
            .expect("upsert should succeed");

        assert_eq!(skill.name, "greet_user");
        assert_eq!(skill.description, "Greet the user warmly");
        assert_eq!(skill.instructions, "Say hello and ask how they are doing");
        assert_eq!(skill.trigger_hint.as_deref(), Some("when user says hi"));
        assert_eq!(skill.tags, vec!["greeting", "social"]);
        assert_eq!(skill.version, 1);

        let fetched = store
            .get("greet_user")
            .expect("get should succeed")
            .expect("skill should exist");

        assert_eq!(fetched.name, "greet_user");
        assert_eq!(fetched.description, "Greet the user warmly");
        assert_eq!(fetched.instructions, "Say hello and ask how they are doing");
        assert_eq!(fetched.trigger_hint.as_deref(), Some("when user says hi"));
        assert_eq!(fetched.tags, vec!["greeting", "social"]);
        assert_eq!(fetched.version, 1);
    }

    #[test]
    fn upsert_updates_existing_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        store
            .upsert(
                "summarize",
                "Summarize text",
                "Provide a brief summary",
                None,
                &["text"],
            )
            .expect("first upsert should succeed");

        let updated = store
            .upsert(
                "summarize",
                "Summarize any content",
                "Provide a concise summary with key points",
                Some("when asked to summarize"),
                &["text", "analysis"],
            )
            .expect("second upsert should succeed");

        assert_eq!(updated.name, "summarize");
        assert_eq!(updated.description, "Summarize any content");
        assert_eq!(
            updated.instructions,
            "Provide a concise summary with key points"
        );
        assert_eq!(
            updated.trigger_hint.as_deref(),
            Some("when asked to summarize")
        );
        assert_eq!(updated.tags, vec!["text", "analysis"]);
        assert_eq!(updated.version, 2);
    }

    #[test]
    fn list_all_skills() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        store
            .upsert("alpha_skill", "Alpha", "Do alpha things", None, &[])
            .expect("upsert alpha should succeed");
        store
            .upsert("beta_skill", "Beta", "Do beta things", None, &[])
            .expect("upsert beta should succeed");
        store
            .upsert("gamma_skill", "Gamma", "Do gamma things", None, &[])
            .expect("upsert gamma should succeed");

        let all = store.list_all().expect("list_all should succeed");
        assert_eq!(all.len(), 3);

        // list_all orders by name ASC
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha_skill", "beta_skill", "gamma_skill"]);
    }

    #[test]
    fn delete_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        store
            .upsert("to_delete", "Temporary skill", "Will be removed", None, &[])
            .expect("upsert should succeed");

        assert!(
            store.get("to_delete").unwrap().is_some(),
            "skill should exist before delete"
        );

        let deleted = store.delete("to_delete").expect("delete should succeed");
        assert!(deleted, "delete should return true for existing skill");

        assert!(
            store.get("to_delete").unwrap().is_none(),
            "skill should not exist after delete"
        );

        let deleted_again = store
            .delete("to_delete")
            .expect("delete of missing should succeed");
        assert!(
            !deleted_again,
            "delete should return false for nonexistent skill"
        );
    }

    #[test]
    fn search_by_tag() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        store
            .upsert(
                "code_review",
                "Review code",
                "Analyze code for issues",
                None,
                &["code", "review"],
            )
            .expect("upsert code_review should succeed");
        store
            .upsert(
                "write_tests",
                "Write tests",
                "Generate test cases",
                None,
                &["code", "testing"],
            )
            .expect("upsert write_tests should succeed");
        store
            .upsert(
                "draft_email",
                "Draft an email",
                "Compose a professional email",
                None,
                &["writing", "communication"],
            )
            .expect("upsert draft_email should succeed");

        let code_skills = store
            .find_by_tag("code")
            .expect("find_by_tag should succeed");
        assert_eq!(code_skills.len(), 2);
        let code_names: Vec<&str> = code_skills.iter().map(|s| s.name.as_str()).collect();
        assert!(code_names.contains(&"code_review"));
        assert!(code_names.contains(&"write_tests"));

        let review_skills = store
            .find_by_tag("review")
            .expect("find_by_tag should succeed");
        assert_eq!(review_skills.len(), 1);
        assert_eq!(review_skills[0].name, "code_review");

        let writing_skills = store
            .find_by_tag("writing")
            .expect("find_by_tag should succeed");
        assert_eq!(writing_skills.len(), 1);
        assert_eq!(writing_skills[0].name, "draft_email");

        let no_match = store
            .find_by_tag("nonexistent")
            .expect("find_by_tag should succeed");
        assert!(no_match.is_empty());
    }

    #[test]
    fn list_all_paginated_returns_total_and_page() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SkillStore::new(&database_path);
        for i in 0..4 {
            store
                .upsert(&format!("skill_{i}"), "desc", "instr", None, &[])
                .expect("upsert should succeed");
        }

        let (page, total) = store.list_all_paginated(2, 0).expect("first page");
        assert_eq!(total, 4);
        assert_eq!(page.len(), 2);

        let (page2, total2) = store.list_all_paginated(2, 2).expect("second page");
        assert_eq!(total2, 4);
        assert_eq!(page2.len(), 2);
        assert_ne!(page[0].name, page2[0].name);
    }
}

