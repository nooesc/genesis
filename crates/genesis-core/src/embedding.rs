//! Embedding provider and vector search for semantic memory retrieval.
//!
//! Provides:
//! - `EmbeddingProvider` for OpenAI-compatible `/v1/embeddings` APIs
//! - `cosine_similarity` for comparing embedding vectors
//! - `hybrid_search` delegating keyword/vector/hybrid memory retrieval
//!   through `genesis-storage`

use genesis_config::EmbeddingConfig;
use genesis_storage::{EmbeddingStore, MemoryStore, ScoredMemory};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(feature = "local-embeddings")]
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding as FastTextEmbedding};

#[cfg(feature = "local-embeddings")]
use std::path::PathBuf;

#[cfg(feature = "local-embeddings")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "local-embeddings")]
const LOCAL_EMBEDDING_DIMENSIONS: usize = 384;

#[cfg(feature = "local-embeddings")]
const LOCAL_EMBEDDING_MODEL_NAME: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Errors from embedding operations.
#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("embedding API returned error status {status}: {body}")]
    ApiError { status: u16, body: String },
    #[error("embedding API returned empty data array")]
    EmptyResponse,
    #[error("storage error: {0}")]
    Storage(#[from] genesis_storage::StorageError),
    #[error("no embedding provider configured")]
    NotConfigured,
    #[error("embedding dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("failed to deserialize embedding response: {0}")]
    Deserialize(#[from] serde_json::Error),
}

/// Request body for the OpenAI embeddings API.
#[derive(Debug, Serialize)]
struct EmbeddingRequest {
    model: String,
    input: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

/// Response from the OpenAI embeddings API.
#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// OpenAI-compatible embedding provider.
/// Works with any API that implements the `/v1/embeddings` endpoint
/// (OpenAI, OpenRouter, Azure OpenAI, local vLLM, etc.).
pub struct RemoteEmbeddingProvider {
    http: reqwest::Client,
    endpoint: String,
    backend: String,
    model: String,
    dimensions: Option<usize>,
}

impl RemoteEmbeddingProvider {
    /// Create a new remote provider from an `EmbeddingConfig`.
    /// Resolves the API key from the environment using the same strategy
    /// as the main provider (explicit env var name, then standard fallbacks).
    fn from_config(config: &EmbeddingConfig) -> Result<Self, EmbeddingError> {
        let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let resolved = genesis_provider::resolve(
            &config.backend,
            &config.model,
            config.base_url.as_deref(),
            config.api_key_env.as_deref(),
            &env,
        );

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        if !resolved.api_key.is_empty() {
            let auth = format!("Bearer {}", resolved.api_key);
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&auth) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let base = resolved.base_url.trim_end_matches('/');
        let endpoint = format!("{base}/embeddings");

        Ok(Self {
            http,
            endpoint,
            backend: resolved.backend,
            model: resolved.model,
            dimensions: config.dimensions,
        })
    }
}

#[cfg(feature = "local-embeddings")]
fn local_embedding_cache_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("genesis")
        .join("models")
        .join("fastembed")
}

#[cfg(feature = "local-embeddings")]
fn resolve_local_embedding_model(
    config: &EmbeddingConfig,
) -> Result<(EmbeddingModel, String), EmbeddingError> {
    let requested_model = config.model.trim();
    let requested_dimensions = config.dimensions.unwrap_or(LOCAL_EMBEDDING_DIMENSIONS);

    if requested_dimensions != LOCAL_EMBEDDING_DIMENSIONS {
        return Err(EmbeddingError::ApiError {
            status: 400,
            body: format!(
                "local embedding backend requires dimensions={LOCAL_EMBEDDING_DIMENSIONS}, got {requested_dimensions}"
            ),
        });
    }

    if requested_model.eq_ignore_ascii_case(LOCAL_EMBEDDING_MODEL_NAME)
        || requested_model.eq_ignore_ascii_case("AllMiniLML6V2")
    {
        return Ok((
            EmbeddingModel::AllMiniLML6V2,
            LOCAL_EMBEDDING_MODEL_NAME.to_owned(),
        ));
    }

    Err(EmbeddingError::ApiError {
        status: 400,
        body: format!(
            "unsupported local embedding model '{requested_model}'; supported model: {LOCAL_EMBEDDING_MODEL_NAME} (alias: AllMiniLML6V2)"
        ),
    })
}

#[cfg(feature = "local-embeddings")]
pub struct LocalEmbeddingProvider {
    model: String,
    dimensions: usize,
    inner: Arc<Mutex<FastTextEmbedding>>,
}

#[cfg(not(feature = "local-embeddings"))]
#[derive(Debug, Clone)]
pub struct LocalEmbeddingProvider {
    model: String,
}

impl LocalEmbeddingProvider {
    fn from_config(config: &EmbeddingConfig) -> Result<Self, EmbeddingError> {
        #[cfg(feature = "local-embeddings")]
        {
            let (model, model_name) = resolve_local_embedding_model(config)?;
            let cache_dir = local_embedding_cache_dir();
            std::fs::create_dir_all(&cache_dir).map_err(|e| EmbeddingError::ApiError {
                status: 500,
                body: e.to_string(),
            })?;

            let init = InitOptions::new(model)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(true);
            let inner = FastTextEmbedding::try_new(init).map_err(|e| EmbeddingError::ApiError {
                status: 500,
                body: e.to_string(),
            })?;

            return Ok(Self {
                model: model_name,
                dimensions: LOCAL_EMBEDDING_DIMENSIONS,
                inner: Arc::new(Mutex::new(inner)),
            });
        }

        #[cfg(not(feature = "local-embeddings"))]
        Ok(Self {
            model: config.model.clone(),
        })
    }
}

/// Public embedding provider facade.
pub enum EmbeddingProvider {
    Remote(RemoteEmbeddingProvider),
    Local(LocalEmbeddingProvider),
}

impl EmbeddingProvider {
    /// Create a provider from an `EmbeddingConfig`.
    pub fn from_config(config: &EmbeddingConfig) -> Result<Self, EmbeddingError> {
        if config.is_local_backend() {
            Ok(Self::Local(LocalEmbeddingProvider::from_config(config)?))
        } else {
            Ok(Self::Remote(RemoteEmbeddingProvider::from_config(config)?))
        }
    }

    /// The backend name this provider is configured for.
    pub fn backend(&self) -> &str {
        match self {
            Self::Remote(provider) => &provider.backend,
            Self::Local(_) => "local",
        }
    }

    /// The model name this provider is configured for.
    pub fn model(&self) -> &str {
        match self {
            Self::Remote(provider) => &provider.model,
            Self::Local(provider) => &provider.model,
        }
    }

    /// Generate embeddings for a batch of texts.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        match self {
            Self::Remote(provider) => {
                let request = EmbeddingRequest {
                    model: provider.model.clone(),
                    input: texts.to_vec(),
                    dimensions: provider.dimensions,
                };

                let response = provider
                    .http
                    .post(&provider.endpoint)
                    .json(&request)
                    .send()
                    .await?;

                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_else(|e| e.to_string());
                    return Err(EmbeddingError::ApiError {
                        status: status.as_u16(),
                        body,
                    });
                }

                let response: EmbeddingResponse = response.json().await?;
                if response.data.is_empty() {
                    return Err(EmbeddingError::EmptyResponse);
                }

                if let Some(expected) = provider.dimensions {
                    for item in &response.data {
                        if item.embedding.len() != expected {
                            return Err(EmbeddingError::DimensionMismatch {
                                expected,
                                actual: item.embedding.len(),
                            });
                        }
                    }
                }

                Ok(response.data.into_iter().map(|d| d.embedding).collect())
            }
            #[cfg(feature = "local-embeddings")]
            Self::Local(provider) => {
                let inner = Arc::clone(&provider.inner);
                let texts = texts.to_vec();
                let expected_dimensions = provider.dimensions;

                tokio::task::spawn_blocking(move || {
                    let mut inner = inner.lock().map_err(|_| EmbeddingError::ApiError {
                        status: 500,
                        body: "embedding model lock poisoned".to_owned(),
                    })?;
                    let embeddings =
                        inner
                            .embed(&texts, None)
                            .map_err(|e| EmbeddingError::ApiError {
                                status: 500,
                                body: e.to_string(),
                            })?;

                    for embedding in &embeddings {
                        if embedding.len() != expected_dimensions {
                            return Err(EmbeddingError::DimensionMismatch {
                                expected: expected_dimensions,
                                actual: embedding.len(),
                            });
                        }
                    }

                    Ok(embeddings)
                })
                .await
                .map_err(|error| EmbeddingError::ApiError {
                    status: 500,
                    body: format!("local embedding task failed: {error}"),
                })?
            }
            #[cfg(not(feature = "local-embeddings"))]
            Self::Local(_provider) => {
                let _ = texts;
                Err(EmbeddingError::NotConfigured)
            }
        }
    }

    /// Generate a single embedding.
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let results = self.embed(&[text.to_owned()]).await?;
        results
            .into_iter()
            .next()
            .ok_or(EmbeddingError::EmptyResponse)
    }
}

/// Compute cosine similarity between two vectors.
/// Returns a value in [-1, 1] where 1 means identical direction.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;

    for (&ai, &bi) in a.iter().zip(b.iter()) {
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < f32::EPSILON {
        return 0.0;
    }

    dot / denom
}

/// Search mode for memory queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// FTS5 keyword search only (default, always available).
    #[default]
    Keyword,
    /// Graph search: keyword primaries plus linked memories with decay-aware ranking.
    Graph,
    /// Vector similarity search only (requires embeddings).
    Vector,
    /// Hybrid: combine FTS5 and vector results via reciprocal rank fusion.
    Hybrid,
}

impl SearchMode {
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("graph") => Self::Graph,
            Some("vector") => Self::Vector,
            Some("hybrid") => Self::Hybrid,
            _ => Self::Keyword,
        }
    }
}

/// Perform hybrid memory search combining FTS5 keyword matching with
/// vector similarity, using Reciprocal Rank Fusion (RRF) to merge results.
///
/// RRF score for each document: sum(1 / (k + rank_i)) across all result lists.
/// The constant k (default 60) dampens the influence of high-ranked items.
pub async fn hybrid_search(
    query: &str,
    limit: usize,
    mode: SearchMode,
    memory_store: &MemoryStore,
    provider: Option<&EmbeddingProvider>,
) -> Result<Vec<ScoredMemory>, EmbeddingError> {
    match mode {
        SearchMode::Keyword => {
            let results = memory_store.search(query, limit)?;
            Ok(results
                .into_iter()
                .enumerate()
                .map(|(i, memory)| ScoredMemory {
                    memory,
                    score: 1.0 / (60.0 + i as f64),
                    source: "fts".to_owned(),
                })
                .collect())
        }
        SearchMode::Graph => memory_store
            .graph_search(query, limit)
            .map_err(EmbeddingError::from),
        SearchMode::Vector => {
            let provider = provider.ok_or(EmbeddingError::NotConfigured)?;
            let query_embedding = provider.embed_one(query).await?;
            memory_store
                .vector_search(&query_embedding, limit)
                .map_err(EmbeddingError::from)
        }
        SearchMode::Hybrid => {
            let provider = provider.ok_or(EmbeddingError::NotConfigured)?;
            let query_embedding = provider.embed_one(query).await?;
            memory_store
                .hybrid_search(query, &query_embedding, limit)
                .map_err(EmbeddingError::from)
        }
    }
}

/// Embed a memory and store its embedding.
/// Called when a memory is stored, if an embedding provider is configured.
pub async fn embed_and_store(
    memory_id: &str,
    content: &str,
    embedding_store: &EmbeddingStore,
    provider: &EmbeddingProvider,
    model: &str,
) -> Result<(), EmbeddingError> {
    let embedding = provider.embed_one(content).await?;
    embedding_store.store(memory_id, &embedding, model)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_identical_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "identical vectors should have similarity 1.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim.abs() < 1e-6,
            "orthogonal vectors should have similarity 0.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - (-1.0)).abs() < 1e-6,
            "opposite vectors should have similarity -1.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_handles_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_similarity_handles_mismatched_lengths() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_similarity_normalized_vectors() {
        // Two unit vectors at 45 degrees: cos(45) = sqrt(2)/2 ~ 0.7071
        let a = vec![1.0, 0.0];
        let b = vec![0.7071068, 0.7071068];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.7071068).abs() < 1e-4, "expected ~0.707, got {sim}");
    }

    #[test]
    fn cosine_similarity_with_real_like_embeddings() {
        // Simulate small embedding vectors
        let a = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let b = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - 1.0).abs() < 1e-5,
            "same vector should give 1.0, got {sim}"
        );

        let c = vec![0.5, 0.4, 0.3, 0.2, 0.1];
        let sim2 = cosine_similarity(&a, &c);
        assert!(
            sim2 > 0.0,
            "similar vectors should have positive similarity"
        );
        assert!(sim2 < 1.0, "different vectors should have similarity < 1.0");
    }

    #[test]
    fn search_mode_from_str_opt() {
        assert_eq!(SearchMode::from_str_opt(None), SearchMode::Keyword);
        assert_eq!(
            SearchMode::from_str_opt(Some("keyword")),
            SearchMode::Keyword
        );
        assert_eq!(SearchMode::from_str_opt(Some("vector")), SearchMode::Vector);
        assert_eq!(SearchMode::from_str_opt(Some("graph")), SearchMode::Graph);
        assert_eq!(SearchMode::from_str_opt(Some("hybrid")), SearchMode::Hybrid);
        assert_eq!(
            SearchMode::from_str_opt(Some("unknown")),
            SearchMode::Keyword
        );
    }

    #[test]
    fn embedding_store_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        // Create a session and memory for FK constraints
        let session_store = genesis_storage::SessionStore::new(&db_path);
        session_store.create_session("s1", "test", None).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem1', 's1', 'fact', 'hello world', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        drop(conn);

        let store = EmbeddingStore::new(&db_path);

        // Store an embedding
        let embedding = vec![0.1, 0.2, 0.3, 0.4];
        store.store("mem1", &embedding, "test-model").unwrap();

        // Check it exists
        assert!(store.has_embedding("mem1").unwrap());
        assert!(!store.has_embedding("nonexistent").unwrap());

        // Count
        assert_eq!(store.count().unwrap(), 1);

        // Retrieve all embeddings
        let all = store.all_embeddings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "mem1");
        assert_eq!(all[0].1.len(), 4);
        assert!((all[0].1[0] - 0.1).abs() < 1e-6);
        assert!((all[0].1[3] - 0.4).abs() < 1e-6);

        // Delete
        assert!(store.delete("mem1").unwrap());
        assert!(!store.has_embedding("mem1").unwrap());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn embedding_store_upsert_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let session_store = genesis_storage::SessionStore::new(&db_path);
        session_store.create_session("s1", "test", None).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem1', 's1', 'fact', 'test', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        drop(conn);

        let store = EmbeddingStore::new(&db_path);
        store.store("mem1", &[1.0, 2.0], "model-a").unwrap();
        store.store("mem1", &[3.0, 4.0], "model-b").unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let all = store.all_embeddings().unwrap();
        assert_eq!(all[0].1.len(), 2);
        assert!((all[0].1[0] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn embedding_store_rejects_dimension_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let session_store = genesis_storage::SessionStore::new(&db_path);
        session_store.create_session("s1", "test", None).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem1', 's1', 'fact', 'test', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem2', 's1', 'fact', 'test', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        drop(conn);

        let store = EmbeddingStore::new(&db_path);
        store.store("mem1", &[1.0, 2.0], "model-a").unwrap();

        let error = store
            .store("mem2", &[3.0, 4.0, 5.0], "model-b")
            .expect_err("mixed dimensions should be rejected");

        assert!(matches!(
            error,
            genesis_storage::StorageError::EmbeddingDimensionMismatch {
                expected: 2,
                actual: 3,
                ..
            }
        ));
    }

    #[test]
    fn local_backend_builds_without_api_key() {
        let config = genesis_config::EmbeddingConfig {
            backend: "local".to_owned(),
            model: "sentence-transformers/all-MiniLM-L6-v2".to_owned(),
            base_url: None,
            api_key_env: None,
            dimensions: Some(384),
        };

        let provider = EmbeddingProvider::from_config(&config).expect("provider should build");
        assert!(matches!(provider, EmbeddingProvider::Local(_)));
        assert_eq!(provider.backend(), "local");
        assert_eq!(provider.model(), "sentence-transformers/all-MiniLM-L6-v2");
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn local_provider_returns_non_zero_embeddings() {
        let config = genesis_config::EmbeddingConfig {
            backend: "local".to_owned(),
            model: "sentence-transformers/all-MiniLM-L6-v2".to_owned(),
            base_url: None,
            api_key_env: None,
            dimensions: Some(384),
        };

        let provider = EmbeddingProvider::from_config(&config).unwrap();
        let vectors = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(provider.embed(&["query: genesis memory".to_owned()]))
            .unwrap();

        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), 384);
        assert!(vectors[0].iter().any(|value| value.abs() > 0.0));
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn local_backend_rejects_unsupported_model_names() {
        let config = genesis_config::EmbeddingConfig {
            backend: "local".to_owned(),
            model: "sentence-transformers/all-mpnet-base-v2".to_owned(),
            base_url: None,
            api_key_env: None,
            dimensions: Some(384),
        };

        let error = match EmbeddingProvider::from_config(&config) {
            Ok(_) => panic!("unsupported local model should fail"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("unsupported local embedding model"),
            "unexpected error: {error}"
        );
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn local_backend_accepts_fastembed_alias_and_reports_canonical_model() {
        let config = genesis_config::EmbeddingConfig {
            backend: "local".to_owned(),
            model: "AllMiniLML6V2".to_owned(),
            base_url: None,
            api_key_env: None,
            dimensions: Some(384),
        };

        let provider = EmbeddingProvider::from_config(&config).expect("provider should build");
        assert_eq!(provider.model(), "sentence-transformers/all-MiniLM-L6-v2");
    }

    #[tokio::test]
    async fn hybrid_search_keyword_mode_delegates_to_fts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let session_store = genesis_storage::SessionStore::new(&db_path);
        session_store.create_session("s1", "test", None).unwrap();

        // Insert a memory and index it
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(
                memory_row_id UNINDEXED, kind, content
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at)
             VALUES ('mem1', 's1', 'project', 'genesis is a rust agent', CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO memory_search (memory_row_id, kind, content) VALUES (?1, 'project', 'genesis is a rust agent')",
            rusqlite::params![rowid],
        )
        .unwrap();
        drop(conn);

        let memory_store = MemoryStore::new(&db_path);
        let results = hybrid_search("genesis", 10, SearchMode::Keyword, &memory_store, None)
            .await
            .expect("keyword search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.id, "mem1");
        assert_eq!(results[0].source, "fts");
    }

    #[tokio::test]
    async fn hybrid_search_vector_mode_fails_without_provider() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let memory_store = MemoryStore::new(&db_path);
        let result = hybrid_search("test", 10, SearchMode::Vector, &memory_store, None).await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), EmbeddingError::NotConfigured),
            "should fail with NotConfigured when no provider given"
        );
    }
}
