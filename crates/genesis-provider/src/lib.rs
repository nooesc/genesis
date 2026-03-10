mod anthropic_types;
mod api_types;
mod client;
mod error;
mod gemini_types;
pub mod model_metadata;
pub mod parsers;
pub mod pricing;
mod resolve;

pub use api_types::{
    ChatChunkChoice, ChatChunkDelta, ChatCompletionChunk, ChatCompletionRequest,
    ChatCompletionResponse, ChatChoice, ChatMessage, ChatTool, ChatToolFunction, ChatUsage,
    ContentPart, FunctionCall, ImageUrl, JsonSchemaSpec, MessageContent, ResponseFormat,
    ThinkingConfig, ToolCallEntry, ToolChoice,
};
pub use client::{ChatClient, ChatCompletionChunkStream};
pub use error::ProviderError;
pub use resolve::{resolve, ResolvedProvider};

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
    // For openai-codex backend, try OAuth credentials first
    if backend.trim().eq_ignore_ascii_case("openai-codex") {
        match genesis_auth::default_auth_path() {
            Ok(auth_path) => {
                match genesis_auth::codex::resolve_credentials(&auth_path).await {
                    Ok(creds) => {
                        tracing::debug!(source = ?creds.source, "resolved OAuth credentials for openai-codex");
                        let provider = ResolvedProvider {
                            base_url: base_url.map(str::to_owned).unwrap_or(creds.base_url),
                            api_key: creds.api_key,
                            model: model.to_owned(),
                            backend: "openai-codex".to_owned(),
                        };
                        return ChatClient::new(&provider);
                    }
                    Err(genesis_auth::AuthError::NotLoggedIn) => {
                        tracing::debug!("no OAuth session for openai-codex, falling back to env vars");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "OAuth resolution failed for openai-codex, falling back to env vars");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not determine auth store path, falling back to env vars");
            }
        }
    }

    let env: BTreeMap<String, String> = std::env::vars().collect();
    let provider = resolve(backend, model, base_url, api_key_env, &env);
    ChatClient::new(&provider)
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
