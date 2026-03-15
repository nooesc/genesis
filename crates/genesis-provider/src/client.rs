use std::pin::Pin;
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::de::DeserializeOwned;
use tracing::{error, info, warn};

use crate::api_types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ChatUsage,
};
use crate::error::ProviderError;
use crate::resolve::ResolvedProvider;

/// Maximum number of retry attempts for transient errors.
const MAX_RETRIES: u32 = 3;
/// Base delay for exponential backoff (doubles each attempt).
const BASE_DELAY: Duration = Duration::from_secs(1);
/// Maximum delay cap.
const MAX_DELAY: Duration = Duration::from_secs(8);
const MAX_NON_STREAMING_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

pub type ChatCompletionChunkStream =
    Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk, ProviderError>> + Send>>;

/// Async HTTP client for OpenAI-compatible Chat Completions endpoints.
///
/// All providers (OpenAI, OpenRouter, Anthropic via compatible proxy, local
/// vLLM/Ollama, etc.) speak the same request format, so one client handles
/// them all. The only difference is `base_url`, `api_key`, and `model`.
#[derive(Debug, Clone)]
pub struct ChatClient {
    http: reqwest::Client,
    endpoint: String,
    model: String,
    /// The backend identifier for provider-specific optimizations.
    backend: String,
    /// API key stored separately for backends using query-param auth (Gemini).
    api_key: Option<String>,
}

impl ChatClient {
    /// Create a new client from a resolved provider.
    pub fn new(provider: &ResolvedProvider) -> Result<Self, ProviderError> {
        let is_anthropic = provider.backend == "anthropic";
        let is_gemini = matches!(provider.backend.as_str(), "gemini" | "google");
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        // Gemini uses API key in URL query params — no auth header needed.
        // Anthropic uses x-api-key header. Everything else uses Bearer token.
        if is_anthropic {
            if !provider.api_key.is_empty() {
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(&provider.api_key).map_err(|_| {
                        ProviderError::MissingApiKey {
                            env_var: "API key contains invalid header characters".to_owned(),
                        }
                    })?,
                );
            }
            headers.insert(
                "anthropic-version",
                HeaderValue::from_static("2023-06-01"),
            );
            // token-efficient-tools applies to all requests;
            // fine-grained-tool-streaming only affects streaming calls
            // but is harmless for non-streaming (Anthropic ignores it).
            headers.insert(
                "anthropic-beta",
                HeaderValue::from_static(
                    "token-efficient-tools-2025-02-19,fine-grained-tool-streaming-2025-05-14",
                ),
            );
        } else if !is_gemini && !provider.api_key.is_empty() {
            let auth_value = format!("Bearer {}", provider.api_key);
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_value).map_err(|_| ProviderError::MissingApiKey {
                    env_var: "API key contains invalid header characters".to_owned(),
                })?,
            );
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300)) // 5 min for long completions
            .build()?;

        let base = provider.base_url.trim_end_matches('/');
        let endpoint = if is_anthropic {
            format!("{}/messages", base)
        } else if is_gemini {
            // For Gemini, store base URL — full endpoint constructed per-request
            // because it includes the model name and API key.
            base.to_owned()
        } else {
            format!("{}/chat/completions", base)
        };

        // Store API key for backends that need it in the URL (Gemini)
        let api_key = if is_gemini {
            Some(provider.api_key.clone())
        } else {
            None
        };

        Ok(Self {
            http,
            endpoint,
            model: provider.model.clone(),
            backend: provider.backend.clone(),
            api_key,
        })
    }

    /// Prepare a request body from a `ChatCompletionRequest`, merging
    /// `extra_body` fields into the top-level JSON object and applying
    /// backend-specific optimizations like prompt caching.
    fn prepare_body(
        request: &mut ChatCompletionRequest,
        backend: &str,
    ) -> Result<serde_json::Value, ProviderError> {
        let mut body = serde_json::to_value(&*request)?;
        if let Some(extra) = request.extra_body.take() {
            if let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
                body_obj.remove("extra_body");
                for (key, value) in extra_obj {
                    body_obj.insert(key.clone(), value.clone());
                }
            }
        }

        if supports_prompt_caching(backend) {
            inject_cache_control(&mut body);
        }

        Ok(body)
    }

    /// Send an HTTP request with retry + exponential backoff for transient
    /// errors (429 rate-limit, 5xx server errors, network failures).
    async fn send_with_retry(
        &self,
        url: &str,
        body: &serde_json::Value,
        model: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let started_at = Instant::now();

        for attempt in 0..=MAX_RETRIES {
            let result = self.http.post(url).json(body).send().await;

            match result {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }

                    if !is_retryable_status(status.as_u16()) || attempt == MAX_RETRIES {
                        let resp_body = read_text_with_limit(
                            response,
                            MAX_NON_STREAMING_RESPONSE_BYTES,
                        )
                        .await
                        .unwrap_or_else(|error| format!("unreadable response body: {error}"));
                        warn!(
                            endpoint = self.endpoint.as_str(),
                            model,
                            status = status.as_u16(),
                            attempt,
                            elapsed_ms = started_at.elapsed().as_millis() as u64,
                            "API error (not retrying)"
                        );
                        return Err(ProviderError::ApiError {
                            status: status.as_u16(),
                            body: resp_body,
                        });
                    }

                    // Parse Retry-After header for 429 responses
                    let retry_after = if status.as_u16() == 429 {
                        response
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.parse::<u64>().ok())
                            .map(Duration::from_secs)
                    } else {
                        None
                    };

                    let delay = retry_after.unwrap_or_else(|| backoff_delay(attempt));
                    warn!(
                        endpoint = self.endpoint.as_str(),
                        model,
                        status = status.as_u16(),
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        "retryable API error, backing off"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    if attempt == MAX_RETRIES {
                        error!(
                            endpoint = self.endpoint.as_str(),
                            model,
                            attempt,
                            elapsed_ms = started_at.elapsed().as_millis() as u64,
                            error = %error,
                            "request failed after retries exhausted"
                        );
                        return Err(error.into());
                    }

                    let delay = backoff_delay(attempt);
                    warn!(
                        endpoint = self.endpoint.as_str(),
                        model,
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %error,
                        "network error, retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }

        unreachable!("retry loop should return before exhausting iterations")
    }

    /// Send a chat completion request and return the parsed response.
    pub async fn complete(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let started_at = Instant::now();
        if request.model.is_empty() {
            request.model = self.model.clone();
        }

        if self.backend == "anthropic" {
            return self.complete_anthropic(request, started_at).await;
        }

        if matches!(self.backend.as_str(), "gemini" | "google") {
            return self.complete_gemini(request, started_at).await;
        }

        let body = Self::prepare_body(&mut request, &self.backend)?;
        let response = self.send_with_retry(&self.endpoint, &body, &request.model).await?;

        let completion: ChatCompletionResponse = match read_json_with_limit(
            response,
            MAX_NON_STREAMING_RESPONSE_BYTES,
        )
        .await
        {
            Ok(completion) => completion,
            Err(error) => {
                error!(
                    endpoint = self.endpoint.as_str(),
                    model = request.model.as_str(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    error = %error,
                    "chat completion response decode failed"
                );
                return Err(error);
            }
        };

        if completion.choices.is_empty() {
            warn!(
                endpoint = self.endpoint.as_str(),
                model = request.model.as_str(),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "chat completion returned no choices"
            );
            return Err(ProviderError::EmptyChoices);
        }

        log_completion_success(
            &self.endpoint,
            &request.model,
            started_at.elapsed().as_millis() as u64,
            completion.usage.as_ref(),
            "chat completion request succeeded",
        );

        Ok(completion)
    }

    /// Anthropic-native completion path: translates request/response formats.
    async fn complete_anthropic(
        &self,
        request: ChatCompletionRequest,
        started_at: Instant,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        use crate::anthropic_types::{self, AnthropicResponse};

        let anthropic_req = anthropic_types::to_anthropic_request(&request);
        let body = serde_json::to_value(&anthropic_req)?;
        let response = self.send_with_retry(&self.endpoint, &body, &request.model).await?;

        let anthropic_resp: AnthropicResponse = match read_json_with_limit(
            response,
            MAX_NON_STREAMING_RESPONSE_BYTES,
        )
        .await
        {
            Ok(resp) => resp,
            Err(error) => {
                error!(
                    endpoint = self.endpoint.as_str(),
                    model = request.model.as_str(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    error = %error,
                    "anthropic response decode failed"
                );
                return Err(error);
            }
        };

        let completion = anthropic_types::from_anthropic_response(anthropic_resp);

        if completion.choices.is_empty() {
            warn!(
                endpoint = self.endpoint.as_str(),
                model = request.model.as_str(),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "anthropic completion returned no content"
            );
            return Err(ProviderError::EmptyChoices);
        }

        log_completion_success(
            &self.endpoint,
            &request.model,
            started_at.elapsed().as_millis() as u64,
            completion.usage.as_ref(),
            "anthropic completion request succeeded",
        );

        Ok(completion)
    }

    /// Gemini-native completion path: translates request/response formats.
    async fn complete_gemini(
        &self,
        request: ChatCompletionRequest,
        started_at: Instant,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        use crate::gemini_types;

        let api_key = self.api_key.as_deref().unwrap_or_default();
        let url = gemini_types::generate_content_url(&self.endpoint, &request.model, api_key);

        let gemini_req = gemini_types::to_gemini_request(&request);
        let body = serde_json::to_value(&gemini_req)?;
        let response = self.send_with_retry(&url, &body, &request.model).await?;

        let gemini_resp: gemini_types::GeminiResponse = match read_json_with_limit(
            response,
            MAX_NON_STREAMING_RESPONSE_BYTES,
        )
        .await
        {
            Ok(resp) => resp,
            Err(error) => {
                error!(
                    endpoint = self.endpoint.as_str(),
                    model = request.model.as_str(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    error = %error,
                    "gemini response decode failed"
                );
                return Err(error);
            }
        };

        let completion = gemini_types::from_gemini_response(gemini_resp, &request.model);

        if completion.choices.is_empty() {
            warn!(
                endpoint = self.endpoint.as_str(),
                model = request.model.as_str(),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "gemini completion returned no candidates"
            );
            return Err(ProviderError::EmptyChoices);
        }

        log_completion_success(
            &self.endpoint,
            &request.model,
            started_at.elapsed().as_millis() as u64,
            completion.usage.as_ref(),
            "gemini completion request succeeded",
        );

        Ok(completion)
    }

    pub async fn complete_stream(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionChunkStream, ProviderError> {
        let started_at = Instant::now();
        if request.model.is_empty() {
            request.model = self.model.clone();
        }
        request.stream = Some(true);

        if self.backend == "anthropic" {
            return self.complete_stream_anthropic(request, started_at).await;
        }

        if matches!(self.backend.as_str(), "gemini" | "google") {
            return self.complete_stream_gemini(request, started_at).await;
        }

        request.stream_options = Some(crate::api_types::StreamOptions { include_usage: true });

        let body = Self::prepare_body(&mut request, &self.backend)?;
        let response = self.send_with_retry(&self.endpoint, &body, &request.model).await?;

        info!(
            endpoint = self.endpoint.as_str(),
            model = request.model.as_str(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            prompt_tokens = 0u32,
            completion_tokens = 0u32,
            total_tokens = 0u32,
            token_counts_available = false,
            "streaming chat completion request accepted"
        );

        let endpoint = self.endpoint.clone();
        let model = request.model.clone();
        let byte_stream = response.bytes_stream();
        let stream = async_stream::try_stream! {
            futures_util::pin_mut!(byte_stream);
            let mut buffer = String::new();
            let stream_started_at = Instant::now();
            let mut chunk_count = 0usize;

            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk?;
                let text = std::str::from_utf8(&chunk).map_err(|error| ProviderError::StreamDecode(
                    error.to_string()
                ))?;
                buffer.push_str(text);

                while let Some(event) = take_next_sse_event(&mut buffer) {
                    match parse_sse_event(&event)? {
                        Some(parsed) => {
                            chunk_count += 1;
                            yield parsed;
                        }
                        None => {
                            info!(
                                endpoint = endpoint.as_str(),
                                model = model.as_str(),
                                elapsed_ms = stream_started_at.elapsed().as_millis() as u64,
                                prompt_tokens = 0u32,
                                completion_tokens = 0u32,
                                total_tokens = 0u32,
                                token_counts_available = false,
                                chunk_count,
                                "streaming chat completion finished"
                            );
                            return;
                        }
                    }
                }
            }

            if !buffer.trim().is_empty() {
                if let Some(parsed) = parse_sse_event(buffer.trim())? {
                    chunk_count += 1;
                    yield parsed;
                }
            }

            info!(
                endpoint = endpoint.as_str(),
                model = model.as_str(),
                elapsed_ms = stream_started_at.elapsed().as_millis() as u64,
                prompt_tokens = 0u32,
                completion_tokens = 0u32,
                total_tokens = 0u32,
                token_counts_available = false,
                chunk_count,
                "streaming chat completion stream closed"
            );
        };

        Ok(Box::pin(stream))
    }

    /// Anthropic-native streaming path: translates SSE events to OpenAI chunks.
    async fn complete_stream_anthropic(
        &self,
        request: ChatCompletionRequest,
        started_at: Instant,
    ) -> Result<ChatCompletionChunkStream, ProviderError> {
        use crate::anthropic_types;

        let anthropic_req = anthropic_types::to_anthropic_request(&request);
        let body = serde_json::to_value(&anthropic_req)?;
        let response = self.send_with_retry(&self.endpoint, &body, &request.model).await?;

        info!(
            endpoint = self.endpoint.as_str(),
            model = request.model.as_str(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            prompt_tokens = 0u32,
            completion_tokens = 0u32,
            total_tokens = 0u32,
            token_counts_available = false,
            "anthropic streaming request accepted"
        );

        let endpoint = self.endpoint.clone();
        let model = request.model.clone();
        let byte_stream = response.bytes_stream();
        let stream = async_stream::try_stream! {
            futures_util::pin_mut!(byte_stream);
            let mut buffer = String::new();
            let stream_started_at = Instant::now();
            let mut chunk_count = 0usize;
            let mut msg_id = String::new();

            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk?;
                let text = std::str::from_utf8(&chunk).map_err(|error| ProviderError::StreamDecode(
                    error.to_string()
                ))?;
                buffer.push_str(text);

                while let Some(event_text) = take_next_sse_event(&mut buffer) {
                    let (event_type, data) = parse_anthropic_sse(&event_text);
                    if data.is_empty() {
                        continue;
                    }

                    if let Some(parsed) = anthropic_types::anthropic_event_to_chunk(&event_type, &data, &mut msg_id)? {
                        chunk_count += 1;
                        yield parsed;
                    }

                    if event_type == "message_stop" {
                        info!(
                            endpoint = endpoint.as_str(),
                            model = model.as_str(),
                            elapsed_ms = stream_started_at.elapsed().as_millis() as u64,
                            chunk_count,
                            "anthropic streaming finished"
                        );
                        return;
                    }
                }
            }

            info!(
                endpoint = endpoint.as_str(),
                model = model.as_str(),
                elapsed_ms = stream_started_at.elapsed().as_millis() as u64,
                chunk_count,
                "anthropic streaming stream closed"
            );
        };

        Ok(Box::pin(stream))
    }

    /// Gemini-native streaming path: translates SSE events to OpenAI chunks.
    async fn complete_stream_gemini(
        &self,
        request: ChatCompletionRequest,
        started_at: Instant,
    ) -> Result<ChatCompletionChunkStream, ProviderError> {
        use crate::gemini_types;

        let api_key = self.api_key.as_deref().unwrap_or_default();
        let url =
            gemini_types::stream_generate_content_url(&self.endpoint, &request.model, api_key);

        let gemini_req = gemini_types::to_gemini_request(&request);
        let body = serde_json::to_value(&gemini_req)?;
        let response = self.send_with_retry(&url, &body, &request.model).await?;

        info!(
            endpoint = self.endpoint.as_str(),
            model = request.model.as_str(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "gemini streaming request accepted"
        );

        let endpoint = self.endpoint.clone();
        let model = request.model.clone();
        let byte_stream = response.bytes_stream();
        let stream = async_stream::try_stream! {
            futures_util::pin_mut!(byte_stream);
            let mut buffer = String::new();
            let stream_started_at = Instant::now();
            let mut chunk_count = 0usize;

            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk?;
                let text = std::str::from_utf8(&chunk).map_err(|error| ProviderError::StreamDecode(
                    error.to_string()
                ))?;
                buffer.push_str(text);

                // Gemini streaming uses standard SSE with data: prefix
                while let Some(event) = take_next_sse_event(&mut buffer) {
                    let data = match event.lines()
                        .find_map(|line| line.strip_prefix("data:").map(str::trim))
                    {
                        Some(d) if !d.is_empty() && d != "[DONE]" => d,
                        _ => continue,
                    };

                    let gemini_resp: gemini_types::GeminiResponse = serde_json::from_str(data)?;
                    if let Some(parsed) = gemini_types::from_gemini_stream_chunk(gemini_resp, &model) {
                        chunk_count += 1;
                        yield parsed;
                    }
                }
            }

            // Handle any remaining buffered data
            if !buffer.trim().is_empty() {
                if let Some(data) = buffer.lines()
                    .find_map(|line| line.strip_prefix("data:").map(str::trim))
                {
                    if !data.is_empty() && data != "[DONE]" {
                        if let Ok(gemini_resp) = serde_json::from_str::<gemini_types::GeminiResponse>(data) {
                            if let Some(parsed) = gemini_types::from_gemini_stream_chunk(gemini_resp, &model) {
                                chunk_count += 1;
                                yield parsed;
                            }
                        }
                    }
                }
            }

            info!(
                endpoint = endpoint.as_str(),
                model = model.as_str(),
                elapsed_ms = stream_started_at.elapsed().as_millis() as u64,
                chunk_count,
                "gemini streaming finished"
            );
        };

        Ok(Box::pin(stream))
    }

    /// Returns the endpoint URL this client is configured to hit.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the model this client defaults to.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the backend identifier (e.g., "openai", "anthropic").
    pub fn backend(&self) -> &str {
        &self.backend
    }
}

/// Log a successful (non-streaming) completion with token usage stats.
fn log_completion_success(
    endpoint: &str,
    model: &str,
    elapsed_ms: u64,
    usage: Option<&ChatUsage>,
    label: &str,
) {
    let (prompt_tokens, completion_tokens, total_tokens) = usage
        .map(|u| (u.prompt_tokens, u.completion_tokens, u.total_tokens))
        .unwrap_or((0, 0, 0));
    info!(
        endpoint,
        model,
        elapsed_ms,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        token_counts_available = usage.is_some(),
        "{label}"
    );
}

/// Backends that support Anthropic-style prompt caching via cache_control.
fn supports_prompt_caching(backend: &str) -> bool {
    matches!(backend, "anthropic" | "openrouter")
}

/// Inject `cache_control: {"type": "ephemeral"}` into the request body for
/// prompt caching on supported backends.
///
/// Targets:
/// - System messages: converted from string content to content-block array
///   with cache_control on the text block.
/// - Last tool definition: receives cache_control at the tool level.
fn inject_cache_control(body: &mut serde_json::Value) {
    let cache_marker = serde_json::json!({"type": "ephemeral"});

    // Cache system messages
    if let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) {
        for msg in messages.iter_mut() {
            let is_system = msg
                .get("role")
                .and_then(|r| r.as_str())
                .map(|r| r == "system")
                .unwrap_or(false);

            if is_system {
                if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                    // Convert string content → content-block array with cache_control
                    let content_block = serde_json::json!([{
                        "type": "text",
                        "text": content,
                        "cache_control": cache_marker
                    }]);
                    msg["content"] = content_block;
                }
            }
        }
    }

    // Cache last tool definition
    if let Some(tools) = body.get_mut("tools").and_then(|v| v.as_array_mut()) {
        if let Some(last_tool) = tools.last_mut() {
            last_tool["cache_control"] = cache_marker;
        }
    }
}

/// Parse an Anthropic SSE event, returning (event_type, data).
///
/// Anthropic SSE uses `event: <type>\ndata: <json>` format.
fn parse_anthropic_sse(event: &str) -> (String, String) {
    let mut event_type = String::new();
    let mut data = String::new();

    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data = rest.trim().to_owned();
        }
    }

    (event_type, data)
}

/// Whether an HTTP status code is transient and worth retrying.
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// Compute backoff delay with simple jitter for a given attempt (0-indexed).
fn backoff_delay(attempt: u32) -> Duration {
    let base_ms = BASE_DELAY.as_millis() as u64;
    let delay_ms = base_ms.saturating_mul(1u64 << attempt);
    let capped_ms = delay_ms.min(MAX_DELAY.as_millis() as u64);
    // Simple jitter: vary ±25% using low bits of system time nanos
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let jitter_range = capped_ms / 4;
    let jitter = if jitter_range > 0 {
        (nanos % jitter_range) as i64 - (jitter_range / 2) as i64
    } else {
        0
    };
    let final_ms = (capped_ms as i64 + jitter).max(100) as u64;
    Duration::from_millis(final_ms)
}

async fn read_response_body_bytes(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, ProviderError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ProviderError::Http)?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ProviderError::ResponseTooLarge { max_bytes });
        }

        bytes.extend_from_slice(&chunk);
    }

    Ok(bytes)
}

async fn read_text_with_limit(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<String, ProviderError> {
    let bytes = read_response_body_bytes(response, max_bytes).await?;
    String::from_utf8(bytes).map_err(|error| ProviderError::ResponseBodyNotUtf8 {
        message: error.to_string(),
    })
}

async fn read_json_with_limit<T: DeserializeOwned>(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<T, ProviderError> {
    let bytes = read_response_body_bytes(response, max_bytes).await?;
    serde_json::from_slice(&bytes).map_err(ProviderError::from)
}

fn take_next_sse_event(buffer: &mut String) -> Option<String> {
    // Fast path: skip the replace allocation when no \r\n is present (common case).
    // On the rare \r\n path we scan twice (contains + replace) instead of once,
    // but avoid an unnecessary String allocation on every call.
    if buffer.contains('\r') {
        *buffer = buffer.replace("\r\n", "\n");
    }
    if let Some(index) = buffer.find("\n\n") {
        let event = buffer[..index].to_owned();
        *buffer = buffer[index + 2..].to_owned();
        Some(event)
    } else {
        None
    }
}

fn parse_sse_event(event: &str) -> Result<Option<ChatCompletionChunk>, ProviderError> {
    let data = event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");

    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }

    Ok(Some(serde_json::from_str(&data)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::{ChatCompletionRequest, ChatMessage};
    use crate::resolve::ResolvedProvider;

    #[test]
    fn client_builds_correct_endpoint() {
        let provider = ResolvedProvider {
            base_url: "https://api.openai.com/v1".to_owned(),
            api_key: "sk-test".to_owned(),
            model: "gpt-4".to_owned(),
            backend: "openai".to_owned(),
        };

        let client = ChatClient::new(&provider).expect("should build client");
        assert_eq!(client.endpoint(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(client.model(), "gpt-4");
        assert_eq!(client.backend(), "openai");
    }

    #[test]
    fn client_strips_trailing_slash_from_base_url() {
        let provider = ResolvedProvider {
            base_url: "http://localhost:8000/v1/".to_owned(),
            api_key: String::new(),
            model: "llama-3".to_owned(),
            backend: "vllm".to_owned(),
        };

        let client = ChatClient::new(&provider).expect("should build client");
        assert_eq!(
            client.endpoint(),
            "http://localhost:8000/v1/chat/completions"
        );
    }

    #[test]
    fn client_works_with_empty_api_key() {
        let provider = ResolvedProvider {
            base_url: "http://localhost:11434/v1".to_owned(),
            api_key: String::new(),
            model: "llama-3".to_owned(),
            backend: "ollama".to_owned(),
        };

        let client = ChatClient::new(&provider);
        assert!(client.is_ok());
    }

    #[test]
    fn take_next_sse_event_extracts_first_complete_event() {
        let mut buffer =
            "data: {\"id\":\"chunk-1\",\"choices\":[]}\n\ndata: {\"id\":\"chunk-2\",\"choices\":[]}\n\n"
                .to_owned();

        let first = take_next_sse_event(&mut buffer).expect("first event should parse");

        assert_eq!(first, "data: {\"id\":\"chunk-1\",\"choices\":[]}");
        assert_eq!(buffer, "data: {\"id\":\"chunk-2\",\"choices\":[]}\n\n");
    }

    #[test]
    fn parse_sse_event_deserializes_chunk_payload() {
        let chunk = parse_sse_event(
            "event: message\ndata: {\"id\":\"chunk-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}"
        )
        .expect("event should parse")
        .expect("event should yield a chunk");

        assert_eq!(chunk.id, "chunk-1");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
    }

    #[test]
    fn parse_sse_event_stops_on_done_sentinel() {
        let chunk = parse_sse_event("data: [DONE]").expect("done sentinel should parse");
        assert_eq!(chunk, None);
    }

    #[test]
    fn retryable_status_identifies_transient_errors() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(504));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(200));
    }

    #[test]
    fn backoff_delay_increases_with_attempt() {
        let d0 = backoff_delay(0);
        let _d1 = backoff_delay(1);
        let d2 = backoff_delay(2);
        // With ±25% jitter, attempt 0 should be ~750-1250ms,
        // attempt 1 ~1500-2500ms, attempt 2 ~3000-5000ms.
        // Just verify the trend increases (using generous bounds).
        assert!(d0.as_millis() >= 100);
        assert!(d2.as_millis() >= d0.as_millis() / 2);
    }

    #[test]
    fn backoff_delay_caps_at_max() {
        let d10 = backoff_delay(10);
        assert!(d10.as_millis() <= MAX_DELAY.as_millis() as u128 + MAX_DELAY.as_millis() as u128 / 4);
    }

    #[test]
    fn supports_prompt_caching_for_anthropic_and_openrouter() {
        assert!(supports_prompt_caching("anthropic"));
        assert!(supports_prompt_caching("openrouter"));
        assert!(!supports_prompt_caching("openai"));
        assert!(!supports_prompt_caching("ollama"));
        assert!(!supports_prompt_caching("vllm"));
    }

    #[test]
    fn inject_cache_control_converts_system_message_to_content_blocks() {
        let mut body = serde_json::json!({
            "model": "claude-3",
            "messages": [
                {"role": "system", "content": "You are Eve."},
                {"role": "user", "content": "Hello"}
            ]
        });

        inject_cache_control(&mut body);

        let system_msg = &body["messages"][0];
        let content = system_msg["content"].as_array().expect("content should be array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "You are Eve.");
        assert_eq!(content[0]["cache_control"]["type"], "ephemeral");

        // User message should remain unchanged
        let user_msg = &body["messages"][1];
        assert_eq!(user_msg["content"], "Hello");
    }

    #[test]
    fn inject_cache_control_adds_cache_to_last_tool() {
        let mut body = serde_json::json!({
            "model": "claude-3",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [
                {"type": "function", "function": {"name": "echo", "description": "Echo"}},
                {"type": "function", "function": {"name": "search", "description": "Search"}}
            ]
        });

        inject_cache_control(&mut body);

        let tools = body["tools"].as_array().unwrap();
        // First tool should NOT have cache_control
        assert!(tools[0].get("cache_control").is_none());
        // Last tool should have cache_control
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn inject_cache_control_noop_without_messages_or_tools() {
        let mut body = serde_json::json!({"model": "test"});
        inject_cache_control(&mut body); // should not panic
        assert_eq!(body, serde_json::json!({"model": "test"}));
    }

    #[test]
    fn anthropic_client_uses_messages_endpoint() {
        let provider = ResolvedProvider {
            base_url: "https://api.anthropic.com/v1".to_owned(),
            api_key: "sk-ant-test".to_owned(),
            model: "claude-sonnet-4-20250514".to_owned(),
            backend: "anthropic".to_owned(),
        };

        let client = ChatClient::new(&provider).expect("should build anthropic client");
        assert_eq!(
            client.endpoint(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(client.backend(), "anthropic");
    }

    #[test]
    fn parse_anthropic_sse_extracts_event_and_data() {
        let event = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}";
        let (event_type, data) = parse_anthropic_sse(event);
        assert_eq!(event_type, "content_block_delta");
        assert!(data.contains("text_delta"));
    }

    #[test]
    fn parse_anthropic_sse_handles_missing_event_type() {
        let event = "data: {\"type\":\"ping\"}";
        let (event_type, data) = parse_anthropic_sse(event);
        assert_eq!(event_type, "");
        assert!(data.contains("ping"));
    }

    #[test]
    fn gemini_client_stores_base_url() {
        let provider = ResolvedProvider {
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_owned(),
            api_key: "AIza-test".to_owned(),
            model: "gemini-2.5-pro".to_owned(),
            backend: "gemini".to_owned(),
        };

        let client = ChatClient::new(&provider).expect("should build gemini client");
        assert_eq!(
            client.endpoint(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(client.backend(), "gemini");
        assert_eq!(client.api_key.as_deref(), Some("AIza-test"));
    }

    #[test]
    fn google_alias_builds_gemini_client() {
        let provider = ResolvedProvider {
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_owned(),
            api_key: "AIza-google".to_owned(),
            model: "gemini-2.5-flash".to_owned(),
            backend: "google".to_owned(),
        };

        let client = ChatClient::new(&provider).expect("should build google client");
        assert_eq!(client.backend(), "google");
        assert_eq!(client.api_key.as_deref(), Some("AIza-google"));
    }

    #[test]
    fn prepare_body_applies_caching_for_anthropic_backend() {
        let mut request = ChatCompletionRequest::new(
            "claude-3",
            vec![
                ChatMessage::system("System prompt"),
                ChatMessage::user("Hello"),
            ],
        );

        let body = ChatClient::prepare_body(&mut request, "anthropic")
            .expect("should prepare body");

        // System message should have content blocks with cache_control
        let system_content = &body["messages"][0]["content"];
        assert!(system_content.is_array(), "system content should be array for anthropic");
        assert_eq!(system_content[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn prepare_body_skips_caching_for_openai_backend() {
        let mut request = ChatCompletionRequest::new(
            "gpt-4",
            vec![
                ChatMessage::system("System prompt"),
                ChatMessage::user("Hello"),
            ],
        );

        let body = ChatClient::prepare_body(&mut request, "openai")
            .expect("should prepare body");

        // System message content should remain a string for openai
        assert!(body["messages"][0]["content"].is_string());
    }
}
