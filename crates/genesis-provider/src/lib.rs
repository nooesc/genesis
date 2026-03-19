mod anthropic_types;
mod api_types;
pub mod circuit_breaker;
mod client;
mod error;
mod gemini_types;
pub mod model_metadata;
pub mod openrouter_models;
pub mod parsers;
pub mod pricing;
mod resolve;
pub(crate) mod responses_types;

pub use api_types::{
    ChatChoice, ChatChunkChoice, ChatChunkDelta, ChatCompletionChunk, ChatCompletionRequest,
    ChatCompletionResponse, ChatMessage, ChatTool, ChatToolFunction, ChatUsage, ContentPart,
    FunctionCall, ImageUrl, JsonSchemaSpec, MessageContent, ResponseFormat, ThinkingConfig,
    ToolCallEntry, ToolChoice,
};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerRegistry, CircuitState};
pub use client::{ChatClient, ChatCompletionChunkStream};
pub use error::ProviderError;
pub use resolve::{resolve, ResolvedProvider, OPENAI_BASE_URL};

use std::collections::BTreeMap;

/// Build a [`ChatClient`] from genesis config values using the current
/// process environment for API key resolution.
///
/// For the `openai-codex` backend, this will attempt to resolve OAuth
/// credentials from the auth store (with automatic token refresh).
/// Falls back to env-var resolution if OAuth is unavailable.
pub async fn client_from_config(
    backend: &str,
    model: &str,
    base_url: Option<&str>,
    api_key_env: Option<&str>,
) -> Result<ChatClient, ProviderError> {
    let backend = backend.trim().to_ascii_lowercase();
    let backend = backend.as_str();

    // For openai-codex backend, try OAuth credentials first
    if backend == "openai-codex" {
        match genesis_auth::default_auth_path() {
            Ok(auth_path) => match genesis_auth::codex::resolve_credentials(&auth_path).await {
                Ok(creds) => {
                    tracing::debug!(source = ?creds.source, "resolved OAuth credentials for openai-codex");
                    let provider = ResolvedProvider {
                        base_url: base_url.map(str::to_owned).unwrap_or(creds.base_url),
                        api_key: creds.api_key,
                        model: model.to_owned(),
                        backend: "openai-codex".to_owned(),
                    };
                    let client = ChatClient::new(&provider)?;
                    spawn_warmup(&client);
                    return Ok(client);
                }
                Err(genesis_auth::AuthError::NotLoggedIn) => {
                    tracing::debug!("no OAuth session for openai-codex, falling back to env vars");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "OAuth resolution failed for openai-codex, falling back to env vars");
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "could not determine auth store path, falling back to env vars");
            }
        }
    }

    let env: BTreeMap<String, String> = std::env::vars().collect();
    let provider = resolve(backend, model, base_url, api_key_env, &env);
    let client = ChatClient::new(&provider)?;
    spawn_warmup(&client);
    Ok(client)
}

/// Build a `ChatClient` from config, applying circuit breaker thresholds if set.
pub async fn client_from_config_with_circuit_breaker(
    backend: &str,
    model: &str,
    base_url: Option<&str>,
    api_key_env: Option<&str>,
    failure_threshold: Option<u32>,
    cooldown_secs: Option<u64>,
) -> Result<ChatClient, ProviderError> {
    let mut client = client_from_config(backend, model, base_url, api_key_env).await?;
    if let (Some(ft), Some(cs)) = (failure_threshold, cooldown_secs) {
        client.set_circuit_breaker(ft, cs);
    }
    Ok(client)
}

/// Build a `ChatClient` using a shared [`CircuitBreakerRegistry`].
///
/// The registry ensures that every client targeting the same
/// `(backend, model)` pair shares one `Arc<CircuitBreaker>`, enabling the
/// gateway health endpoint to report live circuit state.
pub async fn client_from_config_with_registry(
    backend: &str,
    model: &str,
    base_url: Option<&str>,
    api_key_env: Option<&str>,
    failure_threshold: Option<u32>,
    cooldown_secs: Option<u64>,
    registry: &CircuitBreakerRegistry,
) -> Result<ChatClient, ProviderError> {
    let mut client = client_from_config(backend, model, base_url, api_key_env).await?;
    let shared_cb = registry.get_or_create(backend, model, failure_threshold, cooldown_secs);
    client.set_shared_circuit_breaker(shared_cb);
    Ok(client)
}

/// Spawn a background task to pre-establish the TCP+TLS connection.
///
/// This runs concurrently with other initialization work (system prompt
/// building, tool registration, etc.) so the connection is likely ready
/// by the time the first real LLM request is made.
///
/// Skipped when running tests (`cfg!(test)`) or when the endpoint points
/// to a local address, to avoid firing real network requests in test suites.
fn spawn_warmup(client: &ChatClient) {
    if cfg!(test) {
        return;
    }

    let endpoint = client.endpoint();
    if endpoint.contains("localhost")
        || endpoint.contains("127.0.0.1")
        || endpoint.contains("[::1]")
    {
        tracing::debug!(endpoint = %endpoint, "skipping connection warmup for local endpoint");
        return;
    }

    let client = client.clone();
    tokio::spawn(async move { client.warmup().await });
}

/// Clear cached Codex credentials and build a fresh client.
///
/// Called when a 401 response suggests the token may have expired.
/// Clears the in-memory cache so `resolve_credentials` will re-read
/// from disk and refresh if needed.
pub async fn refresh_codex_client(
    model: &str,
    base_url: Option<&str>,
) -> Result<ChatClient, ProviderError> {
    genesis_auth::codex::clear_credentials_cache().await;
    client_from_config("openai-codex", model, base_url, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_from_config_builds_without_api_key() {
        let client = client_from_config("openai", "gpt-4", None, None).await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn client_from_config_codex_falls_back_when_not_logged_in() {
        // openai-codex with no auth store should fall through to env-var resolution
        // without panicking or returning an error
        let client = client_from_config("openai-codex", "o3-pro", None, None).await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn client_from_config_non_codex_skips_oauth() {
        // Non-codex backends should never attempt OAuth resolution
        let client = client_from_config("openai", "gpt-4", None, None).await;
        assert!(client.is_ok());
        assert_eq!(client.unwrap().backend(), "openai");
    }

    #[tokio::test]
    async fn client_from_config_respects_custom_base_url() {
        let client =
            client_from_config("local", "llama-3", Some("http://localhost:11434/v1"), None)
                .await
                .expect("should build");
        assert_eq!(
            client.endpoint(),
            "http://localhost:11434/v1/chat/completions"
        );
    }
}
