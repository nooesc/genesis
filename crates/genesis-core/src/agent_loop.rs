use std::collections::BTreeMap;

use genesis_provider::{
    ChatClient, ChatCompletionRequest, ChatMessage, ChatTool, ProviderError, ToolCallEntry,
};
use genesis_tools::{ToolCall, ToolError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ToolRuntime;

const DEFAULT_MAX_TURNS: usize = 20;

/// Result of a complete agent turn (user message → final assistant response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub response: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
    pub finished_naturally: bool,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

/// Configuration for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentLoopConfig {
    pub system_prompt: Option<String>,
    pub max_turns: usize,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            max_turns: DEFAULT_MAX_TURNS,
            temperature: None,
            max_tokens: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),
    #[error("failed to parse tool call arguments: {0}")]
    ArgumentParse(String),
    #[error("agent loop exceeded maximum of {0} turns")]
    MaxTurnsExceeded(usize),
}

/// The core agent loop that wires provider (LLM) and tool execution together.
///
/// Flow: user message → LLM → [tool_calls → execute → LLM]* → final text
pub struct AgentLoop {
    client: ChatClient,
    tools: ToolRuntime,
    config: AgentLoopConfig,
    messages: Vec<ChatMessage>,
}

impl AgentLoop {
    pub fn new(client: ChatClient, tools: ToolRuntime, config: AgentLoopConfig) -> Self {
        let mut messages = Vec::new();

        if let Some(system_prompt) = &config.system_prompt {
            messages.push(ChatMessage::system(system_prompt));
        }

        Self {
            client,
            tools,
            config,
            messages,
        }
    }

    /// Run a single user turn through the agent loop.
    ///
    /// Appends the user message, calls the LLM, handles any tool calls
    /// iteratively, and returns once the LLM produces a text-only response
    /// or the turn limit is reached.
    pub async fn run_turn(&mut self, user_message: &str) -> Result<AgentResult, AgentError> {
        self.messages.push(ChatMessage::user(user_message));

        let tool_defs: Vec<ChatTool> = self.tools.definitions().iter().map(ChatTool::from).collect();

        let mut turns_used = 0;
        let mut tool_calls_made = 0;
        let mut total_input_tokens = 0u32;
        let mut total_output_tokens = 0u32;

        loop {
            turns_used += 1;
            if turns_used > self.config.max_turns {
                return Err(AgentError::MaxTurnsExceeded(self.config.max_turns));
            }

            let mut request = ChatCompletionRequest::new("", self.messages.clone());
            request.tools = tool_defs.clone();
            request.temperature = self.config.temperature;
            request.max_tokens = self.config.max_tokens;

            let response = self.client.complete(request).await?;

            if let Some(usage) = &response.usage {
                total_input_tokens = total_input_tokens.saturating_add(usage.prompt_tokens);
                total_output_tokens = total_output_tokens.saturating_add(usage.completion_tokens);
            }

            let choice = &response.choices[0];
            let assistant_msg = &choice.message;

            // Check if the assistant wants to call tools
            if let Some(tool_calls) = &assistant_msg.tool_calls {
                if !tool_calls.is_empty() {
                    // Append the assistant message (with tool_calls) to history
                    self.messages.push(ChatMessage::assistant_with_tool_calls(
                        assistant_msg.content.clone(),
                        tool_calls.clone(),
                    ));

                    // Execute each tool call and append results
                    for tc in tool_calls {
                        tool_calls_made += 1;
                        let result = self.execute_tool_call(tc)?;
                        self.messages
                            .push(ChatMessage::tool_result(&tc.id, result));
                    }

                    // Continue the loop - send tool results back to LLM
                    continue;
                }
            }

            // No tool calls - this is the final text response
            let response_text = assistant_msg
                .content
                .clone()
                .unwrap_or_default();

            self.messages.push(ChatMessage::assistant(&response_text));

            return Ok(AgentResult {
                response: response_text,
                turns_used,
                tool_calls_made,
                finished_naturally: choice.finish_reason.as_deref() != Some("length"),
                total_input_tokens,
                total_output_tokens,
            });
        }
    }

    /// Execute a single tool call from the LLM response.
    fn execute_tool_call(&self, tc: &ToolCallEntry) -> Result<String, AgentError> {
        let arguments = parse_tool_arguments(&tc.function.arguments)?;

        let call = ToolCall {
            name: tc.function.name.clone(),
            arguments,
        };

        match self.tools.execute(&call) {
            Ok(output) => Ok(output.content),
            Err(err) => {
                // Return tool errors as content so the LLM can see what went wrong
                // and potentially retry or adjust. Only propagate if the tool is
                // completely not found.
                match &err {
                    ToolError::ToolNotFound(_) => Err(err.into()),
                    _ => Ok(format!("Error: {err}")),
                }
            }
        }
    }

    /// Access the full conversation history.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }
}

/// Parse a JSON arguments string into a flat BTreeMap<String, String>.
///
/// LLM tool call arguments come as a JSON string like `{"message":"hello"}`.
/// We flatten all values to their string representation for the ToolCall struct.
fn parse_tool_arguments(raw: &str) -> Result<BTreeMap<String, String>, AgentError> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| AgentError::ArgumentParse(format!("{raw}: {e}")))?;

    let obj = value.as_object().ok_or_else(|| {
        AgentError::ArgumentParse(format!("expected JSON object, got: {raw}"))
    })?;

    Ok(obj
        .iter()
        .map(|(k, v)| {
            let string_value = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), string_value)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tool_arguments_handles_simple_object() {
        let args = parse_tool_arguments(r#"{"message":"hello","count":"3"}"#)
            .expect("should parse");
        assert_eq!(args.get("message").unwrap(), "hello");
        assert_eq!(args.get("count").unwrap(), "3");
    }

    #[test]
    fn parse_tool_arguments_stringifies_non_string_values() {
        let args = parse_tool_arguments(r#"{"flag":true,"num":42}"#)
            .expect("should parse");
        assert_eq!(args.get("flag").unwrap(), "true");
        assert_eq!(args.get("num").unwrap(), "42");
    }

    #[test]
    fn parse_tool_arguments_rejects_non_object() {
        let result = parse_tool_arguments(r#"[1,2,3]"#);
        assert!(result.is_err());
    }

    #[test]
    fn parse_tool_arguments_rejects_invalid_json() {
        let result = parse_tool_arguments("not json");
        assert!(result.is_err());
    }

    #[test]
    fn agent_loop_config_has_sensible_defaults() {
        let config = AgentLoopConfig::default();
        assert_eq!(config.max_turns, 20);
        assert!(config.system_prompt.is_none());
        assert!(config.temperature.is_none());
    }

    #[test]
    fn agent_result_serializes_to_json() {
        let result = AgentResult {
            response: "Hello!".to_owned(),
            turns_used: 1,
            tool_calls_made: 0,
            finished_naturally: true,
            total_input_tokens: 100,
            total_output_tokens: 50,
        };
        let json = serde_json::to_string(&result).expect("should serialize");
        assert!(json.contains("\"response\":\"Hello!\""));
        assert!(json.contains("\"turns_used\":1"));
    }
}
