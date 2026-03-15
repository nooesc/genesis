use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{open, StorageError};

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

pub struct UserModelStore {
    database_path: PathBuf,
}

impl UserModelStore {
    pub fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
        }
    }

    pub fn observe(
        &self,
        trait_key: &str,
        category: &str,
        value: &str,
        source_session: Option<&str>,
    ) -> Result<StoredUserTrait, StorageError> {
        let connection = open(&self.database_path)?;

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
                path: self.database_path.clone(),
                source,
            })?;

        self.get(trait_key)?.ok_or_else(|| StorageError::Sqlite {
            path: self.database_path.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        })
    }

    pub fn get(&self, trait_key: &str) -> Result<Option<StoredUserTrait>, StorageError> {
        let connection = open(&self.database_path)?;
        connection
            .query_row(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE trait_key = ?1",
                params![trait_key],
                Self::row_to_trait,
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })
    }

    pub fn list_all(&self) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model ORDER BY confidence DESC, evidence_count DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let traits = stmt
            .query_map([], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(traits)
    }

    pub fn list_by_category(&self, category: &str) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE category = ?1 ORDER BY confidence DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let traits = stmt
            .query_map(params![category], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(traits)
    }

    pub fn confident_traits(&self, threshold: f64) -> Result<Vec<StoredUserTrait>, StorageError> {
        let connection = open(&self.database_path)?;
        let mut stmt = connection
            .prepare(
                "SELECT trait_key, category, value, confidence, evidence_count, source_session, created_at, updated_at
                 FROM user_model WHERE confidence >= ?1 ORDER BY confidence DESC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        let traits = stmt
            .query_map(params![threshold], Self::row_to_trait)
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
                source,
            })?;

        Ok(traits)
    }

    pub fn delete(&self, trait_key: &str) -> Result<bool, StorageError> {
        let connection = open(&self.database_path)?;
        let rows = connection
            .execute("DELETE FROM user_model WHERE trait_key = ?1", params![trait_key])
            .map_err(|source| StorageError::Sqlite {
                path: self.database_path.clone(),
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
