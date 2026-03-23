//! Memory listing, searching, embedding, and deletion handlers.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use genesis_storage::{EmbeddingStore, MemoryStore};
use serde::Deserialize;
use tracing::warn;

use crate::helpers::*;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

/// Query parameters for listing memories.
#[derive(Debug, Deserialize)]
pub(crate) struct ListMemoriesQuery {
    #[serde(default = "default_page_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

/// Query parameters for searching memories.
#[derive(Debug, Deserialize)]
pub(crate) struct SearchMemoriesQuery {
    pub q: String,
    #[serde(default = "default_memory_search_limit")]
    pub limit: usize,
    /// Search mode: "keyword" (default), "vector", or "hybrid".
    /// Vector and hybrid modes require an embedding provider to be configured.
    #[serde(default)]
    pub mode: Option<String>,
}

fn default_memory_search_limit() -> usize {
    10
}

// ---------------------------------------------------------------------------
// Embedding helpers
// ---------------------------------------------------------------------------

pub(crate) fn build_embedding_provider(
    config: &genesis_config::EmbeddingConfig,
) -> Result<genesis_core::embedding::EmbeddingProvider, (StatusCode, String)> {
    genesis_core::embedding::EmbeddingProvider::from_config(config)
        .map_err(|error| embedding_provider_error(config, error))
}

fn embedding_provider_error(
    _config: &genesis_config::EmbeddingConfig,
    error: genesis_core::embedding::EmbeddingError,
) -> (StatusCode, String) {
    #[cfg(not(feature = "local-embeddings"))]
    if _config.is_local_backend()
        && matches!(
            error,
            genesis_core::embedding::EmbeddingError::NotConfigured
        )
    {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "local embedding backend requires the 'local-embeddings' feature; rebuild genesis-gateway with --features local-embeddings to enable it.".to_owned(),
        );
    }

    match error {
        genesis_core::embedding::EmbeddingError::ApiError { status, body } => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            format!("embedding provider error: {body}"),
        ),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("embedding provider error: {other}"),
        ),
    }
}

fn embedding_runtime_error(
    _provider: Option<&genesis_core::embedding::EmbeddingProvider>,
    context: &str,
    error: genesis_core::embedding::EmbeddingError,
) -> ApiError {
    #[cfg(not(feature = "local-embeddings"))]
    if let Some(provider) = _provider {
        if provider.backend() == "local"
            && matches!(
                error,
                genesis_core::embedding::EmbeddingError::NotConfigured
            )
        {
            return ApiError::with_status(
                StatusCode::NOT_IMPLEMENTED,
                "local embedding backend requires the 'local-embeddings' feature; rebuild genesis-gateway with --features local-embeddings to enable it",
            );
        }
    }

    match error {
        genesis_core::embedding::EmbeddingError::ApiError { status, body } => {
            ApiError::with_status(
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                format!("{context} error: {body}"),
            )
        }
        other => ApiError::internal(format!("{context} error: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_memories_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListMemoriesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = MemoryStore::new(&path);
        let (memories, total) = store.list_paginated(limit, offset).map_err(storage_err)?;

        let has_more = (offset + memories.len()) < total as usize;
        Ok(Json(
            serde_json::to_value(MemoryListResponse {
                memories,
                total,
                limit,
                offset,
                has_more,
            })
            .map_err(|e| ApiError::internal(format!("serialization error: {e}")))?,
        ))
    })
    .await
}

pub(crate) async fn search_memories_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<SearchMemoriesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = &state.loaded.config.storage.database_path;

    let mode = genesis_core::embedding::SearchMode::from_str_opt(params.mode.as_deref());

    // Build embedding provider only for embedding-backed modes.
    let provider = if matches!(
        mode,
        genesis_core::embedding::SearchMode::Vector | genesis_core::embedding::SearchMode::Hybrid
    ) {
        state.embedding_provider()?
    } else {
        None
    };

    // The MemoryStore is created inside spawn_blocking for keyword/graph modes,
    // but hybrid_search is async (it may call the embedding API), so we keep
    // the existing pattern for search — the DB calls inside hybrid_search are
    // already short-lived.
    let db_path_owned = db_path.clone();
    let memory_store = MemoryStore::new(&db_path_owned);

    let results = genesis_core::embedding::hybrid_search(
        &params.q,
        params.limit,
        mode,
        &memory_store,
        provider.as_deref(),
    )
    .await
    .map_err(|error| embedding_runtime_error(provider.as_deref(), "search", error))?;

    let count = results.len();
    let mode_str = match mode {
        genesis_core::embedding::SearchMode::Keyword => "keyword",
        genesis_core::embedding::SearchMode::Graph => "graph",
        genesis_core::embedding::SearchMode::Vector => "vector",
        genesis_core::embedding::SearchMode::Hybrid => "hybrid",
        genesis_core::embedding::SearchMode::Advanced => "advanced",
    };

    Ok(Json(serde_json::json!({
        "memories": results.iter().map(|r| serde_json::json!({
            "id": r.memory.id,
            "session_id": r.memory.session_id,
            "kind": r.memory.kind,
            "content": r.memory.content,
            "created_at": r.memory.created_at,
            "score": r.score,
            "source": r.source,
        })).collect::<Vec<_>>(),
        "count": count,
        "mode": mode_str,
    })))
}

pub(crate) async fn delete_memory_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = MemoryStore::new(&path);
        let deleted = store.delete(&id).map_err(storage_err)?;

        if deleted {
            // Also clean up any associated embedding
            if let Err(e) = EmbeddingStore::new(&path).delete(&id) {
                warn!(memory_id = %id, error = %e, "failed to delete embedding for memory");
            }
            Ok(Json(serde_json::json!({"deleted": true, "id": id})))
        } else {
            Err(ApiError::not_found(format!("memory '{id}' not found")))
        }
    })
    .await
}

/// Embed all un-embedded memories. Requires an embedding provider to be configured.
pub(crate) async fn embed_memories_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if state.loaded.config.embedding.is_none() {
        return Err(ApiError::bad_request(
            "no embedding provider configured; add an [embedding] section to config",
        ));
    }

    let provider = state
        .embedding_provider()?
        .expect("embedding config should yield a provider");

    let db_path = state.loaded.config.storage.database_path.clone();

    // Load memories and check dimensions on a blocking thread.
    let db_path_for_blocking = db_path.clone();
    let (memories, existing_dimensions) =
        spawn_blocking_storage(db_path_for_blocking, move |path| {
            let memory_store = MemoryStore::new(&path);
            let embedding_store = EmbeddingStore::new(&path);
            let memories = memory_store.list(10000).map_err(storage_err)?;
            let existing_dimensions = embedding_store.dimensions().map_err(storage_err)?;
            Ok((memories, existing_dimensions))
        })
        .await?;

    let mut embedded = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;
    let mut reset = false;
    let mut first_probe: Option<(String, Vec<f32>)> = None;

    if let (Some(existing_dim), Some(first_memory)) = (existing_dimensions, memories.first()) {
        match provider.embed_one(&first_memory.content).await {
            Ok(embedding) => {
                if embedding.len() != existing_dim {
                    let db_path_clear = db_path.clone();
                    spawn_blocking_storage(db_path_clear, move |path| {
                        EmbeddingStore::new(&path).clear().map_err(storage_err)
                    })
                    .await?;
                    reset = true;
                    first_probe = Some((first_memory.id.clone(), embedding));
                }
            }
            Err(e) => {
                if provider.backend() == "local"
                    && matches!(e, genesis_core::embedding::EmbeddingError::NotConfigured)
                {
                    return Err(embedding_runtime_error(
                        Some(provider.as_ref()),
                        "bulk embedding",
                        e,
                    ));
                }
                tracing::warn!(memory_id = %first_memory.id, error = %e, "failed to probe embedding dimensions");
                errors += 1;
            }
        }
    }

    for memory in &memories {
        if !reset {
            let db_path_check = db_path.clone();
            let memory_id = memory.id.clone();
            let has = spawn_blocking_storage(db_path_check, move |path| {
                Ok(EmbeddingStore::new(&path)
                    .has_embedding(&memory_id)
                    .unwrap_or(false))
            })
            .await?;
            if has {
                skipped += 1;
                continue;
            }
        }

        let result: Result<(), genesis_core::embedding::EmbeddingError> = if first_probe
            .as_ref()
            .is_some_and(|(memory_id, _)| memory_id == &memory.id)
        {
            let (_, embedding) = first_probe.take().expect("probe embedding should exist");
            let db_path_store = db_path.clone();
            let memory_id = memory.id.clone();
            let model = provider.model().to_owned();
            let store_result = tokio::task::spawn_blocking(move || {
                EmbeddingStore::new(&db_path_store)
                    .store(&memory_id, &embedding, &model)
                    .map_err(genesis_core::embedding::EmbeddingError::from)
            })
            .await;
            match store_result {
                Ok(inner) => inner,
                Err(e) => {
                    tracing::error!(error = %e, "blocking embedding store task panicked");
                    Err(genesis_core::embedding::EmbeddingError::NotConfigured)
                }
            }
        } else {
            // embed_and_store is async (calls the embedding API), but
            // the storage write inside it is short-lived. We keep the
            // original pattern here to avoid excessive complexity.
            let db_path_embed = db_path.clone();
            let embedding_store = EmbeddingStore::new(&db_path_embed);
            genesis_core::embedding::embed_and_store(
                &memory.id,
                &memory.content,
                &embedding_store,
                &provider,
                provider.model(),
            )
            .await
        };

        match result {
            Ok(()) => embedded += 1,
            Err(e) => {
                if provider.backend() == "local"
                    && matches!(e, genesis_core::embedding::EmbeddingError::NotConfigured)
                {
                    return Err(embedding_runtime_error(
                        Some(provider.as_ref()),
                        "bulk embedding",
                        e,
                    ));
                }
                tracing::warn!(memory_id = %memory.id, error = %e, "failed to embed memory");
                errors += 1;
            }
        }
    }

    Ok(Json(serde_json::json!({
        "embedded": embedded,
        "skipped": skipped,
        "errors": errors,
        "total": memories.len(),
        "reset": reset,
    })))
}

/// Embed a single memory by ID.
pub(crate) async fn embed_single_memory_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if state.loaded.config.embedding.is_none() {
        return Err(ApiError::bad_request("no embedding provider configured"));
    }

    let provider = state
        .embedding_provider()?
        .expect("embedding config should yield a provider");

    let db_path = state.loaded.config.storage.database_path.clone();

    // Find the memory on a blocking thread.
    let db_path_find = db_path.clone();
    let id_clone = id.clone();
    let memory = spawn_blocking_storage(db_path_find, move |path| {
        let memory_store = MemoryStore::new(&path);
        memory_store
            .get(&id_clone)
            .map_err(storage_err)?
            .ok_or_else(|| ApiError::not_found(format!("memory '{id_clone}' not found")))
    })
    .await?;

    let embedding_store = EmbeddingStore::new(&db_path);

    genesis_core::embedding::embed_and_store(
        &memory.id,
        &memory.content,
        &embedding_store,
        &provider,
        provider.model(),
    )
    .await
    .map_err(|error| embedding_runtime_error(Some(provider.as_ref()), "embedding", error))?;

    Ok(Json(serde_json::json!({
        "embedded": true,
        "memory_id": id,
        "model": provider.model(),
    })))
}
