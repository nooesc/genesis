use std::collections::BTreeMap;
use std::time::Instant;

use futures_util::StreamExt;
use genesis_provider::{
    ChatClient, ChatCompletionChunk, ChatCompletionRequest, ChatMessage, ChatTool, MessageContent,
    ProviderError, ToolCallEntry,
};
use genesis_tools::{ToolCall, ToolError};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, error, info, info_span, warn};

use std::sync::Arc;

use crate::cost::{BudgetStatus, SessionCost};
use crate::trajectory::TrajectoryRecorder;
use crate::ToolRuntime;

const DEFAULT_MAX_TURNS: usize = 20;

/// Events emitted during streaming execution.
#[derive(Debug, Clone)]
pub enum StreamEvent<'a> {
    /// A text content chunk from the LLM.
    Chunk(&'a str),
    /// A tool call is about to be executed.
    ToolCallStart { name: &'a str },
    /// A tool call finished executing.
    ToolCallEnd { name: &'a str },
    /// The agent is requesting clarification from the user.
    ClarificationNeeded { question: &'a str },
}

/// Result of a complete agent turn (user message → final assistant response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub response: String,
    pub turns_used: usize,
    pub tool_calls_made: usize,
    pub finished_naturally: bool,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    /// Estimated cost in USD for this turn, if pricing is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    /// If set, the agent is paused waiting for user clarification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_clarification: Option<String>,
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
    /// Optional budget limit in USD. When exceeded, the agent loop stops early.
    pub budget_limit: Option<f64>,
    /// Maximum number of tool calls to execute concurrently (default: 4).
    pub max_concurrency: usize,
    /// After this many tool calls, inject a memory consolidation nudge asking
    /// the agent to save useful observations. Set to `None` to disable.
    /// Default: 15 tool calls.
    pub memory_nudge_interval: Option<usize>,
    /// Enable trajectory recording for agent training data capture.
    pub enable_trajectory: bool,
}

/// Default number of tool calls between memory consolidation nudges.
const DEFAULT_MEMORY_NUDGE_INTERVAL: usize = 15;

/// The memory nudge message injected as a system message.
const MEMORY_NUDGE: &str = "\
[Memory consolidation reminder] You've been working for a while. \
Consider saving any useful observations, patterns, or user preferences \
you've noticed using `memory_create`. Focus on durable insights that \
would be valuable in future sessions — not session-specific details.";

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            max_turns: DEFAULT_MAX_TURNS,
            temperature: None,
            max_tokens: None,
            max_context_messages: None,
            budget_limit: None,
            max_concurrency: 4,
            memory_nudge_interval: Some(DEFAULT_MEMORY_NUDGE_INTERVAL),
            enable_trajectory: false,
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
    #[error("budget exceeded: ${used:.4} / ${limit:.4}")]
    BudgetExceeded { used: f64, limit: f64 },
}

/// Callback for spawning subagent workstreams. Called when the agent
/// invokes the `spawn_subagent` tool with the child session ID and task.
pub trait SubagentSpawner: Send + Sync {
    fn spawn(&self, child_session_id: &str, subagent_id: &str, task: &str);
}

/// The core agent loop that wires provider (LLM) and tool execution together.
///
/// Flow: user message → LLM → [tool_calls → execute → LLM]* → final text
pub struct AgentLoop {
    client: ChatClient,
    /// Optional cheaper client for tool-calling turns. When set, turns that
    /// follow tool results use this client while turns following user messages
    /// use the primary `client`.
    tool_client: Option<ChatClient>,
    tools: ToolRuntime,
    config: AgentLoopConfig,
    messages: Vec<ChatMessage>,
    subagent_spawner: Option<Arc<dyn SubagentSpawner>>,
    cost: SessionCost,
    trajectory: TrajectoryRecorder,
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

        let cost = SessionCost::new(config.budget_limit);

        let trajectory = if config.enable_trajectory {
            let sys = config.system_prompt.as_deref().unwrap_or("");
            TrajectoryRecorder::new("session", client.model(), sys)
        } else {
            TrajectoryRecorder::disabled()
        };

        Self {
            client,
            tool_client: None,
            tools,
            config,
            messages,
            subagent_spawner: None,
            cost,
            trajectory,
        }
    }

    /// Set an optional cheaper client for tool-calling turns.
    pub fn set_tool_client(&mut self, client: ChatClient) {
        self.tool_client = Some(client);
    }

    /// Attach a subagent spawner so the agent can spawn parallel workstreams.
    pub fn set_subagent_spawner(&mut self, spawner: Arc<dyn SubagentSpawner>) {
        self.subagent_spawner = Some(spawner);
    }

    /// Pick the right client for the current turn. Uses the tool client when
    /// the most recent message is a tool result (the agent is processing tool
    /// output and will likely make more tool calls). Falls back to the primary
    /// client otherwise.
    fn active_client(&self) -> &ChatClient {
        if let Some(ref tool_client) = self.tool_client {
            let last_role = self.messages.last().map(|m| m.role.as_str());
            if last_role == Some("tool") {
                return tool_client;
            }
        }
        &self.client
    }

    /// Run a single user turn through the agent loop.
    ///
    /// Appends the user message, calls the LLM, handles any tool calls
    /// iteratively, and returns once the LLM produces a text-only response
    /// or the turn limit is reached.
    pub async fn run_turn(&mut self, user_message: &str) -> Result<AgentResult, AgentError> {
        self.run_turn_with_images(user_message, Vec::new()).await
    }

    /// Run a single user turn with optional image attachments.
    pub async fn run_turn_with_images(
        &mut self,
        user_message: &str,
        images: Vec<genesis_provider::ImageUrl>,
    ) -> Result<AgentResult, AgentError> {
        self.trajectory.record_user_message(user_message);

        if images.is_empty() {
            self.messages.push(ChatMessage::user(user_message));
        } else {
            self.messages
                .push(ChatMessage::user_with_images(user_message, images));
        }

        let tool_defs: Vec<ChatTool> = self.tools.definitions_async().await.iter().map(ChatTool::from).collect();

        let mut turns_used = 0;
        let mut tool_calls_made = 0;
        let mut total_input_tokens = 0u32;
        let mut total_output_tokens = 0u32;

        loop {
            turns_used += 1;
            if turns_used > self.config.max_turns {
                warn!(max_turns = self.config.max_turns, "agent loop reached turn limit");
                return Ok(AgentResult {
                    response: format!(
                        "I've reached the maximum of {} turns for this request. \
                         The work so far has been saved. You can continue by sending another message.",
                        self.config.max_turns
                    ),
                    turns_used: turns_used - 1,
                    tool_calls_made,
                    finished_naturally: false,
                    total_input_tokens,
                    total_output_tokens,
                    estimated_cost: Some(self.cost.total_cost),
                    pending_clarification: None,
                });
            }
            debug!(turn = turns_used, mode = "blocking", "starting agent turn iteration");

            self.prune_context().await;
            let client = self.active_client().clone();
            let mut request = ChatCompletionRequest::new("", self.messages.clone());
            request.tools = tool_defs.clone();
            request.temperature = self.config.temperature;
            request.max_tokens = self.config.max_tokens;

            let response = client.complete(request).await?;

            if let Some(usage) = &response.usage {
                total_input_tokens = total_input_tokens.saturating_add(usage.prompt_tokens);
                total_output_tokens = total_output_tokens.saturating_add(usage.completion_tokens);
                self.record_usage(turns_used, usage.prompt_tokens, usage.completion_tokens)?;
            }

            let choice = &response.choices[0];
            let assistant_msg = &choice.message;

            // Check if the assistant wants to call tools
            if let Some(tool_calls) = &assistant_msg.tool_calls {
                if !tool_calls.is_empty() {
                    // Record assistant message with tool calls
                    if let Some(text) = assistant_msg.content_text() {
                        self.trajectory.record_assistant_message(text);
                    }

                    // Append the assistant message (with tool_calls) to history
                    self.messages.push(ChatMessage::assistant_with_tool_calls(
                        assistant_msg.content.clone(),
                        tool_calls.clone(),
                    ));

                    // Execute tool calls in parallel (up to max_concurrency).
                    tool_calls_made += tool_calls.len();

                    // Record each tool call
                    for tc in tool_calls {
                        self.trajectory
                            .record_tool_call(&tc.function.name, &tc.function.arguments);
                    }

                    let results = execute_tool_calls_parallel(
                        &self.tools,
                        &self.subagent_spawner,
                        tool_calls,
                        self.config.max_concurrency,
                    )
                    .await?;

                    let mut clarification = None;
                    for (tc, (result, requires_input)) in tool_calls.iter().zip(results) {
                        self.trajectory
                            .record_tool_result(&tc.function.name, &result);
                        if requires_input {
                            clarification = Some(result.clone());
                        }
                        self.messages
                            .push(ChatMessage::tool_result(&tc.id, result));
                    }

                    // If a tool requested user input, pause the agent loop
                    if let Some(question) = clarification {
                        return Ok(AgentResult {
                            response: String::new(),
                            turns_used,
                            tool_calls_made,
                            finished_naturally: false,
                            total_input_tokens,
                            total_output_tokens,
                            estimated_cost: Some(self.cost.total_cost),
                            pending_clarification: Some(question),
                        });
                    }

                    // Inject memory nudge if due.
                    self.maybe_inject_memory_nudge(tool_calls_made);

                    // Continue the loop - send tool results back to LLM
                    continue;
                }
            }

            // No tool calls - this is the final text response
            let response_text = assistant_msg
                .content_text()
                .unwrap_or("")
                .to_owned();

            self.trajectory.record_assistant_message(&response_text);
            self.messages.push(ChatMessage::assistant(&response_text));

            return Ok(AgentResult {
                response: response_text,
                turns_used,
                tool_calls_made,
                finished_naturally: choice.finish_reason.as_deref() != Some("length"),
                total_input_tokens,
                total_output_tokens,
                estimated_cost: Some(self.cost.total_cost),
                pending_clarification: None,
            });
        }
    }

    pub async fn run_turn_streaming<F>(
        &mut self,
        user_message: &str,
        on_event: F,
    ) -> Result<AgentResult, AgentError>
    where
        F: FnMut(StreamEvent<'_>),
    {
        self.run_turn_streaming_with_images(user_message, Vec::new(), on_event)
            .await
    }

    /// Run a streaming turn with optional image attachments.
    pub async fn run_turn_streaming_with_images<F>(
        &mut self,
        user_message: &str,
        images: Vec<genesis_provider::ImageUrl>,
        mut on_event: F,
    ) -> Result<AgentResult, AgentError>
    where
        F: FnMut(StreamEvent<'_>),
    {
        self.trajectory.record_user_message(user_message);

        if images.is_empty() {
            self.messages.push(ChatMessage::user(user_message));
        } else {
            self.messages
                .push(ChatMessage::user_with_images(user_message, images));
        }

        let tool_defs: Vec<ChatTool> = self.tools.definitions_async().await.iter().map(ChatTool::from).collect();

        let mut turns_used = 0;
        let mut tool_calls_made = 0;
        let mut total_input_tokens = 0u32;
        let mut total_output_tokens = 0u32;

        loop {
            turns_used += 1;
            if turns_used > self.config.max_turns {
                warn!(max_turns = self.config.max_turns, "agent loop reached turn limit (streaming)");
                let msg = format!(
                    "I've reached the maximum of {} turns for this request. \
                     The work so far has been saved. You can continue by sending another message.",
                    self.config.max_turns
                );
                on_event(StreamEvent::Chunk(&msg));
                return Ok(AgentResult {
                    response: msg,
                    turns_used: turns_used - 1,
                    tool_calls_made,
                    finished_naturally: false,
                    total_input_tokens,
                    total_output_tokens,
                    estimated_cost: Some(self.cost.total_cost),
                    pending_clarification: None,
                });
            }
            debug!(turn = turns_used, mode = "streaming", "starting agent turn iteration");

            self.prune_context().await;
            let client = self.active_client().clone();
            let mut request = ChatCompletionRequest::new("", self.messages.clone());
            request.tools = tool_defs.clone();
            request.temperature = self.config.temperature;
            request.max_tokens = self.config.max_tokens;

            match client.complete_stream(request.clone()).await {
                Ok(mut stream) => {
                    let mut response_text = String::new();
                    let mut streamed_tool_calls = Vec::new();
                    let mut finished_naturally = true;
                    let mut turn_input_tokens = 0u32;
                    let mut turn_output_tokens = 0u32;

                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk?;
                        let update = collect_stream_update(chunk);

                        for content in update.contents {
                            on_event(StreamEvent::Chunk(&content));
                            response_text.push_str(&content);
                        }

                        if let Some(reason) = update.finish_reason {
                            finished_naturally = reason != "length";
                        }

                        if let Some(usage) = update.usage {
                            turn_input_tokens = turn_input_tokens.saturating_add(usage.prompt_tokens);
                            turn_output_tokens = turn_output_tokens.saturating_add(usage.completion_tokens);
                        }

                        streamed_tool_calls.extend(update.tool_calls);
                    }

                    total_input_tokens = total_input_tokens.saturating_add(turn_input_tokens);
                    total_output_tokens = total_output_tokens.saturating_add(turn_output_tokens);
                    self.record_usage(turns_used, turn_input_tokens, turn_output_tokens)?;

                    if !streamed_tool_calls.is_empty() {
                        // Record assistant message if present
                        if !response_text.is_empty() {
                            self.trajectory.record_assistant_message(&response_text);
                        }

                        self.messages.push(ChatMessage::assistant_with_tool_calls(
                            if response_text.is_empty() {
                                None
                            } else {
                                Some(MessageContent::Text(response_text.clone()))
                            },
                            streamed_tool_calls.clone(),
                        ));

                        // Emit start events and record tool calls.
                        for tc in &streamed_tool_calls {
                            on_event(StreamEvent::ToolCallStart { name: &tc.function.name });
                            self.trajectory
                                .record_tool_call(&tc.function.name, &tc.function.arguments);
                        }

                        // Execute tool calls in parallel.
                        tool_calls_made += streamed_tool_calls.len();
                        let results = execute_tool_calls_parallel(
                            &self.tools,
                            &self.subagent_spawner,
                            &streamed_tool_calls,
                            self.config.max_concurrency,
                        )
                        .await?;

                        let mut clarification = None;
                        for (tc, (result, requires_input)) in
                            streamed_tool_calls.iter().zip(results)
                        {
                            on_event(StreamEvent::ToolCallEnd { name: &tc.function.name });
                            self.trajectory
                                .record_tool_result(&tc.function.name, &result);
                            if requires_input {
                                on_event(StreamEvent::ClarificationNeeded { question: &result });
                                clarification = Some(result.clone());
                            }
                            self.messages.push(ChatMessage::tool_result(&tc.id, result));
                        }

                        if let Some(question) = clarification {
                            return Ok(AgentResult {
                                response: String::new(),
                                turns_used,
                                tool_calls_made,
                                finished_naturally: false,
                                total_input_tokens,
                                total_output_tokens,
                                estimated_cost: Some(self.cost.total_cost),
                                pending_clarification: Some(question),
                            });
                        }

                        self.maybe_inject_memory_nudge(tool_calls_made);
                        continue;
                    }

                    self.trajectory.record_assistant_message(&response_text);
                    self.messages.push(ChatMessage::assistant(&response_text));

                    return Ok(AgentResult {
                        response: response_text,
                        turns_used,
                        tool_calls_made,
                        finished_naturally,
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    });
                }
                Err(_) => {
                    warn!(turn = turns_used, "streaming provider request failed; falling back to blocking completion");
                    let response = client.complete(request).await?;

                    if let Some(usage) = &response.usage {
                        total_input_tokens =
                            total_input_tokens.saturating_add(usage.prompt_tokens);
                        total_output_tokens =
                            total_output_tokens.saturating_add(usage.completion_tokens);
                        self.record_usage(turns_used, usage.prompt_tokens, usage.completion_tokens)?;
                    }

                    let choice = &response.choices[0];
                    let assistant_msg = &choice.message;

                    if let Some(tool_calls) = &assistant_msg.tool_calls {
                        if !tool_calls.is_empty() {
                            if let Some(text) = assistant_msg.content_text() {
                                self.trajectory.record_assistant_message(text);
                            }

                            self.messages.push(ChatMessage::assistant_with_tool_calls(
                                assistant_msg.content.clone(),
                                tool_calls.clone(),
                            ));

                            // Emit start events and record tool calls.
                            for tc in tool_calls.iter() {
                                on_event(StreamEvent::ToolCallStart { name: &tc.function.name });
                                self.trajectory
                                    .record_tool_call(&tc.function.name, &tc.function.arguments);
                            }

                            // Execute tool calls in parallel.
                            tool_calls_made += tool_calls.len();
                            let results = execute_tool_calls_parallel(
                                &self.tools,
                                &self.subagent_spawner,
                                tool_calls,
                                self.config.max_concurrency,
                            )
                            .await?;

                            let mut clarification = None;
                            for (tc, (result, requires_input)) in
                                tool_calls.iter().zip(results)
                            {
                                on_event(StreamEvent::ToolCallEnd { name: &tc.function.name });
                                self.trajectory
                                    .record_tool_result(&tc.function.name, &result);
                                if requires_input {
                                    on_event(StreamEvent::ClarificationNeeded { question: &result });
                                    clarification = Some(result.clone());
                                }
                                self.messages.push(ChatMessage::tool_result(&tc.id, result));
                            }

                            if let Some(question) = clarification {
                                return Ok(AgentResult {
                                    response: String::new(),
                                    turns_used,
                                    tool_calls_made,
                                    finished_naturally: false,
                                    total_input_tokens,
                                    total_output_tokens,
                                    estimated_cost: Some(self.cost.total_cost),
                                    pending_clarification: Some(question),
                                });
                            }

                            self.maybe_inject_memory_nudge(tool_calls_made);
                            continue;
                        }
                    }

                    let response_text = assistant_msg
                        .content_text()
                        .unwrap_or("")
                        .to_owned();
                    self.trajectory.record_assistant_message(&response_text);
                    self.messages.push(ChatMessage::assistant(&response_text));

                    return Ok(AgentResult {
                        response: response_text,
                        turns_used,
                        tool_calls_made,
                        finished_naturally: choice.finish_reason.as_deref() != Some("length"),
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    });
                }
            }
        }
    }

    /// Inject a memory nudge system message if enough tool calls have
    /// accumulated since the last nudge.
    fn maybe_inject_memory_nudge(&mut self, tool_calls_made: usize) {
        if let Some(interval) = self.config.memory_nudge_interval {
            if interval > 0 && tool_calls_made > 0 && tool_calls_made % interval == 0 {
                debug!(
                    tool_calls_made,
                    interval, "injecting memory consolidation nudge"
                );
                self.messages
                    .push(ChatMessage::system(MEMORY_NUDGE));
            }
        }
    }

    /// Access the full conversation history.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Access the accumulated cost tracker.
    pub fn cost(&self) -> &SessionCost {
        &self.cost
    }

    /// Access the trajectory recorder.
    pub fn trajectory(&self) -> &TrajectoryRecorder {
        &self.trajectory
    }

    /// Access the trajectory recorder mutably.
    pub fn trajectory_mut(&mut self) -> &mut TrajectoryRecorder {
        &mut self.trajectory
    }

    /// Record token usage from an LLM turn and check the budget.
    fn record_usage(&mut self, turn: usize, input_tokens: u32, output_tokens: u32) -> Result<(), AgentError> {
        self.cost.record_turn(
            self.client.model(),
            turn,
            input_tokens,
            output_tokens,
        );

        match self.cost.check_budget() {
            BudgetStatus::Exceeded { used, limit } => {
                Err(AgentError::BudgetExceeded { used, limit })
            }
            BudgetStatus::Warning { used, limit } => {
                warn!(
                    used = format!("${used:.4}"),
                    limit = format!("${limit:.4}"),
                    "approaching budget limit"
                );
                Ok(())
            }
            BudgetStatus::Ok => Ok(()),
        }
    }

    /// Prune messages to stay within `max_context_messages`, preserving the
    /// system prompt at index 0 (if present) and the most recent messages.
    ///
    /// Before dropping old messages, the agent calls the LLM to produce a
    /// concise summary of the discarded conversation. This summary is
    /// inserted as a system message right after the main system prompt so
    /// the agent retains awareness of prior context.
    async fn prune_context(&mut self) {
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

        // Extract the messages we're about to drop and summarize them.
        let to_drop: Vec<ChatMessage> =
            self.messages[drop_start..drop_start + drop_count].to_vec();

        let summary = self.summarize_messages(&to_drop).await;

        // Remove the old messages.
        self.messages.drain(drop_start..drop_start + drop_count);

        // Inject the summary right after the system prompt (or at position 0).
        if let Some(text) = summary {
            let summary_msg = ChatMessage::system(format!(
                "[Prior conversation summary]\n{text}"
            ));
            self.messages.insert(drop_start, summary_msg);
        }
    }

    /// Ask the LLM to produce a compact summary of a slice of conversation
    /// messages. Returns `None` on any failure so the caller can degrade
    /// gracefully to plain pruning.
    async fn summarize_messages(&self, messages: &[ChatMessage]) -> Option<String> {
        if messages.is_empty() {
            return None;
        }

        // Build a transcript for the summarizer.
        let mut transcript = String::new();
        for msg in messages {
            let role = &msg.role;
            let content = msg.content_text().unwrap_or("[tool call]");
            // Truncate very long tool results to keep the summarization prompt small.
            let truncated = match content.char_indices().nth(500) {
                Some((i, _)) => format!("{}...", &content[..i]),
                None => content.to_owned(),
            };
            transcript.push_str(&format!("{role}: {truncated}\n"));
        }

        let prompt = format!(
            "Summarize the following conversation excerpt in 2-4 sentences. \
             Focus on: key decisions made, tasks completed, important facts \
             established, and any open questions. Be factual and concise.\n\n\
             ---\n{transcript}---"
        );

        let request = ChatCompletionRequest {
            model: String::new(), // client fills this in
            messages: vec![ChatMessage::user(&prompt)],
            tools: Vec::new(),
            temperature: Some(0.3),
            max_tokens: Some(256),
            stream: None,
            stream_options: None,
            response_format: None,
            extra_body: None,
        };

        match self.client.complete(request).await {
            Ok(response) => {
                let text = response
                    .choices
                    .first()
                    .and_then(|c| c.message.content_text().map(|s| s.to_owned()))
                    .unwrap_or_default();
                if text.is_empty() {
                    None
                } else {
                    info!(
                        summary_len = text.len(),
                        dropped_messages = messages.len(),
                        "summarized pruned context"
                    );
                    Some(text)
                }
            }
            Err(e) => {
                warn!(error = %e, "context summarization failed; dropping messages without summary");
                None
            }
        }
    }
}

struct StreamUpdate {
    contents: Vec<String>,
    tool_calls: Vec<ToolCallEntry>,
    finish_reason: Option<String>,
    usage: Option<genesis_provider::ChatUsage>,
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
        usage: chunk.usage,
    }
}

/// Parse a JSON arguments string into a flat BTreeMap<String, String>.
///
/// LLM tool call arguments come as a JSON string like `{"message":"hello"}`.
/// We flatten all values to their string representation for the ToolCall struct.
/// Execute multiple tool calls concurrently up to the given concurrency limit.
///
/// Results are returned in the same order as the input `tool_calls`, preserving
/// the tool-call-to-result correspondence required by the LLM message format.
/// If any tool call fails with a hard error (e.g., tool not found), that error
/// is propagated and the remaining results are discarded.
async fn execute_tool_calls_parallel(
    tools: &ToolRuntime,
    subagent_spawner: &Option<Arc<dyn SubagentSpawner>>,
    tool_calls: &[ToolCallEntry],
    max_concurrency: usize,
) -> Result<Vec<(String, bool)>, AgentError> {
    if tool_calls.len() == 1 {
        // Fast path: avoid semaphore overhead for single tool calls.
        let result = execute_single_tool(tools, subagent_spawner, &tool_calls[0]).await?;
        return Ok(vec![result]);
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrency.max(1)));
    let futs: Vec<_> = tool_calls
        .iter()
        .map(|tc| {
            let sem = Arc::clone(&semaphore);
            async move {
                let _permit = sem.acquire().await.expect("semaphore closed");
                execute_single_tool(tools, subagent_spawner, tc).await
            }
        })
        .collect();

    let results = futures_util::future::join_all(futs).await;

    // Collect results, short-circuiting on the first hard error.
    results.into_iter().collect()
}

/// Execute a single tool call against the provided runtime, returning the
/// content string for the LLM and whether the tool requests user input.
///
/// This is a free function (not a method) so it can be used for concurrent
/// execution from `&mut self` methods via field-level borrow splitting.
async fn execute_single_tool(
    tools: &ToolRuntime,
    subagent_spawner: &Option<Arc<dyn SubagentSpawner>>,
    tc: &ToolCallEntry,
) -> Result<(String, bool), AgentError> {
    let span = info_span!(
        "agent.tool_call",
        tool_name = tc.function.name.as_str(),
        tool_call_id = tc.id.as_str()
    );
    let started_at = Instant::now();
    let arguments = {
        let _entered = span.enter();
        let args = parse_tool_arguments(&tc.function.arguments)?;
        debug!(argument_count = args.len(), "parsed tool arguments");
        args
    };

    let call = ToolCall {
        name: tc.function.name.clone(),
        arguments,
    };

    match tools.execute_async(&call).await {
        Ok(output) => {
            info!(
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                output_bytes = output.content.len(),
                "tool call succeeded"
            );
            // Check for subagent spawn metadata.
            if let Some(spawner) = subagent_spawner {
                if output.metadata.get("__subagent_spawn").map(String::as_str) == Some("true") {
                    if let (Some(child_session_id), Some(subagent_id), Some(task)) = (
                        output.metadata.get("child_session_id"),
                        output.metadata.get("subagent_id"),
                        output.metadata.get("task"),
                    ) {
                        info!(
                            subagent_id = subagent_id.as_str(),
                            child_session_id = child_session_id.as_str(),
                            "spawning subagent workstream"
                        );
                        spawner.spawn(child_session_id, subagent_id, task);
                    }
                }
            }
            let requires_input = output
                .metadata
                .get("requires_input")
                .map(|v| v == "true")
                .unwrap_or(false);
            Ok((output.content, requires_input))
        }
        Err(err) => {
            match &err {
                ToolError::ToolNotFound(_) => {
                    error!(
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        error = %err,
                        "tool call failed with missing tool"
                    );
                    Err(err.into())
                }
                _ => {
                    warn!(
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        error = %err,
                        "tool call returned recoverable error content"
                    );
                    Ok((format!("Error: {err}"), false))
                }
            }
        }
    }
}

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

    fn test_agent() -> AgentLoop {
        let provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:8000/v1".to_owned(),
            api_key: String::new(),
            model: "test-model".to_owned(),
            backend: "openai".to_owned(),
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

        AgentLoop::with_history(
            client,
            tools,
            AgentLoopConfig {
                system_prompt: Some("system".to_owned()),
                ..AgentLoopConfig::default()
            },
            Vec::new(),
        )
    }

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
            usage: None,
        });

        assert_eq!(update.contents, vec!["hello".to_owned()]);
        assert_eq!(update.finish_reason.as_deref(), Some("stop"));
        assert!(update.tool_calls.is_empty());
        assert!(update.usage.is_none());
    }

    #[test]
    fn collect_stream_update_captures_usage_from_final_chunk() {
        let update = collect_stream_update(ChatCompletionChunk {
            id: "chunk-final".to_owned(),
            choices: vec![],
            usage: Some(genesis_provider::ChatUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            }),
        });

        let usage = update.usage.expect("usage should be present");
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
    }

    #[test]
    fn agent_loop_config_has_sensible_defaults() {
        let config = AgentLoopConfig::default();
        assert_eq!(config.max_turns, 20);
        assert!(config.system_prompt.is_none());
        assert!(config.temperature.is_none());
        assert_eq!(config.memory_nudge_interval, Some(15));
    }

    #[test]
    fn memory_nudge_injects_at_interval() {
        let mut agent = test_agent();
        let initial_len = agent.messages().len();

        // At 14 tool calls: no nudge
        agent.maybe_inject_memory_nudge(14);
        assert_eq!(agent.messages().len(), initial_len);

        // At 15 tool calls (interval): nudge injected
        agent.maybe_inject_memory_nudge(15);
        assert_eq!(agent.messages().len(), initial_len + 1);
        let last = agent.messages().last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content_text().unwrap().contains("Memory consolidation"));

        // At 16: no nudge
        agent.maybe_inject_memory_nudge(16);
        assert_eq!(agent.messages().len(), initial_len + 1);

        // At 30: second nudge
        agent.maybe_inject_memory_nudge(30);
        assert_eq!(agent.messages().len(), initial_len + 2);
    }

    #[test]
    fn memory_nudge_disabled_when_none() {
        let mut agent = test_agent();
        agent.config.memory_nudge_interval = None;
        let initial_len = agent.messages().len();

        agent.maybe_inject_memory_nudge(15);
        assert_eq!(agent.messages().len(), initial_len, "no nudge when disabled");
    }

    #[test]
    fn with_history_keeps_system_prompt_and_appends_prior_messages() {
        let mut agent = test_agent();
        agent.messages.push(ChatMessage::user("hi"));
        agent.messages.push(ChatMessage::assistant("hello"));

        assert_eq!(agent.messages().len(), 3);
        assert_eq!(agent.messages()[0].role, "system");
        assert_eq!(agent.messages()[1].role, "user");
        assert_eq!(agent.messages()[2].role, "assistant");
    }

    #[tokio::test]
    async fn execute_tool_call_propagates_missing_tool() {
        let agent = test_agent();
        let result = execute_single_tool(
            &agent.tools,
            &agent.subagent_spawner,
            &ToolCallEntry {
                id: "tool-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "does_not_exist".to_owned(),
                    arguments: "{}".to_owned(),
                },
            },
        ).await;

        assert!(matches!(result, Err(AgentError::Tool(ToolError::ToolNotFound(_)))));
    }

    #[tokio::test]
    async fn execute_tool_call_returns_error_content_for_recoverable_tool_error() {
        let agent = test_agent();
        let result = execute_single_tool(
            &agent.tools,
            &agent.subagent_spawner,
            &ToolCallEntry {
                id: "tool-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "echo".to_owned(),
                    arguments: "{}".to_owned(),
                },
            },
        )
        .await
        .expect("recoverable tool errors should return content");

        assert!(result.0.starts_with("Error:"));
        assert!(!result.1, "error results should not require input");
    }

    #[tokio::test]
    async fn parallel_tool_execution_preserves_order() {
        let agent = test_agent();
        let tool_calls = vec![
            ToolCallEntry {
                id: "call-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "echo".to_owned(),
                    arguments: r#"{"message":"first"}"#.to_owned(),
                },
            },
            ToolCallEntry {
                id: "call-2".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "echo".to_owned(),
                    arguments: r#"{"message":"second"}"#.to_owned(),
                },
            },
        ];

        let results = execute_tool_calls_parallel(
            &agent.tools,
            &agent.subagent_spawner,
            &tool_calls,
            4,
        )
        .await
        .expect("parallel execution should succeed");

        assert_eq!(results.len(), 2);
        assert!(results[0].0.contains("first"), "first result should contain 'first'");
        assert!(results[1].0.contains("second"), "second result should contain 'second'");
    }

    #[tokio::test]
    async fn parallel_tool_execution_single_item_fast_path() {
        let agent = test_agent();
        let tool_calls = vec![ToolCallEntry {
            id: "call-1".to_owned(),
            call_type: "function".to_owned(),
            function: genesis_provider::FunctionCall {
                name: "echo".to_owned(),
                arguments: r#"{"message":"solo"}"#.to_owned(),
            },
        }];

        let results = execute_tool_calls_parallel(
            &agent.tools,
            &agent.subagent_spawner,
            &tool_calls,
            4,
        )
        .await
        .expect("single-item parallel should succeed");

        assert_eq!(results.len(), 1);
        assert!(results[0].0.contains("solo"));
    }

    #[tokio::test]
    async fn parallel_tool_execution_propagates_first_error() {
        let agent = test_agent();
        let tool_calls = vec![
            ToolCallEntry {
                id: "call-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "echo".to_owned(),
                    arguments: r#"{"message":"ok"}"#.to_owned(),
                },
            },
            ToolCallEntry {
                id: "call-2".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "nonexistent_tool".to_owned(),
                    arguments: "{}".to_owned(),
                },
            },
        ];

        let result = execute_tool_calls_parallel(
            &agent.tools,
            &agent.subagent_spawner,
            &tool_calls,
            4,
        )
        .await;

        assert!(result.is_err(), "should propagate ToolNotFound error");
    }

    #[tokio::test]
    async fn prune_context_keeps_system_prompt_and_recent_messages() {
        let provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:8000/v1".to_owned(),
            api_key: String::new(),
            model: "test-model".to_owned(),
            backend: "openai".to_owned(),
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

        // Summarization will fail (no real LLM) so messages are dropped without
        // a summary — same count as the old synchronous test.
        agent.prune_context().await;

        // Should keep system + 3 most recent (no summary injected on failure)
        assert_eq!(agent.messages().len(), 4);
        assert_eq!(agent.messages()[0].role, "system");
        assert_eq!(
            agent.messages()[1].content_text(),
            Some("msg2")
        );
        assert_eq!(
            agent.messages()[2].content_text(),
            Some("reply2")
        );
        assert_eq!(
            agent.messages()[3].content_text(),
            Some("msg3")
        );
    }

    #[tokio::test]
    async fn prune_context_noop_when_under_limit() {
        let provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:8000/v1".to_owned(),
            api_key: String::new(),
            model: "test-model".to_owned(),
            backend: "openai".to_owned(),
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

        agent.prune_context().await;
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
            estimated_cost: Some(0.001),
            pending_clarification: None,
        };
        let json = serde_json::to_string(&result).expect("should serialize");
        assert!(json.contains("\"response\":\"Hello!\""));
        assert!(json.contains("\"turns_used\":1"));
        assert!(!json.contains("pending_clarification"));
    }

    #[tokio::test]
    async fn summarize_messages_returns_none_for_empty_slice() {
        let agent = test_agent();
        let result = agent.summarize_messages(&[]).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn summarize_messages_degrades_gracefully_when_provider_unavailable() {
        let agent = test_agent();
        let messages = vec![
            ChatMessage::user("What is 2+2?"),
            ChatMessage::assistant("4"),
        ];
        // Provider at localhost:8000 won't be running, so this should return None
        let result = agent.summarize_messages(&messages).await;
        assert!(result.is_none());
    }

    #[test]
    fn active_client_uses_primary_after_user_message() {
        let mut agent = test_agent();

        // Set up a tool client on a different endpoint so we can distinguish
        let tool_provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:9999/v1".to_owned(),
            api_key: String::new(),
            model: "cheap-model".to_owned(),
            backend: "openai".to_owned(),
        };
        let tool_client = ChatClient::new(&tool_provider).expect("tool client should build");
        agent.set_tool_client(tool_client);

        // After user message, should use primary
        agent.messages.push(ChatMessage::user("hello"));
        assert_eq!(agent.active_client().endpoint(), "http://localhost:8000/v1/chat/completions");
    }

    #[test]
    fn active_client_uses_tool_client_after_tool_result() {
        let mut agent = test_agent();

        let tool_provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:9999/v1".to_owned(),
            api_key: String::new(),
            model: "cheap-model".to_owned(),
            backend: "openai".to_owned(),
        };
        let tool_client = ChatClient::new(&tool_provider).expect("tool client should build");
        agent.set_tool_client(tool_client);

        // After tool result, should use tool client
        agent.messages.push(ChatMessage::tool_result("call-1", "result"));
        assert_eq!(agent.active_client().endpoint(), "http://localhost:9999/v1/chat/completions");
    }

    #[test]
    fn active_client_falls_back_when_no_tool_client() {
        let mut agent = test_agent();

        // No tool client set — should always use primary
        agent.messages.push(ChatMessage::tool_result("call-1", "result"));
        assert_eq!(agent.active_client().endpoint(), "http://localhost:8000/v1/chat/completions");
    }

    #[test]
    fn record_usage_tracks_cost() {
        let mut agent = test_agent();
        agent.record_usage(1, 1000, 500).expect("should succeed without budget");
        assert_eq!(agent.cost().total_input_tokens, 1000);
        assert_eq!(agent.cost().total_output_tokens, 500);
        assert_eq!(agent.cost().turns.len(), 1);
    }

    #[test]
    fn record_usage_returns_budget_exceeded() {
        let provider = genesis_provider::ResolvedProvider {
            base_url: "http://localhost:8000/v1".to_owned(),
            api_key: String::new(),
            model: "gpt-4.1-mini".to_owned(),
            backend: "openai".to_owned(),
        };
        let client = ChatClient::new(&provider).expect("client should build");
        let tools = crate::build_default_tool_runtime(&crate::ExecutionContext {
            plan: crate::SessionPlan {
                session_id: "s".to_owned(),
                profile: "default".to_owned(),
                platform: genesis_types::DeliveryPlatform::Cli,
                model: genesis_types::ModelSelection {
                    provider: genesis_types::ModelProviderKind::OpenAi,
                    model: "gpt-4.1-mini".to_owned(),
                    base_url: None,
                },
                initial_events: Vec::new(),
            },
            data_dir: "/tmp".to_owned(),
            database_path: "/tmp/genesis.db".to_owned(),
            max_concurrency: 4,
            allow_destructive_tools: false,
        });

        let mut agent = AgentLoop::new(
            client,
            tools,
            AgentLoopConfig {
                budget_limit: Some(0.001), // very tight budget
                ..AgentLoopConfig::default()
            },
        );

        // gpt-4.1-mini: $0.40/M input, $1.60/M output
        // 1M input = $0.40, way over $0.001 budget
        let result = agent.record_usage(1, 1_000_000, 0);
        assert!(matches!(result, Err(AgentError::BudgetExceeded { .. })));
    }

    #[test]
    fn cost_accessor_returns_session_cost() {
        let agent = test_agent();
        assert_eq!(agent.cost().total_input_tokens, 0);
        assert_eq!(agent.cost().total_cost, 0.0);
        assert!(agent.cost().budget_limit.is_none());
    }

    #[test]
    fn agent_result_skips_none_fields_in_json() {
        let result = AgentResult {
            response: "Hi".to_owned(),
            turns_used: 1,
            tool_calls_made: 0,
            finished_naturally: true,
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost: None,
            pending_clarification: None,
        };
        let json = serde_json::to_string(&result).expect("should serialize");
        assert!(!json.contains("estimated_cost"));
    }
}
