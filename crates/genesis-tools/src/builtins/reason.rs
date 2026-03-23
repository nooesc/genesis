use std::collections::BTreeMap;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that queries a secondary LLM for a second opinion or specialized reasoning.
///
/// This enables multi-model collaboration: the main agent can delegate specific
/// sub-tasks (code review, math verification, creative writing, etc.) to a
/// different model that may be better suited for that task.
///
/// The tool constructs a one-shot completion request and returns the secondary
/// model's response. The calling agent can then integrate the response into its
/// own reasoning.
pub struct ReasonWithModelTool;

impl ToolHandler for ReasonWithModelTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let prompt = call
            .arguments
            .get("prompt")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "prompt",
            })?;

        let model = call
            .arguments
            .get("model")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "model",
            })?;

        let system = call.arguments.get("system").map(|s| s.as_str());
        let backend = call
            .arguments
            .get("backend")
            .map(|s| s.as_str())
            .unwrap_or("openai");

        let temperature = call
            .arguments
            .get("temperature")
            .and_then(|t| t.parse::<f64>().ok());

        let max_tokens = call
            .arguments
            .get("max_tokens")
            .and_then(|t| t.parse::<u32>().ok());

        // Build provider and client. We use the sync wrapper since ToolHandler::run is sync.
        // The actual HTTP call happens in a blocking tokio runtime context.
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let resolved = genesis_provider::resolve(backend, model, None, None, &env);
        let client = genesis_provider::ChatClient::new(&resolved).map_err(|e| {
            ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to create client for {backend}/{model}: {e}"),
            }
        })?;

        let mut messages = Vec::new();
        if let Some(sys) = system {
            messages.push(genesis_provider::ChatMessage::system(sys));
        }
        messages.push(genesis_provider::ChatMessage::user(prompt));

        let mut request = genesis_provider::ChatCompletionRequest::new("", messages);
        request.temperature = temperature;
        request.max_tokens = max_tokens;

        // Execute the request synchronously. This may be called from either
        // spawn_blocking (ToolRegistry) or a Tokio worker thread (MCP server).
        // block_in_place is safe in both cases: it's a no-op on blocking threads
        // and moves the task off the worker thread when on a Tokio worker.
        let response = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(client.complete(request))
        })
        .map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("model query failed: {e}"),
        })?;

        let response_text = response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
            .unwrap_or("")
            .to_owned();

        let (input_tokens, output_tokens): (u32, u32) = response
            .usage
            .as_ref()
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((0, 0));

        Ok(ToolOutput {
            content: format!(
                "Response from {model} ({backend}):\n\n{response_text}\n\n\
                 [tokens: {input_tokens} in / {output_tokens} out]"
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("model".to_owned(), model.clone()),
                ("backend".to_owned(), backend.to_owned()),
                ("input_tokens".to_owned(), input_tokens.to_string()),
                ("output_tokens".to_owned(), output_tokens.to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx_destructive()
    }

    #[test]
    fn reason_requires_prompt() {
        let tool = ReasonWithModelTool;
        let call = ToolCall {
            name: "reason_with_model".to_owned(),
            arguments: BTreeMap::from([("model".to_owned(), "gpt-4".to_owned())]),
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
    fn reason_requires_model() {
        let tool = ReasonWithModelTool;
        let call = ToolCall {
            name: "reason_with_model".to_owned(),
            arguments: BTreeMap::from([("prompt".to_owned(), "What is 2+2?".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "model",
                ..
            }
        ));
    }

    // Note: Full integration tests require a running LLM endpoint.
    // The tool gracefully handles connection errors via ToolError::ExecutionFailed.
}
