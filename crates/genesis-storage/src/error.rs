use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("failed to create storage directory for {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open sqlite database at {path}: {source}")]
    OpenDatabase {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("sqlite error at {path}: {source}")]
    Sqlite {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("database at {path} contains mixed embedding dimensions: {dimensions:?}")]
    MixedEmbeddingDimensions { path: PathBuf, dimensions: Vec<i64> },
    #[error("database at {path} uses embedding dimensions {expected}, cannot store vector with {actual}")]
    EmbeddingDimensionMismatch {
        path: PathBuf,
        expected: usize,
        actual: usize,
    },
    #[error("database at {path} has an unrecognized memory_vec schema: {sql}")]
    InvalidVectorIndexSchema { path: PathBuf, sql: String },
    #[error("connection pool mutex poisoned for database at {path}")]
    ConnectionPoolPoisoned { path: PathBuf },
    #[error("invalid timestamp: {0}")]
    InvalidTimestamp(String),
    #[error("unknown import status in database: {0}")]
    UnknownImportStatus(String),
}
