//! Subagent handlers.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use genesis_storage::SubagentStore;

use crate::helpers::{spawn_blocking_storage, storage_err, ApiError};
use crate::state::AppState;

pub(crate) async fn get_subagent_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SubagentStore::new(&path);
        let subagent = store.get(&id).map_err(storage_err)?;

        match subagent {
            Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
                ApiError::internal(format!("serialization error: {e}"))
            })?)),
            None => Err(ApiError::not_found(format!("subagent '{id}' not found"))),
        }
    })
    .await
}

pub(crate) async fn list_session_subagents_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SubagentStore::new(&path);
        let subagents = store.list_by_parent(&id).map_err(storage_err)?;

        let count = subagents.len();
        Ok(Json(serde_json::json!({
            "session_id": id,
            "subagents": subagents,
            "count": count,
        })))
    })
    .await
}
