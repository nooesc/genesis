//! Skill CRUD and usage stat handlers.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use genesis_storage::{SkillStore, SkillUsageStore};
use serde::Deserialize;

use crate::helpers::*;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

/// Request body for creating/updating a skill.
#[derive(Debug, Deserialize)]
pub(crate) struct UpsertSkillRequest {
    pub name: String,
    pub description: String,
    pub instructions: String,
    #[serde(default)]
    pub trigger_hint: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Query parameters for searching skills by tag.
#[derive(Debug, Deserialize)]
pub(crate) struct SearchSkillsQuery {
    pub tag: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListSkillsQuery {
    #[serde(default = "default_page_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SkillUsageRecentQuery {
    #[serde(default = "default_usage_limit")]
    pub limit: usize,
}

fn default_usage_limit() -> usize {
    20
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_skills_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListSkillsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let (skills, total) = store
        .list_all_paginated(limit, offset)
        .map_err(storage_err)?;

    let has_more = (offset + skills.len()) < total as usize;
    Ok(Json(
        serde_json::to_value(SkillListResponse {
            skills,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| ApiError::internal(format!("serialization error: {e}")))?,
    ))
}

pub(crate) async fn get_skill_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let skill = store.get(&name).map_err(storage_err)?;

    match skill {
        Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
            ApiError::internal(format!("serialization error: {e}"))
        })?)),
        None => Err(ApiError::not_found(format!("skill '{name}' not found"))),
    }
}

pub(crate) async fn upsert_skill_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<UpsertSkillRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let tag_refs: Vec<&str> = request.tags.iter().map(|s| s.as_str()).collect();
    let skill = store
        .upsert(
            &request.name,
            &request.description,
            &request.instructions,
            request.trigger_hint.as_deref(),
            &tag_refs,
        )
        .map_err(storage_err)?;

    Ok(Json(serde_json::to_value(skill).map_err(|e| {
        ApiError::internal(format!("serialization error: {e}"))
    })?))
}

pub(crate) async fn delete_skill_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let deleted = store.delete(&name).map_err(storage_err)?;

    if deleted {
        Ok(Json(serde_json::json!({"deleted": true, "name": name})))
    } else {
        Err(ApiError::not_found(format!("skill '{name}' not found")))
    }
}

pub(crate) async fn search_skills_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<SearchSkillsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = SkillStore::new(&state.loaded.config.storage.database_path);
    let skills = store.find_by_tag(&params.tag).map_err(storage_err)?;

    let count = skills.len();
    Ok(Json(serde_json::json!({
        "skills": skills,
        "count": count,
    })))
}

pub(crate) async fn skill_usage_stats_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = SkillUsageStore::new(&state.loaded.config.storage.database_path);
    let stats = store.stats(&name).map_err(storage_err)?;

    Ok(Json(serde_json::to_value(stats).map_err(|e| {
        ApiError::internal(format!("serialization error: {e}"))
    })?))
}

pub(crate) async fn skill_usage_recent_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SkillUsageRecentQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = SkillUsageStore::new(&state.loaded.config.storage.database_path);
    let usages = store
        .recent_usages(&name, params.limit)
        .map_err(storage_err)?;

    let count = usages.len();
    Ok(Json(serde_json::json!({
        "skill_name": name,
        "usages": usages,
        "count": count,
    })))
}
