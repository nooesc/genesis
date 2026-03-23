use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::Database;
use crate::error::StorageError;
use crate::util::collect_rows;

/// Supporting files associated with a skill, stored in SQLite.
pub struct SkillFileStore {
    db: Database,
}

impl SkillFileStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    pub fn store_file(
        &self,
        skill_name: &str,
        file_path: &str,
        content: &str,
    ) -> Result<(), StorageError> {
        let connection = self.db.conn()?;
        connection
            .execute(
                "INSERT INTO skill_files (skill_name, file_path, content, created_at)
                 VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
                 ON CONFLICT(skill_name, file_path) DO UPDATE SET
                    content = excluded.content",
                params![skill_name, file_path, content],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(())
    }

    pub fn get_file(
        &self,
        skill_name: &str,
        file_path: &str,
    ) -> Result<Option<String>, StorageError> {
        let connection = self.db.conn()?;
        connection
            .query_row(
                "SELECT content FROM skill_files WHERE skill_name = ?1 AND file_path = ?2",
                params![skill_name, file_path],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })
    }

    pub fn list_files(&self, skill_name: &str) -> Result<Vec<String>, StorageError> {
        let connection = self.db.conn()?;
        let mut stmt = connection
            .prepare(
                "SELECT file_path FROM skill_files WHERE skill_name = ?1 ORDER BY file_path ASC",
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(params![skill_name], |row| row.get(0))
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        collect_rows(rows, self.db.path())
    }

    pub fn delete_file(&self, skill_name: &str, file_path: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "DELETE FROM skill_files WHERE skill_name = ?1 AND file_path = ?2",
                params![skill_name, file_path],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows > 0)
    }

    pub fn delete_all_files(&self, skill_name: &str) -> Result<u64, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "DELETE FROM skill_files WHERE skill_name = ?1",
                params![skill_name],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows as u64)
    }
}

#[cfg(test)]
mod skill_file_store_tests {
    use crate::{bootstrap, SkillStore};
    use super::SkillFileStore;
    use tempfile::tempdir;

    /// Helper: create a skill so the foreign key constraint is satisfied.
    fn create_skill(database_path: &std::path::Path, name: &str) {
        let store = SkillStore::new(database_path);
        store
            .upsert(name, "test skill", "do stuff", None, &[])
            .expect("skill upsert should succeed");
    }

    #[test]
    fn store_and_get_file() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        create_skill(&database_path, "my_skill");

        let store = SkillFileStore::new(&database_path);
        store
            .store_file("my_skill", "config.yaml", "key: value")
            .expect("store_file should succeed");

        let content = store
            .get_file("my_skill", "config.yaml")
            .expect("get_file should succeed")
            .expect("file should exist");

        assert_eq!(content, "key: value");
    }

    #[test]
    fn list_files_for_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        create_skill(&database_path, "multi_file_skill");

        let store = SkillFileStore::new(&database_path);
        store
            .store_file("multi_file_skill", "b_second.txt", "two")
            .expect("store second file");
        store
            .store_file("multi_file_skill", "a_first.txt", "one")
            .expect("store first file");
        store
            .store_file("multi_file_skill", "c_third.txt", "three")
            .expect("store third file");

        let files = store
            .list_files("multi_file_skill")
            .expect("list_files should succeed");

        assert_eq!(files.len(), 3);
        // list_files returns file_path ordered ASC
        assert_eq!(files[0], "a_first.txt");
        assert_eq!(files[1], "b_second.txt");
        assert_eq!(files[2], "c_third.txt");
    }

    #[test]
    fn delete_file() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        create_skill(&database_path, "del_skill");

        let store = SkillFileStore::new(&database_path);
        store
            .store_file("del_skill", "to_remove.txt", "ephemeral")
            .expect("store_file should succeed");

        let deleted = store
            .delete_file("del_skill", "to_remove.txt")
            .expect("delete_file should succeed");
        assert!(deleted, "delete should return true for existing file");

        let gone = store
            .get_file("del_skill", "to_remove.txt")
            .expect("get_file should succeed");
        assert!(gone.is_none(), "file should no longer exist after deletion");

        let deleted_again = store
            .delete_file("del_skill", "to_remove.txt")
            .expect("delete of missing file should succeed");
        assert!(
            !deleted_again,
            "delete should return false for nonexistent file"
        );
    }

    #[test]
    fn files_isolated_per_skill() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");
        create_skill(&database_path, "skill_a");
        create_skill(&database_path, "skill_b");

        let store = SkillFileStore::new(&database_path);
        store
            .store_file("skill_a", "shared_name.txt", "content from A")
            .expect("store file for skill_a");
        store
            .store_file("skill_b", "shared_name.txt", "content from B")
            .expect("store file for skill_b");
        store
            .store_file("skill_b", "extra.txt", "only in B")
            .expect("store extra file for skill_b");

        let files_a = store.list_files("skill_a").expect("list_files for skill_a");
        let files_b = store.list_files("skill_b").expect("list_files for skill_b");

        assert_eq!(files_a.len(), 1);
        assert_eq!(files_a[0], "shared_name.txt");
        assert_eq!(files_b.len(), 2);

        let content_a = store
            .get_file("skill_a", "shared_name.txt")
            .expect("get_file skill_a")
            .expect("file should exist for skill_a");
        let content_b = store
            .get_file("skill_b", "shared_name.txt")
            .expect("get_file skill_b")
            .expect("file should exist for skill_b");

        assert_eq!(content_a, "content from A");
        assert_eq!(content_b, "content from B");
    }
}

