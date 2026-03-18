use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::StreamExt;
use genesis_provider::{
    ChatClient, ChatCompletionChunk, ChatCompletionRequest, ChatMessage, ChatTool, ContentPart,
    MessageContent, ProviderError, ToolCallEntry,
};
use genesis_tools::{ToolCall, ToolError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::{debug, info, info_span, warn};

use crate::cost::{BudgetStatus, SessionCost};
use crate::hooks::{HookEvent, HookResult, HookRunner};
use crate::nudge::SKILL_CREATION_NUDGE;
use crate::sanitize;
use crate::trajectory::TrajectoryRecorder;
use crate::ToolRuntime;

const DEFAULT_MAX_TURNS: usize = 20;

/// Events emitted during streaming execution.
#[derive(Debug, Clone)]
pub enum StreamEvent<'a> {
    /// A text content chunk from the LLM.
    Chunk(&'a str),
    /// A new agent turn (LLM call) is starting.
    TurnStarted,
    /// A tool call is about to be executed.
    ToolCallStart {
        name: &'a str,
        /// The provider-assigned tool call ID (e.g. `call_abc123`).
        call_id: &'a str,
        /// Short summary of the arguments (max ~40 chars).
        /// Owned `String` (not `&'a str`) because it's derived/truncated
        /// from the raw JSON args by `summarize_args()`.
        args_summary: String,
    },
    /// A tool call finished executing.
    ToolCallEnd {
        name: &'a str,
        /// The provider-assigned tool call ID.
        call_id: &'a str,
        /// How long the tool call took to execute in milliseconds.
        duration_ms: u64,
        /// Whether the tool call succeeded (no `Error:` prefix in output).
        success: bool,
    },
    /// Cumulative token usage for the current streaming turn.
    TokenUsage {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// The agent is requesting clarification from the user.
    ClarificationNeeded { question: &'a str },
    /// A non-fatal warning (e.g. budget approaching, context pruned).
    Warning(&'a str),
}

/// Produce a short summary string (max ~40 chars) from a tool call's JSON
/// arguments. Tries to show the first key-value pair; falls back to truncating
/// the raw string.
fn summarize_args(args_json: &str) -> String {
    if args_json.is_empty() || args_json == "{}" {
        return String::new();
    }
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(args_json)
    {
        if let Some((key, val)) = map.iter().next() {
            let v = match val {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let combined = format!("{key}: {v}");
            if combined.len() <= 40 {
                return combined;
            }
            let truncated: String = combined.chars().take(37).collect();
            return format!("{truncated}...");
        }
    }
    let raw = args_json.trim_matches(|c| c == '{' || c == '}').trim();
    if raw.len() <= 40 {
        return raw.to_owned();
    }
    let truncated: String = raw.chars().take(37).collect();
    format!("{truncated}...")
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
    /// Maximum input tokens before context compression triggers. When the last
    /// API response reports prompt_tokens above this threshold, the middle
    /// portion of the conversation is summarized and replaced. Protects the
    /// first 3 and last 4 non-system messages. Set to `None` to disable.
    pub max_context_tokens: Option<u32>,
    /// Enable trajectory recording for agent training data capture.
    pub enable_trajectory: bool,
    /// Directory to save trajectory files. When set with enable_trajectory,
    /// trajectories are auto-saved after each turn. Files are written to
    /// `{trajectory_dir}/{session_id}.json`.
    pub trajectory_dir: Option<String>,
    /// Session ID for trajectory file naming.
    pub session_id: Option<String>,
    /// Extended thinking configuration. When set, requests include reasoning
    /// parameters for providers that support it (e.g. Claude, o1/o3).
    pub thinking: Option<genesis_provider::ThinkingConfig>,
    /// Optional response format constraint (e.g. json_object, json_schema).
    /// When set, every chat completion request includes this format directive.
    pub response_format: Option<genesis_provider::ResponseFormat>,
    /// Timeout for individual tool calls in seconds. When a tool exceeds this
    /// duration, it is cancelled and the LLM receives a timeout error. Default: 120s.
    pub tool_timeout_secs: u64,
    /// Maximum number of iterations (LLM calls) across the lifetime of this
    /// agent loop. Unlike `max_turns` which resets per user message, this is a
    /// hard cap on total LLM round-trips. Useful for autonomous/batch agents
    /// that run many turns. `None` means unlimited.
    pub max_iterations: Option<usize>,
    /// Tool call parser for models that embed tool calls in text content.
    /// When set, responses are normalized to extract tool calls from text.
    /// Auto-detected from model name when `None`.
    pub tool_call_parser: Option<String>,
    /// Reasoning effort level. Injected into the request as `reasoning_effort`
    /// for providers that support it (OpenRouter, some custom providers).
    pub reasoning_effort: Option<genesis_config::ReasoningEffort>,
    /// Response cache configuration. When set, identical LLM requests are
    /// served from a SQLite cache instead of calling the provider.
    pub cache: Option<genesis_config::CacheConfig>,
    /// Guardrails configuration. When set, user input is validated before
    /// processing and agent output is validated before returning.
    pub guardrails: Option<crate::guardrails::GuardrailConfig>,
}

/// Default number of tool calls between memory consolidation nudges.
const DEFAULT_MEMORY_NUDGE_INTERVAL: usize = 15;

/// The memory nudge message injected as a system message.
const MEMORY_NUDGE: &str = "\
[Memory consolidation reminder] You've been working for a while. \
Consider saving any useful observations, patterns, or user preferences \
you've noticed using `memory_create`. Focus on durable insights that \
would be valuable in future sessions — not session-specific details.";

/// Number of tool calls in a single turn that triggers a skill creation nudge.
const SKILL_CREATION_THRESHOLD: usize = 8;

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            max_turns: DEFAULT_MAX_TURNS,
            temperature: None,
            max_tokens: None,
            max_context_messages: None,
            max_context_tokens: None,
            budget_limit: None,
            max_concurrency: 4,
            memory_nudge_interval: Some(DEFAULT_MEMORY_NUDGE_INTERVAL),
            enable_trajectory: false,
            trajectory_dir: None,
            session_id: None,
            thinking: None,
            response_format: None,
            tool_timeout_secs: 120,
            max_iterations: None,
            tool_call_parser: None,
            reasoning_effort: None,
            cache: None,
            guardrails: None,
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
    #[error("iteration budget exhausted: {used} / {limit} iterations")]
    IterationsExhausted { used: usize, limit: usize },
    #[error("agent loop was cancelled")]
    Cancelled,
}

/// Callback for spawning subagent workstreams. Called when the agent
/// invokes the `spawn_subagent` tool with the child session ID and task.
pub trait SubagentSpawner: Send + Sync {
    fn spawn(&self, child_session_id: &str, subagent_id: &str, task: &str);
}

/// Lifecycle hooks for the agent loop.
///
/// All methods have default no-op implementations, so consumers only need
/// to override the events they care about. Hooks are called synchronously
/// and should return quickly — expensive work should be spawned.
///
/// Errors in hooks are logged but never propagate to the agent loop.
pub trait AgentHooks: Send + Sync {
    /// Called when a user turn begins, before the first LLM call.
    fn on_turn_start(&self, _session_id: &str, _user_message: &str) {}

    /// Called when a user turn completes (naturally or by limit/budget).
    fn on_turn_end(&self, _session_id: &str, _result: &AgentResult) {}

    /// Called before each tool is executed.
    fn on_tool_call_start(&self, _session_id: &str, _tool_name: &str) {}

    /// Called after a tool finishes executing (success or failure).
    fn on_tool_call_end(
        &self,
        _session_id: &str,
        _tool_name: &str,
        _success: bool,
        _duration_ms: u64,
    ) {
    }

    /// Called before each LLM API request.
    fn on_llm_request(&self, _session_id: &str, _model: &str, _turn: usize) {}

    /// Called after each LLM API response with token counts.
    fn on_llm_response(
        &self,
        _session_id: &str,
        _model: &str,
        _input_tokens: u32,
        _output_tokens: u32,
    ) {
    }

    /// Called when context compression is triggered.
    fn on_context_prune(&self, _session_id: &str, _messages_before: usize, _messages_after: usize) {
    }

    /// Called when a tool is detected in a stuck loop.
    fn on_stuck_loop(&self, _session_id: &str, _tool_name: &str, _failure_count: usize) {}
}

/// No-op hook implementation (default).
pub struct NoopHooks;
impl AgentHooks for NoopHooks {}

/// The core agent loop that wires provider (LLM) and tool execution together.
///
/// Flow: user message → LLM → [tool_calls → execute → LLM]* → final text
pub struct AgentLoop {
    client: ChatClient,
    /// Optional cheaper client for tool-calling turns. When set, turns that
    /// follow tool results use this client while turns following user messages
    /// use the primary `client`.
    tool_client: Option<ChatClient>,
    /// Fallback clients tried in order when the primary (or active) client fails.
    fallback_clients: Vec<ChatClient>,
    tools: ToolRuntime,
    config: AgentLoopConfig,
    messages: Vec<ChatMessage>,
    subagent_spawner: Option<Arc<dyn SubagentSpawner>>,
    hooks: Arc<dyn AgentHooks>,
    hook_runner: HookRunner,
    hook_results: Vec<HookResult>,
    cost: SessionCost,
    trajectory: TrajectoryRecorder,
    /// Last reported prompt token count from the API. Used for token-aware
    /// context compression.
    last_prompt_tokens: u32,
    /// Tracks consecutive failures per tool name. When a tool fails
    /// `STUCK_LOOP_THRESHOLD` times in a row, a system nudge tells the LLM
    /// to try a different approach.
    tool_failure_counts: HashMap<String, usize>,
    /// Total number of LLM iterations consumed across all user turns.
    /// Checked against `config.max_iterations` at each loop boundary.
    iterations_used: usize,
    /// Cancellation flag. When set to `true`, the loop exits gracefully
    /// at the next turn boundary.
    cancelled: Arc<AtomicBool>,
    /// Optional response cache for deduplicating identical LLM calls.
    response_cache: Option<genesis_storage::ResponseCacheStore>,
    cache_hits: u32,
    cache_misses: u32,
    /// Pre-compiled guardrails (avoids recompiling regexes on every check).
    compiled_guardrails: Option<crate::guardrails::CompiledGuardrails>,
}

/// Number of consecutive failures for the same tool before injecting a
/// "try a different approach" nudge.
const STUCK_LOOP_THRESHOLD: usize = 3;

impl AgentLoop {
    pub fn new(
        client: ChatClient,
        tools: ToolRuntime,
        config: AgentLoopConfig,
        hook_runner: HookRunner,
    ) -> Self {
        Self::with_history(client, tools, config, hook_runner, Vec::new())
    }

    /// Return the session ID as a `&str`, defaulting to `""` when unset.
    fn session_id_str(&self) -> &str {
        self.config.session_id.as_deref().unwrap_or_default()
    }

    pub fn with_history(
        client: ChatClient,
        tools: ToolRuntime,
        config: AgentLoopConfig,
        hook_runner: HookRunner,
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
            let session_id = config.session_id.as_deref().unwrap_or("session");
            TrajectoryRecorder::new(session_id, client.model(), sys)
        } else {
            TrajectoryRecorder::disabled()
        };

        let compiled_guardrails = config
            .guardrails
            .as_ref()
            .map(crate::guardrails::CompiledGuardrails::new);

        Self {
            client,
            tool_client: None,
            fallback_clients: Vec::new(),
            tools,
            config,
            messages,
            subagent_spawner: None,
            hooks: Arc::new(NoopHooks),
            hook_runner,
            hook_results: Vec::new(),
            cost,
            trajectory,
            last_prompt_tokens: 0,
            tool_failure_counts: HashMap::new(),
            iterations_used: 0,
            cancelled: Arc::new(AtomicBool::new(false)),
            response_cache: None,
            cache_hits: 0,
            cache_misses: 0,
            compiled_guardrails,
        }
    }

    /// Returns a cancellation handle that can be used to stop the agent loop
    /// from another task. Call `handle.store(true, Ordering::Relaxed)` to cancel.
    pub fn cancellation_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// Set an optional cheaper client for tool-calling turns.
    pub fn set_tool_client(&mut self, client: ChatClient) {
        self.tool_client = Some(client);
    }

    /// Set fallback clients to try when the primary provider fails.
    pub fn set_fallback_clients(&mut self, clients: Vec<ChatClient>) {
        self.fallback_clients = clients;
    }

    /// Set the response cache store for deduplicating LLM calls.
    pub fn set_response_cache(&mut self, cache: genesis_storage::ResponseCacheStore) {
        self.response_cache = Some(cache);
    }

    /// Compute a deterministic cache key from the model, recent messages, and tools.
    fn compute_cache_key(&self, model: &str, tools: &[ChatTool]) -> String {
        let max_msgs = self
            .config
            .cache
            .as_ref()
            .map(|c| c.max_context_messages)
            .unwrap_or(4);

        let mut hasher = Sha256::new();
        hasher.update(model.as_bytes());

        // Hash the last N messages (skip system prompt at index 0)
        let msgs = if self.messages.len() > max_msgs {
            &self.messages[self.messages.len() - max_msgs..]
        } else {
            &self.messages[..]
        };
        for msg in msgs {
            hasher.update(msg.role.as_bytes());
            if let Some(text) = msg.content_text() {
                hasher.update(text.as_bytes());
            }
        }

        // Hash tool names (not full definitions — too verbose)
        for tool in tools {
            hasher.update(tool.function.name.as_bytes());
        }

        hex::encode(hasher.finalize())
    }

    /// Attach a subagent spawner so the agent can spawn parallel workstreams.
    pub fn set_subagent_spawner(&mut self, spawner: Arc<dyn SubagentSpawner>) {
        self.subagent_spawner = Some(spawner);
    }

    /// Attach lifecycle hooks for monitoring, logging, or integration.
    pub fn set_hooks(&mut self, hooks: Arc<dyn AgentHooks>) {
        self.hooks = hooks;
    }

    /// Access recorded shell hook executions for inspection/testing.
    pub fn hook_results(&self) -> &[HookResult] {
        &self.hook_results
    }

    /// Returns how many LLM iterations remain, or `None` if no limit is set.
    pub fn remaining_iterations(&self) -> Option<usize> {
        self.config
            .max_iterations
            .map(|limit| limit.saturating_sub(self.iterations_used))
    }

    /// Returns the total number of LLM iterations consumed so far.
    pub fn iterations_used(&self) -> usize {
        self.iterations_used
    }

    /// Resolve the tool call parser: explicit config takes priority, then auto-detect.
    fn resolve_parser(
        &self,
        model: &str,
    ) -> Option<Box<dyn genesis_provider::parsers::ToolCallParser>> {
        if let Some(ref name) = self.config.tool_call_parser {
            genesis_provider::parsers::get_parser(name)
        } else {
            genesis_provider::parsers::detect_parser(model)
        }
    }

    /// Apply tool call parser to normalize responses from models that embed
    /// tool calls in text content rather than using the native tool_calls field.
    fn apply_tool_call_parser(
        &self,
        response: &mut genesis_provider::ChatCompletionResponse,
        model: &str,
    ) {
        if let Some(parser) = self.resolve_parser(model) {
            genesis_provider::parsers::normalize_response(response, parser.as_ref());
        }
    }

    /// Inject reasoning effort into the request's extra_body if configured.
    fn inject_reasoning_effort(&self, request: &mut ChatCompletionRequest) {
        if let Some(ref effort) = self.config.reasoning_effort {
            let effort_str = match effort {
                genesis_config::ReasoningEffort::High => "high",
                genesis_config::ReasoningEffort::Medium => "medium",
                genesis_config::ReasoningEffort::Low => "low",
            };

            let extra = request
                .extra_body
                .get_or_insert_with(|| serde_json::json!({}));

            if let Some(obj) = extra.as_object_mut() {
                obj.insert(
                    "reasoning_effort".to_owned(),
                    serde_json::Value::String(effort_str.to_owned()),
                );
            }
        }
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

    /// Try a blocking completion against the active client, falling back to
    /// each fallback client in order if the primary fails.
    async fn complete_with_failover(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<(genesis_provider::ChatCompletionResponse, String), ProviderError> {
        let client = self.active_client().clone();
        let model = client.model().to_owned();

        match client.complete(request.clone()).await {
            Ok(response) => return Ok((response, model)),
            Err(err) => {
                if self.fallback_clients.is_empty() {
                    return Err(err);
                }
                warn!(
                    model = model.as_str(),
                    error = %err,
                    fallback_count = self.fallback_clients.len(),
                    "primary provider failed, trying fallbacks"
                );
            }
        }

        for (i, fallback) in self.fallback_clients.iter().enumerate() {
            let fb_model = fallback.model().to_owned();
            match fallback.complete(request.clone()).await {
                Ok(response) => {
                    info!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        "fallback provider succeeded"
                    );
                    return Ok((response, fb_model));
                }
                Err(err) => {
                    warn!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        error = %err,
                        "fallback provider failed"
                    );
                }
            }
        }

        Err(ProviderError::AllProvidersFailed {
            count: 1 + self.fallback_clients.len(),
        })
    }

    /// Try a streaming completion against the active client, falling back to
    /// each fallback client in order if the primary fails to connect.
    async fn complete_stream_with_failover(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<(genesis_provider::ChatCompletionChunkStream, String), ProviderError> {
        let client = self.active_client().clone();
        let model = client.model().to_owned();

        match client.complete_stream(request.clone()).await {
            Ok(stream) => return Ok((stream, model)),
            Err(err) => {
                if self.fallback_clients.is_empty() {
                    return Err(err);
                }
                warn!(
                    model = model.as_str(),
                    error = %err,
                    fallback_count = self.fallback_clients.len(),
                    "primary provider stream failed, trying fallbacks"
                );
            }
        }

        for (i, fallback) in self.fallback_clients.iter().enumerate() {
            let fb_model = fallback.model().to_owned();
            match fallback.complete_stream(request.clone()).await {
                Ok(stream) => {
                    info!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        "fallback provider stream succeeded"
                    );
                    return Ok((stream, fb_model));
                }
                Err(err) => {
                    warn!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        error = %err,
                        "fallback provider stream failed"
                    );
                }
            }
        }

        Err(ProviderError::AllProvidersFailed {
            count: 1 + self.fallback_clients.len(),
        })
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
        // Record turn-level span attributes via tracing events rather than
        // holding a non-Send span guard across await points.
        info!(
            session_id = self.session_id_str(),
            image_count = images.len(),
            "agent.turn.start"
        );
        let hook_session = self.session_id_str().to_owned();
        self.fire_shell_hooks(
            HookEvent::PreTurn,
            serde_json::json!({
                "session_id": hook_session,
                "user_message": user_message,
                "image_count": images.len(),
            }),
        );

        // Run input guardrails if configured
        let user_message = if let Some(ref cg) = self.compiled_guardrails {
            let result = cg.check_input(user_message);
            if !result.passed {
                let agent_result = AgentResult {
                    response: format!(
                        "Your input was blocked by guardrails: {}",
                        format_blocked_reasons(&result)
                    ),
                    turns_used: 0,
                    tool_calls_made: 0,
                    finished_naturally: true,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    estimated_cost: None,
                    pending_clarification: None,
                };
                self.fire_shell_hooks(
                    HookEvent::PostTurn,
                    self.turn_result_context(&hook_session, &agent_result),
                );
                self.hooks.on_turn_end(&hook_session, &agent_result);
                return Ok(agent_result);
            }
            result.content
        } else {
            user_message.to_owned()
        };

        self.trajectory.record_user_message(&user_message);

        if images.is_empty() {
            self.messages.push(ChatMessage::user(&user_message));
        } else {
            self.messages
                .push(ChatMessage::user_with_images(user_message.clone(), images));
        }

        // Fire turn-start hook
        self.hooks.on_turn_start(&hook_session, &user_message);

        let tool_defs: Vec<ChatTool> = self
            .tools
            .definitions_async()
            .await
            .iter()
            .map(ChatTool::from)
            .collect();

        let mut turns_used = 0;
        let mut tool_calls_made = 0;
        let mut total_input_tokens = 0u32;
        let mut total_output_tokens = 0u32;

        loop {
            // Check cancellation at each turn boundary
            if self.cancelled.load(Ordering::Relaxed) {
                info!("agent loop cancelled by external signal");
                self.save_trajectory();
                let result = AgentResult {
                    response: "The operation was cancelled.".to_owned(),
                    turns_used,
                    tool_calls_made,
                    finished_naturally: false,
                    total_input_tokens,
                    total_output_tokens,
                    estimated_cost: Some(self.cost.total_cost),
                    pending_clarification: None,
                };
                self.fire_shell_hooks(
                    HookEvent::PostTurn,
                    self.turn_result_context(&hook_session, &result),
                );
                self.hooks.on_turn_end(&hook_session, &result);
                return Ok(result);
            }

            turns_used += 1;
            if turns_used > self.config.max_turns {
                warn!(
                    max_turns = self.config.max_turns,
                    "agent loop reached turn limit"
                );
                self.save_trajectory();
                let result = AgentResult {
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
                };
                self.fire_shell_hooks(
                    HookEvent::PostTurn,
                    self.turn_result_context(&hook_session, &result),
                );
                self.hooks.on_turn_end(&hook_session, &result);
                return Ok(result);
            }

            // Check iteration budget (lifetime cap across all user turns)
            if let Some(limit) = self.config.max_iterations {
                if self.iterations_used >= limit {
                    warn!(
                        iterations = self.iterations_used,
                        limit, "iteration budget exhausted"
                    );
                    self.save_trajectory();
                    let result = AgentResult {
                        response: format!(
                            "Iteration budget exhausted ({limit} iterations). \
                             The work so far has been saved."
                        ),
                        turns_used,
                        tool_calls_made,
                        finished_naturally: false,
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    };
                    self.fire_shell_hooks(
                        HookEvent::PostTurn,
                        self.turn_result_context(&hook_session, &result),
                    );
                    self.hooks.on_turn_end(&hook_session, &result);
                    return Ok(result);
                }
            }
            self.iterations_used += 1;

            debug!(
                turn = turns_used,
                mode = "blocking",
                prompt_version = crate::prompt::PROMPT_VERSION,
                "starting agent turn iteration"
            );

            self.prune_context().await;
            let mut request = ChatCompletionRequest::new("", self.messages.clone());
            request.tools = tool_defs.clone();
            request.temperature = self.config.temperature;
            request.max_tokens = self.config.max_tokens;
            request.thinking = self.config.thinking.clone();
            request.response_format = self.config.response_format.clone();
            self.inject_reasoning_effort(&mut request);

            // Check response cache before making an LLM call
            let cache_key = if self.response_cache.is_some()
                && self.config.cache.as_ref().is_some_and(|c| c.enabled)
            {
                Some(self.compute_cache_key(self.active_client().model(), &tool_defs))
            } else {
                None
            };

            let cached = cache_key.as_ref().and_then(|key| {
                self.response_cache
                    .as_ref()
                    .and_then(|cache| cache.get(key).ok().flatten())
            });

            self.hooks
                .on_llm_request(&hook_session, self.active_client().model(), turns_used);
            let llm_started_at = std::time::Instant::now();
            let (mut response, active_model) = if let Some(hit) = cached {
                debug!(
                    cache_key = cache_key.as_deref().unwrap_or(""),
                    "response cache hit"
                );
                self.cache_hits += 1;

                // Reconstruct a ChatCompletionResponse from the cached data
                let tool_calls: Option<Vec<ToolCallEntry>> = hit
                    .tool_calls_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str(json).ok());
                let choice = genesis_provider::ChatChoice {
                    index: 0,
                    message: ChatMessage {
                        role: "assistant".to_owned(),
                        content: Some(MessageContent::Text(hit.response)),
                        thinking: None,
                        tool_calls,
                        tool_call_id: None,
                        name: None,
                        provider_metadata: None,
                    },
                    finish_reason: Some("stop".to_owned()),
                };
                let resp = genesis_provider::ChatCompletionResponse {
                    id: cache_key
                        .as_deref()
                        .map(|k| format!("cache-{}", &k[..k.len().min(8)]))
                        .unwrap_or_else(|| "cache-unknown".to_owned()),
                    choices: vec![choice],
                    usage: Some(genesis_provider::ChatUsage {
                        prompt_tokens: hit.input_tokens,
                        completion_tokens: hit.output_tokens,
                        total_tokens: hit.input_tokens + hit.output_tokens,
                    }),
                };
                (resp, hit.model)
            } else {
                if cache_key.is_some() {
                    self.cache_misses += 1;
                }
                match self.complete_with_failover(request).await {
                    Ok(result) => result,
                    Err(err) => {
                        return Err(self.report_error(&hook_session, "llm_request", err.into()))
                    }
                }
            };

            // Apply tool call parser for models that embed tool calls in text
            self.apply_tool_call_parser(&mut response, &active_model);

            // Store response in cache (only on cache miss with a valid key)
            if let (Some(ref key), Some(ref cache), Some(ref cache_cfg)) =
                (&cache_key, &self.response_cache, &self.config.cache)
            {
                if cache_cfg.enabled
                    && !response.id.starts_with("cache-")
                    && !response.choices.is_empty()
                {
                    let choice = &response.choices[0];
                    let text = choice.message.content_text().unwrap_or("");
                    let tc_json = choice
                        .message
                        .tool_calls
                        .as_ref()
                        .and_then(|tc| serde_json::to_string(tc).ok());
                    let (in_tok, out_tok) = response
                        .usage
                        .as_ref()
                        .map(|u| (u.prompt_tokens, u.completion_tokens))
                        .unwrap_or((0, 0));
                    let _ = cache.set(
                        key,
                        &active_model,
                        text,
                        tc_json.as_deref(),
                        in_tok,
                        out_tok,
                        cache_cfg.ttl_seconds,
                    );
                }
            }

            // Log LLM response metrics as a tracing event.
            if let Some(usage) = &response.usage {
                info!(
                    model = active_model.as_str(),
                    turn = turns_used,
                    input_tokens = usage.prompt_tokens,
                    output_tokens = usage.completion_tokens,
                    latency_ms = llm_started_at.elapsed().as_millis() as u64,
                    "agent.llm_response"
                );
            }

            if let Some(usage) = &response.usage {
                total_input_tokens = total_input_tokens.saturating_add(usage.prompt_tokens);
                total_output_tokens = total_output_tokens.saturating_add(usage.completion_tokens);
                self.last_prompt_tokens = usage.prompt_tokens;
                if let Err(err) = self.record_usage_with_model(
                    &active_model,
                    turns_used,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                ) {
                    return Err(self.report_error(&hook_session, "usage_record", err));
                }
                self.hooks.on_llm_response(
                    &hook_session,
                    &active_model,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                );
            }

            let choice = response.choices.first().ok_or_else(|| {
                self.report_error(
                    &hook_session,
                    "empty_response",
                    AgentError::Provider(ProviderError::EmptyChoices),
                )
            })?;
            let assistant_msg = &choice.message;

            // Check if the assistant wants to call tools
            if let Some(tool_calls) = &assistant_msg.tool_calls {
                if !tool_calls.is_empty() {
                    // Record assistant message with tool calls
                    if let Some(text) = assistant_msg.content_text() {
                        self.trajectory.record_assistant_message(text);
                    }

                    // Append the assistant message (with tool_calls) to history,
                    // preserving provider_metadata (e.g. reasoning blobs) for multi-turn continuity.
                    let mut msg = ChatMessage::assistant_with_tool_calls(
                        assistant_msg.content.clone(),
                        tool_calls.clone(),
                    );
                    msg.provider_metadata = assistant_msg.provider_metadata.clone();
                    self.messages.push(msg);

                    // Execute tool calls in parallel (up to max_concurrency).
                    tool_calls_made += tool_calls.len();

                    // Record each tool call and fire hooks
                    for tc in tool_calls {
                        self.trajectory
                            .record_tool_call(&tc.function.name, &tc.function.arguments);
                        self.hooks
                            .on_tool_call_start(&hook_session, &tc.function.name);
                        self.fire_shell_hooks(
                            HookEvent::PreToolCall,
                            serde_json::json!({
                                "session_id": hook_session,
                                "tool_name": tc.function.name,
                                "tool_call_id": tc.id,
                                "arguments": tc.function.arguments,
                            }),
                        );
                    }

                    let tool_start = Instant::now();
                    let results = match execute_tool_calls_parallel(
                        &self.tools,
                        &self.subagent_spawner,
                        tool_calls,
                        self.config.max_concurrency,
                        self.config.tool_timeout_secs,
                    )
                    .await
                    {
                        Ok(results) => results,
                        Err(err) => {
                            return Err(self.report_error(&hook_session, "tool_execution", err))
                        }
                    };
                    let tool_elapsed_ms = tool_start.elapsed().as_millis() as u64;

                    let mut clarification = None;
                    for (tc, (result, requires_input)) in tool_calls.iter().zip(results) {
                        let result = sanitize::sanitize_credentials(&result);
                        self.trajectory
                            .record_tool_result(&tc.function.name, &result);
                        // Track consecutive failures per tool
                        let success = !result.starts_with("Error:");
                        if !success {
                            let count = self
                                .tool_failure_counts
                                .entry(tc.function.name.clone())
                                .or_insert(0);
                            *count += 1;
                        } else {
                            self.tool_failure_counts.remove(&tc.function.name);
                        }
                        self.hooks.on_tool_call_end(
                            &hook_session,
                            &tc.function.name,
                            success,
                            tool_elapsed_ms,
                        );
                        self.fire_shell_hooks(
                            HookEvent::PostToolCall,
                            serde_json::json!({
                                "session_id": hook_session,
                                "tool_name": tc.function.name,
                                "tool_call_id": tc.id,
                                "success": success,
                                "result": result,
                                "requires_input": requires_input,
                                "duration_ms": tool_elapsed_ms,
                            }),
                        );
                        if requires_input {
                            clarification = Some(result.clone());
                        }
                        self.messages.push(ChatMessage::tool_result(&tc.id, result));
                    }

                    // Inject stuck-loop nudge if any tool failed too many times
                    self.maybe_inject_stuck_nudge();

                    // If a tool requested user input, pause the agent loop
                    if let Some(question) = clarification {
                        self.save_trajectory();
                        let result = AgentResult {
                            response: String::new(),
                            turns_used,
                            tool_calls_made,
                            finished_naturally: false,
                            total_input_tokens,
                            total_output_tokens,
                            estimated_cost: Some(self.cost.total_cost),
                            pending_clarification: Some(question),
                        };
                        self.fire_shell_hooks(
                            HookEvent::PostTurn,
                            self.turn_result_context(&hook_session, &result),
                        );
                        self.hooks.on_turn_end(&hook_session, &result);
                        return Ok(result);
                    }

                    // Inject memory nudge if due.
                    self.maybe_inject_memory_nudge(tool_calls_made);

                    // Continue the loop - send tool results back to LLM
                    continue;
                }
            }

            // No tool calls - this is the final text response
            let mut response_text = assistant_msg.content_text().unwrap_or("").to_owned();

            // Run output guardrails if configured
            if let Some(ref cg) = self.compiled_guardrails {
                let result = cg.check_output(&response_text);
                if !result.passed {
                    response_text = format!(
                        "Response blocked by guardrails: {}",
                        format_blocked_reasons(&result)
                    );
                } else {
                    response_text = result.content;
                }
            }

            self.trajectory.record_assistant_message(&response_text);
            let mut msg = ChatMessage::assistant(&response_text);
            msg.provider_metadata = assistant_msg.provider_metadata.clone();
            self.messages.push(msg);

            self.save_trajectory();
            let result = AgentResult {
                response: response_text,
                turns_used,
                tool_calls_made,
                finished_naturally: !matches!(
                    choice.finish_reason.as_deref(),
                    Some("length") | Some("incomplete")
                ),
                total_input_tokens,
                total_output_tokens,
                estimated_cost: Some(self.cost.total_cost),
                pending_clarification: None,
            };
            self.fire_shell_hooks(
                HookEvent::PostTurn,
                self.turn_result_context(&hook_session, &result),
            );
            self.fire_shell_hooks(
                HookEvent::OnComplete,
                self.turn_result_context(&hook_session, &result),
            );
            self.hooks.on_turn_end(&hook_session, &result);
            return Ok(result);
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
        let hook_session = self.session_id_str().to_owned();
        self.fire_shell_hooks(
            HookEvent::PreTurn,
            serde_json::json!({
                "session_id": hook_session,
                "user_message": user_message,
                "image_count": images.len(),
                "streaming": true,
            }),
        );

        // Run input guardrails if configured (streaming path)
        let user_message = if let Some(ref cg) = self.compiled_guardrails {
            let result = cg.check_input(user_message);
            if !result.passed {
                let agent_result = AgentResult {
                    response: format!(
                        "Your input was blocked by guardrails: {}",
                        format_blocked_reasons(&result)
                    ),
                    turns_used: 0,
                    tool_calls_made: 0,
                    finished_naturally: true,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    estimated_cost: None,
                    pending_clarification: None,
                };
                self.fire_shell_hooks(
                    HookEvent::PostTurn,
                    self.turn_result_context(&hook_session, &agent_result),
                );
                self.hooks.on_turn_end(&hook_session, &agent_result);
                return Ok(agent_result);
            }
            result.content
        } else {
            user_message.to_owned()
        };

        self.trajectory.record_user_message(&user_message);

        if images.is_empty() {
            self.messages.push(ChatMessage::user(&user_message));
        } else {
            self.messages
                .push(ChatMessage::user_with_images(user_message.clone(), images));
        }

        // Fire turn-start hook (streaming)
        self.hooks.on_turn_start(&hook_session, &user_message);
        on_event(StreamEvent::TurnStarted);

        let tool_defs: Vec<ChatTool> = self
            .tools
            .definitions_async()
            .await
            .iter()
            .map(ChatTool::from)
            .collect();

        let mut turns_used = 0;
        let mut tool_calls_made = 0;
        let mut total_input_tokens = 0u32;
        let mut total_output_tokens = 0u32;

        loop {
            if self.cancelled.load(Ordering::Relaxed) {
                info!("agent loop cancelled by external signal (streaming)");
                self.save_trajectory();
                let result = AgentResult {
                    response: "The operation was cancelled.".to_owned(),
                    turns_used,
                    tool_calls_made,
                    finished_naturally: false,
                    total_input_tokens,
                    total_output_tokens,
                    estimated_cost: Some(self.cost.total_cost),
                    pending_clarification: None,
                };
                self.fire_shell_hooks(
                    HookEvent::PostTurn,
                    self.turn_result_context(&hook_session, &result),
                );
                self.hooks.on_turn_end(&hook_session, &result);
                return Ok(result);
            }

            turns_used += 1;
            if turns_used > self.config.max_turns {
                warn!(
                    max_turns = self.config.max_turns,
                    "agent loop reached turn limit (streaming)"
                );
                let msg = format!(
                    "I've reached the maximum of {} turns for this request. \
                     The work so far has been saved. You can continue by sending another message.",
                    self.config.max_turns
                );
                on_event(StreamEvent::Chunk(&msg));
                self.save_trajectory();
                let result = AgentResult {
                    response: msg,
                    turns_used: turns_used - 1,
                    tool_calls_made,
                    finished_naturally: false,
                    total_input_tokens,
                    total_output_tokens,
                    estimated_cost: Some(self.cost.total_cost),
                    pending_clarification: None,
                };
                self.fire_shell_hooks(
                    HookEvent::PostTurn,
                    self.turn_result_context(&hook_session, &result),
                );
                self.hooks.on_turn_end(&hook_session, &result);
                return Ok(result);
            }

            // Check iteration budget (lifetime cap across all user turns)
            if let Some(limit) = self.config.max_iterations {
                if self.iterations_used >= limit {
                    warn!(
                        iterations = self.iterations_used,
                        limit, "iteration budget exhausted (streaming)"
                    );
                    let msg = format!(
                        "Iteration budget exhausted ({limit} iterations). \
                         The work so far has been saved."
                    );
                    on_event(StreamEvent::Chunk(&msg));
                    self.save_trajectory();
                    let result = AgentResult {
                        response: msg,
                        turns_used,
                        tool_calls_made,
                        finished_naturally: false,
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    };
                    self.fire_shell_hooks(
                        HookEvent::PostTurn,
                        self.turn_result_context(&hook_session, &result),
                    );
                    self.hooks.on_turn_end(&hook_session, &result);
                    return Ok(result);
                }
            }
            self.iterations_used += 1;

            debug!(
                turn = turns_used,
                mode = "streaming",
                prompt_version = crate::prompt::PROMPT_VERSION,
                "starting agent turn iteration"
            );

            self.prune_context().await;
            let mut request = ChatCompletionRequest::new("", self.messages.clone());
            request.tools = tool_defs.clone();
            request.temperature = self.config.temperature;
            request.max_tokens = self.config.max_tokens;
            request.thinking = self.config.thinking.clone();
            request.response_format = self.config.response_format.clone();
            self.inject_reasoning_effort(&mut request);

            self.hooks
                .on_llm_request(&hook_session, self.active_client().model(), turns_used);
            let stream_result = self.complete_stream_with_failover(request.clone()).await;
            match stream_result {
                Ok((mut stream, active_model)) => {
                    let mut response_text = String::new();
                    let mut streamed_tool_calls = Vec::new();
                    let mut finished_naturally = true;
                    let mut turn_input_tokens = 0u32;
                    let mut turn_output_tokens = 0u32;

                    while let Some(chunk) = stream.next().await {
                        let chunk = match chunk {
                            Ok(chunk) => chunk,
                            Err(err) => {
                                return Err(self.report_error(
                                    &hook_session,
                                    "stream_chunk",
                                    err.into(),
                                ))
                            }
                        };
                        let update = collect_stream_update(chunk);

                        for content in update.contents {
                            on_event(StreamEvent::Chunk(&content));
                            response_text.push_str(&content);
                        }

                        if let Some(reason) = update.finish_reason {
                            finished_naturally =
                                !matches!(reason.as_str(), "length" | "incomplete");
                        }

                        if let Some(usage) = update.usage {
                            turn_input_tokens =
                                turn_input_tokens.saturating_add(usage.prompt_tokens);
                            turn_output_tokens =
                                turn_output_tokens.saturating_add(usage.completion_tokens);
                        }

                        merge_streamed_tool_calls(&mut streamed_tool_calls, update.tool_calls);
                    }

                    total_input_tokens = total_input_tokens.saturating_add(turn_input_tokens);
                    total_output_tokens = total_output_tokens.saturating_add(turn_output_tokens);
                    self.last_prompt_tokens = turn_input_tokens;
                    if let Err(err) = self.record_usage_with_model(
                        &active_model,
                        turns_used,
                        turn_input_tokens,
                        turn_output_tokens,
                    ) {
                        return Err(self.report_error(&hook_session, "usage_record", err));
                    }
                    self.hooks.on_llm_response(
                        &hook_session,
                        &active_model,
                        turn_input_tokens,
                        turn_output_tokens,
                    );
                    on_event(StreamEvent::TokenUsage {
                        input_tokens: total_input_tokens,
                        output_tokens: total_output_tokens,
                    });

                    // If streaming didn't produce native tool calls, try parsing from text
                    if streamed_tool_calls.is_empty() && !response_text.is_empty() {
                        if let Some(parser) = self.resolve_parser(&active_model) {
                            if let Some(result) = parser.parse(&response_text) {
                                streamed_tool_calls = result.tool_calls;
                                response_text = result.content.unwrap_or_default();
                            }
                        }
                    }

                    if !streamed_tool_calls.is_empty() {
                        // Record assistant message if present
                        if !response_text.is_empty() {
                            self.trajectory.record_assistant_message(&response_text);
                        }

                        // TODO: propagate provider_metadata from response.completed event for streaming reasoning continuity
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
                            on_event(StreamEvent::ToolCallStart {
                                name: &tc.function.name,
                                call_id: &tc.id,
                                args_summary: summarize_args(&tc.function.arguments),
                            });
                            self.trajectory
                                .record_tool_call(&tc.function.name, &tc.function.arguments);
                            self.fire_shell_hooks(
                                HookEvent::PreToolCall,
                                serde_json::json!({
                                    "session_id": hook_session,
                                    "tool_name": tc.function.name,
                                    "tool_call_id": tc.id,
                                    "arguments": tc.function.arguments,
                                    "streaming": true,
                                }),
                            );
                        }

                        // Execute tool calls in parallel.
                        tool_calls_made += streamed_tool_calls.len();
                        let tool_exec_start = Instant::now();
                        let results = match execute_tool_calls_parallel(
                            &self.tools,
                            &self.subagent_spawner,
                            &streamed_tool_calls,
                            self.config.max_concurrency,
                            self.config.tool_timeout_secs,
                        )
                        .await
                        {
                            Ok(results) => results,
                            Err(err) => {
                                return Err(self.report_error(&hook_session, "tool_execution", err))
                            }
                        };
                        let tool_exec_duration = tool_exec_start.elapsed();

                        let tool_exec_duration_ms = tool_exec_duration.as_millis() as u64;
                        let mut clarification = None;
                        for (tc, (result, requires_input)) in
                            streamed_tool_calls.iter().zip(results)
                        {
                            let result = sanitize::sanitize_credentials(&result);
                            let tool_success = !result.starts_with("Error:");
                            on_event(StreamEvent::ToolCallEnd {
                                name: &tc.function.name,
                                call_id: &tc.id,
                                duration_ms: tool_exec_duration_ms,
                                success: tool_success,
                            });
                            self.trajectory
                                .record_tool_result(&tc.function.name, &result);
                            if result.starts_with("Error:") {
                                let count = self
                                    .tool_failure_counts
                                    .entry(tc.function.name.clone())
                                    .or_insert(0);
                                *count += 1;
                            } else {
                                self.tool_failure_counts.remove(&tc.function.name);
                            }
                            self.fire_shell_hooks(
                                HookEvent::PostToolCall,
                                serde_json::json!({
                                    "session_id": hook_session,
                                    "tool_name": tc.function.name,
                                    "tool_call_id": tc.id,
                                    "success": !result.starts_with("Error:"),
                                    "result": result,
                                    "requires_input": requires_input,
                                    "streaming": true,
                                }),
                            );
                            if requires_input {
                                on_event(StreamEvent::ClarificationNeeded { question: &result });
                                clarification = Some(result.clone());
                            }
                            self.messages.push(ChatMessage::tool_result(&tc.id, result));
                        }

                        self.maybe_inject_stuck_nudge();

                        if let Some(question) = clarification {
                            self.save_trajectory();
                            let result = AgentResult {
                                response: String::new(),
                                turns_used,
                                tool_calls_made,
                                finished_naturally: false,
                                total_input_tokens,
                                total_output_tokens,
                                estimated_cost: Some(self.cost.total_cost),
                                pending_clarification: Some(question),
                            };
                            self.fire_shell_hooks(
                                HookEvent::PostTurn,
                                self.turn_result_context(&hook_session, &result),
                            );
                            self.hooks.on_turn_end(&hook_session, &result);
                            return Ok(result);
                        }

                        self.maybe_inject_memory_nudge(tool_calls_made);
                        continue;
                    }

                    self.trajectory.record_assistant_message(&response_text);
                    // TODO: propagate provider_metadata for streaming reasoning continuity
                    self.messages.push(ChatMessage::assistant(&response_text));

                    self.maybe_inject_skill_nudge(tool_calls_made);
                    self.save_trajectory();
                    let result = AgentResult {
                        response: response_text,
                        turns_used,
                        tool_calls_made,
                        finished_naturally,
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    };
                    self.fire_shell_hooks(
                        HookEvent::PostTurn,
                        self.turn_result_context(&hook_session, &result),
                    );
                    self.fire_shell_hooks(
                        HookEvent::OnComplete,
                        self.turn_result_context(&hook_session, &result),
                    );
                    self.hooks.on_turn_end(&hook_session, &result);
                    return Ok(result);
                }
                Err(_) => {
                    warn!(
                        turn = turns_used,
                        "streaming provider request failed; falling back to blocking completion"
                    );
                    let (mut response, fb_model) = match self.complete_with_failover(request).await
                    {
                        Ok(result) => result,
                        Err(err) => {
                            return Err(self.report_error(
                                &hook_session,
                                "llm_request_fallback",
                                err.into(),
                            ))
                        }
                    };

                    // Apply tool call parser for models that embed tool calls in text
                    self.apply_tool_call_parser(&mut response, &fb_model);

                    if let Some(usage) = &response.usage {
                        total_input_tokens = total_input_tokens.saturating_add(usage.prompt_tokens);
                        total_output_tokens =
                            total_output_tokens.saturating_add(usage.completion_tokens);
                        self.last_prompt_tokens = usage.prompt_tokens;
                        if let Err(err) = self.record_usage_with_model(
                            &fb_model,
                            turns_used,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                        ) {
                            return Err(self.report_error(&hook_session, "usage_record", err));
                        }
                        on_event(StreamEvent::TokenUsage {
                            input_tokens: total_input_tokens,
                            output_tokens: total_output_tokens,
                        });
                    }

                    let choice = response.choices.first().ok_or_else(|| {
                        self.report_error(
                            &hook_session,
                            "empty_response",
                            AgentError::Provider(ProviderError::EmptyChoices),
                        )
                    })?;
                    let assistant_msg = &choice.message;

                    if let Some(tool_calls) = &assistant_msg.tool_calls {
                        if !tool_calls.is_empty() {
                            if let Some(text) = assistant_msg.content_text() {
                                self.trajectory.record_assistant_message(text);
                            }

                            let mut msg = ChatMessage::assistant_with_tool_calls(
                                assistant_msg.content.clone(),
                                tool_calls.clone(),
                            );
                            msg.provider_metadata = assistant_msg.provider_metadata.clone();
                            self.messages.push(msg);

                            // Emit start events and record tool calls.
                            for tc in tool_calls.iter() {
                                on_event(StreamEvent::ToolCallStart {
                                    name: &tc.function.name,
                                    call_id: &tc.id,
                                    args_summary: summarize_args(&tc.function.arguments),
                                });
                                self.trajectory
                                    .record_tool_call(&tc.function.name, &tc.function.arguments);
                                self.fire_shell_hooks(
                                    HookEvent::PreToolCall,
                                    serde_json::json!({
                                        "session_id": hook_session,
                                        "tool_name": tc.function.name,
                                        "tool_call_id": tc.id,
                                        "arguments": tc.function.arguments,
                                        "streaming": false,
                                    }),
                                );
                            }

                            // Execute tool calls in parallel.
                            tool_calls_made += tool_calls.len();
                            let tool_exec_start = Instant::now();
                            let results = match execute_tool_calls_parallel(
                                &self.tools,
                                &self.subagent_spawner,
                                tool_calls,
                                self.config.max_concurrency,
                                self.config.tool_timeout_secs,
                            )
                            .await
                            {
                                Ok(results) => results,
                                Err(err) => {
                                    return Err(self.report_error(
                                        &hook_session,
                                        "tool_execution",
                                        err,
                                    ))
                                }
                            };
                            let tool_exec_duration = tool_exec_start.elapsed();
                            let tool_exec_duration_ms = tool_exec_duration.as_millis() as u64;

                            let mut clarification = None;
                            for (tc, (result, requires_input)) in tool_calls.iter().zip(results) {
                                let result = sanitize::sanitize_credentials(&result);
                                let tool_success = !result.starts_with("Error:");
                                on_event(StreamEvent::ToolCallEnd {
                                    name: &tc.function.name,
                                    call_id: &tc.id,
                                    duration_ms: tool_exec_duration_ms,
                                    success: tool_success,
                                });
                                self.trajectory
                                    .record_tool_result(&tc.function.name, &result);
                                if result.starts_with("Error:") {
                                    let count = self
                                        .tool_failure_counts
                                        .entry(tc.function.name.clone())
                                        .or_insert(0);
                                    *count += 1;
                                } else {
                                    self.tool_failure_counts.remove(&tc.function.name);
                                }
                                self.fire_shell_hooks(
                                    HookEvent::PostToolCall,
                                    serde_json::json!({
                                        "session_id": hook_session,
                                        "tool_name": tc.function.name,
                                        "tool_call_id": tc.id,
                                        "success": !result.starts_with("Error:"),
                                        "result": result,
                                        "requires_input": requires_input,
                                        "streaming": false,
                                    }),
                                );
                                if requires_input {
                                    on_event(StreamEvent::ClarificationNeeded {
                                        question: &result,
                                    });
                                    clarification = Some(result.clone());
                                }
                                self.messages.push(ChatMessage::tool_result(&tc.id, result));
                            }

                            self.maybe_inject_stuck_nudge();

                            if let Some(question) = clarification {
                                self.save_trajectory();
                                let result = AgentResult {
                                    response: String::new(),
                                    turns_used,
                                    tool_calls_made,
                                    finished_naturally: false,
                                    total_input_tokens,
                                    total_output_tokens,
                                    estimated_cost: Some(self.cost.total_cost),
                                    pending_clarification: Some(question),
                                };
                                self.fire_shell_hooks(
                                    HookEvent::PostTurn,
                                    self.turn_result_context(&hook_session, &result),
                                );
                                self.hooks.on_turn_end(&hook_session, &result);
                                return Ok(result);
                            }

                            self.maybe_inject_memory_nudge(tool_calls_made);
                            continue;
                        }
                    }

                    let mut response_text = assistant_msg.content_text().unwrap_or("").to_owned();

                    // Run output guardrails if configured (streaming path)
                    if let Some(ref cg) = self.compiled_guardrails {
                        let gr = cg.check_output(&response_text);
                        if !gr.passed {
                            response_text = format!(
                                "Response blocked by guardrails: {}",
                                format_blocked_reasons(&gr)
                            );
                        } else {
                            response_text = gr.content;
                        }
                    }

                    self.trajectory.record_assistant_message(&response_text);
                    let mut msg = ChatMessage::assistant(&response_text);
                    msg.provider_metadata = assistant_msg.provider_metadata.clone();
                    self.messages.push(msg);

                    self.maybe_inject_skill_nudge(tool_calls_made);
                    self.save_trajectory();
                    let result = AgentResult {
                        response: response_text,
                        turns_used,
                        tool_calls_made,
                        finished_naturally: !matches!(
                            choice.finish_reason.as_deref(),
                            Some("length") | Some("incomplete")
                        ),
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    };
                    self.fire_shell_hooks(
                        HookEvent::PostTurn,
                        self.turn_result_context(&hook_session, &result),
                    );
                    self.fire_shell_hooks(
                        HookEvent::OnComplete,
                        self.turn_result_context(&hook_session, &result),
                    );
                    self.hooks.on_turn_end(&hook_session, &result);
                    return Ok(result);
                }
            }
        }
    }

    /// Inject a memory nudge system message if enough tool calls have
    /// accumulated since the last nudge.
    fn maybe_inject_memory_nudge(&mut self, tool_calls_made: usize) {
        if let Some(interval) = self.config.memory_nudge_interval {
            if interval > 0 && tool_calls_made > 0 && tool_calls_made.is_multiple_of(interval) {
                debug!(
                    tool_calls_made,
                    interval, "injecting memory consolidation nudge"
                );
                self.messages.push(ChatMessage::system(MEMORY_NUDGE));
            }
        }
    }

    /// Inject a skill creation nudge if the turn involved many tool calls,
    /// suggesting the agent save the procedure as a reusable skill.
    fn maybe_inject_skill_nudge(&mut self, tool_calls_made: usize) {
        if tool_calls_made >= SKILL_CREATION_THRESHOLD {
            debug!(
                tool_calls_made,
                threshold = SKILL_CREATION_THRESHOLD,
                "injecting skill creation nudge"
            );
            self.messages
                .push(ChatMessage::system(SKILL_CREATION_NUDGE));
        }
    }

    /// Check if any tool has failed too many times in a row and inject a
    /// system nudge telling the LLM to try a different approach.
    fn maybe_inject_stuck_nudge(&mut self) {
        let stuck_tools: Vec<String> = self
            .tool_failure_counts
            .iter()
            .filter(|(_, count)| **count >= STUCK_LOOP_THRESHOLD)
            .map(|(name, _)| name.clone())
            .collect();

        if stuck_tools.is_empty() {
            return;
        }

        let hook_session = self.session_id_str().to_owned();
        for tool in &stuck_tools {
            let count = self.tool_failure_counts[tool];
            warn!(
                tool_name = tool.as_str(),
                failure_count = count,
                "tool has repeated failures, injecting stuck-loop nudge",
            );
            self.hooks.on_stuck_loop(&hook_session, tool, count);
            // Reset the counter so we don't spam nudges
            self.tool_failure_counts.remove(tool);
        }

        let tools_list = stuck_tools.join(", ");
        let nudge = format!(
            "[Stuck loop detected] The tool(s) {tools_list} have failed multiple times in a row. \
             Stop retrying the same approach. Consider: (1) using a different tool, \
             (2) modifying the arguments, (3) breaking the task into smaller steps, or \
             (4) asking the user for clarification."
        );
        self.messages.push(ChatMessage::system(&nudge));
    }

    /// Save the trajectory to disk if a trajectory directory is configured.
    fn save_trajectory(&self) {
        if let Some(dir) = &self.config.trajectory_dir {
            let session_id = self.config.session_id.as_deref().unwrap_or("unknown");
            let path = std::path::Path::new(dir).join(format!("{session_id}.json"));
            if let Err(e) = self.trajectory.save_to_file(&path) {
                warn!(error = %e, "failed to save trajectory");
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

    fn fire_shell_hooks(&mut self, event: HookEvent, context: serde_json::Value) {
        let results = self.hook_runner.run_hooks(event, &context);
        self.hook_results.extend(results);
    }

    fn report_error(&mut self, session_id: &str, stage: &str, error: AgentError) -> AgentError {
        self.fire_shell_hooks(
            HookEvent::OnError,
            serde_json::json!({
                "session_id": session_id,
                "stage": stage,
                "error": error.to_string(),
            }),
        );
        error
    }

    fn turn_result_context(&self, session_id: &str, result: &AgentResult) -> serde_json::Value {
        serde_json::json!({
            "session_id": session_id,
            "response": result.response,
            "turns_used": result.turns_used,
            "tool_calls_made": result.tool_calls_made,
            "finished_naturally": result.finished_naturally,
            "total_input_tokens": result.total_input_tokens,
            "total_output_tokens": result.total_output_tokens,
            "estimated_cost": result.estimated_cost,
            "pending_clarification": result.pending_clarification,
        })
    }

    /// Record token usage from an LLM turn and check the budget.
    #[cfg(test)]
    fn record_usage(
        &mut self,
        turn: usize,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<(), AgentError> {
        let model = self.client.model().to_owned();
        self.record_usage_with_model(&model, turn, input_tokens, output_tokens)
    }

    fn record_usage_with_model(
        &mut self,
        model: &str,
        turn: usize,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<(), AgentError> {
        self.cost
            .record_turn(model, turn, input_tokens, output_tokens);

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

    /// Replace old tool result content with a compact placeholder to reduce
    /// token usage without an LLM call. Preserves tool call (assistant)
    /// messages so the reasoning chain remains intact.
    ///
    /// Based on "The Complexity Trap" (NeurIPS 2025): observation masking
    /// achieves ~52% cost reduction while maintaining or improving solve rate.
    fn mask_old_tool_outputs(&mut self) {
        /// Number of recent messages to protect from masking (approximately
        /// the last 4 assistant + tool result pairs).
        const PROTECT_RECENT: usize = 8;
        /// Only mask tool outputs longer than this many bytes.
        const MIN_CONTENT_LEN: usize = 200;

        let has_system = self.messages.first().is_some_and(|m| m.role == "system");
        let start = if has_system { 1 } else { 0 };
        let end = self.messages.len().saturating_sub(PROTECT_RECENT);

        if end <= start {
            return;
        }

        let mut masked_count = 0u32;
        for msg in &mut self.messages[start..end] {
            if msg.role == "tool" {
                if let Some(ref content) = msg.content {
                    let text_len = match content {
                        MessageContent::Text(t) => t.len(),
                        MessageContent::Parts(parts) => {
                            // Skip masking if any part is non-text (e.g. images)
                            // to avoid silently discarding non-text content.
                            let all_text =
                                parts.iter().all(|p| matches!(p, ContentPart::Text { .. }));
                            if !all_text {
                                continue;
                            }
                            parts
                                .iter()
                                .map(|p| match p {
                                    ContentPart::Text { text } => text.len(),
                                    _ => 0,
                                })
                                .sum()
                        }
                    };
                    if text_len > MIN_CONTENT_LEN {
                        msg.content = Some(MessageContent::Text(
                            "[Tool output masked — see preceding tool call for context]".to_owned(),
                        ));
                        masked_count += 1;
                    }
                }
            }
        }

        if masked_count > 0 {
            info!(
                masked_count,
                "masked old tool outputs to reduce context tokens"
            );
        }
    }

    /// Prune messages to stay within context limits, preserving the system
    /// prompt at index 0 (if present) and the most recent messages.
    ///
    /// Two triggers:
    /// 1. **Message count**: `max_context_messages` caps total non-system messages.
    /// 2. **Token count**: `max_context_tokens` triggers when the last API call's
    ///    prompt_tokens exceeds 85% of the limit, compressing the middle of the
    ///    conversation while protecting the first 3 and last 4 non-system messages.
    ///
    /// Before dropping old messages, the agent calls the LLM to produce a
    /// concise summary. This summary is inserted as a system message right
    /// after the main system prompt so the agent retains awareness of context.
    async fn prune_context(&mut self) {
        let has_system = self.messages.first().is_some_and(|m| m.role == "system");
        let drop_start = if has_system { 1 } else { 0 };
        let non_system_count = self.messages.len() - drop_start;

        // Determine how many messages to drop.
        let drop_count = self.compute_drop_count(non_system_count, drop_start);

        if drop_count == 0 {
            return;
        }

        // Lightweight first pass: mask old tool outputs (no LLM call).
        // Only runs when context is actually under pressure (drop_count > 0).
        self.mask_old_tool_outputs();

        // Extract the messages we're about to drop and summarize them.
        let to_drop: Vec<ChatMessage> = self.messages[drop_start..drop_start + drop_count].to_vec();

        info!(
            drop_count,
            remaining = non_system_count - drop_count,
            trigger = if self.token_compression_needed() {
                "tokens"
            } else {
                "messages"
            },
            "pruning conversation context"
        );

        let summary = self.summarize_messages(&to_drop).await;

        let messages_before = self.messages.len();
        // Remove the old messages.
        self.messages.drain(drop_start..drop_start + drop_count);

        // Inject the summary right after the system prompt (or at position 0).
        if let Some(text) = summary {
            let summary_msg = ChatMessage::system(format!("[Prior conversation summary]\n{text}"));
            self.messages.insert(drop_start, summary_msg);
        }

        let hook_session = self.session_id_str().to_owned();
        self.hooks
            .on_context_prune(&hook_session, messages_before, self.messages.len());
    }

    /// Check if token-based compression should trigger (>85% of max_context_tokens).
    fn token_compression_needed(&self) -> bool {
        if let Some(max_tokens) = self.config.max_context_tokens {
            let threshold = (max_tokens as f64 * 0.85) as u32;
            self.last_prompt_tokens > threshold
        } else {
            false
        }
    }

    /// Compute how many messages to drop. Returns 0 if no pruning needed.
    ///
    /// Prefers token-based compression (protects first 3 + last 4) over
    /// simple message-count pruning. If both triggers fire, uses whichever
    /// drops more messages.
    fn compute_drop_count(&self, non_system_count: usize, _drop_start: usize) -> usize {
        let mut drop = 0;

        // Message-count trigger.
        if let Some(limit) = self.config.max_context_messages {
            if non_system_count > limit {
                drop = non_system_count - limit;
            }
        }

        // Token-count trigger: protect first 3 and last 4 non-system messages.
        if self.token_compression_needed() {
            let protect_head = 3usize;
            let protect_tail = 4usize;
            let protected = protect_head + protect_tail;
            if non_system_count > protected {
                let token_drop = non_system_count - protected;
                // Use whichever drops more to aggressively reclaim context.
                drop = drop.max(token_drop);
            }
        }

        drop
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
            tool_choice: None,
            thinking: None,
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

/// Merge streaming tool call deltas into the accumulated tool calls list.
///
/// In SSE streaming (both Chat Completions and Responses API), tool calls
/// arrive as incremental chunks:
///   1. First chunk: `id` + `name` + empty `arguments` → new entry
///   2. Subsequent chunks: empty `id` + empty `name` + argument fragment → append
///
/// This function appends argument fragments to the last matching tool call
/// instead of creating separate ghost entries with empty names.
fn merge_streamed_tool_calls(accumulated: &mut Vec<ToolCallEntry>, deltas: Vec<ToolCallEntry>) {
    for delta in deltas {
        if !delta.id.is_empty() && !delta.function.name.is_empty() {
            // New tool call — push as a new entry
            accumulated.push(delta);
        } else if !delta.function.arguments.is_empty() {
            // Argument fragment — append to the last tool call
            if let Some(last) = accumulated.last_mut() {
                last.function.arguments.push_str(&delta.function.arguments);
            }
        }
        // Ignore entries with empty id, empty name, AND empty arguments
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
    timeout_secs: u64,
) -> Result<Vec<(String, bool)>, AgentError> {
    let timeout_duration = std::time::Duration::from_secs(timeout_secs);

    if tool_calls.len() == 1 {
        // Fast path: avoid semaphore overhead for single tool calls.
        // execute_single_tool converts all errors to soft "Error:" content,
        // so the Ok(r) branch always succeeds and timeouts are the only
        // additional failure mode to handle.
        let result = match tokio::time::timeout(
            timeout_duration,
            execute_single_tool(tools, subagent_spawner, &tool_calls[0]),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => {
                // Defensive: execute_single_tool should never return Err
                // after error-as-data conversion, but handle it gracefully.
                (
                    format!(
                        "Error: tool `{}` encountered an unexpected error. \
                         Try a different approach.",
                        tool_calls[0].function.name
                    ),
                    false,
                )
            }
            Err(_) => {
                warn!(
                    tool_name = tool_calls[0].function.name.as_str(),
                    timeout_secs, "tool call timed out"
                );
                (
                    format!(
                        "Error: tool `{}` timed out after {timeout_secs}s. \
                         The operation took too long. Try a simpler approach \
                         or break the task into smaller steps.",
                        tool_calls[0].function.name
                    ),
                    false,
                )
            }
        };
        return Ok(vec![result]);
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrency.max(1)));
    let futs: Vec<_> = tool_calls
        .iter()
        .map(|tc| {
            let sem = Arc::clone(&semaphore);
            let tool_name = tc.function.name.clone();
            async move {
                let Ok(_permit) = sem.acquire().await else {
                    return Ok((
                        format!("Error: tool `{tool_name}` skipped — concurrency semaphore closed"),
                        false,
                    ));
                };
                match tokio::time::timeout(
                    timeout_duration,
                    execute_single_tool(tools, subagent_spawner, tc),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        warn!(
                            tool_name = tool_name.as_str(),
                            timeout_secs, "tool call timed out"
                        );
                        Ok((
                            format!(
                                "Error: tool `{tool_name}` timed out after {timeout_secs}s. \
                                 The operation took too long. Try a simpler approach \
                                 or break the task into smaller steps."
                            ),
                            false,
                        ))
                    }
                }
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
    let tool_name = &tc.function.name;

    // Parse arguments — malformed JSON from the LLM is a recoverable error
    // (feed it back so the model can self-correct) rather than a hard failure.
    let arguments = {
        let _entered = span.enter();
        match parse_tool_arguments(&tc.function.arguments) {
            Ok(args) => {
                debug!(argument_count = args.len(), "parsed tool arguments");
                args
            }
            Err(e) => {
                warn!(
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    tool_name = tool_name.as_str(),
                    error = %e,
                    "tool argument parse failed, feeding error back to LLM"
                );
                return Ok((
                    format!(
                        "Error: tool `{tool_name}` received invalid arguments: {e}\n\n\
                         Please fix the JSON arguments and try again."
                    ),
                    false,
                ));
            }
        }
    };

    let call = ToolCall {
        name: tool_name.clone(),
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
            warn!(
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                tool_name = tool_name.as_str(),
                error = %err,
                "tool call failed, feeding error back to LLM"
            );
            match &err {
                ToolError::ToolNotFound(name) => {
                    let suggestions = suggest_similar_tools(name, tools);
                    let msg = if suggestions.is_empty() {
                        format!(
                            "Error: tool `{name}` not found. \
                             Use only tools listed in the system prompt."
                        )
                    } else {
                        format!(
                            "Error: tool `{name}` not found. Did you mean: {}?\n\n\
                             Try calling one of the suggested tools instead.",
                            suggestions.join(", ")
                        )
                    };
                    Ok((msg, false))
                }
                ToolError::MissingArgument { tool, argument } => Ok((
                    format!(
                        "Error: tool `{tool}` is missing required argument `{argument}`.\n\n\
                         Please include the `{argument}` parameter and try again."
                    ),
                    false,
                )),
                ToolError::ApprovalDenied { tool, reason } => Ok((
                    format!(
                        "Error: tool `{tool}` was denied: {reason}\n\n\
                         Try a different approach that doesn't require this operation."
                    ),
                    false,
                )),
                ToolError::ExecutionFailed { tool, reason } => Ok((
                    format!(
                        "Error: tool `{tool}` execution failed: {reason}\n\n\
                         You can try a different approach or use an alternative tool."
                    ),
                    false,
                )),
            }
        }
    }
}

fn parse_tool_arguments(raw: &str) -> Result<BTreeMap<String, String>, AgentError> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| AgentError::ArgumentParse(format!("{raw}: {e}")))?;

    let obj = value
        .as_object()
        .ok_or_else(|| AgentError::ArgumentParse(format!("expected JSON object, got: {raw}")))?;

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

/// Suggest tool names similar to `name` using edit distance.
/// Returns up to 3 suggestions sorted by similarity.
fn suggest_similar_tools(name: &str, tools: &ToolRuntime) -> Vec<String> {
    let name_lower = name.to_lowercase();
    let mut scored: Vec<(String, usize)> = tools
        .definitions()
        .iter()
        .filter_map(|def| {
            let def_lower = def.name.to_lowercase();
            let dist = edit_distance(&name_lower, &def_lower);
            let max_len = name.len().max(def.name.len());
            // Only suggest if within 40% edit distance
            if max_len > 0 && dist <= max_len * 2 / 5 {
                Some((def.name.clone(), dist))
            } else {
                // Also match if one is a substring of the other
                if def_lower.contains(&name_lower) || name_lower.contains(&def_lower) {
                    Some((def.name.clone(), dist))
                } else {
                    None
                }
            }
        })
        .collect();
    scored.sort_by_key(|(_, d)| *d);
    scored.truncate(3);
    scored.into_iter().map(|(n, _)| format!("`{n}`")).collect()
}

/// Simple Levenshtein edit distance.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

fn format_blocked_reasons(result: &crate::guardrails::GuardrailResult) -> String {
    result
        .violations
        .iter()
        .filter(|v| v.action == crate::guardrails::ViolationAction::Block)
        .map(|v| v.message.as_str())
        .collect::<Vec<&str>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookConfig;
    use std::io::{Read, Write};

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
            provider_backend: "openai".to_owned(),
        });

        AgentLoop::with_history(
            client,
            tools,
            AgentLoopConfig {
                system_prompt: Some("system".to_owned()),
                ..AgentLoopConfig::default()
            },
            HookRunner::default(),
            Vec::new(),
        )
    }

    fn test_agent_with_endpoint(base_url: String, hooks: Vec<HookConfig>) -> AgentLoop {
        let provider = genesis_provider::ResolvedProvider {
            base_url,
            api_key: String::new(),
            model: "test-model".to_owned(),
            backend: "openai".to_owned(),
        };
        let client = ChatClient::new(&provider).expect("client should build");
        let tools = crate::build_default_tool_runtime(&crate::ExecutionContext {
            plan: crate::SessionPlan {
                session_id: "session-hooks".to_owned(),
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
            provider_backend: "openai".to_owned(),
        });

        AgentLoop::with_history(
            client,
            tools,
            AgentLoopConfig {
                system_prompt: Some("system".to_owned()),
                session_id: Some("session-hooks".to_owned()),
                ..AgentLoopConfig::default()
            },
            HookRunner::new(hooks),
            Vec::new(),
        )
    }

    fn start_mock_server(responses: Vec<String>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for body in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buffer = [0u8; 8192];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).expect("write");
                stream.flush().expect("flush");
            }
        });
        format!("http://{addr}/v1")
    }

    #[test]
    fn parse_tool_arguments_handles_simple_object() {
        let args =
            parse_tool_arguments(r#"{"message":"hello","count":"3"}"#).expect("should parse");
        assert_eq!(args.get("message").unwrap(), "hello");
        assert_eq!(args.get("count").unwrap(), "3");
    }

    #[test]
    fn parse_tool_arguments_stringifies_non_string_values() {
        let args = parse_tool_arguments(r#"{"flag":true,"num":42}"#).expect("should parse");
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
        assert!(last
            .content_text()
            .unwrap()
            .contains("Memory consolidation"));

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
        assert_eq!(
            agent.messages().len(),
            initial_len,
            "no nudge when disabled"
        );
    }

    #[test]
    fn cancellation_handle_returns_shared_flag() {
        let agent = test_agent();
        let handle = agent.cancellation_handle();
        assert!(!handle.load(Ordering::Relaxed));

        // Setting it from outside should be visible to the agent
        handle.store(true, Ordering::Relaxed);
        assert!(agent.cancelled.load(Ordering::Relaxed));
    }

    #[test]
    fn stuck_loop_nudge_fires_after_threshold() {
        let mut agent = test_agent();
        let initial_len = agent.messages().len();

        // Simulate 2 failures — not enough to trigger
        agent.tool_failure_counts.insert("web_search".to_owned(), 2);
        agent.maybe_inject_stuck_nudge();
        assert_eq!(
            agent.messages().len(),
            initial_len,
            "no nudge below threshold"
        );

        // Simulate 3 failures — should trigger
        agent.tool_failure_counts.insert("web_search".to_owned(), 3);
        agent.maybe_inject_stuck_nudge();
        assert_eq!(
            agent.messages().len(),
            initial_len + 1,
            "nudge injected at threshold"
        );
        let last = agent.messages().last().unwrap();
        assert!(last.content_text().unwrap().contains("Stuck loop"));
        assert!(last.content_text().unwrap().contains("web_search"));

        // Counter should be cleared, so next check shouldn't nudge again
        agent.maybe_inject_stuck_nudge();
        assert_eq!(agent.messages().len(), initial_len + 1, "no double nudge");
    }

    #[test]
    fn tool_success_resets_failure_count() {
        let mut agent = test_agent();

        // Track 2 failures
        agent.tool_failure_counts.insert("shell_exec".to_owned(), 2);
        assert_eq!(agent.tool_failure_counts.get("shell_exec"), Some(&2));

        // A success should clear the counter
        agent.tool_failure_counts.remove("shell_exec");
        assert!(agent.tool_failure_counts.get("shell_exec").is_none());
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
    async fn execute_tool_call_suggests_alternatives_for_missing_tool() {
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
        )
        .await;

        // ToolNotFound is now a recoverable error with a helpful message
        let (msg, _) = result.expect("should be recoverable");
        assert!(msg.contains("not found"));
    }

    #[tokio::test]
    async fn execute_tool_call_suggests_similar_name() {
        let agent = test_agent();
        // "ech" is close to "echo" — should suggest it
        let result = execute_single_tool(
            &agent.tools,
            &agent.subagent_spawner,
            &ToolCallEntry {
                id: "tool-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "ech".to_owned(),
                    arguments: "{}".to_owned(),
                },
            },
        )
        .await;

        let (msg, _) = result.expect("should be recoverable");
        assert!(msg.contains("Did you mean"));
        assert!(msg.contains("echo"));
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
    async fn malformed_arguments_produce_soft_error_not_hard_failure() {
        let agent = test_agent();
        let result = execute_single_tool(
            &agent.tools,
            &agent.subagent_spawner,
            &ToolCallEntry {
                id: "tool-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "echo".to_owned(),
                    arguments: "not valid json at all".to_owned(),
                },
            },
        )
        .await;

        // Malformed JSON should be a soft error, not a hard failure
        let (msg, requires_input) = result.expect("malformed args should be recoverable");
        assert!(msg.starts_with("Error:"), "should start with Error: prefix");
        assert!(
            msg.contains("invalid arguments"),
            "should mention invalid arguments: {msg}"
        );
        assert!(!requires_input);
    }

    #[tokio::test]
    async fn non_object_arguments_produce_soft_error() {
        let agent = test_agent();
        let result = execute_single_tool(
            &agent.tools,
            &agent.subagent_spawner,
            &ToolCallEntry {
                id: "tool-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "echo".to_owned(),
                    arguments: "[1,2,3]".to_owned(),
                },
            },
        )
        .await;

        let (msg, _) = result.expect("non-object JSON should be recoverable");
        assert!(msg.starts_with("Error:"));
        assert!(msg.contains("invalid arguments"));
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

        let results =
            execute_tool_calls_parallel(&agent.tools, &agent.subagent_spawner, &tool_calls, 4, 120)
                .await
                .expect("parallel execution should succeed");

        assert_eq!(results.len(), 2);
        assert!(
            results[0].0.contains("first"),
            "first result should contain 'first'"
        );
        assert!(
            results[1].0.contains("second"),
            "second result should contain 'second'"
        );
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

        let results =
            execute_tool_calls_parallel(&agent.tools, &agent.subagent_spawner, &tool_calls, 4, 120)
                .await
                .expect("single-item parallel should succeed");

        assert_eq!(results.len(), 1);
        assert!(results[0].0.contains("solo"));
    }

    #[tokio::test]
    async fn parallel_tool_execution_recovers_from_missing_tool() {
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

        let result =
            execute_tool_calls_parallel(&agent.tools, &agent.subagent_spawner, &tool_calls, 4, 120)
                .await;

        // ToolNotFound is now recoverable, so the parallel execution should succeed
        let results = result.expect("should recover from ToolNotFound");
        assert_eq!(results.len(), 2);
        assert!(results[0].0.contains("ok")); // echo succeeded
        assert!(results[1].0.contains("not found")); // helpful error message
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
            provider_backend: "openai".to_owned(),
        });

        let mut agent = AgentLoop::with_history(
            client,
            tools,
            AgentLoopConfig {
                system_prompt: Some("system".to_owned()),
                max_context_messages: Some(3),
                ..AgentLoopConfig::default()
            },
            HookRunner::default(),
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
        assert_eq!(agent.messages()[1].content_text(), Some("msg2"));
        assert_eq!(agent.messages()[2].content_text(), Some("reply2"));
        assert_eq!(agent.messages()[3].content_text(), Some("msg3"));
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
            provider_backend: "openai".to_owned(),
        });

        let mut agent = AgentLoop::with_history(
            client,
            tools,
            AgentLoopConfig {
                system_prompt: Some("system".to_owned()),
                max_context_messages: Some(10),
                ..AgentLoopConfig::default()
            },
            HookRunner::default(),
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
        assert_eq!(
            agent.active_client().endpoint(),
            "http://localhost:8000/v1/chat/completions"
        );
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
        agent
            .messages
            .push(ChatMessage::tool_result("call-1", "result"));
        assert_eq!(
            agent.active_client().endpoint(),
            "http://localhost:9999/v1/chat/completions"
        );
    }

    #[test]
    fn active_client_falls_back_when_no_tool_client() {
        let mut agent = test_agent();

        // No tool client set — should always use primary
        agent
            .messages
            .push(ChatMessage::tool_result("call-1", "result"));
        assert_eq!(
            agent.active_client().endpoint(),
            "http://localhost:8000/v1/chat/completions"
        );
    }

    #[test]
    fn record_usage_tracks_cost() {
        let mut agent = test_agent();
        agent
            .record_usage(1, 1000, 500)
            .expect("should succeed without budget");
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
            provider_backend: "openai".to_owned(),
        });

        let mut agent = AgentLoop::new(
            client,
            tools,
            AgentLoopConfig {
                budget_limit: Some(0.001), // very tight budget
                ..AgentLoopConfig::default()
            },
            HookRunner::default(),
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

    // --- AgentHooks tests ---

    #[test]
    fn noop_hooks_compiles_and_runs() {
        let hooks = NoopHooks;
        hooks.on_turn_start("session-1", "hello");
        hooks.on_turn_end(
            "session-1",
            &AgentResult {
                response: "hi".to_owned(),
                turns_used: 1,
                tool_calls_made: 0,
                finished_naturally: true,
                total_input_tokens: 10,
                total_output_tokens: 5,
                estimated_cost: None,
                pending_clarification: None,
            },
        );
        hooks.on_tool_call_start("session-1", "shell");
        hooks.on_tool_call_end("session-1", "shell", true, 100);
        hooks.on_llm_request("session-1", "gpt-4", 1);
        hooks.on_llm_response("session-1", "gpt-4", 100, 50);
        hooks.on_context_prune("session-1", 20, 10);
        hooks.on_stuck_loop("session-1", "shell", 3);
    }

    #[test]
    fn custom_hooks_can_capture_events() {
        use std::sync::Mutex;

        struct CapturingHooks {
            events: Mutex<Vec<String>>,
        }

        impl AgentHooks for CapturingHooks {
            fn on_turn_start(&self, session_id: &str, message: &str) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("turn_start:{session_id}:{message}"));
            }
            fn on_tool_call_end(
                &self,
                _session_id: &str,
                tool_name: &str,
                success: bool,
                duration_ms: u64,
            ) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("tool_end:{tool_name}:{success}:{duration_ms}ms"));
            }
        }

        let hooks = CapturingHooks {
            events: Mutex::new(Vec::new()),
        };

        hooks.on_turn_start("s1", "hello");
        hooks.on_tool_call_end("s1", "shell", true, 42);
        hooks.on_tool_call_end("s1", "patch", false, 200);

        let events = hooks.events.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], "turn_start:s1:hello");
        assert_eq!(events[1], "tool_end:shell:true:42ms");
        assert_eq!(events[2], "tool_end:patch:false:200ms");
    }

    #[test]
    fn agent_loop_defaults_to_noop_hooks() {
        let agent = test_agent();
        // The agent should have NoopHooks by default — just verify it doesn't panic
        agent.hooks.on_turn_start("test", "verify hooks work");
    }

    #[test]
    fn agent_loop_accepts_custom_hooks() {
        let mut agent = test_agent();
        let hooks = Arc::new(NoopHooks);
        agent.set_hooks(hooks);
        // Should compile and not panic
        agent.hooks.on_turn_start("test", "custom hooks set");
    }

    #[tokio::test]
    async fn shell_hooks_fire_for_successful_turn() {
        let response = serde_json::json!({
            "id": "cmpl-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "done"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        })
        .to_string();
        let endpoint = start_mock_server(vec![response]);
        let mut agent = test_agent_with_endpoint(
            endpoint,
            vec![
                HookConfig {
                    event: HookEvent::PreTurn,
                    command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
                    timeout_ms: 1000,
                    enabled: true,
                },
                HookConfig {
                    event: HookEvent::PostTurn,
                    command: "echo post-turn".to_owned(),
                    timeout_ms: 1000,
                    enabled: true,
                },
                HookConfig {
                    event: HookEvent::OnComplete,
                    command: "echo complete".to_owned(),
                    timeout_ms: 1000,
                    enabled: true,
                },
            ],
        );

        let result = agent.run_turn("hello").await.expect("turn should succeed");
        assert_eq!(result.response, "done");

        let events = agent
            .hook_results()
            .iter()
            .map(|result| result.event.clone())
            .collect::<Vec<_>>();
        assert!(events.contains(&HookEvent::PreTurn));
        assert!(events.contains(&HookEvent::PostTurn));
        assert!(events.contains(&HookEvent::OnComplete));
        assert!(agent.hook_results()[0]
            .stdout
            .contains("\"user_message\":\"hello\""));
    }

    #[tokio::test]
    async fn shell_hooks_fire_for_tool_calls() {
        let first = serde_json::json!({
            "id": "cmpl-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "echo",
                            "arguments": "{\"message\":\"hi\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        })
        .to_string();
        let second = serde_json::json!({
            "id": "cmpl-2",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        })
        .to_string();

        let endpoint = start_mock_server(vec![first, second]);
        let mut agent = test_agent_with_endpoint(
            endpoint,
            vec![
                HookConfig {
                    event: HookEvent::PreToolCall,
                    command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
                    timeout_ms: 1000,
                    enabled: true,
                },
                HookConfig {
                    event: HookEvent::PostToolCall,
                    command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
                    timeout_ms: 1000,
                    enabled: true,
                },
            ],
        );

        let result = agent
            .run_turn("use a tool")
            .await
            .expect("turn should succeed");
        assert_eq!(result.response, "final");

        let pre = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::PreToolCall)
            .expect("pre tool hook");
        let post = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::PostToolCall)
            .expect("post tool hook");

        assert!(pre.stdout.contains("\"tool_name\":\"echo\""));
        assert!(post.stdout.contains("\"tool_name\":\"echo\""));
        assert!(post.stdout.contains("\"success\":true"));
    }

    #[tokio::test]
    async fn shell_hooks_fire_on_error() {
        let mut agent = test_agent_with_endpoint(
            "http://127.0.0.1:1/v1".to_owned(),
            vec![
                HookConfig {
                    event: HookEvent::PreTurn,
                    command: "echo pre".to_owned(),
                    timeout_ms: 1000,
                    enabled: true,
                },
                HookConfig {
                    event: HookEvent::OnError,
                    command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
                    timeout_ms: 1000,
                    enabled: true,
                },
            ],
        );

        let result = agent.run_turn("hello").await;
        assert!(result.is_err());

        let error_hook = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::OnError)
            .expect("error hook");
        assert!(error_hook.stdout.contains("\"stage\":\"llm_request\""));
    }

    // --- Iteration budget tests ---

    #[test]
    fn remaining_iterations_none_when_unlimited() {
        let agent = test_agent();
        assert!(agent.remaining_iterations().is_none());
        assert_eq!(agent.iterations_used(), 0);
    }

    #[test]
    fn remaining_iterations_tracks_budget() {
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
                    model: "test-model".to_owned(),
                    base_url: None,
                },
                initial_events: Vec::new(),
            },
            data_dir: "/tmp".to_owned(),
            database_path: "/tmp/genesis.db".to_owned(),
            max_concurrency: 4,
            allow_destructive_tools: false,
            provider_backend: "openai".to_owned(),
        });

        let mut agent = AgentLoop::new(
            client,
            tools,
            AgentLoopConfig {
                max_iterations: Some(10),
                ..AgentLoopConfig::default()
            },
            HookRunner::default(),
        );

        assert_eq!(agent.remaining_iterations(), Some(10));
        assert_eq!(agent.iterations_used(), 0);

        // Simulate consuming iterations
        agent.iterations_used = 7;
        assert_eq!(agent.remaining_iterations(), Some(3));
        assert_eq!(agent.iterations_used(), 7);

        // At the limit
        agent.iterations_used = 10;
        assert_eq!(agent.remaining_iterations(), Some(0));

        // Past the limit (saturating_sub prevents underflow)
        agent.iterations_used = 15;
        assert_eq!(agent.remaining_iterations(), Some(0));
    }

    #[tokio::test]
    async fn iteration_budget_stops_loop_when_exhausted() {
        // Set up two responses: first does a tool call, second gives text.
        // But with max_iterations=1, the agent should stop after one LLM call
        // (the tool-call response), and the second call will report exhaustion.
        let first = serde_json::json!({
            "id": "cmpl-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "echo",
                            "arguments": "{\"message\":\"hi\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        })
        .to_string();

        let endpoint = start_mock_server(vec![first]);

        let provider = genesis_provider::ResolvedProvider {
            base_url: endpoint,
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
                    model: "test-model".to_owned(),
                    base_url: None,
                },
                initial_events: Vec::new(),
            },
            data_dir: "/tmp".to_owned(),
            database_path: "/tmp/genesis.db".to_owned(),
            max_concurrency: 4,
            allow_destructive_tools: false,
            provider_backend: "openai".to_owned(),
        });

        let mut agent = AgentLoop::new(
            client,
            tools,
            AgentLoopConfig {
                max_iterations: Some(1),
                ..AgentLoopConfig::default()
            },
            HookRunner::default(),
        );

        let result = agent
            .run_turn("use a tool")
            .await
            .expect("should return result, not error");
        // After 1 iteration (tool call), the loop tries to iterate again but
        // iteration budget is exhausted, so it returns gracefully.
        assert!(!result.finished_naturally);
        assert!(result.response.contains("Iteration budget exhausted"));
        assert_eq!(agent.iterations_used(), 1);
        assert_eq!(agent.remaining_iterations(), Some(0));
    }

    #[test]
    fn iteration_budget_default_is_none() {
        let config = AgentLoopConfig::default();
        assert!(config.max_iterations.is_none());
    }

    #[test]
    fn summarize_args_empty_input() {
        assert_eq!(summarize_args(""), "");
        assert_eq!(summarize_args("{}"), "");
    }

    #[test]
    fn summarize_args_simple_object() {
        let args = r#"{"command":"git status"}"#;
        assert_eq!(summarize_args(args), "command: git status");
    }

    #[test]
    fn summarize_args_truncates_long_value() {
        let args =
            r#"{"path":"/very/long/path/that/definitely/exceeds/forty/characters/limit/here"}"#;
        let summary = summarize_args(args);
        assert!(summary.len() <= 40);
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn summarize_args_non_string_value() {
        let args = r#"{"count":42}"#;
        assert_eq!(summarize_args(args), "count: 42");
    }

    #[test]
    fn summarize_args_invalid_json_fallback() {
        let args = "not json";
        let summary = summarize_args(args);
        assert_eq!(summary, "not json");
    }

    #[test]
    fn mask_old_tool_outputs_replaces_long_content() {
        let mut agent = test_agent();

        let long_output = "x".repeat(300);
        let short_output = "short result";

        // Build a conversation with tool results:
        // [0] system
        // [1] user
        // [2] assistant (tool call)
        // [3] tool result (long — should be masked)
        // [4] assistant
        // [5] user
        // [6] assistant (tool call)
        // [7] tool result (short — should NOT be masked even though old)
        // [8] assistant
        // --- last 8 messages boundary: messages 9..16 are protected ---
        // [9] user
        // [10] assistant (tool call)
        // [11] tool result (long — protected, should NOT be masked)
        // [12] assistant
        // [13] user
        // [14] assistant (tool call)
        // [15] tool result (long — protected, should NOT be masked)
        // [16] assistant

        // Old region (will be checked for masking)
        agent.messages.push(ChatMessage::user("do something"));
        agent.messages.push(ChatMessage::assistant_with_tool_calls(
            None,
            vec![ToolCallEntry {
                id: "tc1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "shell".to_owned(),
                    arguments: "{}".to_owned(),
                },
            }],
        ));
        agent
            .messages
            .push(ChatMessage::tool_result("tc1", &long_output));
        agent.messages.push(ChatMessage::assistant("got it"));
        agent.messages.push(ChatMessage::user("more"));
        agent.messages.push(ChatMessage::assistant_with_tool_calls(
            None,
            vec![ToolCallEntry {
                id: "tc2".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "shell".to_owned(),
                    arguments: "{}".to_owned(),
                },
            }],
        ));
        agent
            .messages
            .push(ChatMessage::tool_result("tc2", short_output));
        agent.messages.push(ChatMessage::assistant("ok"));

        // Protected region (last 8 messages)
        agent.messages.push(ChatMessage::user("recent"));
        agent.messages.push(ChatMessage::assistant_with_tool_calls(
            None,
            vec![ToolCallEntry {
                id: "tc3".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "shell".to_owned(),
                    arguments: "{}".to_owned(),
                },
            }],
        ));
        agent
            .messages
            .push(ChatMessage::tool_result("tc3", &long_output));
        agent.messages.push(ChatMessage::assistant("noted"));
        agent.messages.push(ChatMessage::user("last one"));
        agent.messages.push(ChatMessage::assistant_with_tool_calls(
            None,
            vec![ToolCallEntry {
                id: "tc4".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "shell".to_owned(),
                    arguments: "{}".to_owned(),
                },
            }],
        ));
        agent
            .messages
            .push(ChatMessage::tool_result("tc4", &long_output));
        agent.messages.push(ChatMessage::assistant("done"));

        assert_eq!(agent.messages().len(), 17); // 1 system + 16

        agent.mask_old_tool_outputs();

        // Message [3] (tool, long, old) → masked
        assert_eq!(
            agent.messages()[3].content_text().unwrap(),
            "[Tool output masked — see preceding tool call for context]"
        );

        // Message [7] (tool, short, old) → NOT masked (below threshold)
        assert_eq!(agent.messages()[7].content_text().unwrap(), short_output,);

        // Message [11] (tool, long, recent/protected) → NOT masked
        assert_eq!(
            agent.messages()[11].content_text().unwrap(),
            long_output.as_str(),
        );

        // Message [15] (tool, long, recent/protected) → NOT masked
        assert_eq!(
            agent.messages()[15].content_text().unwrap(),
            long_output.as_str(),
        );

        // System prompt untouched
        assert_eq!(agent.messages()[0].role, "system");
    }
}
