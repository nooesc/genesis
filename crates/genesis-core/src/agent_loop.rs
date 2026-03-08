use std::collections::BTreeMap;

use futures_util::StreamExt;
use genesis_provider::{
    ChatClient, ChatCompletionChunk, ChatCompletionRequest, ChatMessage, ChatTool, ProviderError,
    ToolCallEntry,
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
    /// Maximum number of conversation messages to keep in context (excluding
    /// the system prompt). When the history exceeds this limit, the oldest
    /// non-system messages are dropped. Set to `None` for unlimited.
    pub max_context_messages: Option<usize>,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            max_turns: DEFAULT_MAX_TURNS,
            temperature: None,
            max_tokens: None,
            max_context_messages: None,
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
        Self::with_history(client, tools, config, Vec::new())
    }

    pub fn with_history(
        client: ChatClient,
        tools: ToolRuntime,
        config: AgentLoopConfig,
        history: Vec<ChatMessage>,
    ) -> Self {
        let mut messages = Vec::new();

        if let Some(system_prompt) = &config.system_prompt {
            messages.push(ChatMessage::system(system_prompt));
        }

        messages.extend(history);

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

            self.prune_context();
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

    pub async fn run_turn_streaming<F>(
        &mut self,
        user_message: &str,
        mut on_chunk: F,
    ) -> Result<AgentResult, AgentError>
    where
        F: FnMut(&str),
    {
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

            self.prune_context();
            let mut request = ChatCompletionRequest::new("", self.messages.clone());
            request.tools = tool_defs.clone();
            request.temperature = self.config.temperature;
            request.max_tokens = self.config.max_tokens;

            match self.client.complete_stream(request.clone()).await {
                Ok(mut stream) => {
                    let mut response_text = String::new();
                    let mut streamed_tool_calls = Vec::new();
                    let mut finished_naturally = true;

                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk?;
                        let update = collect_stream_update(chunk);

                        for content in update.contents {
                            on_chunk(&content);
                            response_text.push_str(&content);
                        }

                        if let Some(reason) = update.finish_reason {
                            finished_naturally = reason != "length";
                        }

                        streamed_tool_calls.extend(update.tool_calls);
                    }

                    if !streamed_tool_calls.is_empty() {
                        self.messages.push(ChatMessage::assistant_with_tool_calls(
                            if response_text.is_empty() {
                                None
                            } else {
                                Some(response_text.clone())
                            },
                            streamed_tool_calls.clone(),
                        ));

                        for tc in &streamed_tool_calls {
                            tool_calls_made += 1;
                            let result = self.execute_tool_call(tc)?;
                            self.messages.push(ChatMessage::tool_result(&tc.id, result));
                        }

                        continue;
                    }

                    self.messages.push(ChatMessage::assistant(&response_text));

                    return Ok(AgentResult {
                        response: response_text,
                        turns_used,
                        tool_calls_made,
                        finished_naturally,
                        total_input_tokens,
                        total_output_tokens,
                    });
                }
                Err(_) => {
                    let response = self.client.complete(request).await?;

                    if let Some(usage) = &response.usage {
                        total_input_tokens =
                            total_input_tokens.saturating_add(usage.prompt_tokens);
                        total_output_tokens =
                            total_output_tokens.saturating_add(usage.completion_tokens);
                    }

                    let choice = &response.choices[0];
                    let assistant_msg = &choice.message;

                    if let Some(tool_calls) = &assistant_msg.tool_calls {
                        if !tool_calls.is_empty() {
                            self.messages.push(ChatMessage::assistant_with_tool_calls(
                                assistant_msg.content.clone(),
                                tool_calls.clone(),
                            ));

                            for tc in tool_calls {
                                tool_calls_made += 1;
                                let result = self.execute_tool_call(tc)?;
                                self.messages.push(ChatMessage::tool_result(&tc.id, result));
                            }

                            continue;
                        }
                    }

                    let response_text = assistant_msg.content.clone().unwrap_or_default();
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

    /// Prune messages to stay within `max_context_messages`, preserving the
    /// system prompt at index 0 (if present) and the most recent messages.
    fn prune_context(&mut self) {
        let limit = match self.config.max_context_messages {
            Some(limit) => limit,
            None => return,
        };

        let has_system = self
            .messages
            .first()
            .is_some_and(|m| m.role == "system");

        let non_system_count = if has_system {
            self.messages.len() - 1
        } else {
            self.messages.len()
        };

        if non_system_count <= limit {
            return;
        }

        let drop_count = non_system_count - limit;
        let drop_start = if has_system { 1 } else { 0 };
        self.messages.drain(drop_start..drop_start + drop_count);
    }
}

struct StreamUpdate {
    contents: Vec<String>,
    tool_calls: Vec<ToolCallEntry>,
    finish_reason: Option<String>,
}

fn collect_stream_update(chunk: ChatCompletionChunk) -> StreamUpdate {
    let mut contents = Vec::new();
    let mut tool_calls = Vec::new();
    let mut finish_reason = None;

    for choice in chunk.choices {
        if let Some(content) = choice.delta.content {
            contents.push(content);
        }
        if let Some(delta_tool_calls) = choice.delta.tool_calls {
            tool_calls.extend(delta_tool_calls);
        }
        if choice.finish_reason.is_some() {
            finish_reason = choice.finish_reason;
        }
    }

    StreamUpdate {
        contents,
        tool_calls,
        finish_reason,
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
    fn collect_stream_update_gathers_content_and_finish_reason() {
        let update = collect_stream_update(ChatCompletionChunk {
            id: "chunk-1".to_owned(),
            choices: vec![genesis_provider::ChatChunkChoice {
                index: 0,
                delta: genesis_provider::ChatChunkDelta {
                    role: Some("assistant".to_owned()),
                    content: Some("hello".to_owned()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_owned()),
            }],
        });

        assert_eq!(update.contents, vec!["hello".to_owned()]);
        assert_eq!(update.finish_reason.as_deref(), Some("stop"));
        assert!(update.tool_calls.is_empty());
    }

    #[test]
    fn agent_loop_config_has_sensible_defaults() {
        let config = AgentLoopConfig::default();
        assert_eq!(config.max_turns, 20);
        assert!(config.system_prompt.is_none());
        assert!(config.temperature.is_none());
    }

    #[test]
    fn with_history_keeps_system_prompt_and_appends_prior_messages() {
        let provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:8000/v1".to_owned(),
            api_key: String::new(),
            model: "test-model".to_owned(),
        };
        let client = ChatClient::new(&provider).expect("client should build");
        let tools = crate::build_default_tool_runtime(&crate::ExecutionContext {
            plan: crate::SessionPlan {
                session_id: "session-1".to_owned(),
                profile: "default".to_owned(),
                platform: genesis_types::DeliveryPlatform::Cli,
                model: genesis_types::ModelSelection {
                    provider: genesis_types::ModelProviderKind::OpenAi,
                    model: "test-model".to_owned(),
                    base_url: None,
                },
                initial_events: Vec::new(),
            },
            data_dir: "/tmp/genesis".to_owned(),
            database_path: "/tmp/genesis/genesis.db".to_owned(),
            max_concurrency: 4,
            allow_destructive_tools: false,
        });

        let agent = AgentLoop::with_history(
            client,
            tools,
            AgentLoopConfig {
                system_prompt: Some("system".to_owned()),
                ..AgentLoopConfig::default()
            },
            vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")],
        );

        assert_eq!(agent.messages().len(), 3);
        assert_eq!(agent.messages()[0].role, "system");
        assert_eq!(agent.messages()[1].role, "user");
        assert_eq!(agent.messages()[2].role, "assistant");
    }

    #[test]
    fn prune_context_keeps_system_prompt_and_recent_messages() {
        let provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:8000/v1".to_owned(),
            api_key: String::new(),
            model: "test-model".to_owned(),
        };
        let client = ChatClient::new(&provider).expect("client should build");
        let tools = crate::build_default_tool_runtime(&crate::ExecutionContext {
            plan: crate::SessionPlan {
                session_id: "s".to_owned(),
                profile: "default".to_owned(),
                platform: genesis_types::DeliveryPlatform::Cli,
                model: genesis_types::ModelSelection {
                    provider: genesis_types::ModelProviderKind::OpenAi,
                    model: "m".to_owned(),
                    base_url: None,
                },
                initial_events: Vec::new(),
            },
            data_dir: "/tmp".to_owned(),
            database_path: "/tmp/genesis.db".to_owned(),
            max_concurrency: 4,
            allow_destructive_tools: false,
        });

        let mut agent = AgentLoop::with_history(
            client,
            tools,
            AgentLoopConfig {
                system_prompt: Some("system".to_owned()),
                max_context_messages: Some(3),
                ..AgentLoopConfig::default()
            },
            vec![
                ChatMessage::user("msg1"),
                ChatMessage::assistant("reply1"),
                ChatMessage::user("msg2"),
                ChatMessage::assistant("reply2"),
                ChatMessage::user("msg3"),
            ],
        );

        // 1 system + 5 history = 6 messages total
        assert_eq!(agent.messages().len(), 6);

        agent.prune_context();

        // Should keep system + 3 most recent
        assert_eq!(agent.messages().len(), 4);
        assert_eq!(agent.messages()[0].role, "system");
        assert_eq!(
            agent.messages()[1].content.as_deref(),
            Some("msg2")
        );
        assert_eq!(
            agent.messages()[2].content.as_deref(),
            Some("reply2")
        );
        assert_eq!(
            agent.messages()[3].content.as_deref(),
            Some("msg3")
        );
    }

    #[test]
    fn prune_context_noop_when_under_limit() {
        let provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:8000/v1".to_owned(),
            api_key: String::new(),
            model: "test-model".to_owned(),
        };
        let client = ChatClient::new(&provider).expect("client should build");
        let tools = crate::build_default_tool_runtime(&crate::ExecutionContext {
            plan: crate::SessionPlan {
                session_id: "s".to_owned(),
                profile: "default".to_owned(),
                platform: genesis_types::DeliveryPlatform::Cli,
                model: genesis_types::ModelSelection {
                    provider: genesis_types::ModelProviderKind::OpenAi,
                    model: "m".to_owned(),
                    base_url: None,
                },
                initial_events: Vec::new(),
            },
            data_dir: "/tmp".to_owned(),
            database_path: "/tmp/genesis.db".to_owned(),
            max_concurrency: 4,
            allow_destructive_tools: false,
        });

        let mut agent = AgentLoop::with_history(
            client,
            tools,
            AgentLoopConfig {
                system_prompt: Some("system".to_owned()),
                max_context_messages: Some(10),
                ..AgentLoopConfig::default()
            },
            vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")],
        );

        agent.prune_context();
        assert_eq!(agent.messages().len(), 3); // system + 2 unchanged
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
