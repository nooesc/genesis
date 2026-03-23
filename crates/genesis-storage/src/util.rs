use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, Connection};

use crate::error::StorageError;

static SQLITE_VEC_REGISTERED: OnceLock<()> = OnceLock::new();

pub(crate) fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, StorageError> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }
    if let Ok(parsed) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
        return Ok(parsed.and_utc());
    }
    Err(StorageError::InvalidTimestamp(value.to_owned()))
}

pub(crate) fn decayed_importance(
    importance: f32,
    accessed_at: &str,
    now: &DateTime<Utc>,
) -> Result<f32, StorageError> {
    let accessed_at = parse_timestamp(accessed_at)?;
    let days = (((*now) - accessed_at).num_seconds().max(0) as f32) / 86_400.0;
    Ok(importance * 0.99_f32.powf(days))
}

/// Return the retrieval weight multiplier for a given edge type.
pub(crate) fn edge_type_weight(edge_type: &str) -> f64 {
    match edge_type {
        "consolidation" => 1.2,
        "semantic" => 1.0,
        "causal" => 0.9,
        "temporal" => 0.7,
        "entity" => 0.6,
        _ => 0.5,
    }
}

pub(crate) fn sql_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Collect mapped rows into a Vec, converting any SQLite error into a StorageError.
pub(crate) fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    database_path: &Path,
) -> Result<Vec<T>, StorageError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })
}

/// Check whether a column exists in a table (used for idempotent migrations).
pub(crate) fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    // All callers pass compile-time string literals. Validate identifiers
    // unconditionally (not just in debug builds) as defense-in-depth.
    assert!(
        table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "column_exists: table name must be alphanumeric, got {table:?}"
    );
    assert!(
        column
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "column_exists: column name must be alphanumeric, got {column:?}"
    );
    conn.prepare(&format!("SELECT \"{column}\" FROM \"{table}\" LIMIT 0"))
        .is_ok()
}

/// Run a batch of SQL statements as a migration step.
pub(crate) fn exec_migration(
    conn: &Connection,
    path: &Path,
    sql: &str,
) -> Result<(), StorageError> {
    conn.execute_batch(sql)
        .map_err(|source| StorageError::Sqlite {
            path: path.to_path_buf(),
            source,
        })
}

pub(crate) fn register_sqlite_vec() {
    SQLITE_VEC_REGISTERED.get_or_init(|| unsafe {
        // SAFETY: `sqlite_vec::sqlite3_vec_init` is the sqlite-vec extension entry point
        // with the exact function signature expected by `sqlite3_auto_extension`.
        #[allow(clippy::missing_transmute_annotations)]
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

pub(crate) fn detect_uniform_embedding_dimensions(
    conn: &Connection,
    database_path: &Path,
) -> Result<Option<usize>, StorageError> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT dimensions FROM memory_embeddings ORDER BY dimensions ASC LIMIT 2",
        )
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    let dimensions = collect_rows(rows, database_path)?;

    match dimensions.as_slice() {
        [] => Ok(None),
        [dimension] => Ok(Some(*dimension as usize)),
        _ => Err(StorageError::MixedEmbeddingDimensions {
            path: database_path.to_path_buf(),
            dimensions,
        }),
    }
}

pub(crate) fn parse_memory_vec_dimensions(sql: &str) -> Option<usize> {
    let start = sql.find("float[")? + "float[".len();
    let end = sql[start..].find(']')? + start;
    sql[start..end].parse().ok()
}

pub(crate) fn memory_vec_declared_dimensions(
    conn: &Connection,
    database_path: &Path,
) -> Result<Option<usize>, StorageError> {
    use rusqlite::OptionalExtension;

    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memory_vec'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;

    match sql {
        None => Ok(None),
        Some(sql) => parse_memory_vec_dimensions(&sql)
            .ok_or(StorageError::InvalidVectorIndexSchema {
                path: database_path.to_path_buf(),
                sql,
            })
            .map(Some),
    }
}

pub(crate) fn create_memory_vec_table(
    conn: &Connection,
    database_path: &Path,
    dimensions: usize,
) -> Result<(), StorageError> {
    match exec_migration(
        conn,
        database_path,
        &format!(
            "CREATE VIRTUAL TABLE memory_vec USING vec0(
                memory_rowid integer primary key,
                embedding float[{dimensions}] distance_metric=cosine
            );"
        ),
    ) {
        Ok(()) => Ok(()),
        Err(StorageError::Sqlite { source, .. })
            if source.to_string().contains("already exists") =>
        {
            match memory_vec_declared_dimensions(conn, database_path)? {
                Some(existing) if existing == dimensions => Ok(()),
                Some(existing) => Err(StorageError::EmbeddingDimensionMismatch {
                    path: database_path.to_path_buf(),
                    expected: existing,
                    actual: dimensions,
                }),
                None => Err(StorageError::Sqlite {
                    path: database_path.to_path_buf(),
                    source,
                }),
            }
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn memory_embeddings_count(
    conn: &Connection,
    database_path: &Path,
) -> Result<usize, StorageError> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| {
            row.get(0)
        })
        .map_err(|source| StorageError::Sqlite {
            path: database_path.to_path_buf(),
            source,
        })?;
    Ok(count as usize)
}

pub(crate) fn ensure_memory_vec_table(
    conn: &Connection,
    database_path: &Path,
    dimensions: usize,
) -> Result<(), StorageError> {
    match memory_vec_declared_dimensions(conn, database_path)? {
        Some(existing) if existing == dimensions => Ok(()),
        Some(_existing) if memory_embeddings_count(conn, database_path)? == 0 => {
            conn.execute("DROP TABLE memory_vec", [])
                .map_err(|source| StorageError::Sqlite {
                    path: database_path.to_path_buf(),
                    source,
                })?;
            create_memory_vec_table(conn, database_path, dimensions)
        }
        Some(existing) => Err(StorageError::EmbeddingDimensionMismatch {
            path: database_path.to_path_buf(),
            expected: existing,
            actual: dimensions,
        }),
        None => {
            if let Some(existing) = detect_uniform_embedding_dimensions(conn, database_path)? {
                if existing != dimensions {
                    return Err(StorageError::EmbeddingDimensionMismatch {
                        path: database_path.to_path_buf(),
                        expected: existing,
                        actual: dimensions,
                    });
                }
            }
            create_memory_vec_table(conn, database_path, dimensions)
        }
    }
}

pub(crate) fn rebuild_memory_vec_index(
    conn: &Connection,
    database_path: &Path,
) -> Result<(), StorageError> {
    let dimensions = match detect_uniform_embedding_dimensions(conn, database_path) {
        Ok(Some(dimensions)) => dimensions,
        Ok(None) => return Ok(()),
        Err(StorageError::MixedEmbeddingDimensions { .. }) => return Ok(()),
        Err(error) => return Err(error),
    };

    ensure_memory_vec_table(conn, database_path, dimensions)?;
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         DELETE FROM memory_vec;
         INSERT INTO memory_vec(memory_rowid, embedding)
         SELECT m.rowid, me.embedding
         FROM memory_embeddings me
         JOIN memories m ON m.id = me.memory_id;
         COMMIT;",
    )
    .map_err(|source| StorageError::Sqlite {
        path: database_path.to_path_buf(),
        source,
    })?;
    Ok(())
}

pub(crate) fn sqlite_table_exists(
    conn: &Connection,
    database_path: &Path,
    table_name: &str,
) -> Result<bool, StorageError> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE (type = 'table' OR type = 'view') AND name = ?1
        )",
        params![table_name],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
    .map_err(|source| StorageError::Sqlite {
        path: database_path.to_path_buf(),
        source,
    })
}

pub(crate) fn memory_vec_table_exists(
    conn: &Connection,
    database_path: &Path,
) -> Result<bool, StorageError> {
    memory_vec_declared_dimensions(conn, database_path).map(|value| value.is_some())
}

pub(crate) fn open(database_path: &Path) -> Result<Connection, StorageError> {
    register_sqlite_vec();
    let conn = Connection::open(database_path).map_err(|source| StorageError::OpenDatabase {
        path: database_path.to_path_buf(),
        source,
    })?;

    // Performance PRAGMAs — WAL mode enables concurrent readers + single writer,
    // NORMAL sync is crash-safe in WAL mode, busy_timeout prevents SQLITE_BUSY
    // under concurrent access from gateway + CLI.
    // foreign_keys enables ON DELETE CASCADE for all connections (required per-connection).
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;
         PRAGMA foreign_keys = ON;",
    )
    .map_err(|source| StorageError::OpenDatabase {
        path: database_path.to_path_buf(),
        source,
    })?;

    Ok(conn)
}

pub(crate) fn first_existing_file<I>(mut candidates: I) -> Option<PathBuf>
where
    I: Iterator<Item = PathBuf>,
{
    candidates.find(|path| path.is_file())
}

pub(crate) fn first_existing_dir<I>(mut candidates: I) -> Option<PathBuf>
where
    I: Iterator<Item = PathBuf>,
{
    candidates.find(|path| path.is_dir())
}
