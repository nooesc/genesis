//! Chat, streaming, batch, WebSocket, and OpenAI-compatible handlers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use genesis_core::agent_loop::StreamEvent;
use genesis_core::execution::{delivery_platform_from_str, SessionTurnInput};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{error, info, info_span, warn, Instrument};

use crate::helpers::*;
use crate::sse::*;
use crate::state::AppState;
use crate::webhooks;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// Request body for the `/chat` endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct ChatRequest {
    pub message: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    pub session_id: Option<String>,
    /// Optional image URLs for multimodal prompts.
    #[serde(default)]
    pub images: Vec<ImageInput>,
    /// Optional system prompt override for this request.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Optional response format constraint (json_object, json_schema, or text).
    #[serde(default)]
    pub response_format: Option<genesis_provider::ResponseFormat>,
    /// Optional model override (e.g. "openai/gpt-4.1" or "anthropic/claude-sonnet-4").
    /// Format: "backend/model" or just "model" (uses default backend).
    #[serde(default)]
    pub model: Option<String>,
}

/// An image input for multimodal chat requests.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ImageInput {
    /// Image URL (http/https) or base64 data URI.
    pub url: String,
    /// Optional detail level: "low", "high", or "auto" (default).
    #[serde(default)]
    pub detail: Option<String>,
}

/// Response body from the `/chat` endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct ChatResponse {
    pub session_id: String,
    pub response: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    /// If set, the agent is paused waiting for user clarification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_clarification: Option<String>,
}

/// SSE payload for a streamed token chunk.
#[derive(Debug, Serialize)]
pub(crate) struct StreamChunkResponse {
    pub session_id: String,
    pub content: String,
}

/// SSE payload signaling final completion.
#[derive(Debug, Serialize)]
pub(crate) struct StreamDoneResponse {
    pub session_id: String,
    pub response: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

/// SSE payload signaling an execution failure.
#[derive(Debug, Serialize)]
pub(crate) struct StreamErrorResponse {
    pub session_id: String,
    pub error: String,
}

/// A single prompt within a batch request.
#[derive(Debug, Deserialize)]
pub(crate) struct BatchItem {
    pub message: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    pub session_id: Option<String>,
    #[serde(default)]
    pub images: Vec<ImageInput>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub response_format: Option<genesis_provider::ResponseFormat>,
}

/// Request body for the `/chat/batch` endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct BatchRequest {
    pub items: Vec<BatchItem>,
    /// Maximum concurrent executions (default: 4, max: 16).
    #[serde(default = "default_batch_concurrency")]
    pub concurrency: usize,
}

fn default_batch_concurrency() -> usize {
    4
}

/// Result of a single item in a batch.
#[derive(Debug, Serialize)]
pub(crate) struct BatchItemResult {
    pub index: usize,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub turns_used: usize,
    pub tool_calls_made: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

/// Response body for the `/chat/batch` endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct BatchResponse {
    pub results: Vec<BatchItemResult>,
    pub total_items: usize,
    pub successful: usize,
    pub failed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_estimated_cost: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiCompletionsRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    /// Accepted for OpenAI API compatibility but not yet forwarded to the
    /// underlying provider.  Removing these fields would cause deserialization
    /// errors for clients that send them.
    #[serde(default)]
    #[allow(dead_code)]
    temperature: Option<f64>,
    /// See `temperature` above.
    #[serde(default)]
    #[allow(dead_code)]
    max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
}

const MAX_BATCH_CONCURRENCY: usize = 16;
const MAX_BATCH_SIZE: usize = 100;

// ---------------------------------------------------------------------------
// Chat handler
// ---------------------------------------------------------------------------

pub(crate) async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, String)> {
    let loaded = &state.loaded;
    let mut service = state.session_service();
    if let Some(system_prompt) = request.system_prompt {
        service.set_system_prompt_override(system_prompt);
    }
    if let Some(response_format) = request.response_format {
        service.set_response_format(response_format);
    }
    if let Some(ref model_spec) = request.model {
        let (backend, model) = parse_model_spec(model_spec, &loaded.config.provider.backend);
        service.set_model_override(backend, model);
    }
    let session_id = request.session_id.unwrap_or_else(default_api_session_id);
    let request_id = default_request_id();
    let span = info_span!(
        "gateway.chat",
        request_id = request_id.as_str(),
        session_id = session_id.as_str(),
        platform = request.platform.as_str()
    );
    let images: Vec<genesis_provider::ImageUrl> = request
        .images
        .iter()
        .map(|img| genesis_provider::ImageUrl {
            url: img.url.clone(),
            detail: img.detail.clone(),
        })
        .collect();

    let webhooks = state.webhooks.clone();
    let metrics_state = Arc::clone(&state);
    let request_started = std::time::Instant::now();
    async move {
        info!("received chat request");
        metrics_state.requests_total.fetch_add(1, Ordering::Relaxed);

        // Emit message_received webhook
        webhooks.emit(
            webhooks::WebhookEventType::MessageReceived,
            Some(&session_id),
            Some(&request.platform),
            serde_json::json!({"message_length": request.message.len()}),
        );

        let outcome = service
            .run_turn(SessionTurnInput {
                session_id: &session_id,
                session_platform: &request.platform,
                delivery_platform: delivery_platform_from_str(&request.platform),
                prompt: &request.message,
                title: None,
                images,
            })
            .await
            .map_err(|e| {
                metrics_state.errors_total.fetch_add(1, Ordering::Relaxed);
                // Emit error webhook
                webhooks.emit(
                    webhooks::WebhookEventType::Error,
                    Some(&session_id),
                    Some(&request.platform),
                    serde_json::json!({"error": e.to_string()}),
                );
                error!(
                    request_id = request_id.as_str(),
                    error = %e,
                    "chat request failed"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("execution error: {e}"),
                )
            })?;
        info!(
            request_id = request_id.as_str(),
            turns_used = outcome.result.turns_used,
            tool_calls_made = outcome.result.tool_calls_made,
            "chat request completed"
        );

        // Emit response_sent webhook
        webhooks.emit(
            webhooks::WebhookEventType::ResponseSent,
            Some(&session_id),
            Some(&request.platform),
            serde_json::json!({
                "turns_used": outcome.result.turns_used,
                "tool_calls_made": outcome.result.tool_calls_made,
                "response_length": outcome.result.response.len(),
                "estimated_cost": outcome.result.estimated_cost,
                "input_tokens": outcome.result.total_input_tokens,
                "output_tokens": outcome.result.total_output_tokens,
            }),
        );

        // Append delivery mirror for cross-platform visibility.
        crate::mirror::append_delivery_mirror_to_session(
            &metrics_state.loaded.config.storage.database_path,
            &session_id,
            &outcome.result.response,
            "api",
        );

        // Record token metrics + duration histogram
        metrics_state
            .input_tokens_total
            .fetch_add(outcome.result.total_input_tokens as u64, Ordering::Relaxed);
        metrics_state
            .output_tokens_total
            .fetch_add(outcome.result.total_output_tokens as u64, Ordering::Relaxed);
        if let Ok(mut hist) = metrics_state.request_duration_histogram.lock() {
            hist.observe(request_started.elapsed().as_millis() as u64);
        }

        Ok(Json(ChatResponse {
            session_id: outcome.session_id,
            response: outcome.result.response,
            turns_used: outcome.result.turns_used,
            tool_calls_made: outcome.result.tool_calls_made,
            estimated_cost: outcome.result.estimated_cost,
            total_input_tokens: outcome.result.total_input_tokens,
            total_output_tokens: outcome.result.total_output_tokens,
            pending_clarification: outcome.result.pending_clarification,
        }))
    }
    .instrument(span)
    .await
}

// ---------------------------------------------------------------------------
// OpenAI-compatible endpoints
// ---------------------------------------------------------------------------

/// OpenAI-compatible `/v1/chat/completions` endpoint.
///
/// Accepts the standard OpenAI request format and routes the last user message
/// through the Genesis agent, returning a response in OpenAI's format.
/// This allows any OpenAI SDK client to talk to Genesis directly.
pub(crate) async fn openai_chat_completions_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<OpenAiCompletionsRequest>,
) -> Result<Response, (StatusCode, String)> {
    // Extract the last user message as the prompt
    let prompt = request
        .messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "no user message found in messages array".to_owned(),
            )
        })?
        .to_owned();

    // Extract optional system prompt
    let system_prompt = request
        .messages
        .iter()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .map(str::to_owned);

    let session_id = default_api_session_id();
    let request_id = default_request_id();
    let model = request.model.clone();
    let streaming = request.stream.unwrap_or(false);

    let span = info_span!(
        "gateway.openai_compat",
        request_id = request_id.as_str(),
        session_id = session_id.as_str(),
        model = model.as_str(),
        streaming,
    );

    if streaming {
        openai_streaming_response(state, prompt, system_prompt, session_id, model, span).await
    } else {
        openai_blocking_response(state, prompt, system_prompt, session_id, model, span).await
    }
}

/// Non-streaming OpenAI-compatible response.
async fn openai_blocking_response(
    state: Arc<AppState>,
    prompt: String,
    system_prompt: Option<String>,
    session_id: String,
    model: String,
    span: tracing::Span,
) -> Result<Response, (StatusCode, String)> {
    state.requests_total.fetch_add(1, Ordering::Relaxed);

    let mut service = state.session_service();
    if let Some(sp) = system_prompt {
        service.set_system_prompt_override(sp);
    }

    async move {
        info!("received OpenAI-compatible chat completions request");

        let outcome = service
            .run_turn(SessionTurnInput {
                session_id: &session_id,
                session_platform: "api",
                delivery_platform: delivery_platform_from_str("api"),
                prompt: &prompt,
                title: None,
                images: Vec::new(),
            })
            .await
            .map_err(|e| {
                error!(error = %e, "OpenAI-compat request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("execution error: {e}"),
                )
            })?;

        let body = serde_json::json!({
            "id": format!("chatcmpl-{}", session_id),
            "object": "chat.completion",
            "created": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": outcome.result.response,
                },
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": outcome.result.total_input_tokens,
                "completion_tokens": outcome.result.total_output_tokens,
                "total_tokens": outcome.result.total_input_tokens + outcome.result.total_output_tokens,
            },
        });

        Ok(Json(body).into_response())
    }
    .instrument(span)
    .await
}

/// Streaming OpenAI-compatible response (SSE with `data: {...}` chunks).
///
/// Follows the OpenAI streaming format:
/// - Each chunk: `data: {"id":"...","object":"chat.completion.chunk","choices":[{"delta":{"content":"..."}}]}`
/// - Final: `data: [DONE]`
async fn openai_streaming_response(
    state: Arc<AppState>,
    prompt: String,
    system_prompt: Option<String>,
    session_id: String,
    model: String,
    span: tracing::Span,
) -> Result<Response, (StatusCode, String)> {
    state.requests_total.fetch_add(1, Ordering::Relaxed);
    state.stream_requests_total.fetch_add(1, Ordering::Relaxed);

    let (tx, mut rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(SSE_CHANNEL_BUFFER);
    let state_for_task = Arc::clone(&state);
    let session_id_for_task = session_id.clone();
    let model_for_task = model.clone();

    // Shared cancellation flag — set when the client disconnects or on timeout.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_for_task = Arc::clone(&cancelled);

    let timeout_secs = state
        .loaded
        .config
        .gateway
        .as_ref()
        .and_then(|g| g.stream_timeout_secs)
        .unwrap_or(DEFAULT_STREAM_TIMEOUT_SECS);

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let completion_id = format!("chatcmpl-{}", session_id);

    tokio::spawn(async move {
        let cancelled = cancelled_for_task;
        let mut service = state_for_task.session_service();
        if let Some(sp) = system_prompt {
            service.set_system_prompt_override(sp);
        }

        info!("received OpenAI-compatible streaming chat completions request");

        // Send initial role chunk
        let initial_chunk = serde_json::json!({
            "id": &completion_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": &model_for_task,
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "" },
                "finish_reason": null,
            }],
        });
        let initial_data = match serde_json::to_string(&initial_chunk) {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(error = %e, "failed to serialize SSE event");
                String::from(r#"{"id":"error","object":"chat.completion.chunk","choices":[]}"#)
            }
        };
        send_sse(&tx, Ok(Event::default().data(initial_data)), &cancelled);

        let completion_id_for_event = completion_id.clone();
        let model_for_event = model_for_task.clone();
        let tx_for_event = tx.clone();
        let cancelled_cb = Arc::clone(&cancelled);

        let agent_future = service
            .run_turn_streaming(
                SessionTurnInput {
                    session_id: &session_id_for_task,
                    session_platform: "api",
                    delivery_platform: delivery_platform_from_str("api"),
                    prompt: &prompt,
                    title: None,
                    images: Vec::new(),
                },
                |event| {
                    if let StreamEvent::Chunk(chunk) = event {
                        let data = serde_json::json!({
                            "id": &completion_id_for_event,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": &model_for_event,
                            "choices": [{
                                "index": 0,
                                "delta": { "content": chunk },
                                "finish_reason": null,
                            }],
                        });
                        let chunk_data = match serde_json::to_string(&data) {
                            Ok(json) => json,
                            Err(e) => {
                                tracing::error!(error = %e, "failed to serialize SSE event");
                                String::from(r#"{"id":"error","object":"chat.completion.chunk","choices":[]}"#)
                            }
                        };
                        send_sse(
                            &tx_for_event,
                            Ok(Event::default().data(chunk_data)),
                            &cancelled_cb,
                        );
                    }
                },
            );

        let run_result =
            match tokio::time::timeout(Duration::from_secs(timeout_secs), agent_future).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    warn!(timeout_secs, "OpenAI streaming request timed out");
                    // Emit an error chunk so OpenAI-compatible clients know
                    // the response was truncated, then send [DONE].
                    let error_event = serde_json::json!({
                        "error": {
                            "message": format!(
                                "Stream timeout exceeded after {timeout_secs}s"
                            ),
                            "type": "timeout",
                        }
                    });
                    if let Ok(payload) = serde_json::to_string(&error_event) {
                        let _ = tx.try_send(Ok(Event::default().data(payload)));
                    }
                    let _ = tx.try_send(Ok(Event::default().data("[DONE]")));
                    cancelled.store(true, Ordering::Relaxed);
                    return;
                }
            };

        // Send finish chunk
        let finish_chunk = serde_json::json!({
            "id": &completion_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": &model_for_task,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
        });
        let finish_data = match serde_json::to_string(&finish_chunk) {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(error = %e, "failed to serialize SSE event");
                String::from(r#"{"id":"error","object":"chat.completion.chunk","choices":[]}"#)
            }
        };
        send_sse(&tx, Ok(Event::default().data(finish_data)), &cancelled);

        // Send usage chunk if we got a successful outcome
        if let Ok(outcome) = run_result {
            let usage_chunk = serde_json::json!({
                "id": &completion_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": &model_for_task,
                "choices": [],
                "usage": {
                    "prompt_tokens": outcome.result.total_input_tokens,
                    "completion_tokens": outcome.result.total_output_tokens,
                    "total_tokens": outcome.result.total_input_tokens + outcome.result.total_output_tokens,
                },
            });
            let usage_data = match serde_json::to_string(&usage_chunk) {
                Ok(json) => json,
                Err(e) => {
                    tracing::error!(error = %e, "failed to serialize SSE event");
                    String::from(r#"{"id":"error","object":"chat.completion.chunk","choices":[]}"#)
                }
            };
            send_sse(&tx, Ok(Event::default().data(usage_data)), &cancelled);
        }

        // Send [DONE] sentinel
        send_sse(
            &tx,
            Ok(Event::default().data("[DONE]")),
            &cancelled,
        );
    }.instrument(span));

    // When the client disconnects, Axum drops the stream future.
    let cancelled_for_stream = Arc::clone(&cancelled);
    let stream = async_stream::stream! {
        let _guard = CancelOnDrop(cancelled_for_stream);
        while let Some(event) = rx.recv().await {
            yield event;
        }
    };

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// OpenAI-compatible `/v1/models` endpoint.
pub(crate) async fn openai_models_handler(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let config = &state.loaded.config;
    let model_id = format!("{}/{}", config.provider.backend, config.provider.model);
    Json(serde_json::json!({
        "object": "list",
        "data": [{
            "id": model_id,
            "object": "model",
            "created": 0,
            "owned_by": config.provider.backend,
        }],
    }))
}

// ---------------------------------------------------------------------------
// Genesis native streaming
// ---------------------------------------------------------------------------

pub(crate) async fn chat_stream_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<
    Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, String),
> {
    let session_id = request.session_id.unwrap_or_else(default_api_session_id);
    let request_id = default_request_id();
    info!(
        request_id = request_id.as_str(),
        session_id = session_id.as_str(),
        platform = request.platform.as_str(),
        "accepted streaming chat request"
    );

    let platform = request.platform;
    let message = request.message;
    let system_prompt = request.system_prompt;
    let response_format = request.response_format;
    let images: Vec<genesis_provider::ImageUrl> = request
        .images
        .into_iter()
        .map(|img| genesis_provider::ImageUrl {
            url: img.url,
            detail: img.detail,
        })
        .collect();
    let (tx, mut rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(SSE_CHANNEL_BUFFER);
    let state_for_task = Arc::clone(&state);
    let session_id_for_task = session_id.clone();
    let request_id_for_task = request_id.clone();

    // Shared cancellation flag
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_for_task = Arc::clone(&cancelled);

    let timeout_secs = state
        .loaded
        .config
        .gateway
        .as_ref()
        .and_then(|g| g.stream_timeout_secs)
        .unwrap_or(DEFAULT_STREAM_TIMEOUT_SECS);

    let spawn_span = info_span!(
        "gateway.chat_stream",
        request_id = request_id_for_task.as_str(),
        session_id = session_id_for_task.as_str(),
        platform = platform.as_str()
    );
    tokio::spawn(
        async move {
            let cancelled = cancelled_for_task;
            let mut service = state_for_task.session_service();
            if let Some(system_prompt) = system_prompt {
                service.set_system_prompt_override(system_prompt);
            }
            if let Some(response_format) = response_format {
                service.set_response_format(response_format);
            }
            let initial_payload = serde_json::to_string(&serde_json::json!({
                "session_id": session_id_for_task,
            }));

            if let Ok(payload) = initial_payload {
                send_sse(
                    &tx,
                    Ok(Event::default().event("session").data(payload)),
                    &cancelled,
                );
            }

            let tx_cb = tx.clone();
            let cancelled_cb = Arc::clone(&cancelled);

            let agent_future = service
                .run_turn_streaming(
                    SessionTurnInput {
                        session_id: &session_id,
                        session_platform: &platform,
                        delivery_platform: delivery_platform_from_str(&platform),
                        prompt: &message,
                        title: None,
                        images,
                    },
                    |event| match event {
                        StreamEvent::Chunk(chunk) => {
                            if let Ok(payload) = serde_json::to_string(&StreamChunkResponse {
                                session_id: session_id.clone(),
                                content: chunk.to_owned(),
                            }) {
                                send_sse(
                                    &tx_cb,
                                    Ok(Event::default().event("chunk").data(payload)),
                                    &cancelled_cb,
                                );
                            }
                        }
                        StreamEvent::ToolCallStart { name, .. } => {
                            if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                                "session_id": &session_id,
                                "tool": name,
                            })) {
                                send_sse(
                                    &tx_cb,
                                    Ok(Event::default().event("tool_call").data(payload)),
                                    &cancelled_cb,
                                );
                            }
                        }
                        StreamEvent::ToolCallEnd { .. }
                        | StreamEvent::TurnStarted
                        | StreamEvent::TokenUsage { .. }
                        | StreamEvent::Warning(_) => {}
                        StreamEvent::ClarificationNeeded { question } => {
                            if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                                "session_id": &session_id,
                                "question": question,
                            })) {
                                send_sse(
                                    &tx_cb,
                                    Ok(Event::default().event("clarification").data(payload)),
                                    &cancelled_cb,
                                );
                            }
                        }
                    },
                );

            let run_result =
                match tokio::time::timeout(Duration::from_secs(timeout_secs), agent_future).await {
                    Ok(result) => result,
                    Err(_elapsed) => {
                        warn!(
                            request_id = request_id_for_task.as_str(),
                            timeout_secs,
                            "streaming chat request timed out"
                        );
                        cancelled.store(true, Ordering::Relaxed);
                        if let Ok(payload) = serde_json::to_string(&StreamErrorResponse {
                            session_id,
                            error: format!(
                                "streaming request timed out after {timeout_secs}s"
                            ),
                        }) {
                            send_sse(
                                &tx,
                                Ok(Event::default().event("error").data(payload)),
                                &cancelled,
                            );
                        }
                        return;
                    }
                };

            match run_result {
                Ok(outcome) => {
                    info!(
                        request_id = request_id_for_task.as_str(),
                        turns_used = outcome.result.turns_used,
                        tool_calls_made = outcome.result.tool_calls_made,
                        "streaming chat request completed"
                    );

                    // Append delivery mirror for cross-platform visibility.
                    crate::mirror::append_delivery_mirror_to_session(
                        &state_for_task.loaded.config.storage.database_path,
                        &outcome.session_id,
                        &outcome.result.response,
                        "api",
                    );

                    if let Ok(payload) = serde_json::to_string(&StreamDoneResponse {
                        session_id: outcome.session_id,
                        response: outcome.result.response,
                        turns_used: outcome.result.turns_used,
                        tool_calls_made: outcome.result.tool_calls_made,
                        estimated_cost: outcome.result.estimated_cost,
                        total_input_tokens: outcome.result.total_input_tokens,
                        total_output_tokens: outcome.result.total_output_tokens,
                    }) {
                        send_sse(
                            &tx,
                            Ok(Event::default().event("done").data(payload)),
                            &cancelled,
                        );
                    }
                }
                Err(error) => {
                    error!(
                        request_id = request_id_for_task.as_str(),
                        error = %error,
                        "streaming chat request failed"
                    );
                    if let Ok(payload) = serde_json::to_string(&StreamErrorResponse {
                        session_id,
                        error: error.to_string(),
                    }) {
                        send_sse(
                            &tx,
                            Ok(Event::default().event("error").data(payload)),
                            &cancelled,
                        );
                    }
                }
            }
        }
        .instrument(spawn_span),
    );

    let cancelled_for_stream = Arc::clone(&cancelled);
    let stream = async_stream::stream! {
        let _guard = CancelOnDrop(cancelled_for_stream);
        while let Some(event) = rx.recv().await {
            yield event;
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ---------------------------------------------------------------------------
// WebSocket chat endpoint
// ---------------------------------------------------------------------------

/// WebSocket chat handler.
pub(crate) async fn websocket_handler(
    State(state): State<Arc<AppState>>,
    ws: axum::extract::WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| websocket_session(state, socket))
}

async fn websocket_session(state: Arc<AppState>, mut socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    info!("WebSocket client connected");

    while let Some(msg) = socket.recv().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(_) => break,
        };

        let request: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::json!({
                    "type": "error",
                    "error": format!("Invalid JSON: {e}"),
                });
                let _ = socket.send(Message::Text(err.to_string().into())).await;
                continue;
            }
        };

        let message = match request.get("message").and_then(|m| m.as_str()) {
            Some(m) => m.to_owned(),
            None => {
                let err = serde_json::json!({
                    "type": "error",
                    "error": "Missing 'message' field",
                });
                let _ = socket.send(Message::Text(err.to_string().into())).await;
                continue;
            }
        };

        let session_id = request
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(str::to_owned)
            .unwrap_or_else(default_api_session_id);
        let platform = request
            .get("platform")
            .and_then(|p| p.as_str())
            .unwrap_or("websocket");
        let system_prompt = request
            .get("system_prompt")
            .and_then(|s| s.as_str())
            .map(str::to_owned);

        state.requests_total.fetch_add(1, Ordering::Relaxed);
        state.stream_requests_total.fetch_add(1, Ordering::Relaxed);

        let mut service = state.session_service();
        if let Some(sp) = system_prompt {
            service.set_system_prompt_override(sp);
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let session_id_for_stream = session_id.clone();
        let platform_owned = platform.to_owned();
        let run_result = {
            let tx = tx.clone();
            service
                .run_turn_streaming(
                    SessionTurnInput {
                        session_id: &session_id_for_stream,
                        session_platform: &platform_owned,
                        delivery_platform: delivery_platform_from_str(&platform_owned),
                        prompt: &message,
                        title: None,
                        images: Vec::new(),
                    },
                    move |event| {
                        let json = match event {
                            StreamEvent::Chunk(chunk) => {
                                serde_json::json!({"type": "chunk", "content": chunk})
                            }
                            StreamEvent::TurnStarted => {
                                serde_json::json!({"type": "turn_started"})
                            }
                            StreamEvent::ToolCallStart { name, call_id, args_summary } => {
                                serde_json::json!({"type": "tool_call", "tool": name, "call_id": call_id, "args_summary": args_summary})
                            }
                            StreamEvent::ToolCallEnd { name, call_id, duration_ms, success } => {
                                serde_json::json!({"type": "tool_call_end", "tool": name, "call_id": call_id, "duration_ms": duration_ms, "success": success})
                            }
                            StreamEvent::TokenUsage { input_tokens, output_tokens } => {
                                serde_json::json!({"type": "token_usage", "input_tokens": input_tokens, "output_tokens": output_tokens})
                            }
                            StreamEvent::ClarificationNeeded { question } => {
                                serde_json::json!({"type": "clarification", "question": question})
                            }
                            StreamEvent::Warning(msg) => {
                                serde_json::json!({"type": "warning", "message": msg})
                            }
                        };
                        let _ = tx.send(json.to_string());
                    },
                )
                .await
        };
        drop(tx);

        while let Some(event_json) = rx.recv().await {
            if socket.send(Message::Text(event_json.into())).await.is_err() {
                return;
            }
        }

        let final_msg = match run_result {
            Ok(outcome) => {
                state
                    .input_tokens_total
                    .fetch_add(outcome.result.total_input_tokens as u64, Ordering::Relaxed);
                state
                    .output_tokens_total
                    .fetch_add(outcome.result.total_output_tokens as u64, Ordering::Relaxed);
                serde_json::json!({
                    "type": "done",
                    "session_id": outcome.session_id,
                    "response": outcome.result.response,
                    "turns_used": outcome.result.turns_used,
                    "tool_calls_made": outcome.result.tool_calls_made,
                    "total_input_tokens": outcome.result.total_input_tokens,
                    "total_output_tokens": outcome.result.total_output_tokens,
                })
            }
            Err(e) => {
                state.errors_total.fetch_add(1, Ordering::Relaxed);
                serde_json::json!({
                    "type": "error",
                    "error": e.to_string(),
                })
            }
        };

        if socket
            .send(Message::Text(final_msg.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }

    info!("WebSocket client disconnected");
}

// ---------------------------------------------------------------------------
// Batch
// ---------------------------------------------------------------------------

pub(crate) async fn chat_batch_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BatchRequest>,
) -> Result<Json<BatchResponse>, (StatusCode, String)> {
    if request.items.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "batch must contain at least one item".to_owned(),
        ));
    }
    if request.items.len() > MAX_BATCH_SIZE {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("batch exceeds maximum size of {MAX_BATCH_SIZE} items"),
        ));
    }

    let concurrency = request.concurrency.clamp(1, MAX_BATCH_CONCURRENCY);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let state = Arc::clone(&state);

    let mut handles = Vec::with_capacity(request.items.len());

    for (index, item) in request.items.into_iter().enumerate() {
        let state = Arc::clone(&state);
        let sem = Arc::clone(&semaphore);

        handles.push(tokio::spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(permit) => permit,
                Err(_) => {
                    return BatchItemResult {
                        index,
                        session_id: item.session_id.unwrap_or_else(default_api_session_id),
                        response: None,
                        error: Some("batch semaphore closed".to_string()),
                        turns_used: 0,
                        tool_calls_made: 0,
                        estimated_cost: None,
                        total_input_tokens: 0,
                        total_output_tokens: 0,
                    };
                }
            };

            let mut service = state.session_service();
            if let Some(system_prompt) = item.system_prompt {
                service.set_system_prompt_override(system_prompt);
            }
            if let Some(response_format) = item.response_format {
                service.set_response_format(response_format);
            }

            let session_id = item.session_id.unwrap_or_else(default_api_session_id);
            let images: Vec<genesis_provider::ImageUrl> = item
                .images
                .iter()
                .map(|img| genesis_provider::ImageUrl {
                    url: img.url.clone(),
                    detail: img.detail.clone(),
                })
                .collect();

            match service
                .run_turn(SessionTurnInput {
                    session_id: &session_id,
                    session_platform: &item.platform,
                    delivery_platform: delivery_platform_from_str(&item.platform),
                    prompt: &item.message,
                    title: None,
                    images,
                })
                .await
            {
                Ok(outcome) => BatchItemResult {
                    index,
                    session_id: outcome.session_id,
                    response: Some(outcome.result.response),
                    error: None,
                    turns_used: outcome.result.turns_used,
                    tool_calls_made: outcome.result.tool_calls_made,
                    estimated_cost: outcome.result.estimated_cost,
                    total_input_tokens: outcome.result.total_input_tokens,
                    total_output_tokens: outcome.result.total_output_tokens,
                },
                Err(e) => BatchItemResult {
                    index,
                    session_id,
                    response: None,
                    error: Some(e.to_string()),
                    turns_used: 0,
                    tool_calls_made: 0,
                    estimated_cost: None,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                },
            }
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => {
                error!(error = %e, "batch task panicked");
            }
        }
    }

    results.sort_by_key(|r| r.index);

    let total_items = results.len();
    let successful = results.iter().filter(|r| r.error.is_none()).count();
    let failed = total_items - successful;
    let total_estimated_cost: f64 = results.iter().filter_map(|r| r.estimated_cost).sum();

    Ok(Json(BatchResponse {
        results,
        total_items,
        successful,
        failed,
        total_estimated_cost: if total_estimated_cost > 0.0 {
            Some(total_estimated_cost)
        } else {
            None
        },
    }))
}
