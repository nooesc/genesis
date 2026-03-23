//! Admin endpoints: audit, analytics, cache, webhooks, templates, workflows,
//! agent bus, eval, and guardrails.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use tracing::warn;

use crate::helpers::*;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Audit log endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct AuditQueryParams {
    limit: Option<usize>,
    event_type: Option<String>,
}

pub(crate) async fn audit_recent_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuditQueryParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let limit = params.limit.unwrap_or(50);
    let event_type = params.event_type;

    spawn_blocking_storage(db_path, move |path| {
        let store = genesis_storage::AuditLogStore::new(&path);
        let entries = if let Some(ref event_type) = event_type {
            store
                .by_event_type(event_type, limit)
                .map_err(storage_err)?
        } else {
            store.recent(limit).map_err(storage_err)?
        };
        Ok(Json(serde_json::json!({
            "entries": entries,
            "count": entries.len(),
        })))
    })
    .await
}

pub(crate) async fn audit_stats_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();

    spawn_blocking_storage(db_path, move |path| {
        let store = genesis_storage::AuditLogStore::new(&path);
        let stats = store.stats().map_err(storage_err)?;
        let total: i64 = stats.iter().map(|(_, c)| c).sum();
        Ok(Json(serde_json::json!({
            "total_entries": total,
            "by_event_type": stats.into_iter().map(|(t, c)| {
                serde_json::json!({"event_type": t, "count": c})
            }).collect::<Vec<_>>(),
        })))
    })
    .await
}

pub(crate) async fn audit_session_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<AuditQueryParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let limit = params.limit.unwrap_or(100);

    spawn_blocking_storage(db_path, move |path| {
        let store = genesis_storage::AuditLogStore::new(&path);
        let entries = store.by_session(&id, limit).map_err(storage_err)?;
        Ok(Json(serde_json::json!({
            "session_id": id,
            "entries": entries,
            "count": entries.len(),
        })))
    })
    .await
}

#[derive(Deserialize)]
pub(crate) struct AuditPurgeRequest {
    older_than_days: Option<u32>,
}

pub(crate) async fn audit_purge_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AuditPurgeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let days = request.older_than_days.unwrap_or(90);

    spawn_blocking_storage(db_path, move |path| {
        let store = genesis_storage::AuditLogStore::new(&path);
        let deleted = store.purge_older_than(days).map_err(storage_err)?;
        Ok(Json(serde_json::json!({
            "purged": deleted,
            "older_than_days": days,
        })))
    })
    .await
}

// ---------------------------------------------------------------------------
// Analytics endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct AnalyticsQuery {
    days: Option<u32>,
}

pub(crate) async fn tool_analytics_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AnalyticsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let days = params.days.unwrap_or(30);

    spawn_blocking_storage(db_path, move |path| {
        let store = genesis_storage::AuditLogStore::new(&path);
        let analytics = store.tool_analytics(days).map_err(storage_err)?;
        Ok(Json(serde_json::json!({
            "period_days": days,
            "tools": analytics,
        })))
    })
    .await
}

pub(crate) async fn llm_analytics_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AnalyticsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let days = params.days.unwrap_or(30);

    spawn_blocking_storage(db_path, move |path| {
        let store = genesis_storage::AuditLogStore::new(&path);
        let analytics = store.llm_analytics(days).map_err(storage_err)?;
        Ok(Json(serde_json::json!({
            "period_days": days,
            "models": analytics,
        })))
    })
    .await
}

// ---------------------------------------------------------------------------
// Cache management
// ---------------------------------------------------------------------------

pub(crate) async fn cache_stats_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let db_path = state.loaded.config.storage.database_path.clone();
    let enabled = state
        .loaded
        .config
        .runtime
        .cache
        .as_ref()
        .is_some_and(|c| c.enabled);

    let (entries, hits) = tokio::task::spawn_blocking(move || {
        let cache = genesis_storage::ResponseCacheStore::new(&db_path);
        cache.stats().unwrap_or((0, 0))
    })
    .await
    .unwrap_or((0, 0));

    Json(serde_json::json!({
        "enabled": enabled,
        "entries": entries,
        "total_hits": hits,
    }))
}

pub(crate) async fn cache_clear_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let db_path = state.loaded.config.storage.database_path.clone();

    let result = tokio::task::spawn_blocking(move || {
        let cache = genesis_storage::ResponseCacheStore::new(&db_path);
        cache.clear()
    })
    .await;

    match result {
        Ok(Ok(deleted)) => Json(serde_json::json!({
            "cleared": deleted,
        })),
        Ok(Err(e)) => Json(serde_json::json!({
            "error": e.to_string(),
        })),
        Err(e) => Json(serde_json::json!({
            "error": format!("blocking task failed: {e}"),
        })),
    }
}

// ---------------------------------------------------------------------------
// Webhook status
// ---------------------------------------------------------------------------

pub(crate) async fn webhooks_status_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let (delivered, retried, failed) = state.webhooks.metrics();
    let dead_letter_count = state.webhooks.dead_letters().await.len();
    Json(serde_json::json!({
        "configured": !state.webhooks.is_empty(),
        "delivered": delivered,
        "retried": retried,
        "failed": failed,
        "dead_letter_count": dead_letter_count,
    }))
}

pub(crate) async fn webhooks_dead_letters_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let entries = state.webhooks.dead_letters().await;
    Json(serde_json::json!({
        "entries": entries,
        "count": entries.len(),
    }))
}

pub(crate) async fn webhooks_clear_dead_letters_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let cleared = state.webhooks.clear_dead_letters().await;
    Json(serde_json::json!({
        "cleared": cleared,
    }))
}

// ---------------------------------------------------------------------------
// Agent templates
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ListTemplatesQuery {
    #[serde(default = "default_page_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

pub(crate) async fn list_templates_handler(
    axum::extract::Query(params): axum::extract::Query<ListTemplatesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let limit = clamp_limit(params.limit);
    let offset = validate_offset(params.offset)?;
    let all_templates = genesis_core::templates::list_templates();
    let total = all_templates.len() as u64;
    let page: Vec<serde_json::Value> = all_templates
        .iter()
        .skip(offset)
        .take(limit)
        .map(|t| serde_json::to_value(t).unwrap_or_default())
        .collect();
    let has_more = (offset + page.len()) < total as usize;

    Ok(Json(
        serde_json::to_value(TemplateListResponse {
            templates: page,
            total,
            limit,
            offset,
            has_more,
        })
        .map_err(|e| ApiError::internal(format!("serialization error: {e}")))?,
    ))
}

pub(crate) async fn get_template_handler(
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match genesis_core::templates::get_template(&name) {
        Some(t) => {
            let prompt = genesis_core::templates::format_template_prompt(t);
            Ok(Json(serde_json::json!({
                "template": t,
                "formatted_prompt": prompt,
            })))
        }
        None => Err(ApiError::not_found(format!("Template '{name}' not found"))),
    }
}

// ---------------------------------------------------------------------------
// Workflow endpoints
// ---------------------------------------------------------------------------

pub(crate) async fn workflow_validate_handler(
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let yaml = body
        .get("yaml")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("Missing 'yaml' field in request body"))?;

    let workflow = genesis_core::workflow::parse_workflow(yaml)
        .map_err(|e| ApiError::bad_request(format!("Failed to parse workflow: {e}")))?;

    let issues = genesis_core::workflow::validate_workflow(&workflow);
    Ok(Json(serde_json::json!({
        "valid": issues.is_empty(),
        "workflow_name": workflow.name,
        "steps": workflow.steps.len(),
        "issues": issues,
    })))
}

pub(crate) async fn workflow_run_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let yaml = body
        .get("yaml")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("Missing 'yaml' field in request body"))?;
    let input = body.get("input").and_then(|v| v.as_str()).unwrap_or("");
    let session_id = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("workflow-api");

    let workflow = genesis_core::workflow::parse_workflow(yaml)
        .map_err(|e| ApiError::bad_request(format!("Failed to parse workflow: {e}")))?;

    let issues = genesis_core::workflow::validate_workflow(&workflow);
    if !issues.is_empty() {
        return Err(ApiError::bad_request(format!(
            "Workflow validation failed: {}",
            issues.join("; ")
        )));
    }

    let service = state.session_service();
    let result = service
        .run_workflow(&workflow, input, session_id)
        .await
        .map_err(|e| {
            ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Workflow execution failed: {e}"),
            )
        })?;

    Ok(Json(serde_json::json!(result)))
}

// ---------------------------------------------------------------------------
// Agent bus endpoints
// ---------------------------------------------------------------------------

pub(crate) async fn bus_channels_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let channels = state.agent_bus.channels().await;
    Json(serde_json::json!({
        "channels": channels,
        "count": channels.len(),
    }))
}

pub(crate) async fn bus_publish_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let channel = body
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("Missing 'channel' field"))?;
    let sender = body.get("sender").and_then(|v| v.as_str()).unwrap_or("api");
    let payload = body
        .get("payload")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("Missing 'payload' field"))?;
    let kind_str = body.get("kind").and_then(|v| v.as_str()).unwrap_or("text");
    let kind: genesis_core::agent_bus::MessageKind =
        serde_json::from_str(&format!("\"{kind_str}\""))
            .unwrap_or(genesis_core::agent_bus::MessageKind::Text);

    let metadata: std::collections::HashMap<String, String> = body
        .get("metadata")
        .and_then(|v| match serde_json::from_value(v.clone()) {
            Ok(m) => Some(m),
            Err(e) => {
                warn!(error = %e, "invalid metadata in bus publish request, ignoring");
                None
            }
        })
        .unwrap_or_default();

    let msg = genesis_core::agent_bus::AgentMessage {
        id: format!("api-{:016x}", {
            use std::hash::{BuildHasher, Hasher};
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let rng = std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish();
            ts as u64 ^ rng
        }),
        channel: channel.to_owned(),
        sender: sender.to_owned(),
        kind,
        payload: payload.to_owned(),
        metadata,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    let subscribers = state.agent_bus.publish(msg.clone()).await;
    Ok(Json(serde_json::json!({
        "published": true,
        "message_id": msg.id,
        "subscribers_notified": subscribers,
    })))
}

pub(crate) async fn bus_history_handler(
    State(state): State<Arc<AppState>>,
    Path(channel): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let messages = state.agent_bus.history(&channel, limit);
    Json(serde_json::json!({
        "channel": channel,
        "messages": messages,
        "count": messages.len(),
    }))
}

pub(crate) async fn bus_stats_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let stats = state.agent_bus.stats();
    let total: i64 = stats.iter().map(|(_, c)| c).sum();
    Json(serde_json::json!({
        "total_messages": total,
        "channels": stats.iter().map(|(ch, count)| {
            serde_json::json!({"channel": ch, "message_count": count})
        }).collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------------------
// Eval endpoints
// ---------------------------------------------------------------------------

pub(crate) async fn eval_validate_handler(
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let yaml = body
        .get("yaml")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("Missing 'yaml' field"))?;

    let suite = genesis_core::eval::parse_suite(yaml)
        .map_err(|e| ApiError::bad_request(format!("Failed to parse suite: {e}")))?;

    let issues = genesis_core::eval::validate_suite(&suite);
    Ok(Json(serde_json::json!({
        "valid": issues.is_empty(),
        "suite_name": suite.name,
        "cases": suite.cases.len(),
        "issues": issues,
    })))
}

pub(crate) async fn eval_run_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let yaml = body
        .get("yaml")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("Missing 'yaml' field"))?;

    let suite = genesis_core::eval::parse_suite(yaml)
        .map_err(|e| ApiError::bad_request(format!("Failed to parse suite: {e}")))?;

    let issues = genesis_core::eval::validate_suite(&suite);
    if !issues.is_empty() {
        return Err(ApiError::bad_request(format!(
            "Suite validation failed: {}",
            issues.join("; ")
        )));
    }

    let service = state.session_service();
    let report = service.run_eval(&suite).await.map_err(|e| {
        ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Eval run failed: {e}"),
        )
    })?;

    Ok(Json(serde_json::json!(report)))
}

// ---------------------------------------------------------------------------
// Guardrails
// ---------------------------------------------------------------------------

pub(crate) async fn guardrails_check_handler(
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("Missing 'text' field"))?;
    let direction = body
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("input");

    // Parse config from request body, or use a sensible default
    let config: genesis_core::guardrails::GuardrailConfig = body
        .get("config")
        .and_then(|v| match serde_json::from_value(v.clone()) {
            Ok(c) => Some(c),
            Err(e) => {
                warn!(error = %e, "invalid guardrail config in request body, using defaults");
                None
            }
        })
        .unwrap_or_else(|| genesis_core::guardrails::GuardrailConfig {
            detect_pii: true,
            pii_action: genesis_core::guardrails::ViolationAction::Warn,
            ..genesis_core::guardrails::GuardrailConfig::default()
        });

    let result = match direction {
        "output" => genesis_core::guardrails::check_output(&config, text),
        _ => genesis_core::guardrails::check_input(&config, text),
    };

    Ok(Json(serde_json::json!(result)))
}
