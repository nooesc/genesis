//! Mixture of Agents tool — queries multiple LLMs in parallel and synthesizes
//! their responses for higher-quality reasoning.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Maximum number of models to query in parallel.
const MAX_MODELS: usize = 5;

/// Default models when none are specified.
const DEFAULT_MODELS: &[(&str, &str)] = &[
    ("openai", "gpt-4o"),
    ("openai", "gpt-4o-mini"),
    ("anthropic", "claude-sonnet-4-20250514"),
];

/// Queries multiple LLMs in parallel with the same prompt and returns all
/// responses. Optionally synthesizes them into a single answer using a final model.
pub struct MixtureOfAgentsTool;

impl ToolHandler for MixtureOfAgentsTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let prompt = call
            .arguments
            .get("prompt")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "prompt",
            })?;

        let system = call.arguments.get("system").map(|s| s.as_str());

        // Parse models as "backend/model" strings, comma-separated.
        let model_specs: Vec<(&str, &str)> = if let Some(models_str) = call.arguments.get("models")
        {
            models_str
                .split(',')
                .filter_map(|s| {
                    let s = s.trim();
                    s.split_once('/').or_else(|| Some(("openai", s)))
                })
                .take(MAX_MODELS)
                .collect()
        } else {
            DEFAULT_MODELS.to_vec()
        };

        if model_specs.is_empty() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: "no models specified".to_owned(),
            });
        }

        let synthesize = call
            .arguments
            .get("synthesize")
            .map(|s| s == "true" || s == "1" || s == "yes")
            .unwrap_or(true);

        let synthesis_model = call
            .arguments
            .get("synthesis_model")
            .map(|s| s.as_str())
            .unwrap_or("gpt-4o");

        let synthesis_backend = call
            .arguments
            .get("synthesis_backend")
            .map(|s| s.as_str())
            .unwrap_or("openai");

        // Query all models in parallel using threads (since ToolHandler::run is sync).
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let responses: Arc<Mutex<Vec<(String, String, Result<String, String>)>>> =
            Arc::new(Mutex::new(Vec::new()));

        let prompt_owned = prompt.clone();
        let system_owned = system.map(|s| s.to_owned());

        std::thread::scope(|scope| {
            for &(backend, model) in &model_specs {
                let env = &env;
                let prompt = &prompt_owned;
                let system = &system_owned;
                let responses = Arc::clone(&responses);
                let backend = backend.to_owned();
                let model = model.to_owned();

                scope.spawn(move || {
                    let result = query_model(env, &backend, &model, prompt, system.as_deref());
                    let mut lock = responses.lock().unwrap();
                    lock.push((backend, model, result));
                });
            }
        });

        let results = Arc::try_unwrap(responses)
            .unwrap()
            .into_inner()
            .unwrap();

        // Format individual responses.
        let mut individual_parts = Vec::new();
        let mut successful_responses = Vec::new();

        for (backend, model, result) in &results {
            match result {
                Ok(text) => {
                    individual_parts
                        .push(format!("### {backend}/{model}\n{text}"));
                    successful_responses.push((backend.as_str(), model.as_str(), text.as_str()));
                }
                Err(e) => {
                    individual_parts
                        .push(format!("### {backend}/{model}\n[Error: {e}]"));
                }
            }
        }

        // Optionally synthesize responses.
        let synthesis = if synthesize && successful_responses.len() > 1 {
            let synthesis_prompt = build_synthesis_prompt(prompt, &successful_responses);
            match query_model(
                &env,
                synthesis_backend,
                synthesis_model,
                &synthesis_prompt,
                Some("You are a synthesis agent. Combine the multiple expert responses into a single, high-quality answer. Preserve key insights from each response while resolving any contradictions."),
            ) {
                Ok(text) => Some(text),
                Err(e) => Some(format!("[Synthesis failed: {e}]")),
            }
        } else {
            None
        };

        let mut content = format!(
            "Queried {} models:\n\n{}",
            results.len(),
            individual_parts.join("\n\n")
        );

        if let Some(syn) = &synthesis {
            content.push_str(&format!("\n\n---\n## Synthesized Answer\n{syn}"));
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("models_queried".to_owned(), results.len().to_string()),
                (
                    "models_succeeded".to_owned(),
                    successful_responses.len().to_string(),
                ),
                ("synthesized".to_owned(), synthesis.is_some().to_string()),
            ]),
        })
    }
}

fn query_model(
    env: &BTreeMap<String, String>,
    backend: &str,
    model: &str,
    prompt: &str,
    system: Option<&str>,
) -> Result<String, String> {
    let resolved = genesis_provider::resolve(backend, model, None, None, env);
    let client = genesis_provider::ChatClient::new(&resolved)
        .map_err(|e| format!("client creation failed: {e}"))?;

    let mut messages = Vec::new();
    if let Some(sys) = system {
        messages.push(genesis_provider::ChatMessage::system(sys));
    }
    messages.push(genesis_provider::ChatMessage::user(prompt));

    let request = genesis_provider::ChatCompletionRequest::new("", messages);

    let response = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(client.complete(request))
    })
    .map_err(|e| format!("{e}"))?;

    response
        .choices
        .first()
        .and_then(|c| c.message.content_text())
        .map(|s| s.to_owned())
        .ok_or_else(|| "empty response".to_owned())
}

fn build_synthesis_prompt(original_prompt: &str, responses: &[(&str, &str, &str)]) -> String {
    let mut prompt = format!(
        "The following question was asked to {} different AI models:\n\n\
         **Question:** {original_prompt}\n\n\
         Their responses are below:\n\n",
        responses.len()
    );

    for (i, (backend, model, text)) in responses.iter().enumerate() {
        prompt.push_str(&format!(
            "**Response {} ({backend}/{model}):**\n{text}\n\n",
            i + 1
        ));
    }

    prompt.push_str(
        "Please synthesize these responses into a single, comprehensive answer. \
         Highlight areas of agreement, resolve contradictions, and provide the best \
         possible answer based on the collective reasoning.",
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
            terminal_backend: None,
        }
    }

    #[test]
    fn requires_prompt() {
        let tool = MixtureOfAgentsTool;
        let call = ToolCall {
            name: "mixture_of_agents".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "prompt",
                ..
            }
        ));
    }

    #[test]
    fn parses_model_specs() {
        let specs = "openai/gpt-4o, anthropic/claude-sonnet-4-20250514, gpt-4o-mini";
        let parsed: Vec<(&str, &str)> = specs
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                s.split_once('/').or_else(|| Some(("openai", s)))
            })
            .take(MAX_MODELS)
            .collect();

        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], ("openai", "gpt-4o"));
        assert_eq!(parsed[1], ("anthropic", "claude-sonnet-4-20250514"));
        assert_eq!(parsed[2], ("openai", "gpt-4o-mini"));
    }

    #[test]
    fn build_synthesis_prompt_includes_all_responses() {
        let responses = vec![
            ("openai", "gpt-4o", "Answer A"),
            ("anthropic", "claude", "Answer B"),
        ];
        let prompt = build_synthesis_prompt("What is 2+2?", &responses);
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.contains("Answer A"));
        assert!(prompt.contains("Answer B"));
        assert!(prompt.contains("openai/gpt-4o"));
        assert!(prompt.contains("anthropic/claude"));
        assert!(prompt.contains("synthesize"));
    }

    #[test]
    fn default_models_are_reasonable() {
        assert!(DEFAULT_MODELS.len() >= 2);
        assert!(DEFAULT_MODELS.len() <= MAX_MODELS);
    }
}
