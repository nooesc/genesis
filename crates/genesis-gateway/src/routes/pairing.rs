//! DM pairing management handlers.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use genesis_storage::PairingStore;
use serde::Deserialize;

use crate::helpers::*;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct PairingPlatformQuery {
    pub platform: Option<String>,
    #[serde(default = "default_page_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApprovePairingRequest {
    pub platform: String,
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RevokePairingRequest {
    pub platform: String,
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClearPendingRequest {
    pub platform: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_approved_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PairingPlatformQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let (approved, total) = store
        .list_approved_paginated(params.platform.as_deref(), limit, offset)
        .map_err(storage_err)?;

    let has_more = (offset + approved.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(ApprovedListResponse {
            approved,
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

pub(crate) async fn list_pending_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PairingPlatformQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let (pending, total) = store
        .list_pending_paginated(params.platform.as_deref(), limit, offset)
        .map_err(storage_err)?;

    let has_more = (offset + pending.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(PendingListResponse {
            pending,
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

pub(crate) async fn approve_pairing_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ApprovePairingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let approved = store
        .approve_code(&request.platform, &request.code)
        .map_err(storage_err)?;

    match approved {
        Some(user) => Ok(Json(serde_json::json!({
            "approved": true,
            "user": user,
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            "invalid or expired pairing code".to_owned(),
        )),
    }
}

pub(crate) async fn revoke_pairing_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RevokePairingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let revoked = store
        .revoke(&request.platform, &request.user_id)
        .map_err(storage_err)?;

    if revoked {
        Ok(Json(serde_json::json!({
            "revoked": true,
            "platform": request.platform,
            "user_id": request.user_id,
        })))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!(
                "no approved user '{}' on platform '{}'",
                request.user_id, request.platform
            ),
        ))
    }
}

pub(crate) async fn clear_pending_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ClearPendingRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = PairingStore::new(&state.loaded.config.storage.database_path);
    let cleared = store
        .clear_pending(request.platform.as_deref())
        .map_err(storage_err)?;

    Ok(Json(serde_json::json!({
        "cleared": cleared,
        "platform": request.platform,
    })))
}
