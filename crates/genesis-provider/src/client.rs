use std::pin::Pin;

use futures_util::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};

use crate::api_types::{ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse};
use crate::error::ProviderError;
use crate::resolve::ResolvedProvider;

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

    /// Send a chat completion request and return the parsed response.
    pub async fn complete(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        // Ensure the model is set from the client's resolved provider
        if request.model.is_empty() {
            request.model = self.model.clone();
        }

        let mut body = serde_json::to_value(&request)?;

        // Merge extra_body fields into the top-level request if present.
        // This is how provider-specific params (OpenRouter routing hints,
        // Nous Portal tags, etc.) get passed through.
        if let Some(extra) = request.extra_body.take() {
            if let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
                body_obj.remove("extra_body");
                for (key, value) in extra_obj {
                    body_obj.insert(key.clone(), value.clone());
                }
            }
        }

        let response = self.http.post(&self.endpoint).json(&body).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError {
                status: status.as_u16(),
                body,
            });
        }

        let completion: ChatCompletionResponse = response.json().await?;

        if completion.choices.is_empty() {
            return Err(ProviderError::EmptyChoices);
        }

        Ok(completion)
    }

    pub async fn complete_stream(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionChunkStream, ProviderError> {
        if request.model.is_empty() {
            request.model = self.model.clone();
        }
        request.stream = Some(true);

        let mut body = serde_json::to_value(&request)?;

        if let Some(extra) = request.extra_body.take() {
            if let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
                body_obj.remove("extra_body");
                for (key, value) in extra_obj {
                    body_obj.insert(key.clone(), value.clone());
                }
            }
        }

        let response = self.http.post(&self.endpoint).json(&body).send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError {
                status: status.as_u16(),
                body,
            });
        }

        let byte_stream = response.bytes_stream();
        let stream = async_stream::try_stream! {
            futures_util::pin_mut!(byte_stream);
            let mut buffer = String::new();

            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk?;
                let text = std::str::from_utf8(&chunk).map_err(|error| ProviderError::StreamDecode(
                    error.to_string()
                ))?;
                buffer.push_str(text);

                while let Some(event) = take_next_sse_event(&mut buffer) {
                    match parse_sse_event(&event)? {
                        Some(parsed) => yield parsed,
                        None => return,
                    }
                }
            }

            if !buffer.trim().is_empty() {
                if let Some(parsed) = parse_sse_event(buffer.trim())? {
                    yield parsed;
                }
            }
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
}
