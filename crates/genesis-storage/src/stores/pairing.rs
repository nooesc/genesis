use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::StorageError;
use crate::util::collect_rows;
use crate::Database;

// ===========================================================================
// PairingStore — DM pairing system for messaging platform authorization
// ===========================================================================

/// An approved (paired) user on a messaging platform.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApprovedUser {
    pub platform: String,
    pub user_id: String,
    pub user_name: String,
    pub approved_at: String,
}

/// A pending pairing request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingPairing {
    pub platform: String,
    pub code: String,
    pub user_id: String,
    pub user_name: String,
    pub created_at: String,
}

/// Code-based approval flow for authorizing users on messaging platforms.
///
/// Instead of static allowlists, unknown users receive a one-time pairing
/// code that the bot owner approves via the CLI or API.
pub struct PairingStore {
    db: Database,
}

/// Unambiguous alphabet for pairing codes (no 0/O, 1/I).
const PAIRING_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const PAIRING_CODE_LENGTH: usize = 8;
/// Codes expire after 1 hour.
const PAIRING_CODE_TTL_SECS: i64 = 3600;
/// Max pending codes per platform.
const MAX_PENDING_PER_PLATFORM: usize = 3;

fn generate_pairing_code() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Use a combination of time-based entropy and process-level randomness.
    // Not cryptographic, but adequate for pairing codes with short TTL.
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let mut state = seed ^ (std::process::id() as u128) ^ 0xDEAD_BEEF_CAFE_BABE;
    let mut code = String::with_capacity(PAIRING_CODE_LENGTH);
    for _ in 0..PAIRING_CODE_LENGTH {
        // xorshift-style mixing
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let idx = (state as usize) % PAIRING_ALPHABET.len();
        code.push(PAIRING_ALPHABET[idx] as char);
    }
    code
}

impl PairingStore {
    pub fn new(database_path: &Path) -> Self {
        Self::with_db(Database::new(database_path))
    }

    /// Create from a shared [`Database`] handle — multiple stores can share
    /// the same pooled connection.
    pub fn with_db(db: Database) -> Self {
        Self { db }
    }

    /// Delete pending pairing codes that have exceeded their TTL.
    fn cleanup_expired_codes(
        connection: &Connection,
        database_path: &Path,
        expiry: &str,
    ) -> Result<(), StorageError> {
        connection
            .execute(
                "DELETE FROM pairing_pending
                 WHERE created_at < datetime('now', ?1)",
                params![expiry],
            )
            .map_err(|source| StorageError::Sqlite {
                path: database_path.to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Check if a user is approved (paired) on a platform.
    pub fn is_approved(&self, platform: &str, user_id: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pairing_approved
                 WHERE platform = ?1 AND user_id = ?2",
                params![platform, user_id],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(count > 0)
    }

    /// List all approved users, optionally filtered by platform.
    pub fn list_approved(&self, platform: Option<&str>) -> Result<Vec<ApprovedUser>, StorageError> {
        let connection = self.db.conn()?;
        let db = self.db.path();
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.to_path_buf(),
            source,
        };

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<ApprovedUser> {
            Ok(ApprovedUser {
                platform: row.get(0)?,
                user_id: row.get(1)?,
                user_name: row.get(2)?,
                approved_at: row.get(3)?,
            })
        };

        let users = if let Some(p) = platform {
            let mut stmt = connection
                .prepare(
                    "SELECT platform, user_id, user_name, approved_at
                     FROM pairing_approved WHERE platform = ?1
                     ORDER BY approved_at DESC",
                )
                .map_err(me)?;
            let rows = stmt.query_map(params![p], map_row).map_err(me)?;
            collect_rows(rows, self.db.path())?
        } else {
            let mut stmt = connection
                .prepare(
                    "SELECT platform, user_id, user_name, approved_at
                     FROM pairing_approved
                     ORDER BY platform, approved_at DESC",
                )
                .map_err(me)?;
            let rows = stmt.query_map([], map_row).map_err(me)?;
            collect_rows(rows, self.db.path())?
        };

        Ok(users)
    }

    /// Generate a pairing code for a new user.
    ///
    /// Returns `None` if the platform already has the max number of pending
    /// codes, or if the user is already approved.
    pub fn generate_code(
        &self,
        platform: &str,
        user_id: &str,
        user_name: &str,
    ) -> Result<Option<String>, StorageError> {
        // Don't generate if already approved
        if self.is_approved(platform, user_id)? {
            return Ok(None);
        }

        let connection = self.db.conn()?;

        // Clean up expired codes
        Self::cleanup_expired_codes(
            &connection,
            self.db.path(),
            &format!("-{PAIRING_CODE_TTL_SECS} seconds"),
        )?;

        // Check if we've hit the max pending for this platform
        let pending_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pairing_pending WHERE platform = ?1",
                params![platform],
                |row| row.get(0),
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        if pending_count as usize >= MAX_PENDING_PER_PLATFORM {
            return Ok(None);
        }

        let code = generate_pairing_code();

        connection
            .execute(
                "INSERT OR REPLACE INTO pairing_pending
                 (platform, code, user_id, user_name, created_at)
                 VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
                params![platform, &code, user_id, user_name],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        Ok(Some(code))
    }

    /// Approve a pairing code, moving the user to the approved list.
    ///
    /// Returns the approved user info, or `None` if the code is invalid/expired.
    pub fn approve_code(
        &self,
        platform: &str,
        code: &str,
    ) -> Result<Option<ApprovedUser>, StorageError> {
        let code = code.to_uppercase();
        let connection = self.db.conn()?;

        // Clean up expired codes first
        Self::cleanup_expired_codes(
            &connection,
            self.db.path(),
            &format!("-{PAIRING_CODE_TTL_SECS} seconds"),
        )?;

        // Find the pending code
        let pending = connection
            .query_row(
                "SELECT user_id, user_name FROM pairing_pending
                 WHERE platform = ?1 AND code = ?2",
                params![platform, &code],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        let Some((user_id, user_name)) = pending else {
            return Ok(None);
        };

        // Remove the pending code
        connection
            .execute(
                "DELETE FROM pairing_pending WHERE platform = ?1 AND code = ?2",
                params![platform, &code],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Add to approved users
        connection
            .execute(
                "INSERT OR REPLACE INTO pairing_approved
                 (platform, user_id, user_name, approved_at)
                 VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)",
                params![platform, &user_id, &user_name],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        // Retrieve the approved user for the return value
        let approved = connection
            .query_row(
                "SELECT platform, user_id, user_name, approved_at
                 FROM pairing_approved WHERE platform = ?1 AND user_id = ?2",
                params![platform, &user_id],
                |row| {
                    Ok(ApprovedUser {
                        platform: row.get(0)?,
                        user_id: row.get(1)?,
                        user_name: row.get(2)?,
                        approved_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;

        Ok(approved)
    }

    /// List pending pairing requests, optionally filtered by platform.
    pub fn list_pending(
        &self,
        platform: Option<&str>,
    ) -> Result<Vec<PendingPairing>, StorageError> {
        let connection = self.db.conn()?;
        let db = self.db.path();
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.to_path_buf(),
            source,
        };

        // Clean up expired first
        Self::cleanup_expired_codes(
            &connection,
            self.db.path(),
            &format!("-{PAIRING_CODE_TTL_SECS} seconds"),
        )?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<PendingPairing> {
            Ok(PendingPairing {
                platform: row.get(0)?,
                code: row.get(1)?,
                user_id: row.get(2)?,
                user_name: row.get(3)?,
                created_at: row.get(4)?,
            })
        };

        let pending = if let Some(p) = platform {
            let mut stmt = connection
                .prepare(
                    "SELECT platform, code, user_id, user_name, created_at
                     FROM pairing_pending WHERE platform = ?1
                     ORDER BY created_at DESC",
                )
                .map_err(me)?;
            let rows = stmt.query_map(params![p], map_row).map_err(me)?;
            collect_rows(rows, self.db.path())?
        } else {
            let mut stmt = connection
                .prepare(
                    "SELECT platform, code, user_id, user_name, created_at
                     FROM pairing_pending
                     ORDER BY platform, created_at DESC",
                )
                .map_err(me)?;
            let rows = stmt.query_map([], map_row).map_err(me)?;
            collect_rows(rows, self.db.path())?
        };

        Ok(pending)
    }

    /// Revoke an approved user's access.
    pub fn revoke(&self, platform: &str, user_id: &str) -> Result<bool, StorageError> {
        let connection = self.db.conn()?;
        let rows = connection
            .execute(
                "DELETE FROM pairing_approved WHERE platform = ?1 AND user_id = ?2",
                params![platform, user_id],
            )
            .map_err(|source| StorageError::Sqlite {
                path: self.db.path().to_path_buf(),
                source,
            })?;
        Ok(rows > 0)
    }

    /// Clear all pending codes, optionally filtered by platform.
    pub fn clear_pending(&self, platform: Option<&str>) -> Result<usize, StorageError> {
        let connection = self.db.conn()?;
        let rows = if let Some(p) = platform {
            connection
                .execute(
                    "DELETE FROM pairing_pending WHERE platform = ?1",
                    params![p],
                )
                .map_err(|source| StorageError::Sqlite {
                    path: self.db.path().to_path_buf(),
                    source,
                })?
        } else {
            connection
                .execute("DELETE FROM pairing_pending", [])
                .map_err(|source| StorageError::Sqlite {
                    path: self.db.path().to_path_buf(),
                    source,
                })?
        };
        Ok(rows)
    }

    /// List approved users with offset/limit pagination.
    pub fn list_approved_paginated(
        &self,
        platform: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ApprovedUser>, u64), StorageError> {
        let connection = self.db.conn()?;
        let db = self.db.path();
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.to_path_buf(),
            source,
        };

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<ApprovedUser> {
            Ok(ApprovedUser {
                platform: row.get(0)?,
                user_id: row.get(1)?,
                user_name: row.get(2)?,
                approved_at: row.get(3)?,
            })
        };

        let (total, items) = if let Some(p) = platform {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pairing_approved WHERE platform = ?1",
                    params![p],
                    |row| row.get(0),
                )
                .map_err(me)?;
            let mut stmt = connection
                .prepare(
                    "SELECT platform, user_id, user_name, approved_at
                     FROM pairing_approved WHERE platform = ?1
                     ORDER BY approved_at DESC
                     LIMIT ?2 OFFSET ?3",
                )
                .map_err(me)?;
            let rows = stmt
                .query_map(params![p, limit as i64, offset as i64], map_row)
                .map_err(me)?;
            (t, collect_rows(rows, self.db.path())?)
        } else {
            let t: i64 = connection
                .query_row("SELECT COUNT(*) FROM pairing_approved", [], |row| {
                    row.get(0)
                })
                .map_err(me)?;
            let mut stmt = connection
                .prepare(
                    "SELECT platform, user_id, user_name, approved_at
                     FROM pairing_approved
                     ORDER BY platform, approved_at DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(me)?;
            let rows = stmt
                .query_map(params![limit as i64, offset as i64], map_row)
                .map_err(me)?;
            (t, collect_rows(rows, self.db.path())?)
        };

        Ok((items, total as u64))
    }

    /// List pending pairings with offset/limit pagination.
    pub fn list_pending_paginated(
        &self,
        platform: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<PendingPairing>, u64), StorageError> {
        let connection = self.db.conn()?;
        let db = self.db.path();
        let me = |source: rusqlite::Error| StorageError::Sqlite {
            path: db.to_path_buf(),
            source,
        };

        // Clean up expired first
        Self::cleanup_expired_codes(
            &connection,
            self.db.path(),
            &format!("-{PAIRING_CODE_TTL_SECS} seconds"),
        )?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<PendingPairing> {
            Ok(PendingPairing {
                platform: row.get(0)?,
                code: row.get(1)?,
                user_id: row.get(2)?,
                user_name: row.get(3)?,
                created_at: row.get(4)?,
            })
        };

        let (total, items) = if let Some(p) = platform {
            let t: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pairing_pending WHERE platform = ?1",
                    params![p],
                    |row| row.get(0),
                )
                .map_err(me)?;
            let mut stmt = connection
                .prepare(
                    "SELECT platform, code, user_id, user_name, created_at
                     FROM pairing_pending WHERE platform = ?1
                     ORDER BY created_at DESC
                     LIMIT ?2 OFFSET ?3",
                )
                .map_err(me)?;
            let rows = stmt
                .query_map(params![p, limit as i64, offset as i64], map_row)
                .map_err(me)?;
            (t, collect_rows(rows, self.db.path())?)
        } else {
            let t: i64 = connection
                .query_row("SELECT COUNT(*) FROM pairing_pending", [], |row| row.get(0))
                .map_err(me)?;
            let mut stmt = connection
                .prepare(
                    "SELECT platform, code, user_id, user_name, created_at
                     FROM pairing_pending
                     ORDER BY platform, created_at DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(me)?;
            let rows = stmt
                .query_map(params![limit as i64, offset as i64], map_row)
                .map_err(me)?;
            (t, collect_rows(rows, self.db.path())?)
        };

        Ok((items, total as u64))
    }
}

#[cfg(test)]
mod pairing_store_tests {
    use super::PairingStore;
    use crate::bootstrap;
    use tempfile::tempdir;

    #[test]
    fn generate_and_verify_pairing_code() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        let code = store
            .generate_code("telegram", "user123", "Alice")
            .expect("generate_code should succeed");
        assert!(code.is_some(), "code should be generated");

        let pending = store
            .list_pending(Some("telegram"))
            .expect("list_pending should succeed");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].code, code.unwrap());
        assert_eq!(pending[0].user_id, "user123");
        assert_eq!(pending[0].user_name, "Alice");
        assert_eq!(pending[0].platform, "telegram");
    }

    #[test]
    fn approve_code_moves_to_approved() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        let code = store
            .generate_code("discord", "user456", "Bob")
            .expect("generate_code should succeed")
            .expect("code should be generated");

        let approved = store
            .approve_code("discord", &code)
            .expect("approve_code should succeed");
        assert!(approved.is_some(), "approved user should be returned");

        let approved_user = approved.unwrap();
        assert_eq!(approved_user.platform, "discord");
        assert_eq!(approved_user.user_id, "user456");
        assert_eq!(approved_user.user_name, "Bob");

        // Verify it appears in the approved list
        let approved_list = store
            .list_approved(Some("discord"))
            .expect("list_approved should succeed");
        assert_eq!(approved_list.len(), 1);
        assert_eq!(approved_list[0].user_id, "user456");

        // Verify it's no longer in the pending list
        let pending = store
            .list_pending(Some("discord"))
            .expect("list_pending should succeed");
        assert!(
            pending.is_empty(),
            "pending list should be empty after approval"
        );
    }

    #[test]
    fn revoke_user_removes_from_approved() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        // Generate and approve
        let code = store
            .generate_code("slack", "user789", "Charlie")
            .expect("generate_code should succeed")
            .expect("code should be generated");
        store
            .approve_code("slack", &code)
            .expect("approve_code should succeed");

        // Confirm approved
        assert!(
            store
                .is_approved("slack", "user789")
                .expect("is_approved should succeed"),
            "user should be approved"
        );

        // Revoke
        let revoked = store
            .revoke("slack", "user789")
            .expect("revoke should succeed");
        assert!(revoked, "revoke should return true for existing user");

        // Confirm no longer approved
        assert!(
            !store
                .is_approved("slack", "user789")
                .expect("is_approved should succeed"),
            "user should no longer be approved after revocation"
        );

        let approved_list = store
            .list_approved(Some("slack"))
            .expect("list_approved should succeed");
        assert!(
            approved_list.is_empty(),
            "approved list should be empty after revocation"
        );
    }

    #[test]
    fn expired_codes_not_returned() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        let code = store
            .generate_code("telegram", "user_exp", "Expired User")
            .expect("generate_code should succeed")
            .expect("code should be generated");

        // Verify it's in pending
        let pending = store
            .list_pending(Some("telegram"))
            .expect("list_pending should succeed");
        assert_eq!(pending.len(), 1);

        // Backdate the code to 2 hours ago (beyond the 1-hour TTL)
        let conn =
            rusqlite::Connection::open(&database_path).expect("open connection should succeed");
        conn.execute(
            "UPDATE pairing_pending SET created_at = datetime('now', '-7200 seconds')
             WHERE code = ?1",
            rusqlite::params![code],
        )
        .expect("backdate should succeed");

        // list_pending cleans up expired codes, so the expired code should not be returned
        let pending_after = store
            .list_pending(Some("telegram"))
            .expect("list_pending should succeed");
        assert!(
            pending_after.is_empty(),
            "expired codes should not appear in pending list"
        );

        // Trying to approve an expired code should also fail
        let approved = store
            .approve_code("telegram", &code)
            .expect("approve_code should succeed");
        assert!(approved.is_none(), "expired code should not be approvable");
    }

    #[test]
    fn is_approved_returns_correct_status() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        // Unknown user should not be approved
        assert!(
            !store
                .is_approved("telegram", "unknown_user")
                .expect("is_approved should succeed"),
            "unknown user should not be approved"
        );

        // Generate and approve a user
        let code = store
            .generate_code("telegram", "known_user", "Known")
            .expect("generate_code should succeed")
            .expect("code should be generated");
        store
            .approve_code("telegram", &code)
            .expect("approve_code should succeed");

        // Approved user should return true
        assert!(
            store
                .is_approved("telegram", "known_user")
                .expect("is_approved should succeed"),
            "approved user should return true"
        );

        // Different platform should return false
        assert!(
            !store
                .is_approved("discord", "known_user")
                .expect("is_approved should succeed"),
            "user approved on different platform should return false"
        );
    }

    #[test]
    fn clear_pending_removes_codes() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = PairingStore::new(&database_path);

        // Generate codes on two platforms. Sleep between calls to avoid
        // XORShift seed collisions from identical nanosecond timestamps.
        store
            .generate_code("telegram", "user_a", "A")
            .expect("generate_code should succeed");
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .generate_code("telegram", "user_b", "B")
            .expect("generate_code should succeed");
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .generate_code("discord", "user_c", "C")
            .expect("generate_code should succeed");

        // Clear only telegram pending codes
        let cleared = store
            .clear_pending(Some("telegram"))
            .expect("clear_pending should succeed");
        assert_eq!(cleared, 2);

        // Telegram pending should be empty
        let tg_pending = store
            .list_pending(Some("telegram"))
            .expect("list_pending should succeed");
        assert!(
            tg_pending.is_empty(),
            "telegram pending should be empty after clear"
        );

        // Discord pending should still exist
        let dc_pending = store
            .list_pending(Some("discord"))
            .expect("list_pending should succeed");
        assert_eq!(dc_pending.len(), 1);

        // Clear all remaining
        let cleared_all = store
            .clear_pending(None)
            .expect("clear_pending should succeed");
        assert_eq!(cleared_all, 1);

        let all_pending = store
            .list_pending(None)
            .expect("list_pending should succeed");
        assert!(
            all_pending.is_empty(),
            "all pending should be empty after clear_pending(None)"
        );
    }
}
