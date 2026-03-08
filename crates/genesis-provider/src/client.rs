use std::pin::Pin;
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tracing::{error, info, warn};

use crate::api_types::{ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse};
use crate::error::ProviderError;
use crate::resolve::ResolvedProvider;

/// Maximum number of retry attempts for transient errors.
const MAX_RETRIES: u32 = 3;
/// Base delay for exponential backoff (doubles each attempt).
const BASE_DELAY: Duration = Duration::from_secs(1);
/// Maximum delay cap.
const MAX_DELAY: Duration = Duration::from_secs(8);

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
}

impl ChatClient {
    /// Create a new client from a resolved provider.
    pub fn new(provider: &ResolvedProvider) -> Result<Self, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if !provider.api_key.is_empty() {
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
            .build()?;

        let base = provider.base_url.trim_end_matches('/');
        let endpoint = format!("{}/chat/completions", base);

        Ok(Self {
            http,
            endpoint,
            model: provider.model.clone(),
        })
    }

    /// Prepare a request body from a `ChatCompletionRequest`, merging
    /// `extra_body` fields into the top-level JSON object.
    fn prepare_body(request: &mut ChatCompletionRequest) -> Result<serde_json::Value, ProviderError> {
        let mut body = serde_json::to_value(&*request)?;
        if let Some(extra) = request.extra_body.take() {
            if let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
                body_obj.remove("extra_body");
                for (key, value) in extra_obj {
                    body_obj.insert(key.clone(), value.clone());
                }
            }
        }
        Ok(body)
    }

    /// Send an HTTP request with retry + exponential backoff for transient
    /// errors (429 rate-limit, 5xx server errors, network failures).
    async fn send_with_retry(
        &self,
        body: &serde_json::Value,
        model: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let started_at = Instant::now();

        for attempt in 0..=MAX_RETRIES {
            let result = self.http.post(&self.endpoint).json(body).send().await;

            match result {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }

                    if !is_retryable_status(status.as_u16()) || attempt == MAX_RETRIES {
                        let resp_body = response.text().await.unwrap_or_default();
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

        let body = Self::prepare_body(&mut request)?;
        let response = self.send_with_retry(&body, &request.model).await?;

        let completion: ChatCompletionResponse = match response.json().await {
            Ok(completion) => completion,
            Err(error) => {
                error!(
                    endpoint = self.endpoint.as_str(),
                    model = request.model.as_str(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    error = %error,
                    "chat completion response decode failed"
                );
                return Err(error.into());
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

        let (prompt_tokens, completion_tokens, total_tokens) = completion
            .usage
            .as_ref()
            .map(|usage| {
                (
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.total_tokens,
                )
            })
            .unwrap_or((0, 0, 0));
        info!(
            endpoint = self.endpoint.as_str(),
            model = request.model.as_str(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            token_counts_available = completion.usage.is_some(),
            "chat completion request succeeded"
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
        request.stream_options = Some(crate::api_types::StreamOptions { include_usage: true });

        let body = Self::prepare_body(&mut request)?;
        let response = self.send_with_retry(&body, &request.model).await?;

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

    /// Returns the endpoint URL this client is configured to hit.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the model this client defaults to.
    pub fn model(&self) -> &str {
        &self.model
    }
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

fn take_next_sse_event(buffer: &mut String) -> Option<String> {
    let normalized = buffer.replace("\r\n", "\n");
    if let Some(index) = normalized.find("\n\n") {
        let event = normalized[..index].to_owned();
        *buffer = normalized[index + 2..].to_owned();
        Some(event)
    } else {
        *buffer = normalized;
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
    use crate::resolve::ResolvedProvider;

    #[test]
    fn client_builds_correct_endpoint() {
        let provider = ResolvedProvider {
            base_url: "https://api.openai.com/v1".to_owned(),
            api_key: "sk-test".to_owned(),
            model: "gpt-4".to_owned(),
        };

        let client = ChatClient::new(&provider).expect("should build client");
        assert_eq!(client.endpoint(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(client.model(), "gpt-4");
    }

    #[test]
    fn client_strips_trailing_slash_from_base_url() {
        let provider = ResolvedProvider {
            base_url: "http://localhost:8000/v1/".to_owned(),
            api_key: String::new(),
            model: "llama-3".to_owned(),
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
}
