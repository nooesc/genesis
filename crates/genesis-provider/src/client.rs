use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};

use crate::api_types::{ChatCompletionRequest, ChatCompletionResponse};
use crate::error::ProviderError;
use crate::resolve::ResolvedProvider;

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

    /// Returns the endpoint URL this client is configured to hit.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the model this client defaults to.
    pub fn model(&self) -> &str {
        &self.model
    }
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
}
