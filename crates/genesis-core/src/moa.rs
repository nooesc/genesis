//! Mixture of Agents (MoA) — multi-model collaboration.
//!
//! Runs multiple "proposer" models in parallel, then feeds their outputs to an
//! "aggregator" model that synthesizes a single high-quality response. Can be
//! applied in multiple layers for iterative refinement.
//!
//! Reference: <https://arxiv.org/abs/2406.04692>

use std::collections::BTreeMap;

use genesis_provider::{
    resolve, ChatClient, ChatCompletionRequest, ChatMessage, ProviderError, ResolvedProvider,
};
use thiserror::Error;

/// Configuration for a single model in the MoA ensemble.
#[derive(Debug, Clone)]
pub struct MoaModelConfig {
    pub backend: String,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
}

/// Configuration for a Mixture of Agents run.
#[derive(Debug, Clone)]
pub struct MoaConfig {
    /// Models that generate independent proposals.
    pub proposers: Vec<MoaModelConfig>,
    /// Model that synthesizes proposer outputs into a final response.
    pub aggregator: MoaModelConfig,
    /// Number of layers (1 = single round of proposals + aggregation).
    pub layers: usize,
    /// Optional temperature for all models.
    pub temperature: Option<f64>,
    /// Optional max tokens for all models.
    pub max_tokens: Option<u32>,
}

/// The result of an MoA run.
#[derive(Debug, Clone)]
pub struct MoaResult {
    /// The final aggregated response.
    pub response: String,
    /// Individual proposer responses from the last layer.
    pub proposer_responses: Vec<ProposerResponse>,
    /// Number of layers executed.
    pub layers_completed: usize,
}

/// A single proposer's contribution.
#[derive(Debug, Clone)]
pub struct ProposerResponse {
    pub model: String,
    pub content: String,
}

#[derive(Debug, Error)]
pub enum MoaError {
    #[error("no proposers configured")]
    NoProposers,
    #[error("provider error for model '{model}': {source}")]
    Provider {
        model: String,
        #[source]
        source: ProviderError,
    },
    #[error("all proposers failed")]
    AllProposersFailed,
}

const AGGREGATOR_SYSTEM_PROMPT: &str = "\
You have been provided with a set of responses from various AI models to the \
latest user query. Your task is to synthesize these responses into a single, \
high-quality response. It is crucial to critically evaluate the information \
provided in these responses, recognizing that some of it may be biased or \
incorrect. Your response should not simply replicate the given answers but \
should offer a refined, accurate, and comprehensive reply to the instruction. \
Do not mention that you are synthesizing multiple responses.";

/// Run a Mixture of Agents query.
///
/// 1. All proposer models receive the prompt in parallel.
/// 2. Their responses are formatted and sent to the aggregator.
/// 3. For multi-layer configs, proposer outputs feed back as context.
pub async fn run_moa(
    prompt: &str,
    system_prompt: Option<&str>,
    config: &MoaConfig,
) -> Result<MoaResult, MoaError> {
    if config.proposers.is_empty() {
        return Err(MoaError::NoProposers);
    }

    let layers = config.layers.max(1);
    let env: BTreeMap<String, String> = std::env::vars().collect();

    let mut proposer_responses = Vec::new();

    for layer in 0..layers {
        let context = if layer == 0 {
            None
        } else {
            Some(format_proposer_context(&proposer_responses))
        };

        proposer_responses = run_proposer_layer(
            prompt,
            system_prompt,
            context.as_deref(),
            &config.proposers,
            config.temperature,
            config.max_tokens,
            &env,
        )
        .await?;
    }

    // Aggregation
    let aggregator_prompt = build_aggregator_prompt(prompt, &proposer_responses);
    let resolved = resolve_model(&config.aggregator, &env);
    let aggregator_model = config.aggregator.model.clone();
    let client = ChatClient::new(&resolved).map_err(|e| MoaError::Provider {
        model: aggregator_model.clone(),
        source: e,
    })?;

    let mut messages = vec![ChatMessage::system(AGGREGATOR_SYSTEM_PROMPT)];
    messages.push(ChatMessage::user(aggregator_prompt));

    let mut request = ChatCompletionRequest::new(&resolved.model, messages);
    request.temperature = config.temperature;
    request.max_tokens = config.max_tokens;

    let response = client
        .complete(request)
        .await
        .map_err(|e| MoaError::Provider {
            model: aggregator_model,
            source: e,
        })?;

    let content = response
        .choices
        .first()
        .and_then(|c| c.message.content_text())
        .unwrap_or("")
        .to_owned();

    Ok(MoaResult {
        response: content,
        proposer_responses,
        layers_completed: layers,
    })
}

/// Run a single layer of proposer models in parallel.
async fn run_proposer_layer(
    prompt: &str,
    system_prompt: Option<&str>,
    prior_context: Option<&str>,
    proposers: &[MoaModelConfig],
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    env: &BTreeMap<String, String>,
) -> Result<Vec<ProposerResponse>, MoaError> {
    let mut handles = Vec::with_capacity(proposers.len());

    for proposer in proposers {
        let resolved = resolve_model(proposer, env);
        let model_name = proposer.model.clone();
        let prompt = prompt.to_owned();
        let system = system_prompt.map(str::to_owned);
        let context = prior_context.map(str::to_owned);
        let temp = temperature;
        let max_tok = max_tokens;

        handles.push(tokio::spawn(async move {
            let client = ChatClient::new(&resolved)?;

            let mut messages = Vec::new();
            if let Some(sys) = &system {
                messages.push(ChatMessage::system(sys.as_str()));
            }

            let user_content = match &context {
                Some(ctx) => format!(
                    "{}\n\n## Previous model responses for reference:\n{}",
                    prompt, ctx
                ),
                None => prompt.clone(),
            };
            messages.push(ChatMessage::user(user_content));

            let mut request = ChatCompletionRequest::new(&resolved.model, messages);
            request.temperature = temp;
            request.max_tokens = max_tok;

            let response = client.complete(request).await?;
            let content = response
                .choices
                .first()
                .and_then(|c| c.message.content_text())
                .unwrap_or("")
                .to_owned();

            Ok::<ProposerResponse, ProviderError>(ProposerResponse {
                model: model_name,
                content,
            })
        }));
    }

    let mut responses = Vec::new();
    let mut errors = Vec::new();

    for handle in handles {
        match handle.await {
            Ok(Ok(response)) => responses.push(response),
            Ok(Err(e)) => errors.push(e),
            Err(e) => errors.push(ProviderError::StreamDecode(e.to_string())),
        }
    }

    if responses.is_empty() {
        return Err(MoaError::AllProposersFailed);
    }

    Ok(responses)
}

fn resolve_model(config: &MoaModelConfig, env: &BTreeMap<String, String>) -> ResolvedProvider {
    resolve(
        &config.backend,
        &config.model,
        config.base_url.as_deref(),
        config.api_key_env.as_deref(),
        env,
    )
}

fn format_proposer_context(responses: &[ProposerResponse]) -> String {
    responses
        .iter()
        .enumerate()
        .map(|(i, r)| format!("### Model {} ({})\n{}", i + 1, r.model, r.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn build_aggregator_prompt(user_prompt: &str, responses: &[ProposerResponse]) -> String {
    let context = format_proposer_context(responses);
    format!(
        "## Original query\n{}\n\n## Responses from models\n{}",
        user_prompt, context
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_model(name: &str) -> MoaModelConfig {
        MoaModelConfig {
            backend: "openai".to_owned(),
            model: name.to_owned(),
            base_url: Some("http://localhost:8000/v1".to_owned()),
            api_key_env: None,
        }
    }

    #[test]
    fn format_proposer_context_numbers_responses() {
        let responses = vec![
            ProposerResponse {
                model: "gpt-4".to_owned(),
                content: "Answer A".to_owned(),
            },
            ProposerResponse {
                model: "claude-3".to_owned(),
                content: "Answer B".to_owned(),
            },
        ];

        let context = format_proposer_context(&responses);
        assert!(context.contains("### Model 1 (gpt-4)"));
        assert!(context.contains("### Model 2 (claude-3)"));
        assert!(context.contains("Answer A"));
        assert!(context.contains("Answer B"));
    }

    #[test]
    fn build_aggregator_prompt_includes_query_and_responses() {
        let responses = vec![ProposerResponse {
            model: "test-model".to_owned(),
            content: "Test response".to_owned(),
        }];

        let prompt = build_aggregator_prompt("What is 2+2?", &responses);
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.contains("Test response"));
        assert!(prompt.contains("test-model"));
    }

    #[test]
    fn moa_config_defaults() {
        let config = MoaConfig {
            proposers: vec![test_model("model-a"), test_model("model-b")],
            aggregator: test_model("aggregator"),
            layers: 1,
            temperature: Some(0.7),
            max_tokens: Some(1024),
        };

        assert_eq!(config.proposers.len(), 2);
        assert_eq!(config.layers, 1);
    }

    #[tokio::test]
    async fn run_moa_fails_with_no_proposers() {
        let config = MoaConfig {
            proposers: vec![],
            aggregator: test_model("aggregator"),
            layers: 1,
            temperature: None,
            max_tokens: None,
        };

        let result = run_moa("test", None, &config).await;
        assert!(matches!(result, Err(MoaError::NoProposers)));
    }

    #[test]
    fn aggregator_system_prompt_is_non_empty() {
        assert!(!AGGREGATOR_SYSTEM_PROMPT.is_empty());
        assert!(AGGREGATOR_SYSTEM_PROMPT.contains("synthesize"));
    }

    #[test]
    fn resolve_model_produces_valid_provider() {
        let env = BTreeMap::from([("OPENAI_API_KEY".to_owned(), "sk-test".to_owned())]);
        let config = test_model("gpt-4");
        let resolved = resolve_model(&config, &env);

        assert_eq!(resolved.model, "gpt-4");
        assert_eq!(resolved.api_key, "sk-test");
    }
}
