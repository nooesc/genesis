//! User model (traits/preferences) handlers.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use genesis_storage::UserModelStore;
use serde::Deserialize;

use crate::helpers::*;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ObserveTraitRequest {
    pub trait_key: String,
    pub category: String,
    pub value: String,
    pub source_session: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListTraitsQuery {
    pub category: Option<String>,
    pub min_confidence: Option<f64>,
    #[serde(default = "default_page_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_user_traits_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListTraitsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let (traits_list, total) = store
        .list_paginated(
            params.category.as_deref(),
            params.min_confidence,
            limit,
            offset,
        )
        .map_err(storage_err)?;

    let has_more = (offset + traits_list.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(TraitListResponse {
            traits: traits_list,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?,
    ))
}

pub(crate) async fn get_user_trait_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let user_trait = store.get(&key).map_err(storage_err)?;

    match user_trait {
        Some(t) => Ok(Json(serde_json::to_value(t).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?)),
        None => Err((StatusCode::NOT_FOUND, format!("trait '{key}' not found"))),
    }
}

pub(crate) async fn observe_user_trait_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ObserveTraitRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let observed = store
        .observe(
            &request.trait_key,
            &request.category,
            &request.value,
            request.source_session.as_deref(),
        )
        .map_err(storage_err)?;

    Ok((
        StatusCode::OK,
        Json(serde_json::to_value(observed).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?),
    ))
}

pub(crate) async fn delete_user_trait_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = UserModelStore::new(&state.loaded.config.storage.database_path);
    let deleted = store.delete(&key).map_err(storage_err)?;

    if deleted {
        Ok(Json(serde_json::json!({"deleted": true, "trait_key": key})))
    } else {
        Err((StatusCode::NOT_FOUND, format!("trait '{key}' not found")))
    }
}
