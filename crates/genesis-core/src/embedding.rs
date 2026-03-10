//! Embedding provider and vector search for semantic memory retrieval.
//!
//! Provides:
//! - `EmbeddingProvider` for OpenAI-compatible `/v1/embeddings` APIs
//! - `cosine_similarity` for comparing embedding vectors
//! - `hybrid_search` combining FTS5 keyword results with vector similarity
//!   via reciprocal rank fusion (RRF)

use genesis_config::EmbeddingConfig;
use genesis_storage::{EmbeddingStore, MemoryStore, StoredMemory};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
pub struct EmbeddingProvider {
    http: reqwest::Client,
    endpoint: String,
    model: String,
    dimensions: Option<usize>,
}

impl EmbeddingProvider {
    /// Create a new provider from an `EmbeddingConfig`.
    /// Resolves the API key from the environment using the same strategy
    /// as the main provider (explicit env var name, then standard fallbacks).
    pub fn from_config(config: &EmbeddingConfig) -> Result<Self, EmbeddingError> {
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
            model: resolved.model,
            dimensions: config.dimensions,
        })
    }

    /// The model name this provider is configured for.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Generate embeddings for a batch of texts.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let request = EmbeddingRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
            dimensions: self.dimensions,
        };

        let response = self
            .http
            .post(&self.endpoint)
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

        // Validate returned vector dimensions when configured
        if let Some(expected) = self.dimensions {
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

/// A memory search result with its combined score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredMemory {
    pub memory: StoredMemory,
    /// Combined score from hybrid search (higher is better).
    pub score: f64,
    /// Source of the match: "fts", "vector", or "hybrid".
    pub source: String,
}

/// Search mode for memory queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// FTS5 keyword search only (default, always available).
    #[default]
    Keyword,
    /// Vector similarity search only (requires embeddings).
    Vector,
    /// Hybrid: combine FTS5 and vector results via reciprocal rank fusion.
    Hybrid,
}

impl SearchMode {
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
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
    embedding_store: &EmbeddingStore,
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
        SearchMode::Vector => {
            let provider = provider.ok_or(EmbeddingError::NotConfigured)?;
            vector_search(query, limit, memory_store, embedding_store, provider).await
        }
        SearchMode::Hybrid => {
            let provider = provider.ok_or(EmbeddingError::NotConfigured)?;

            // Run FTS5 and vector search, then merge with RRF
            let fts_results = memory_store.search(query, limit * 2)?;
            let vector_results =
                vector_search(query, limit * 2, memory_store, embedding_store, provider).await?;

            Ok(reciprocal_rank_fusion(&fts_results, &vector_results, limit))
        }
    }
}

/// Vector-only search: embed the query, compare against all stored embeddings,
/// then fetch memory details only for the top-ranked results.
async fn vector_search(
    query: &str,
    limit: usize,
    memory_store: &MemoryStore,
    embedding_store: &EmbeddingStore,
    provider: &EmbeddingProvider,
) -> Result<Vec<ScoredMemory>, EmbeddingError> {
    let query_embedding = provider.embed_one(query).await?;
    let all_embeddings = embedding_store.all_embeddings()?;

    if all_embeddings.is_empty() {
        return Ok(Vec::new());
    }

    // Score all embeddings by cosine similarity (only needs IDs + vectors)
    let mut scored: Vec<(String, f32)> = all_embeddings
        .iter()
        .map(|(id, emb)| (id.clone(), cosine_similarity(&query_embedding, emb)))
        .collect();

    // Sort by similarity descending and keep only top results
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    // Fetch memory details only for the top-ranked results
    let mut results = Vec::with_capacity(scored.len());
    for (id, sim) in scored {
        if let Some(memory) = memory_store.get(&id)? {
            results.push(ScoredMemory {
                memory,
                score: sim as f64,
                source: "vector".to_owned(),
            });
        }
    }

    Ok(results)
}

/// Merge FTS5 and vector search results using Reciprocal Rank Fusion.
///
/// For each document appearing in either result list, compute:
///   score = sum(1 / (k + rank_in_list_i)) for each list containing it.
/// Sort by descending score, return top `limit`.
fn reciprocal_rank_fusion(
    fts_results: &[StoredMemory],
    vector_results: &[ScoredMemory],
    limit: usize,
) -> Vec<ScoredMemory> {
    use std::collections::HashMap;

    const K: f64 = 60.0;

    let mut scores: HashMap<String, (f64, Option<StoredMemory>)> = HashMap::new();

    // Add FTS5 results with their rank-based scores
    for (rank, memory) in fts_results.iter().enumerate() {
        let entry = scores.entry(memory.id.clone()).or_insert((0.0, None));
        entry.0 += 1.0 / (K + rank as f64);
        if entry.1.is_none() {
            entry.1 = Some(memory.clone());
        }
    }

    // Add vector results with their rank-based scores
    for (rank, scored) in vector_results.iter().enumerate() {
        let entry = scores
            .entry(scored.memory.id.clone())
            .or_insert((0.0, None));
        entry.0 += 1.0 / (K + rank as f64);
        if entry.1.is_none() {
            entry.1 = Some(scored.memory.clone());
        }
    }

    let mut merged: Vec<ScoredMemory> = scores
        .into_iter()
        .filter_map(|(_, (score, memory))| {
            memory.map(|m| ScoredMemory {
                memory: m,
                score,
                source: "hybrid".to_owned(),
            })
        })
        .collect();

    merged.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(limit);
    merged
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
        assert!(
            (sim - 0.7071068).abs() < 1e-4,
            "expected ~0.707, got {sim}"
        );
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
        assert!(sim2 > 0.0, "similar vectors should have positive similarity");
        assert!(sim2 < 1.0, "different vectors should have similarity < 1.0");
    }

    #[test]
    fn search_mode_from_str_opt() {
        assert_eq!(SearchMode::from_str_opt(None), SearchMode::Keyword);
        assert_eq!(
            SearchMode::from_str_opt(Some("keyword")),
            SearchMode::Keyword
        );
        assert_eq!(
            SearchMode::from_str_opt(Some("vector")),
            SearchMode::Vector
        );
        assert_eq!(
            SearchMode::from_str_opt(Some("hybrid")),
            SearchMode::Hybrid
        );
        assert_eq!(
            SearchMode::from_str_opt(Some("unknown")),
            SearchMode::Keyword
        );
    }

    #[test]
    fn reciprocal_rank_fusion_merges_results() {
        let fts = vec![
            StoredMemory {
                id: "m1".to_owned(),
                session_id: None,
                kind: "fact".to_owned(),
                content: "Rust is great".to_owned(),
                created_at: "2024-01-01".to_owned(),
            },
            StoredMemory {
                id: "m2".to_owned(),
                session_id: None,
                kind: "fact".to_owned(),
                content: "Python is popular".to_owned(),
                created_at: "2024-01-02".to_owned(),
            },
        ];

        let vector = vec![
            ScoredMemory {
                memory: StoredMemory {
                    id: "m2".to_owned(),
                    session_id: None,
                    kind: "fact".to_owned(),
                    content: "Python is popular".to_owned(),
                    created_at: "2024-01-02".to_owned(),
                },
                score: 0.95,
                source: "vector".to_owned(),
            },
            ScoredMemory {
                memory: StoredMemory {
                    id: "m3".to_owned(),
                    session_id: None,
                    kind: "fact".to_owned(),
                    content: "Go is fast".to_owned(),
                    created_at: "2024-01-03".to_owned(),
                },
                score: 0.8,
                source: "vector".to_owned(),
            },
        ];

        let results = reciprocal_rank_fusion(&fts, &vector, 10);

        assert_eq!(results.len(), 3);
        // m2 appears in both lists, so it should have the highest RRF score
        assert_eq!(
            results[0].memory.id, "m2",
            "m2 should rank first (in both lists)"
        );
        assert_eq!(results[0].source, "hybrid");
    }

    #[test]
    fn reciprocal_rank_fusion_respects_limit() {
        let fts: Vec<StoredMemory> = (0..10)
            .map(|i| StoredMemory {
                id: format!("m{i}"),
                session_id: None,
                kind: "fact".to_owned(),
                content: format!("memory {i}"),
                created_at: "2024-01-01".to_owned(),
            })
            .collect();

        let results = reciprocal_rank_fusion(&fts, &[], 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn embedding_store_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        // Create a session and memory for FK constraints
        let session_store = genesis_storage::SessionStore::new(&db_path);
        session_store
            .create_session("s1", "test", None)
            .unwrap();

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
        session_store
            .create_session("s1", "test", None)
            .unwrap();

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
        store.store("mem1", &[3.0, 4.0, 5.0], "model-b").unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let all = store.all_embeddings().unwrap();
        assert_eq!(all[0].1.len(), 3);
        assert!((all[0].1[0] - 3.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn hybrid_search_keyword_mode_delegates_to_fts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let session_store = genesis_storage::SessionStore::new(&db_path);
        session_store
            .create_session("s1", "test", None)
            .unwrap();

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
        let embedding_store = EmbeddingStore::new(&db_path);

        let results = hybrid_search(
            "genesis",
            10,
            SearchMode::Keyword,
            &memory_store,
            &embedding_store,
            None,
        )
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
        let embedding_store = EmbeddingStore::new(&db_path);

        let result = hybrid_search(
            "test",
            10,
            SearchMode::Vector,
            &memory_store,
            &embedding_store,
            None,
        )
        .await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), EmbeddingError::NotConfigured),
            "should fail with NotConfigured when no provider given"
        );
    }
}
