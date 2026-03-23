//! Schedule CRUD handlers.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use genesis_storage::ScheduleStore;
use serde::Deserialize;

use crate::helpers::*;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateScheduleRequest {
    pub id: String,
    pub cron_expression: String,
    pub destination: String,
    pub prompt: String,
    /// IANA timezone name (e.g. "America/New_York"). Defaults to UTC.
    #[serde(default)]
    pub timezone: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListSchedulesQuery {
    #[serde(default)]
    pub enabled_only: bool,
    #[serde(default = "default_page_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_schedules_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListSchedulesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let db_path = state.loaded.config.storage.database_path.clone();
    let enabled_only = params.enabled_only;

    spawn_blocking_storage(db_path, move |path| {
        let store = ScheduleStore::new(&path);
        let (schedules, total) = store
            .list_paginated(enabled_only, limit, offset)
            .map_err(storage_err)?;

        let has_more = (offset + schedules.len()) < total as usize;
        Ok(Json(
            serde_json::to_value(ScheduleListResponse {
                schedules,
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

pub(crate) async fn get_schedule_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = ScheduleStore::new(&path);
        let schedule = store.get(&id).map_err(storage_err)?;

        match schedule {
            Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
                ApiError::internal(format!("serialization error: {e}"))
            })?)),
            None => Err(ApiError::not_found(format!("schedule '{id}' not found"))),
        }
    })
    .await
}

pub(crate) async fn create_schedule_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateScheduleRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Validate cron expression at creation time
    if let Err(e) = genesis_core::scheduler::validate_cron(&request.cron_expression) {
        return Err(ApiError::bad_request(format!(
            "invalid cron expression: {e}"
        )));
    }

    // Validate timezone if provided
    if let Some(ref tz) = request.timezone {
        if let Err(e) = genesis_core::scheduler::resolve_timezone(Some(tz)) {
            return Err(ApiError::bad_request(e));
        }
    }

    let db_path = state.loaded.config.storage.database_path.clone();

    let schedule = spawn_blocking_storage(db_path, move |path| {
        let store = ScheduleStore::new(&path);
        store
            .create_with_timezone(
                &request.id,
                &request.cron_expression,
                &request.destination,
                &request.prompt,
                request.timezone.as_deref(),
            )
            .map_err(storage_err)
    })
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(
            serde_json::to_value(schedule)
                .map_err(|e| ApiError::internal(format!("serialization error: {e}")))?,
        ),
    ))
}

pub(crate) async fn delete_schedule_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = ScheduleStore::new(&path);
        let deleted = store.delete(&id).map_err(storage_err)?;

        if deleted {
            Ok(Json(serde_json::json!({"deleted": true, "id": id})))
        } else {
            Err(ApiError::not_found(format!("schedule '{id}' not found")))
        }
    })
    .await
}

pub(crate) async fn set_schedule_enabled_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SetEnabledRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let enabled = request.enabled;

    spawn_blocking_storage(db_path, move |path| {
        let store = ScheduleStore::new(&path);
        let updated = store.set_enabled(&id, enabled).map_err(storage_err)?;

        if updated {
            Ok(Json(serde_json::json!({
                "id": id,
                "enabled": enabled,
                "updated": true,
            })))
        } else {
            Err(ApiError::not_found(format!("schedule '{id}' not found")))
        }
    })
    .await
}
