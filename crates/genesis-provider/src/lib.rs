mod api_types;
mod client;
mod error;
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
pub fn client_from_config(
    backend: &str,
    model: &str,
    base_url: Option<&str>,
    api_key_env: Option<&str>,
) -> Result<ChatClient, ProviderError> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let provider = resolve(backend, model, base_url, api_key_env, &env);
    ChatClient::new(&provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_from_config_builds_without_api_key() {
        let client = client_from_config("openai", "gpt-4", None, None);
        assert!(client.is_ok());
    }

    #[test]
    fn client_from_config_respects_custom_base_url() {
        let client =
            client_from_config("local", "llama-3", Some("http://localhost:11434/v1"), None)
                .expect("should build");
        assert_eq!(
            client.endpoint(),
            "http://localhost:11434/v1/chat/completions"
        );
    }
}
