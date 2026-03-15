use std::sync::Arc;

use axum::middleware;
use axum::routing::{delete, get, patch, post};
use axum::Router;

use crate::{
    add_session_tag_handler, approve_pairing_handler, audit_purge_handler, audit_recent_handler,
    audit_session_handler, audit_stats_handler, auth_middleware, bus_channels_handler,
    bus_history_handler, bus_publish_handler, bus_stats_handler, cache_clear_handler,
    cache_stats_handler, chat_batch_handler, chat_handler, chat_stream_handler,
    clear_pending_handler, config_handler, create_schedule_handler, delete_memory_handler,
    delete_schedule_handler, delete_session_handler, delete_skill_handler,
    delete_user_trait_handler, embed_memories_handler, embed_single_memory_handler,
    eval_run_handler, eval_validate_handler, export_session_handler, fork_session_handler,
    get_schedule_handler, get_session_handler, get_session_tags_handler, get_skill_handler,
    get_subagent_handler, get_template_handler, get_user_trait_handler, guardrails_check_handler,
    import_session_handler, insights_handler, list_approved_handler, list_memories_handler,
    list_pending_handler, list_schedules_handler, list_session_subagents_handler,
    list_sessions_handler, list_skills_handler, list_templates_handler, list_tools_handler,
    list_user_traits_handler, mcp_status_handler, openai_chat_completions_handler,
    openai_models_handler, observe_user_trait_handler, platforms, prometheus_metrics_handler,
    purge_sessions_handler, rate_limit_middleware, remove_session_tag_handler,
    revoke_pairing_handler, search_memories_handler, search_messages_handler,
    search_skills_handler, session_messages_handler, sessions_by_tag_handler,
    set_schedule_enabled_handler, set_session_tags_handler, skill_usage_recent_handler,
    skill_usage_stats_handler, tool_analytics_handler, update_session_title_handler, usage_handler,
    webhooks_clear_dead_letters_handler, webhooks_dead_letters_handler, webhooks_status_handler,
    websocket_handler, workflow_run_handler, workflow_validate_handler, AppState, health_handler,
    llm_analytics_handler,
};

pub fn protected_router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/chat", post(chat_handler))
        .route("/chat/stream", post(chat_stream_handler))
        .route("/chat/ws", get(websocket_handler))
        .route("/chat/batch", post(chat_batch_handler))
        .route("/sessions", get(list_sessions_handler))
        .route("/sessions/purge", delete(purge_sessions_handler))
        .route("/sessions/import", post(import_session_handler))
        .route("/sessions/export", get(crate::bulk_export_handler))
        .route("/sessions/{id}", get(get_session_handler).delete(delete_session_handler))
        .route("/sessions/{id}/messages", get(session_messages_handler))
        .route("/sessions/{id}/fork", post(fork_session_handler))
        .route("/sessions/{id}/title", patch(update_session_title_handler))
        .route("/sessions/{id}/export", get(export_session_handler))
        .route("/sessions/{id}/tags", get(get_session_tags_handler).put(set_session_tags_handler))
        .route("/sessions/{id}/tags/{tag}", post(add_session_tag_handler).delete(remove_session_tag_handler))
        .route("/sessions/by-tag/{tag}", get(sessions_by_tag_handler))
        .route("/messages/search", get(search_messages_handler))
        .route("/usage", get(usage_handler))
        .route("/insights", get(insights_handler))
        .route("/skills", get(list_skills_handler).post(crate::upsert_skill_handler))
        .route("/skills/search", get(search_skills_handler))
        .route("/skills/{name}", get(get_skill_handler).delete(delete_skill_handler))
        .route("/memories", get(list_memories_handler))
        .route("/memories/search", get(search_memories_handler))
        .route("/memories/embed", post(embed_memories_handler))
        .route("/memories/{id}", delete(delete_memory_handler))
        .route("/memories/{id}/embed", post(embed_single_memory_handler))
        .route("/schedules", get(list_schedules_handler).post(create_schedule_handler))
        .route("/schedules/{id}", get(get_schedule_handler).delete(delete_schedule_handler))
        .route("/schedules/{id}/enabled", patch(set_schedule_enabled_handler))
        .route("/user/traits", get(list_user_traits_handler).post(observe_user_trait_handler))
        .route("/user/traits/{key}", get(get_user_trait_handler).delete(delete_user_trait_handler))
        .route("/subagents/{id}", get(get_subagent_handler))
        .route("/sessions/{id}/subagents", get(list_session_subagents_handler))
        .route("/skills/{name}/usage", get(skill_usage_stats_handler))
        .route("/skills/{name}/usage/recent", get(skill_usage_recent_handler))
        .route("/pairing/approved", get(list_approved_handler))
        .route("/pairing/pending", get(list_pending_handler))
        .route("/pairing/approve", post(approve_pairing_handler))
        .route("/pairing/revoke", post(revoke_pairing_handler))
        .route("/pairing/clear-pending", post(clear_pending_handler))
        .route("/tools", get(list_tools_handler))
        .route("/cache/stats", get(cache_stats_handler))
        .route("/cache/clear", post(cache_clear_handler))
        .route("/audit", get(audit_recent_handler))
        .route("/audit/stats", get(audit_stats_handler))
        .route("/audit/session/{id}", get(audit_session_handler))
        .route("/audit/purge", post(audit_purge_handler))
        .route("/analytics/tools", get(tool_analytics_handler))
        .route("/analytics/llm", get(llm_analytics_handler))
        .route("/webhooks/status", get(webhooks_status_handler))
        .route(
            "/webhooks/dead-letters",
            get(webhooks_dead_letters_handler).delete(webhooks_clear_dead_letters_handler),
        )
        .route("/templates", get(list_templates_handler))
        .route("/templates/{name}", get(get_template_handler))
        .route("/workflows/validate", post(workflow_validate_handler))
        .route("/workflows/run", post(workflow_run_handler))
        .route("/bus/channels", get(bus_channels_handler))
        .route("/bus/publish", post(bus_publish_handler))
        .route("/bus/history/{channel}", get(bus_history_handler))
        .route("/bus/stats", get(bus_stats_handler))
        .route("/eval/validate", post(eval_validate_handler))
        .route("/eval/run", post(eval_run_handler))
        .route("/guardrails/check", post(guardrails_check_handler))
        .route("/config", get(config_handler))
        .route("/v1/chat/completions", post(openai_chat_completions_handler))
        .route("/v1/models", get(openai_models_handler))
        .route("/metrics", get(prometheus_metrics_handler))
        .layer(middleware::from_fn_with_state(state, auth_middleware))
}

pub fn platform_webhooks_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/telegram/webhook", post(platforms::telegram::webhook_handler))
        .route("/discord/interactions", post(platforms::discord::interactions_handler))
        .route("/slack/events", post(platforms::slack::events_handler))
        .route(
            "/whatsapp/webhook",
            get(platforms::whatsapp::verify_handler).post(platforms::whatsapp::webhook_handler),
        )
        .route("/homeassistant/webhook", post(platforms::homeassistant::webhook_handler))
        .route("/signal/webhook", post(platforms::signal::webhook_handler))
        .route("/signal/poll", post(platforms::signal::poll_handler))
}

pub fn public_router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .merge(
            Router::new()
                .merge(protected_router(Arc::clone(&state)))
                .merge(platform_webhooks_router())
                .layer(middleware::from_fn_with_state(state.clone(), rate_limit_middleware)),
        )
        .route("/health", get(health_handler))
        .route("/health/mcp", get(mcp_status_handler))
}
