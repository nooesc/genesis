//! Session CRUD, fork, title, tags, import/export, insights, and usage handlers.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use genesis_storage::SessionStore;
use serde::Deserialize;
use tracing::warn;

use crate::helpers::*;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

/// Query parameters for listing sessions.
#[derive(Debug, Deserialize)]
pub(crate) struct ListSessionsQuery {
    #[serde(default = "default_page_limit")]
    pub(crate) limit: usize,
    #[serde(default)]
    pub(crate) offset: usize,
    pub(crate) search: Option<String>,
}

/// Request body for forking a session.
#[derive(Debug, Deserialize)]
pub(crate) struct ForkRequest {
    #[serde(default)]
    new_session_id: Option<String>,
}

/// Request body for updating a session title.
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateTitleRequest {
    title: String,
}

/// Query parameters for purging old sessions.
#[derive(Debug, Deserialize)]
pub(crate) struct PurgeQuery {
    #[serde(default = "default_purge_days")]
    older_than_days: u32,
}

fn default_purge_days() -> u32 {
    30
}

/// Query parameters for insights.
#[derive(Debug, Deserialize)]
pub(crate) struct InsightsQuery {
    #[serde(default = "default_insights_days")]
    days: u32,
}

fn default_insights_days() -> u32 {
    30
}

#[derive(Deserialize)]
pub(crate) struct ExportQuery {
    format: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SetTagsRequest {
    tags: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct ImportSessionRequest {
    session_id: Option<String>,
    title: Option<String>,
    messages: Vec<ImportMessage>,
}

#[derive(Deserialize)]
pub(crate) struct ImportMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
pub(crate) struct BulkExportQuery {
    #[serde(default = "default_bulk_format")]
    format: String,
    #[serde(default)]
    limit: Option<usize>,
}

fn default_bulk_format() -> String {
    "jsonl".to_owned()
}

#[derive(Deserialize)]
pub(crate) struct SearchMessagesQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    50
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_sessions_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListSessionsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let db_path = state.loaded.config.storage.database_path.clone();
    let search = params.search.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);

        let (sessions, total) = if let Some(query) = &search {
            store
                .search_sessions_paginated(query, limit, offset)
                .map_err(storage_err)?
        } else {
            store
                .list_recent_sessions_paginated(limit, offset)
                .map_err(storage_err)?
        };

        let has_more = (offset + sessions.len()) < total as usize;
        Ok(Json(
            serde_json::to_value(SessionListResponse {
                sessions,
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

pub(crate) async fn get_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let session = store.get_session(&id).map_err(storage_err)?;

        match session {
            Some(s) => Ok(Json(serde_json::to_value(s).map_err(|e| {
                ApiError::internal(format!("serialization error: {e}"))
            })?)),
            None => Err(ApiError::not_found(format!("session '{id}' not found"))),
        }
    })
    .await
}

pub(crate) async fn delete_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let deleted = store.delete_session(&id).map_err(storage_err)?;

        if deleted {
            Ok(Json(serde_json::json!({"deleted": true, "session_id": id})))
        } else {
            Err(ApiError::not_found(format!("session '{id}' not found")))
        }
    })
    .await
}

pub(crate) async fn session_messages_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let messages = store.load_messages(&id).map_err(storage_err)?;

        Ok(Json(serde_json::json!({
            "session_id": id,
            "messages": messages,
            "count": messages.len(),
        })))
    })
    .await
}

pub(crate) async fn fork_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<ForkRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let new_id = request.new_session_id.unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("fork-{id}-{ts}")
    });

    let new_id_clone = new_id.clone();
    let id_clone = id.clone();
    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        store
            .fork_session(&id_clone, &new_id_clone)
            .map_err(storage_err)?;

        Ok(Json(serde_json::json!({
            "source_session_id": id_clone,
            "new_session_id": new_id_clone,
        })))
    })
    .await
}

pub(crate) async fn update_session_title_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateTitleRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let updated = store.set_title(&id, &request.title).map_err(storage_err)?;

        Ok(Json(serde_json::json!({
            "session_id": id,
            "title": request.title,
            "updated": updated,
        })))
    })
    .await
}

pub(crate) async fn export_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let format = params.format.unwrap_or_else(|| "markdown".to_owned());

    let (content, content_type) = spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);

        let session_title = match store.get_session(&id) {
            Ok(s) => s.and_then(|s| s.title),
            Err(e) => {
                warn!(error = %e, session_id = %id, "failed to load session title for export");
                None
            }
        };

        let stored = store.load_messages(&id).map_err(storage_err)?;

        if stored.is_empty() {
            return Err(ApiError::not_found(format!(
                "no messages found for session '{id}'"
            )));
        }

        let messages: Vec<(String, Option<String>, Option<String>, String)> = stored
            .into_iter()
            .map(|m| (m.role, m.content, m.tool_calls_json, m.created_at))
            .collect();

        use genesis_tools::builtins::export::{
            export_chatml, export_json, export_jsonl, export_markdown,
        };

        let result = match format.as_str() {
            "json" => (
                export_json(&id, session_title.as_deref(), &messages),
                "application/json",
            ),
            "markdown" | "md" => (
                export_markdown(&id, session_title.as_deref(), &messages),
                "text/markdown; charset=utf-8",
            ),
            "chatml" => (export_chatml(&messages), "text/plain; charset=utf-8"),
            "jsonl" | "finetune" => (export_jsonl(&messages), "application/jsonl; charset=utf-8"),
            _ => {
                return Err(ApiError::bad_request(format!(
                    "unsupported format '{format}'; use 'markdown', 'json', 'chatml', or 'jsonl'"
                )))
            }
        };

        Ok(result)
    })
    .await?;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .body(axum::body::Body::from(content))
        .map_err(|e| ApiError::internal(format!("failed to build response: {e}")))
}

pub(crate) async fn purge_sessions_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PurgeQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let older_than_days = params.older_than_days;

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let purged = store
            .purge_older_than(older_than_days)
            .map_err(storage_err)?;

        Ok(Json(serde_json::json!({
            "purged": purged,
            "older_than_days": older_than_days,
        })))
    })
    .await
}

pub(crate) async fn get_session_tags_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let tags = store.get_tags(&id).map_err(storage_err)?;
        Ok(Json(serde_json::json!({ "session_id": id, "tags": tags })))
    })
    .await
}

pub(crate) async fn set_session_tags_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SetTagsRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let tag_refs: Vec<&str> = request.tags.iter().map(|s| s.as_str()).collect();
        store.set_tags(&id, &tag_refs).map_err(storage_err)?;
        Ok(Json(
            serde_json::json!({ "session_id": id, "tags": request.tags }),
        ))
    })
    .await
}

pub(crate) async fn add_session_tag_handler(
    State(state): State<Arc<AppState>>,
    Path((id, tag)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let added = store.add_tag(&id, &tag).map_err(storage_err)?;
        let tags = store.get_tags(&id).map_err(storage_err)?;
        Ok(Json(
            serde_json::json!({ "session_id": id, "tag": tag, "added": added, "tags": tags }),
        ))
    })
    .await
}

pub(crate) async fn remove_session_tag_handler(
    State(state): State<Arc<AppState>>,
    Path((id, tag)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let removed = store.remove_tag(&id, &tag).map_err(storage_err)?;
        let tags = store.get_tags(&id).map_err(storage_err)?;
        Ok(Json(
            serde_json::json!({ "session_id": id, "tag": tag, "removed": removed, "tags": tags }),
        ))
    })
    .await
}

pub(crate) async fn sessions_by_tag_handler(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let sessions = store.sessions_by_tag(&tag).map_err(storage_err)?;
        Ok(Json(
            serde_json::json!({ "tag": tag, "sessions": sessions, "count": sessions.len() }),
        ))
    })
    .await
}

pub(crate) async fn import_session_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ImportSessionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    let session_id = request.session_id.unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("import-{ts}")
    });

    let message_count = request.messages.len();
    let messages: Vec<(String, String)> = request
        .messages
        .into_iter()
        .map(|m| (m.role, m.content))
        .collect();
    let title = request.title;

    let sid = session_id.clone();
    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        store
            .import_session(&sid, title.as_deref(), messages)
            .map_err(|e| ApiError::internal(format!("import error: {e}")))?;

        Ok(Json(serde_json::json!({
            "session_id": sid,
            "messages_imported": message_count,
        })))
    })
    .await
}

/// Export multiple sessions as JSONL for fine-tuning or archival.
///
/// Each line is a separate training example with the messages array.
pub(crate) async fn bulk_export_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<BulkExportQuery>,
) -> Result<Response, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let format = params.format;
    let limit = params.limit.unwrap_or(1000);

    let (output, content_type) = spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let sessions = store.list_recent_sessions(limit).map_err(storage_err)?;

        use genesis_tools::builtins::export::{export_json, export_jsonl};

        let mut output = String::new();

        for session in &sessions {
            let stored = match store.load_messages(&session.id) {
                Ok(msgs) if !msgs.is_empty() => msgs,
                _ => continue,
            };

            let messages: Vec<(String, Option<String>, Option<String>, String)> = stored
                .into_iter()
                .map(|m| (m.role, m.content, m.tool_calls_json, m.created_at))
                .collect();

            match format.as_str() {
                "jsonl" | "finetune" => {
                    let line = export_jsonl(&messages);
                    if !line.is_empty() {
                        output.push_str(&line);
                    }
                }
                "json" => {
                    let json = export_json(&session.id, session.title.as_deref(), &messages);
                    output.push_str(&json);
                    output.push('\n');
                }
                _ => {
                    return Err(ApiError::bad_request(format!(
                        "unsupported bulk format '{}'; use 'jsonl' or 'json'",
                        format
                    )))
                }
            }
        }

        let content_type = match format.as_str() {
            "json" => "application/json; charset=utf-8",
            _ => "application/jsonl; charset=utf-8",
        };

        Ok((output, content_type))
    })
    .await?;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .body(axum::body::Body::from(output))
        .map_err(|e| ApiError::internal(format!("failed to build response: {e}")))
}

pub(crate) async fn search_messages_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<SearchMessagesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let results = store
            .search_messages(&params.q, params.limit)
            .map_err(storage_err)?;

        Ok(Json(serde_json::json!({
            "query": params.q,
            "results": results,
            "count": results.len(),
        })))
    })
    .await
}

pub(crate) async fn insights_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<InsightsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let data = store.insights(params.days).map_err(storage_err)?;

        Ok(Json(serde_json::to_value(data).map_err(|e| {
            ApiError::internal(format!("serialization error: {e}"))
        })?))
    })
    .await
}

pub(crate) async fn usage_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = SessionStore::new(&path);
        let stats = store.usage_stats().map_err(storage_err)?;

        Ok(Json(serde_json::to_value(stats).map_err(|e| {
            ApiError::internal(format!("serialization error: {e}"))
        })?))
    })
    .await
}
