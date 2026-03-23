use genesis_provider::ProviderError;
use genesis_tools::ToolError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub(crate) const DEFAULT_MAX_TURNS: usize = genesis_config::defaults::agent::DEFAULT_MAX_TURNS;

/// Default number of tool calls between memory consolidation nudges.
pub(crate) const DEFAULT_MEMORY_NUDGE_INTERVAL: usize = 15;

/// The memory nudge message injected as a system message.
pub(crate) const MEMORY_NUDGE: &str = "\
[Memory consolidation reminder] You've been working for a while. \
Consider saving any useful observations, patterns, or user preferences \
you've noticed using `memory_create`. Focus on durable insights that \
would be valuable in future sessions — not session-specific details.";

/// Number of tool calls in a single turn that triggers a skill creation nudge.
pub(crate) const SKILL_CREATION_THRESHOLD: usize = 8;

/// Number of consecutive failures for the same tool before injecting a
/// "try a different approach" nudge.
pub(crate) const STUCK_LOOP_THRESHOLD: usize =
    genesis_config::defaults::retry::STUCK_LOOP_THRESHOLD;

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
    /// Core tool set. When set, only these tools (plus any discovered via
    /// `find_tools`) are sent in the LLM request. Other tools remain
    /// available in the registry and can be discovered at runtime.
    /// Reduces input tokens by 85-96% for large tool registries.
    pub core_tools: Option<Vec<String>>,
    /// Adaptive model routing configuration. When enabled, the router
    /// classifies each turn's complexity and selects the appropriate model
    /// tier (cheap/mid/top) to optimize cost while maintaining quality.
    pub routing: Option<genesis_config::RoutingConfig>,
    /// Compiled tool policy for permission scoping. When set, tool calls
    /// are checked against allow/deny rules before execution.
    pub tool_policy: Option<crate::tool_policy::ToolPolicy>,
}

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
            core_tools: None,
            routing: None,
            tool_policy: None,
        }
    }
}

/// Default core tool set — the minimum set of tools always available.
/// When `core_tools` is `None`, all tools are sent (backwards compatible).
/// When `core_tools` is `Some(vec![])`, this default set is used.
pub const DEFAULT_CORE_TOOLS: &[&str] = &[
    "shell_exec",
    "read_file",
    "write_file",
    "patch",
    "list_dir",
    "list_tree",
    "search_files",
    "find_tools",
    "memory_recall",
    "clarify",
];

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),
    #[error("failed to parse tool call arguments: {0}")]
    ArgumentParse(String),
    #[error("agent loop exceeded maximum of {0} turns — increase `runtime.max_turns` or set GENESIS_MAX_TURNS env var to allow more")]
    MaxTurnsExceeded(usize),
    #[error("session budget exceeded: ${used:.4} spent of ${limit:.4} limit — increase `runtime.budget_limit` in config, set GENESIS_BUDGET_LIMIT env var, or use 0 for unlimited")]
    BudgetExceeded { used: f64, limit: f64 },
    #[error("iteration budget exhausted: {used}/{limit} iterations used — increase `runtime.max_iterations` to allow more")]
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

pub(crate) fn format_blocked_reasons(result: &crate::guardrails::GuardrailResult) -> String {
    result
        .violations
        .iter()
        .filter(|v| v.action == crate::guardrails::ViolationAction::Block)
        .map(|v| v.message.as_str())
        .collect::<Vec<&str>>()
        .join("; ")
}
