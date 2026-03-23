mod compression;
mod hooks;
mod streaming;
mod tools;
mod types;

// Re-export the full public API (unchanged).
pub use types::{
    AgentError, AgentHooks, AgentLoopConfig, AgentResult, NoopHooks, StreamEvent, SubagentSpawner,
    DEFAULT_CORE_TOOLS,
};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use genesis_provider::{
    ChatClient, ChatCompletionRequest, ChatMessage, ChatTool, MessageContent, ProviderError,
    ToolCallEntry,
};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::cost::{BudgetStatus, SessionCost};
use crate::hooks::{HookEvent, HookResult, HookRunner};
use crate::nudge::SKILL_CREATION_NUDGE;
use crate::sanitize;
use crate::trajectory::TrajectoryRecorder;
use crate::ToolRuntime;
use genesis_lua::hooks::PreHookOutcome;
use genesis_lua::LuaRuntime;

use tools::execute_tool_calls_parallel;
use types::{format_blocked_reasons, MEMORY_NUDGE, SKILL_CREATION_THRESHOLD};

/// The core agent loop that wires provider (LLM) and tool execution together.
///
/// Flow: user message → LLM → [tool_calls → execute → LLM]* → final text
pub struct AgentLoop {
    pub(crate) client: ChatClient,
    /// Optional cheaper client for tool-calling turns. When set, turns that
    /// follow tool results use this client while turns following user messages
    /// use the primary `client`.
    pub(crate) tool_client: Option<ChatClient>,
    /// Fallback clients tried in order when the primary (or active) client fails.
    /// Each entry pairs a client with its per-provider timeout.
    pub(crate) fallback_clients: Vec<(ChatClient, Duration)>,
    pub(crate) tools: ToolRuntime,
    pub(crate) config: AgentLoopConfig,
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) subagent_spawner: Option<Arc<dyn SubagentSpawner>>,
    pub(crate) hooks: Arc<dyn AgentHooks>,
    pub(crate) hook_runner: HookRunner,
    pub(crate) hook_results: Vec<HookResult>,
    pub(crate) lua_runtime: Option<Arc<LuaRuntime>>,
    pub(crate) cost: SessionCost,
    pub(crate) trajectory: TrajectoryRecorder,
    /// Last reported prompt token count from the API. Used for token-aware
    /// context compression.
    pub(crate) last_prompt_tokens: u32,
    /// Tracks consecutive failures per tool name. When a tool fails
    /// `config.stuck_loop_threshold` times in a row, a system nudge tells the
    /// LLM to try a different approach. Cleared at the start of each user turn.
    pub(crate) tool_failure_counts: HashMap<String, usize>,
    /// Total number of LLM iterations consumed across all user turns.
    /// Checked against `config.max_iterations` at each loop boundary.
    pub(crate) iterations_used: usize,
    /// Cancellation flag. When set to `true`, the loop exits gracefully
    /// at the next turn boundary.
    pub(crate) cancelled: Arc<AtomicBool>,
    /// Optional response cache for deduplicating identical LLM calls.
    pub(crate) response_cache: Option<genesis_storage::ResponseCacheStore>,
    pub(crate) cache_hits: u32,
    pub(crate) cache_misses: u32,
    /// Pre-compiled guardrails (avoids recompiling regexes on every check).
    pub(crate) compiled_guardrails: Option<crate::guardrails::CompiledGuardrails>,
    /// Tools discovered via `find_tools` during this session. These are added
    /// to the core set for subsequent LLM requests.
    pub(crate) discovered_tools: HashSet<String>,
    /// Whether a stuck-loop nudge has been sent during this user turn.
    /// Cleared at the start of each new turn so the agent gets a fresh chance.
    pub(crate) nudge_sent: bool,
}

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
    pub(crate) fn session_id_str(&self) -> &str {
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
            lua_runtime: None,
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
            discovered_tools: HashSet::new(),
            nudge_sent: false,
        }
    }

    /// Filter tool definitions to the core set + discovered tools.
    /// Takes a reference to avoid cloning the entire Vec; only clones kept items.
    pub(crate) fn filter_tool_defs(&self, all_defs: &[ChatTool]) -> Vec<ChatTool> {
        let core = match &self.config.core_tools {
            Some(core) if !core.is_empty() => core,
            Some(_) => {
                // Empty list = use defaults
                let defaults: HashSet<&str> = DEFAULT_CORE_TOOLS.iter().copied().collect();
                return all_defs
                    .iter()
                    .filter(|t| {
                        defaults.contains(t.function.name.as_str())
                            || self.discovered_tools.contains(&t.function.name)
                    })
                    .cloned()
                    .collect();
            }
            None => return all_defs.to_vec(), // No core set = send everything
        };

        let mut core_set: HashSet<&str> = core.iter().map(String::as_str).collect();
        // Always include find_tools when filtering is active so discovery works.
        core_set.insert("find_tools");
        all_defs
            .iter()
            .filter(|t| {
                core_set.contains(t.function.name.as_str())
                    || self.discovered_tools.contains(&t.function.name)
            })
            .cloned()
            .collect()
    }

    /// Add a tool name to the discovered set so it's included in future requests.
    pub(crate) fn discover_tool(&mut self, name: &str) {
        if self.discovered_tools.insert(name.to_owned()) {
            debug!(tool = name, "tool discovered and added to active set");
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
    /// Each entry pairs a client with its per-provider timeout.
    pub fn set_fallback_clients(&mut self, clients: Vec<(ChatClient, Duration)>) {
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

    /// Attach the session-scoped Lua runtime used for hook middleware.
    pub fn set_lua_runtime(&mut self, runtime: Arc<LuaRuntime>) {
        self.lua_runtime = Some(runtime);
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
    pub(crate) fn resolve_parser(
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
    pub(crate) fn apply_tool_call_parser(
        &self,
        response: &mut genesis_provider::ChatCompletionResponse,
        model: &str,
    ) {
        if let Some(parser) = self.resolve_parser(model) {
            genesis_provider::parsers::normalize_response(response, parser.as_ref());
        }
    }

    /// Inject reasoning effort into the request's extra_body if configured.
    pub(crate) fn inject_reasoning_effort(&self, request: &mut ChatCompletionRequest) {
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

    /// Apply adaptive routing to the request by overriding the model based on
    /// context classification. Returns the selected tier name for logging.
    pub(crate) fn apply_routing(
        &self,
        request: &mut ChatCompletionRequest,
        user_message: &str,
        tool_calls_made: usize,
        turns_used: usize,
        had_failure: bool,
    ) -> Option<&'static str> {
        let routing = self.config.routing.as_ref()?;
        if !routing.enabled {
            return None;
        }

        let default = crate::routing::Tier::from_str_or_mid(&routing.default_tier);
        let tier = crate::routing::classify(
            user_message,
            tool_calls_made,
            turns_used,
            had_failure,
            default,
        );
        let selected = crate::routing::model_for_tier(
            tier,
            routing.cheap_model.as_deref(),
            routing.mid_model.as_deref(),
            routing.top_model.as_deref(),
            self.client.model(),
        );

        // Override the model on the request (the client will use it instead
        // of its default when the model field is non-empty).
        request.model = selected.to_owned();

        info!(
            tier = tier.as_str(),
            model = selected,
            "adaptive routing selected model"
        );

        Some(tier.as_str())
    }

    /// Pick the right client for the current turn. Uses the tool client when
    /// the most recent message is a tool result (the agent is processing tool
    /// output and will likely make more tool calls). Falls back to the primary
    /// client otherwise.
    pub(crate) fn active_client(&self) -> &ChatClient {
        if let Some(ref tool_client) = self.tool_client {
            let last_role = self.messages.last().map(|m| m.role.as_str());
            if last_role == Some("tool") {
                return tool_client;
            }
        }
        &self.client
    }

    /// Try a blocking completion against the active client, falling back to
    /// each fallback client **sequentially** (in config order) if the primary
    /// fails. Each fallback is capped by its per-provider timeout so a slow
    /// provider does not block the remaining fallbacks indefinitely.
    ///
    /// The request is only cloned when fallback providers are actually tried,
    /// avoiding an expensive clone in the common (no-fallback) success path.
    pub(crate) async fn complete_with_failover(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<(genesis_provider::ChatCompletionResponse, String), ProviderError> {
        let client = self.active_client().clone();
        let model = client.model().to_owned();

        if self.fallback_clients.is_empty() {
            // Fast path: no fallbacks configured, skip the clone entirely.
            return client
                .complete(request)
                .await
                .map(|response| (response, model));
        }

        // Fallbacks exist — clone for the primary attempt, keep original for retries.
        match client.complete(request.clone()).await {
            Ok(response) => return Ok((response, model)),
            Err(err) => {
                warn!(
                    model = model.as_str(),
                    error = %err,
                    fallback_count = self.fallback_clients.len(),
                    "primary provider failed, trying fallbacks in order"
                );
            }
        }

        // Try each fallback provider sequentially, respecting config order.
        let mut attempted = 1_usize; // primary was already attempted
        for (i, (fallback, timeout)) in self.fallback_clients.iter().enumerate() {
            let fb_model = fallback.model().to_owned();
            attempted += 1;
            match tokio::time::timeout(*timeout, fallback.complete(request.clone())).await {
                Ok(Ok(response)) => {
                    info!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        "fallback provider succeeded"
                    );
                    return Ok((response, fb_model));
                }
                Ok(Err(err)) => {
                    warn!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        error = %err,
                        "fallback provider failed"
                    );
                }
                Err(_elapsed) => {
                    warn!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        timeout_secs = timeout.as_secs(),
                        "fallback provider timed out"
                    );
                }
            }
        }

        warn!("all {attempted} providers failed (primary + fallbacks)");
        Err(ProviderError::AllProvidersFailed { count: attempted })
    }

    /// Try a streaming completion against the active client, falling back to
    /// each fallback client **sequentially** (in config order) if the primary
    /// fails to connect. Each fallback is capped by its per-provider timeout.
    ///
    /// The request is only cloned when fallback providers are actually tried,
    /// avoiding an expensive clone in the common (no-fallback) success path.
    pub(crate) async fn complete_stream_with_failover(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<(genesis_provider::ChatCompletionChunkStream, String), ProviderError> {
        let client = self.active_client().clone();
        let model = client.model().to_owned();

        if self.fallback_clients.is_empty() {
            // Fast path: no fallbacks configured, skip the clone entirely.
            return client
                .complete_stream(request)
                .await
                .map(|stream| (stream, model));
        }

        // Fallbacks exist — clone for the primary attempt, keep original for retries.
        match client.complete_stream(request.clone()).await {
            Ok(stream) => return Ok((stream, model)),
            Err(err) => {
                warn!(
                    model = model.as_str(),
                    error = %err,
                    fallback_count = self.fallback_clients.len(),
                    "primary provider stream failed, trying fallbacks in order"
                );
            }
        }

        let mut attempted = 1_usize; // primary was already attempted
        for (i, (fallback, timeout)) in self.fallback_clients.iter().enumerate() {
            let fb_model = fallback.model().to_owned();
            attempted += 1;
            match tokio::time::timeout(*timeout, fallback.complete_stream(request.clone())).await {
                Ok(Ok(stream)) => {
                    info!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        "fallback provider stream succeeded"
                    );
                    return Ok((stream, fb_model));
                }
                Ok(Err(err)) => {
                    warn!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        error = %err,
                        "fallback provider stream failed"
                    );
                }
                Err(_elapsed) => {
                    warn!(
                        fallback_index = i,
                        model = fb_model.as_str(),
                        timeout_secs = timeout.as_secs(),
                        "fallback provider stream timed out"
                    );
                }
            }
        }

        warn!("all {attempted} providers failed (primary + fallbacks)");
        Err(ProviderError::AllProvidersFailed { count: attempted })
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
        // Reset stuck-loop state at the start of each new user turn so
        // stale failure counts from a previous turn don't cause false positives.
        self.tool_failure_counts.clear();
        self.nudge_sent = false;

        // Record turn-level span attributes via tracing events rather than
        // holding a non-Send span guard across await points.
        info!(
            session_id = self.session_id_str(),
            image_count = images.len(),
            "agent.turn.start"
        );
        let hook_session = self.session_id_str().to_owned();
        let lua_pre_turn = self.run_lua_pre_turn(user_message);
        let user_message = match lua_pre_turn {
            PreHookOutcome::Allow(message) => {
                self.fire_shell_hooks(
                    HookEvent::PreTurn,
                    serde_json::json!({
                        "session_id": hook_session,
                        "user_message": message,
                        "image_count": images.len(),
                    }),
                );
                message
            }
            PreHookOutcome::Veto { reason } => {
                self.fire_shell_hooks(
                    HookEvent::PreTurn,
                    serde_json::json!({
                        "session_id": hook_session,
                        "user_message": user_message,
                        "image_count": images.len(),
                        "lua_vetoed": true,
                        "lua_reason": reason.as_deref(),
                    }),
                );
                let result = AgentResult {
                    response: format!(
                        "Your input was blocked by Lua hook: {}",
                        reason.unwrap_or_else(|| "request rejected".to_owned())
                    ),
                    turns_used: 0,
                    tool_calls_made: 0,
                    finished_naturally: true,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    estimated_cost: None,
                    pending_clarification: None,
                };
                return Ok(self.finalize_turn(&hook_session, result, false));
            }
        };

        // Run input guardrails if configured
        let user_message = if let Some(ref cg) = self.compiled_guardrails {
            let result = cg.check_input(&user_message);
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
                return Ok(self.finalize_turn(&hook_session, agent_result, false));
            }
            result.content
        } else {
            user_message.to_owned()
        };

        let user_message = if images.is_empty() {
            self.push_message_with_lua_hooks(&hook_session, ChatMessage::user(&user_message))
                .and_then(|message| message.content_text().map(str::to_owned))
                .unwrap_or_default()
        } else {
            self.push_message_with_lua_hooks(
                &hook_session,
                ChatMessage::user_with_images(user_message.clone(), images),
            )
            .and_then(|message| message.content_text().map(str::to_owned))
            .unwrap_or_default()
        };

        if !user_message.is_empty() {
            self.trajectory.record_user_message(&user_message);
        }

        // Fire turn-start hook
        self.hooks.on_turn_start(&hook_session, &user_message);

        let all_tool_defs: Vec<ChatTool> = self
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
            // When core set filtering is active, recompute the filtered set
            // each iteration (discovered set may grow). Otherwise clone all tools.
            let tool_defs = if self.config.core_tools.is_some() {
                self.filter_tool_defs(&all_tool_defs)
            } else {
                all_tool_defs.clone()
            };

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
                return Ok(self.finalize_turn(&hook_session, result, false));
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
                         The work so far has been saved. You can continue by sending another message, \
                         or increase the limit via `runtime.max_turns` in config or the GENESIS_MAX_TURNS env var.",
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
                return Ok(self.finalize_turn(&hook_session, result, false));
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
                    return Ok(self.finalize_turn(&hook_session, result, false));
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
            request.tools = tool_defs;
            request.temperature = self.config.temperature;
            request.max_tokens = self.config.max_tokens;
            request.thinking = self.config.thinking.clone();
            request.response_format = self.config.response_format.clone();
            self.inject_reasoning_effort(&mut request);

            // Adaptive routing: classify and override model if enabled.
            let had_failure = self.tool_failure_counts.values().any(|&c| c > 0);
            self.apply_routing(
                &mut request,
                &user_message,
                tool_calls_made,
                turns_used,
                had_failure,
            );

            // Check response cache before making an LLM call
            let cache_key = if self.response_cache.is_some()
                && self.config.cache.as_ref().is_some_and(|c| c.enabled)
            {
                let model_for_cache = if request.model.is_empty() {
                    self.active_client().model()
                } else {
                    &request.model
                };
                Some(self.compute_cache_key(model_for_cache, &request.tools))
            } else {
                None
            };

            let cached = cache_key.as_ref().and_then(|key| {
                self.response_cache
                    .as_ref()
                    .and_then(|cache| match cache.get(key) {
                        Ok(hit) => hit,
                        Err(e) => {
                            debug!(error = %e, "response cache lookup failed");
                            None
                        }
                    })
            });

            self.hooks
                .on_llm_request(&hook_session, self.active_client().model(), turns_used);
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
                    .and_then(|json| match serde_json::from_str(json) {
                        Ok(tc) => Some(tc),
                        Err(e) => {
                            debug!(error = %e, "failed to deserialize cached tool calls");
                            None
                        }
                    });
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
                let llm_started_at = std::time::Instant::now();
                let result = match self.complete_with_failover(request).await {
                    Ok(result) => result,
                    Err(err) => {
                        return Err(self.report_error(&hook_session, "llm_request", err.into()));
                    }
                };
                info!(
                    model = result.1.as_str(),
                    turn = turns_used,
                    latency_ms = llm_started_at.elapsed().as_millis() as u64,
                    "agent.llm_request.completed"
                );
                result
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
                    let tc_json =
                        choice.message.tool_calls.as_ref().and_then(
                            |tc| match serde_json::to_string(tc) {
                                Ok(s) => Some(s),
                                Err(e) => {
                                    debug!(error = %e, "failed to serialize tool calls for cache");
                                    None
                                }
                            },
                        );
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

            if let Some(usage) = &response.usage {
                // Log token usage for OTel / structured logging.
                info!(
                    model = active_model.as_str(),
                    turn = turns_used,
                    input_tokens = usage.prompt_tokens,
                    output_tokens = usage.completion_tokens,
                    "agent.llm_response"
                );
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
                    // Append the assistant message (with tool_calls) to history,
                    // preserving provider_metadata (e.g. reasoning blobs) for multi-turn continuity.
                    let mut msg = ChatMessage::assistant_with_tool_calls(
                        assistant_msg.content.clone(),
                        tool_calls.clone(),
                    );
                    msg.provider_metadata = assistant_msg.provider_metadata.clone();
                    if let Some(message) = self.push_message_with_lua_hooks(&hook_session, msg) {
                        if let Some(text) = message.content_text() {
                            if !text.is_empty() {
                                self.trajectory.record_assistant_message(text);
                            }
                        }
                    }

                    // Execute tool calls in parallel (up to max_concurrency).
                    tool_calls_made += tool_calls.len();
                    let (effective_tool_calls, veto_reasons) =
                        self.prepare_tool_calls(&hook_session, tool_calls, false);
                    let executable_tool_calls: Vec<ToolCallEntry> = effective_tool_calls
                        .iter()
                        .zip(veto_reasons.iter())
                        .filter(|(_, veto)| veto.is_none())
                        .map(|(tc, _)| tc.clone())
                        .collect();
                    let tool_start = Instant::now();
                    let executed_results = if executable_tool_calls.is_empty() {
                        Vec::new()
                    } else {
                        match execute_tool_calls_parallel(
                            &self.tools,
                            &self.subagent_spawner,
                            &executable_tool_calls,
                            self.config.max_concurrency,
                            self.config.tool_timeout_secs,
                            self.config.tool_policy.as_ref(),
                        )
                        .await
                        {
                            Ok(results) => results,
                            Err(err) => {
                                return Err(self.report_error(
                                    &hook_session,
                                    "tool_execution",
                                    err,
                                ));
                            }
                        }
                    };
                    let tool_elapsed_ms = tool_start.elapsed().as_millis() as u64;

                    let mut executed_results = executed_results.into_iter();
                    let mut clarification = None;
                    for (tc, veto_reason) in
                        effective_tool_calls.iter().zip(veto_reasons.into_iter())
                    {
                        let lua_vetoed = veto_reason.is_some();
                        let (mut result, requires_input) = match veto_reason {
                            Some(reason) => (
                                format!("Error: tool call blocked by Lua hook: {reason}"),
                                false,
                            ),
                            None => executed_results
                                .next()
                                .expect("executed tool results should align with allowed calls"),
                        };
                        result = sanitize::sanitize_credentials(&result);
                        // When find_tools returns results and core set filtering is active,
                        // extract discovered tool names and add them to the active set.
                        if self.config.core_tools.is_some()
                            && tc.function.name == "find_tools"
                            && !result.starts_with("Error:")
                            && !result.starts_with("No tools")
                        {
                            for line in result.lines() {
                                let trimmed = line.trim();
                                // find_tools output format: "  **tool_name** — description"
                                if let Some(rest) = trimmed.strip_prefix("**") {
                                    if let Some(name_end) = rest.find("**") {
                                        self.discover_tool(&rest[..name_end]);
                                    }
                                }
                            }
                        }

                        let success = !result.starts_with("Error:");
                        if success && self.config.core_tools.is_some() {
                            // Auto-discover tools called outside the core set.
                            self.discover_tool(&tc.function.name);
                        }
                        let result = self.run_lua_post_tool_call(&tc.function.name, &result);
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
                                "lua_vetoed": lua_vetoed,
                            }),
                        );
                        let result = self
                            .push_message_with_lua_hooks(
                                &hook_session,
                                ChatMessage::tool_result(&tc.id, result),
                            )
                            .and_then(|message| message.content_text().map(str::to_owned))
                            .unwrap_or_default();
                        self.trajectory
                            .record_tool_result(&tc.function.name, &result);
                        if requires_input {
                            clarification = Some(result.clone());
                        }
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
                        return Ok(self.finalize_turn(&hook_session, result, false));
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

            let mut msg = ChatMessage::assistant(&response_text);
            msg.provider_metadata = assistant_msg.provider_metadata.clone();
            let response_text = self
                .push_message_with_lua_hooks(&hook_session, msg)
                .and_then(|message| message.content_text().map(str::to_owned))
                .unwrap_or_default();
            if !response_text.is_empty() {
                self.trajectory.record_assistant_message(&response_text);
            }

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
            return Ok(self.finalize_turn(&hook_session, result, true));
        }
    }

    /// Inject a memory nudge system message if enough tool calls have
    /// accumulated since the last nudge.
    pub(crate) fn maybe_inject_memory_nudge(&mut self, tool_calls_made: usize) {
        if let Some(interval) = self.config.memory_nudge_interval {
            if interval > 0 && tool_calls_made > 0 && tool_calls_made.is_multiple_of(interval) {
                debug!(
                    tool_calls_made,
                    interval, "injecting memory consolidation nudge"
                );
                let hook_session = self.session_id_str().to_owned();
                let _ = self
                    .push_message_with_lua_hooks(&hook_session, ChatMessage::system(MEMORY_NUDGE));
            }
        }
    }

    /// Inject a skill creation nudge if the turn involved many tool calls,
    /// suggesting the agent save the procedure as a reusable skill.
    pub(crate) fn maybe_inject_skill_nudge(&mut self, tool_calls_made: usize) {
        if tool_calls_made >= SKILL_CREATION_THRESHOLD {
            debug!(
                tool_calls_made,
                threshold = SKILL_CREATION_THRESHOLD,
                "injecting skill creation nudge"
            );
            let hook_session = self.session_id_str().to_owned();
            let _ = self.push_message_with_lua_hooks(
                &hook_session,
                ChatMessage::system(SKILL_CREATION_NUDGE),
            );
        }
    }

    /// Check if any tool has failed too many times in a row and inject a
    /// system nudge telling the LLM to try a different approach.
    pub(crate) fn maybe_inject_stuck_nudge(&mut self) {
        let threshold = self.config.stuck_loop_threshold;
        let stuck_tools: Vec<String> = self
            .tool_failure_counts
            .iter()
            .filter(|(_, count)| **count >= threshold)
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
                threshold,
                "tool has repeated failures, injecting stuck-loop nudge",
            );
            self.hooks.on_stuck_loop(&hook_session, tool, count);
            // Reset the counter so we don't spam nudges
            self.tool_failure_counts.remove(tool);
        }

        self.nudge_sent = true;
        let tools_list = stuck_tools.join(", ");
        let nudge = format!(
            "[Stuck loop detected] The tool(s) {tools_list} have failed multiple times in a row. \
             Stop retrying the same approach. Consider: (1) using a different tool, \
             (2) modifying the arguments, (3) breaking the task into smaller steps, or \
             (4) asking the user for clarification."
        );
        let _ = self.push_message_with_lua_hooks(&hook_session, ChatMessage::system(&nudge));
    }

    /// Save the trajectory to disk if a trajectory directory is configured.
    pub(crate) fn save_trajectory(&self) {
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

    pub(crate) fn record_usage_with_model(
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookConfig;
    use genesis_lua::{LuaRuntime, LuaRuntimeConfig, LuaSessionContext};
    use genesis_provider::ChatCompletionChunk;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Read as IoRead, Write as IoWrite};
    use std::sync::{Arc, Mutex};

    use streaming::collect_stream_update;
    use tools::{execute_single_tool, parse_tool_arguments, summarize_args};

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
            approval_mode: genesis_config::ApprovalMode::default(),
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

    fn test_lua_runtime(script: &str) -> Arc<LuaRuntime> {
        test_lua_runtime_with_personality(script, None)
    }

    fn test_lua_runtime_with_personality(
        script: &str,
        personality: Option<&str>,
    ) -> Arc<LuaRuntime> {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        write_test_package_plugin_with_permissions(
            dir.path(),
            "hooks",
            script,
            &[
                "PreTurn",
                "PostTurn",
                "PreToolCall",
                "PostToolCall",
                "OnMessage",
                "OnError",
                "OnComplete",
                "OnPluginLoad",
            ],
            &[],
        )
        .expect("lua package plugin should write");

        let runtime = LuaRuntime::builder()
            .with_config(LuaRuntimeConfig {
                plugin_dir: dir.path().to_path_buf(),
                session: LuaSessionContext {
                    id: "session-hooks".to_owned(),
                    model: "test-model".to_owned(),
                    turn_count: 0,
                    total_tokens: 0,
                    platform: "cli".to_owned(),
                    personality: personality.map(str::to_owned),
                },
                disabled_plugins: Vec::new(),
                plugin_verbose: None,
                config_values: BTreeMap::new(),
                ..Default::default()
            })
            .build()
            .expect("lua runtime should build");
        Arc::new(runtime)
    }

    fn write_test_package_plugin_with_permissions(
        plugin_dir: &std::path::Path,
        name: &str,
        source: &str,
        hooks: &[&str],
        tools: &[&str],
    ) -> std::io::Result<()> {
        let package_dir = plugin_dir.join(name);
        fs::create_dir_all(&package_dir)?;
        fs::write(package_dir.join("init.lua"), source)?;

        let hooks_list = hooks
            .iter()
            .map(|hook| format!("\"{hook}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let tools_list = tools
            .iter()
            .map(|tool| format!("\"{tool}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let manifest = format!(
            r#"[plugin]
name = "{name}"
version = "0.1.0"

[permissions]
hooks = [{hooks_list}]
tools = [{tools_list}]
"#
        );
        fs::write(package_dir.join("plugin.toml"), manifest)
    }

    fn start_mock_server_with_capture(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        std::thread::spawn(move || {
            for body in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buffer = [0u8; 8192];
                let read = stream.read(&mut buffer).expect("read request");
                captured_clone
                    .lock()
                    .expect("capture mutex")
                    .push(String::from_utf8_lossy(&buffer[..read]).to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).expect("write");
                stream.flush().expect("flush");
            }
        });
        (format!("http://{addr}/v1"), captured)
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
            approval_mode: genesis_config::ApprovalMode::default(),
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
            provider_metadata: None,
        });

        assert_eq!(update.contents, vec!["hello".to_owned()]);
        assert_eq!(update.finish_reason.as_deref(), Some("stop"));
        assert!(update.tool_calls.is_empty());
        assert!(update.usage.is_none());
        assert!(update.provider_metadata.is_none());
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
            provider_metadata: None,
        });

        let usage = update.usage.expect("usage should be present");
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
    }

    #[test]
    fn agent_loop_config_has_sensible_defaults() {
        let config = AgentLoopConfig::default();
        assert_eq!(
            config.max_turns,
            genesis_config::defaults::agent::DEFAULT_MAX_TURNS
        );
        assert!(config.system_prompt.is_none());
        assert!(config.temperature.is_none());
        assert_eq!(config.memory_nudge_interval, Some(15));
    }

    #[test]
    fn collect_stream_update_captures_provider_metadata() {
        let meta = serde_json::json!({
            "codex_reasoning_items": [{"type": "reasoning", "id": "r_1"}]
        });
        let update = collect_stream_update(ChatCompletionChunk {
            id: "chunk-meta".to_owned(),
            choices: vec![],
            usage: None,
            provider_metadata: Some(meta.clone()),
        });

        assert!(update.provider_metadata.is_some());
        assert_eq!(update.provider_metadata.unwrap(), meta);
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

        // Default threshold is 5 — simulate 4 failures, not enough to trigger
        agent.tool_failure_counts.insert("web_search".to_owned(), 4);
        agent.maybe_inject_stuck_nudge();
        assert_eq!(
            agent.messages().len(),
            initial_len,
            "no nudge below threshold"
        );
        assert!(
            !agent.nudge_sent,
            "nudge_sent should be false below threshold"
        );

        // Simulate 5 failures — should trigger at default threshold
        agent.tool_failure_counts.insert("web_search".to_owned(), 5);
        agent.maybe_inject_stuck_nudge();
        assert_eq!(
            agent.messages().len(),
            initial_len + 1,
            "nudge injected at threshold"
        );
        let last = agent.messages().last().unwrap();
        assert!(last.content_text().unwrap().contains("Stuck loop"));
        assert!(last.content_text().unwrap().contains("web_search"));
        assert!(agent.nudge_sent, "nudge_sent should be true after nudge");

        // Counter should be cleared, so next check shouldn't nudge again
        agent.maybe_inject_stuck_nudge();
        assert_eq!(agent.messages().len(), initial_len + 1, "no double nudge");
    }

    #[test]
    fn stuck_loop_nudge_respects_custom_threshold() {
        let mut agent = test_agent();
        agent.config.stuck_loop_threshold = 3;
        let initial_len = agent.messages().len();

        // 2 failures — below custom threshold of 3
        agent.tool_failure_counts.insert("web_search".to_owned(), 2);
        agent.maybe_inject_stuck_nudge();
        assert_eq!(
            agent.messages().len(),
            initial_len,
            "no nudge below custom threshold"
        );

        // 3 failures — at custom threshold, should fire
        agent.tool_failure_counts.insert("web_search".to_owned(), 3);
        agent.maybe_inject_stuck_nudge();
        assert_eq!(
            agent.messages().len(),
            initial_len + 1,
            "nudge injected at custom threshold"
        );
        assert!(agent.nudge_sent);
    }

    #[test]
    fn tool_success_resets_failure_count() {
        let mut agent = test_agent();

        // Track 2 failures
        agent.tool_failure_counts.insert("shell_exec".to_owned(), 2);
        assert_eq!(agent.tool_failure_counts.get("shell_exec"), Some(&2));

        // A success should clear the counter
        agent.tool_failure_counts.remove("shell_exec");
        assert!(!agent.tool_failure_counts.contains_key("shell_exec"));
    }

    #[test]
    fn nudge_state_cleared_on_new_turn_setup() {
        let mut agent = test_agent();

        // Simulate a stuck-loop nudge was sent in a previous turn
        agent.tool_failure_counts.insert("web_search".to_owned(), 5);
        agent.maybe_inject_stuck_nudge();
        assert!(agent.nudge_sent);
        // Add some leftover failure counts that weren't cleared by the nudge
        agent.tool_failure_counts.insert("read_file".to_owned(), 2);
        assert!(!agent.tool_failure_counts.is_empty());

        // Simulate what run_turn_with_images does at the start of a new turn
        agent.tool_failure_counts.clear();
        agent.nudge_sent = false;

        assert!(
            agent.tool_failure_counts.is_empty(),
            "failure counts should be cleared on new turn"
        );
        assert!(
            !agent.nudge_sent,
            "nudge_sent should be cleared on new turn"
        );
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
        // "session_inf" is close to "session_info" — should suggest it
        let result = execute_single_tool(
            &agent.tools,
            &agent.subagent_spawner,
            &ToolCallEntry {
                id: "tool-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "session_inf".to_owned(),
                    arguments: "{}".to_owned(),
                },
            },
        )
        .await;

        let (msg, _) = result.expect("should be recoverable");
        assert!(msg.contains("Did you mean"));
        assert!(msg.contains("session_info"));
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
                    name: "read_file".to_owned(),
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
                    name: "session_info".to_owned(),
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
                    name: "session_info".to_owned(),
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
                    name: "session_info".to_owned(),
                    arguments: "{}".to_owned(),
                },
            },
            ToolCallEntry {
                id: "call-2".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "session_info".to_owned(),
                    arguments: "{}".to_owned(),
                },
            },
        ];

        let results = execute_tool_calls_parallel(
            &agent.tools,
            &agent.subagent_spawner,
            &tool_calls,
            4,
            120,
            None,
        )
        .await
        .expect("parallel execution should succeed");

        assert_eq!(results.len(), 2);
        assert!(
            results[0].0.contains("session="),
            "first result should contain session info"
        );
        assert!(
            results[1].0.contains("session="),
            "second result should contain session info"
        );
    }

    #[tokio::test]
    async fn parallel_tool_execution_single_item_fast_path() {
        let agent = test_agent();
        let tool_calls = vec![ToolCallEntry {
            id: "call-1".to_owned(),
            call_type: "function".to_owned(),
            function: genesis_provider::FunctionCall {
                name: "session_info".to_owned(),
                arguments: "{}".to_owned(),
            },
        }];

        let results = execute_tool_calls_parallel(
            &agent.tools,
            &agent.subagent_spawner,
            &tool_calls,
            4,
            120,
            None,
        )
        .await
        .expect("single-item parallel should succeed");

        assert_eq!(results.len(), 1);
        assert!(results[0].0.contains("session="));
    }

    #[tokio::test]
    async fn parallel_tool_execution_recovers_from_missing_tool() {
        let agent = test_agent();
        let tool_calls = vec![
            ToolCallEntry {
                id: "call-1".to_owned(),
                call_type: "function".to_owned(),
                function: genesis_provider::FunctionCall {
                    name: "session_info".to_owned(),
                    arguments: "{}".to_owned(),
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
            120,
            None,
        )
        .await;

        // ToolNotFound is now recoverable, so the parallel execution should succeed
        let results = result.expect("should recover from ToolNotFound");
        assert_eq!(results.len(), 2);
        assert!(results[0].0.contains("session=")); // session_info succeeded
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
            approval_mode: genesis_config::ApprovalMode::default(),
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
            approval_mode: genesis_config::ApprovalMode::default(),
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
            approval_mode: genesis_config::ApprovalMode::default(),
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
                            "name": "session_info",
                            "arguments": "{}"
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

        assert!(pre.stdout.contains("\"tool_name\":\"session_info\""));
        assert!(post.stdout.contains("\"tool_name\":\"session_info\""));
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

    #[tokio::test]
    async fn lua_pre_turn_rewrite_updates_request_and_shell_context() {
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
        let (endpoint, captured) = start_mock_server_with_capture(vec![response]);
        let mut agent = test_agent_with_endpoint(
            endpoint,
            vec![HookConfig {
                event: HookEvent::PreTurn,
                command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
                timeout_ms: 1000,
                enabled: true,
            }],
        );
        let runtime = test_lua_runtime(
            r#"
genesis.on("PreTurn", function(ctx)
    return "rewritten: " .. ctx.user_message
end)
"#,
        );
        agent.set_lua_runtime(runtime);

        let result = agent.run_turn("hello").await.expect("turn should succeed");
        assert_eq!(result.response, "done");

        let request = captured.lock().expect("capture mutex").join("\n");
        assert!(
            request.contains("rewritten: hello"),
            "request body should contain the rewritten user message: {request}"
        );

        let pre_turn = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::PreTurn)
            .expect("pre-turn shell hook");
        assert!(
            pre_turn.stdout.contains("rewritten: hello"),
            "shell hook should see the Lua-rewritten message: {}",
            pre_turn.stdout
        );
    }

    #[tokio::test]
    async fn lua_message_hook_rewrites_user_and_assistant_messages() {
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
        let (endpoint, captured) = start_mock_server_with_capture(vec![response]);
        let mut agent = test_agent_with_endpoint(
            endpoint,
            vec![HookConfig {
                event: HookEvent::OnMessage,
                command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
                timeout_ms: 1000,
                enabled: true,
            }],
        );
        let runtime = test_lua_runtime(
            r#"
genesis.on("OnMessage", function(ctx)
    if ctx.role == "user" then
        return "rewritten user: " .. ctx.content
    end
    if ctx.role == "assistant" then
        return ctx.content .. " [lua]"
    end
    return ctx.content
end)
"#,
        );
        agent.set_lua_runtime(runtime);

        let result = agent.run_turn("hello").await.expect("turn should succeed");
        assert_eq!(result.response, "done [lua]");

        let request = captured.lock().expect("capture mutex").join("\n");
        assert!(
            request.contains("rewritten user: hello"),
            "request body should contain the rewritten user message: {request}"
        );

        let hook_outputs: Vec<&str> = agent
            .hook_results()
            .iter()
            .filter(|result| result.event == HookEvent::OnMessage)
            .map(|result| result.stdout.as_str())
            .collect();
        assert!(
            hook_outputs
                .iter()
                .any(|stdout| stdout.contains(r#""role":"user""#)
                    && stdout.contains("rewritten user: hello")),
            "shell on_message hook should see the rewritten user message: {hook_outputs:?}"
        );
        assert!(
            hook_outputs
                .iter()
                .any(|stdout| stdout.contains(r#""role":"assistant""#)
                    && stdout.contains("done [lua]")),
            "shell on_message hook should see the rewritten assistant message: {hook_outputs:?}"
        );
    }

    #[tokio::test]
    async fn lua_message_hook_can_strip_tool_call_text_without_breaking_protocol() {
        let first = serde_json::json!({
            "id": "cmpl-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "tool chatter",
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "session_info",
                            "arguments": "{}"
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
        let (endpoint, captured) = start_mock_server_with_capture(vec![first, second]);
        let mut agent = test_agent_with_endpoint(
            endpoint,
            vec![HookConfig {
                event: HookEvent::OnMessage,
                command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
                timeout_ms: 1000,
                enabled: true,
            }],
        );
        let runtime = test_lua_runtime(
            r#"
genesis.on("OnMessage", function(ctx)
    if ctx.role == "assistant" and ctx.tool_call_count > 0 then
        return false, "hide tool chatter"
    end
    return ctx.content
end)
"#,
        );
        agent.set_lua_runtime(runtime);

        let result = agent
            .run_turn("use a tool")
            .await
            .expect("turn should succeed");
        assert_eq!(result.response, "final");

        let request = captured.lock().expect("capture mutex").join("\n");
        assert!(
            request.contains(r#""tool_calls":[{"#),
            "tool-call protocol should stay intact: {request}"
        );
        assert!(
            !request.contains("tool chatter"),
            "vetoed tool-call chatter should be stripped from follow-up history: {request}"
        );

        let hook_outputs: Vec<&str> = agent
            .hook_results()
            .iter()
            .filter(|result| result.event == HookEvent::OnMessage)
            .map(|result| result.stdout.as_str())
            .collect();
        assert!(
            hook_outputs
                .iter()
                .any(|stdout| stdout.contains(r#""lua_vetoed":true"#)),
            "shell on_message hook should reflect the Lua veto: {hook_outputs:?}"
        );
    }

    #[tokio::test]
    async fn lua_personality_transform_response_rewrites_final_output() {
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
        let mut agent = test_agent_with_endpoint(endpoint, Vec::new());
        let runtime = test_lua_runtime_with_personality(
            r#"
genesis.register_personality({
    name = "pirate",
    description = "A pirate lua personality",
    system_prompt = "Speak like a pirate.",
    transform_response = function(response)
        return response .. " Arrr!"
    end,
})
"#,
            Some("pirate"),
        );
        agent.set_lua_runtime(runtime);

        let result = agent.run_turn("hello").await.expect("turn should succeed");
        assert_eq!(result.response, "done Arrr!");
    }

    #[tokio::test]
    async fn lua_tool_hooks_rewrite_veto_and_update_session_state() {
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
                            "name": "session_info",
                            "arguments": "{}"
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
        let runtime = test_lua_runtime(
            r#"
genesis.on("PreToolCall", function(ctx)
    return '{"message":"rewritten"}'
end)

genesis.on("PostToolCall", function(ctx)
    return ctx.output .. " [lua]"
end)

genesis.on("PostTurn", function(_)
    genesis.log(string.format("post_turn:%d:%d", genesis.session.turn_count, genesis.session.total_tokens))
end)

genesis.on("OnComplete", function(_)
    genesis.log(string.format("complete:%d:%d", genesis.session.turn_count, genesis.session.total_tokens))
end)
"#,
        );
        agent.set_lua_runtime(runtime.clone());

        let result = agent
            .run_turn("use a tool")
            .await
            .expect("turn should succeed");
        assert_eq!(result.response, "final");

        let pre = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::PreToolCall)
            .expect("pre tool shell hook");
        let post = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::PostToolCall)
            .expect("post tool shell hook");

        assert!(
            pre.stdout
                .contains(r#""arguments":"{\"message\":\"rewritten\"}""#),
            "shell hook should see rewritten tool arguments: {}",
            pre.stdout
        );
        assert!(
            post.stdout.contains("[lua]"),
            "shell hook should see Lua-rewritten tool output: {}",
            post.stdout
        );

        let logs = runtime.logs();
        assert!(
            logs.iter().any(|line| line.contains("post_turn:1:30")),
            "PostTurn should see updated session counters: {:?}",
            logs
        );
        assert!(
            logs.iter().any(|line| line.contains("complete:1:30")),
            "OnComplete should see updated session counters: {:?}",
            logs
        );
    }

    #[tokio::test]
    async fn lua_pre_tool_call_veto_becomes_error_and_shell_context_is_preserved() {
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
                            "name": "session_info",
                            "arguments": "{}"
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
        let runtime = test_lua_runtime(
            r#"
genesis.on("PreToolCall", function(_)
    return false, "blocked"
end)
"#,
        );
        agent.set_lua_runtime(runtime);

        let result = agent
            .run_turn("use a tool")
            .await
            .expect("turn should succeed");
        assert_eq!(result.response, "final");

        let pre = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::PreToolCall)
            .expect("pre tool shell hook");
        let post = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::PostToolCall)
            .expect("post tool shell hook");

        assert!(
            pre.stdout.contains(r#""lua_vetoed":true"#),
            "shell hook should reflect the Lua veto: {}",
            pre.stdout
        );
        assert!(
            post.stdout.contains("blocked by Lua hook"),
            "post tool shell hook should see the synthesized veto output: {}",
            post.stdout
        );
    }

    #[tokio::test]
    async fn lua_post_tool_call_rewrite_does_not_change_failure_accounting() {
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
                            "name": "does_not_exist",
                            "arguments": "{}"
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
            vec![HookConfig {
                event: HookEvent::PostToolCall,
                command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
                timeout_ms: 1000,
                enabled: true,
            }],
        );
        let runtime = test_lua_runtime(
            r#"
genesis.on("PostToolCall", function(_)
    return "rewritten"
end)
"#,
        );
        agent.set_lua_runtime(runtime);

        let result = agent
            .run_turn("use a missing tool")
            .await
            .expect("turn should succeed");
        assert_eq!(result.response, "final");
        assert_eq!(
            agent.tool_failure_counts.get("does_not_exist"),
            Some(&1),
            "tool failure accounting should use the underlying tool outcome"
        );

        let post = agent
            .hook_results()
            .iter()
            .find(|result| result.event == HookEvent::PostToolCall)
            .expect("post tool hook");
        assert!(
            post.stdout.contains(r#""success":false"#),
            "shell hook should preserve the underlying failure status: {}",
            post.stdout
        );
        assert!(
            post.stdout.contains("rewritten"),
            "shell hook should still see the Lua-rewritten output: {}",
            post.stdout
        );
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
            approval_mode: genesis_config::ApprovalMode::default(),
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
                            "name": "session_info",
                            "arguments": "{}"
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
            approval_mode: genesis_config::ApprovalMode::default(),
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

        // Message [3] (tool, long, old) -> masked
        assert_eq!(
            agent.messages()[3].content_text().unwrap(),
            "[Tool output masked — see preceding tool call for context]"
        );

        // Message [7] (tool, short, old) -> NOT masked (below threshold)
        assert_eq!(agent.messages()[7].content_text().unwrap(), short_output,);

        // Message [11] (tool, long, recent/protected) -> NOT masked
        assert_eq!(
            agent.messages()[11].content_text().unwrap(),
            long_output.as_str(),
        );

        // Message [15] (tool, long, recent/protected) -> NOT masked
        assert_eq!(
            agent.messages()[15].content_text().unwrap(),
            long_output.as_str(),
        );

        // System prompt untouched
        assert_eq!(agent.messages()[0].role, "system");
    }

    // -- Core tool set filtering tests --

    fn make_chat_tool(name: &str) -> ChatTool {
        ChatTool {
            tool_type: "function".to_owned(),
            function: genesis_provider::ChatToolFunction {
                name: name.to_owned(),
                description: format!("{name} tool"),
                parameters: None,
                strict: None,
            },
        }
    }

    #[test]
    fn filter_tool_defs_returns_all_when_core_tools_is_none() {
        let agent = test_agent();

        let all = vec![
            make_chat_tool("shell_exec"),
            make_chat_tool("read_file"),
            make_chat_tool("web_request"),
        ];
        let filtered = agent.filter_tool_defs(&all);
        assert_eq!(
            filtered.len(),
            3,
            "should return all tools when core_tools is None"
        );
    }

    #[test]
    fn filter_tool_defs_filters_to_core_set() {
        let mut agent = test_agent();
        agent.config.core_tools = Some(vec!["shell_exec".to_owned(), "read_file".to_owned()]);

        let all = vec![
            make_chat_tool("shell_exec"),
            make_chat_tool("read_file"),
            make_chat_tool("web_request"),
            make_chat_tool("memory_store"),
        ];
        let filtered = agent.filter_tool_defs(&all);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|t| t.function.name == "shell_exec"));
        assert!(filtered.iter().any(|t| t.function.name == "read_file"));
    }

    #[test]
    fn filter_tool_defs_includes_discovered_tools() {
        let mut agent = test_agent();
        agent.config.core_tools = Some(vec!["shell_exec".to_owned()]);

        // Discover a new tool.
        agent.discover_tool("web_request");

        let all = vec![
            make_chat_tool("shell_exec"),
            make_chat_tool("web_request"),
            make_chat_tool("memory_store"),
        ];
        let filtered = agent.filter_tool_defs(&all);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|t| t.function.name == "shell_exec"));
        assert!(filtered.iter().any(|t| t.function.name == "web_request"));
    }

    #[test]
    fn filter_tool_defs_empty_core_uses_defaults() {
        let mut agent = test_agent();
        agent.config.core_tools = Some(vec![]); // Empty = use DEFAULT_CORE_TOOLS

        let all = vec![
            make_chat_tool("shell_exec"),
            make_chat_tool("read_file"),
            make_chat_tool("web_request"),
            make_chat_tool("find_tools"),
        ];
        let filtered = agent.filter_tool_defs(&all);
        // shell_exec, read_file, find_tools are in DEFAULT_CORE_TOOLS; web_request is not
        assert_eq!(filtered.len(), 3);
        assert!(!filtered.iter().any(|t| t.function.name == "web_request"));
    }
}
