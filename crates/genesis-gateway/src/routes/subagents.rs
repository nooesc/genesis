//! Subagent handlers.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use genesis_storage::SubagentStore;

use crate::helpers::storage_err;
use crate::state::AppState;

pub(crate) async fn get_subagent_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SubagentStore::new(&state.loaded.config.storage.database_path);
    let subagent = store.get(&id).map_err(storage_err)?;

    match subagent {
        Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
        })?)),
        None => Err((StatusCode::NOT_FOUND, format!("subagent '{id}' not found"))),
    }
}

pub(crate) async fn list_session_subagents_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = SubagentStore::new(&state.loaded.config.storage.database_path);
    let subagents = store.list_by_parent(&id).map_err(storage_err)?;

    let count = subagents.len();
    Ok(Json(serde_json::json!({
        "session_id": id,
        "subagents": subagents,
        "count": count,
    })))
}
