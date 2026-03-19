use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use genesis_config::{LoadedConfig, TerminalConfig};
use genesis_provider::{client_from_config, ChatMessage, MessageContent, ProviderError};
use genesis_storage::{
    bootstrap, format_user_traits, SandboxStore, SessionStore, StorageError, StoredMessage,
    SubagentStore, UserModelStore,
};
use genesis_types::DeliveryPlatform;
use thiserror::Error;
use tracing::{debug, error, info, info_span, warn, Instrument};

use genesis_mcp::McpManager;

use crate::agent_loop::{AgentError, AgentLoop, AgentLoopConfig, AgentResult, SubagentSpawner};
use crate::nudge::SKILL_CREATION_NUDGE;
use crate::prompt::{load_context_file, SystemPromptBuilder};
use crate::sandbox::{
    daytona::DaytonaSandbox, manager::SandboxManager, modal::ModalSandbox,
    singularity::SingularitySandbox, BackendSpecific, SandboxBackend, SandboxConfig,
};
use crate::skills::{load_skills_prompt, load_skills_prompt_for_prompt};
use crate::{build_default_tool_runtime, build_execution_context_from_loaded, ToolRuntime};

/// Pre-built sandbox components that persist across turns within a session.
struct SandboxComponents {
    manager: Arc<SandboxManager>,
    backend: Arc<dyn SandboxBackend>,
    base_config: SandboxConfig,
}

/// Bridges the async `SandboxManager` into the sync `SandboxExecutor` trait.
struct SandboxExecutorImpl {
    manager: Arc<SandboxManager>,
    backend: Arc<dyn SandboxBackend>,
    config: SandboxConfig,
}

impl genesis_tools::SandboxExecutor for SandboxExecutorImpl {
    fn execute_in_sandbox(
        &self,
        command: &str,
        working_dir: Option<&str>,
        timeout_secs: u64,
    ) -> Result<(String, i32), String> {
        let timeout = std::time::Duration::from_secs(timeout_secs);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                self.manager
                    .execute(
                        self.backend.clone(),
                        &self.config,
                        command,
                        working_dir,
                        Some(timeout),
                    )
                    .await
                    .map(|r| (r.output, r.exit_code))
                    .map_err(|e| e.to_string())
            })
        })
    }
}

pub struct SessionExecutionService<'a> {
    loaded: &'a LoadedConfig,
    mcp: Option<Arc<McpManager>>,
    system_prompt_override: Option<String>,
    response_format: Option<genesis_provider::ResponseFormat>,
    approval_handler: Option<Arc<dyn genesis_tools::ApprovalHandler>>,
    /// Default working directory for shell commands (worktree isolation).
    default_working_dir: Option<String>,
    /// Override the model for this service instance (backend, model).
    model_override: Option<(String, String)>,
    /// Override the personality for this service instance.
    personality_override: Option<String>,
    /// Cached sandbox components for lifecycle-managed backends (persists across turns).
    sandbox: std::sync::OnceLock<Option<SandboxComponents>>,
}

#[derive(Debug, Clone)]
pub struct SessionTurnInput<'a> {
    pub session_id: &'a str,
    pub session_platform: &'a str,
    pub delivery_platform: DeliveryPlatform,
    pub prompt: &'a str,
    pub title: Option<&'a str>,
    /// Optional image URLs or base64 data URIs for multimodal prompts.
    #[allow(clippy::vec_box)]
    pub images: Vec<genesis_provider::ImageUrl>,
}

#[derive(Debug, Clone)]
pub struct SessionTurnOutcome {
    pub session_id: String,
    pub created_session: bool,
    pub result: AgentResult,
}

struct ExecutedTurn {
    result: AgentResult,
    emitted_messages: Vec<ChatMessage>,
}

#[derive(Debug, Error)]
pub enum SessionExecutionError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("required MCP servers failed to initialize: {servers:?}")]
    McpStartupFailed { servers: Vec<String> },
}

impl<'a> SessionExecutionService<'a> {
    pub fn new(loaded: &'a LoadedConfig) -> Self {
        Self {
            loaded,
            mcp: None,
            system_prompt_override: None,
            response_format: None,
            approval_handler: None,
            default_working_dir: None,
            model_override: None,
            personality_override: None,
            sandbox: std::sync::OnceLock::new(),
        }
    }

    /// Create a service with MCP servers connected.
    ///
    /// If `strict_startup` is true, startup fails when any configured MCP server
    /// cannot be initialized.
    pub async fn with_mcp(
        loaded: &'a LoadedConfig,
        strict_startup: bool,
    ) -> Result<Self, SessionExecutionError> {
        let mcp = if !loaded.config.mcp_servers.is_empty() {
            let configs = genesis_mcp::build_server_configs(&loaded.config.mcp_servers);

            if configs.is_empty() {
                None
            } else {
                let manager = McpManager::connect_all(configs).await;
                if strict_startup {
                    let failed: Vec<String> = manager
                        .server_status()
                        .await
                        .into_iter()
                        .filter_map(|(name, connected)| (!connected).then_some(name))
                        .collect();
                    if !failed.is_empty() {
                        return Err(SessionExecutionError::McpStartupFailed { servers: failed });
                    }
                }
                let tool_count = manager.tool_count().await;
                if tool_count > 0 {
                    info!(
                        servers = manager.server_count().await,
                        tools = tool_count,
                        "MCP tools registered"
                    );
                    Some(Arc::new(manager))
                } else {
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            loaded,
            mcp,
            system_prompt_override: None,
            response_format: None,
            approval_handler: None,
            default_working_dir: None,
            model_override: None,
            personality_override: None,
            sandbox: std::sync::OnceLock::new(),
        })
    }

    /// Attach an already-connected MCP manager (e.g. from gateway startup).
    pub fn set_mcp(&mut self, mcp: Arc<McpManager>) {
        self.mcp = Some(mcp);
    }

    /// Override the system prompt / agent identity for this service instance.
    pub fn set_system_prompt_override(&mut self, prompt: String) {
        self.system_prompt_override = Some(prompt);
    }

    /// Clear the system prompt override, reverting to the default.
    pub fn clear_system_prompt_override(&mut self) {
        self.system_prompt_override = None;
    }

    /// Set a response format constraint for all chat completions in this
    /// service instance (e.g. json_object or json_schema).
    pub fn set_response_format(&mut self, format: genesis_provider::ResponseFormat) {
        self.response_format = Some(format);
    }

    /// Override the model backend and name for this service instance.
    ///
    /// When set, this model is used instead of the one from config.
    pub fn set_model_override(&mut self, backend: String, model: String) {
        self.model_override = Some((backend, model));
    }

    /// Override the personality for this service instance.
    pub fn set_personality_override(&mut self, name: String) {
        self.personality_override = Some(name);
    }

    /// Set an interactive approval handler for tools requiring user confirmation.
    pub fn set_approval_handler(&mut self, handler: Arc<dyn genesis_tools::ApprovalHandler>) {
        self.approval_handler = Some(handler);
    }

    /// Set the default working directory for shell commands.
    /// Used by worktree isolation to redirect tool execution.
    pub fn set_default_working_dir(&mut self, dir: String) {
        self.default_working_dir = Some(dir);
    }

    /// Return the MCP manager if connected, for sharing with other subsystems.
    pub fn mcp_manager(&self) -> Option<Arc<McpManager>> {
        self.mcp.clone()
    }

    /// Return (builtin_tool_count, mcp_tool_count).
    pub async fn tool_counts(&self) -> (usize, usize) {
        let builtin = crate::default_tool_count();
        let mcp = match self.mcp.as_ref() {
            Some(m) => m.tool_count().await,
            None => 0,
        };
        (builtin, mcp)
    }

    pub fn ensure_session(
        &self,
        session_id: &str,
        platform: &str,
        title: Option<&str>,
    ) -> Result<bool, SessionExecutionError> {
        let _span = info_span!(
            "session.ensure",
            session_id = session_id,
            session_platform = platform
        )
        .entered();
        bootstrap(&self.loaded.config.storage.database_path)?;

        let store = self.session_store();
        if store.get_session(session_id)?.is_some() {
            debug!("reusing existing session");
            return Ok(false);
        }

        store.create_session(session_id, platform, title)?;
        info!("created new session");
        Ok(true)
    }

    pub fn load_history(
        &self,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>, SessionExecutionError> {
        let _span = info_span!("session.load_history", session_id = session_id).entered();
        bootstrap(&self.loaded.config.storage.database_path)?;
        let store = self.session_store();
        let messages = store.load_messages(session_id)?;
        let history = restore_chat_history(messages)?;
        debug!(message_count = history.len(), "loaded persisted history");
        Ok(history)
    }

    pub async fn run_turn(
        &self,
        mut input: SessionTurnInput<'_>,
    ) -> Result<SessionTurnOutcome, SessionExecutionError> {
        let span = info_span!(
            "session.run_turn",
            session_id = input.session_id,
            session_platform = input.session_platform
        );
        let started_at = Instant::now();
        let session_id = input.session_id.to_owned();
        let platform = input.delivery_platform.clone();
        let prompt = input.prompt.to_owned();
        let images = std::mem::take(&mut input.images);

        let outcome = self
            .run_turn_with_runner(input, |history| async move {
                let mut agent = self
                    .build_agent_loop(session_id, platform, history, Some(&prompt))
                    .await?;
                let start_index = agent.messages().len();
                let result = agent.run_turn_with_images(&prompt, images).await?;
                Ok(ExecutedTurn {
                    result,
                    emitted_messages: agent.messages()[start_index..].to_vec(),
                })
            })
            .instrument(span)
            .await?;
        info!(
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "session run_turn latency recorded"
        );
        Ok(outcome)
    }

    pub async fn run_turn_streaming<F>(
        &self,
        mut input: SessionTurnInput<'_>,
        on_chunk: F,
    ) -> Result<SessionTurnOutcome, SessionExecutionError>
    where
        F: FnMut(crate::agent_loop::StreamEvent<'_>),
    {
        let span = info_span!(
            "session.run_turn_streaming",
            session_id = input.session_id,
            session_platform = input.session_platform
        );
        let started_at = Instant::now();
        let session_id = input.session_id.to_owned();
        let platform = input.delivery_platform.clone();
        let prompt = input.prompt.to_owned();
        let images = std::mem::take(&mut input.images);

        let outcome = self
            .run_turn_streaming_with_runner(input, on_chunk, |history, on_chunk| async move {
                let mut agent = self
                    .build_agent_loop(session_id, platform, history, Some(&prompt))
                    .await?;
                let start_index = agent.messages().len();
                let result = agent
                    .run_turn_streaming_with_images(&prompt, images, on_chunk)
                    .await?;
                Ok(ExecutedTurn {
                    result,
                    emitted_messages: agent.messages()[start_index..].to_vec(),
                })
            })
            .instrument(span)
            .await?;
        info!(
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "session run_turn_streaming latency recorded"
        );
        Ok(outcome)
    }

    /// Load high-confidence user traits and format them for prompt injection.
    fn load_user_model_section(&self) -> Option<String> {
        let db_path = &self.loaded.config.storage.database_path;
        let store = UserModelStore::new(db_path);
        let traits = store.confident_traits(0.5).ok()?;
        format_user_traits(&traits)
    }

    /// Search stored memories for content relevant to the user's prompt.
    /// Returns a formatted section or None if no matches found.
    fn recall_memories(&self, prompt: &str) -> Option<String> {
        let db_path = &self.loaded.config.storage.database_path;
        let store = genesis_storage::MemoryStore::new(db_path);

        // Extract meaningful search terms: skip very short prompts that won't
        // produce useful FTS matches.
        if prompt.split_whitespace().count() < 2 {
            return None;
        }

        let memories = store.search(prompt, 5).ok()?;
        if memories.is_empty() {
            return None;
        }

        use std::fmt::Write;
        let mut section = String::new();
        for mem in &memories {
            let _ = writeln!(section, "- [{}] {}", mem.kind, mem.content);
        }
        Some(section)
    }

    fn session_store(&self) -> SessionStore {
        SessionStore::new(&self.loaded.config.storage.database_path)
    }

    async fn build_agent_loop(
        &self,
        session_id: String,
        platform: DeliveryPlatform,
        history: Vec<ChatMessage>,
        user_prompt: Option<&str>,
    ) -> Result<AgentLoop, SessionExecutionError> {
        let execution_context =
            build_execution_context_from_loaded(self.loaded, session_id, platform);
        let mut tool_runtime = build_default_tool_runtime(&execution_context);

        // Attach MCP manager if we connected any servers at service creation
        if let Some(mcp) = &self.mcp {
            tool_runtime.set_mcp(Arc::clone(mcp));
        }

        // Attach interactive approval handler if configured
        if let Some(handler) = &self.approval_handler {
            tool_runtime.set_approval_handler(Arc::clone(handler));
        }

        // Set terminal backend if configured
        if let Some(terminal) = &self.loaded.config.runtime.terminal {
            tool_runtime.set_terminal_backend(terminal_config_to_backend(terminal));

            // Wire up lifecycle-managed sandbox execution (persists across turns)
            let components = self
                .sandbox
                .get_or_init(|| create_sandbox_components(self.loaded));
            if let Some(c) = components {
                let mut config = c.base_config.clone();
                config.task_id = execution_context.plan.session_id.clone();
                let executor: Arc<dyn genesis_tools::SandboxExecutor> =
                    Arc::new(SandboxExecutorImpl {
                        manager: c.manager.clone(),
                        backend: c.backend.clone(),
                        config,
                    });
                tool_runtime.set_sandbox_manager(executor);
            }
        }

        // Set default working directory (worktree isolation)
        if let Some(ref dir) = self.default_working_dir {
            tool_runtime.set_default_working_dir(dir.clone());
        }

        // Start filesystem watcher for tool result cache.
        // Uses the working directory (or worktree dir) as the watch root.
        {
            let watch_dir = self
                .default_working_dir
                .as_deref()
                .map(std::path::Path::new)
                .unwrap_or(std::path::Path::new("."));
            tool_runtime.start_cache_watcher(watch_dir);
        }

        // Apply tool filter (allowlist/denylist)
        if let Some(ref filter) = self.loaded.config.runtime.tool_filter {
            let mut allowed: std::collections::HashSet<String> = if filter.allow.is_empty() {
                tool_runtime
                    .definitions_async()
                    .await
                    .into_iter()
                    .map(|d| d.name)
                    .collect()
            } else {
                filter.allow.iter().cloned().collect()
            };
            for denied in &filter.deny {
                allowed.remove(denied);
            }
            tool_runtime.retain(&allowed);
        }

        // Load skills, user model, project context, and relevant memories
        let db_path = &self.loaded.config.storage.database_path;
        let skills_section = match user_prompt {
            Some(prompt) => load_skills_prompt_for_prompt(db_path, prompt),
            None => load_skills_prompt(db_path),
        };
        let user_model_section = self.load_user_model_section();
        let mut context_section = load_context_file(
            std::path::Path::new("."),
            &self.loaded.config.runtime.context_security,
        );
        // Append worktree info to context if running in an isolated worktree
        if let Some(ref dir) = self.default_working_dir {
            let worktree_note = format!(
                "\n\n[Worktree Isolation] You are working in an isolated git worktree at `{dir}`. \
                 All shell commands execute in this directory by default. Changes here do not \
                 affect the main working tree. Commit your work when done."
            );
            context_section = Some(match context_section {
                Some(existing) => format!("{existing}{worktree_note}"),
                None => worktree_note,
            });
        }
        let memories_section = user_prompt.and_then(|prompt| self.recall_memories(prompt));

        let platform_str = delivery_platform_str(&execution_context.plan.platform);
        let tool_defs = tool_runtime.definitions();
        let mut prompt_builder =
            SystemPromptBuilder::new(&execution_context.plan.profile, &tool_defs)
                .delivery_platform(platform_str);
        if let Some(id) = self.system_prompt_override.as_deref() {
            prompt_builder = prompt_builder.identity(id);
        }
        let effective_personality =
            self.personality_override
                .as_deref()
                .or(self.loaded.config.personality.as_deref());
        if let Some(p) = effective_personality {
            prompt_builder = prompt_builder.personality(p);
        }
        if let Some(s) = skills_section.as_deref() {
            prompt_builder = prompt_builder.skills(s);
        }
        if let Some(u) = user_model_section.as_deref() {
            prompt_builder = prompt_builder.user_model(u);
        }
        if let Some(c) = context_section.as_deref() {
            prompt_builder = prompt_builder.context(c);
        }
        if let Some(m) = memories_section.as_deref() {
            prompt_builder = prompt_builder.memories(m);
        }
        let system_prompt = prompt_builder.build();
        let (backend, model) = match &self.model_override {
            Some((b, m)) => (b.as_str(), m.as_str()),
            None => (
                self.loaded.config.provider.backend.as_str(),
                self.loaded.config.provider.model.as_str(),
            ),
        };
        let cb_cfg = self.loaded.config.provider.circuit_breaker.as_ref();
        let client = genesis_provider::client_from_config_with_circuit_breaker(
            backend,
            model,
            self.loaded.config.provider.base_url.as_deref(),
            self.loaded.config.provider.api_key_env.as_deref(),
            cb_cfg.map(|c| c.failure_threshold),
            cb_cfg.map(|c| c.cooldown_secs),
        )
        .await?;
        debug!(
            provider_backend = %backend,
            model = %model,
            "built agent loop dependencies"
        );

        let hook_runner = crate::hooks::HookRunner::default();
        let hooks: Arc<dyn crate::agent_loop::AgentHooks> =
            crate::audit::AuditHooks::shared(db_path);

        let subagent_tool_runtime = Arc::new(tool_runtime.clone());
        let mut agent = AgentLoop::with_history(
            client,
            tool_runtime,
            AgentLoopConfig {
                system_prompt: Some(system_prompt),
                max_turns: self.loaded.config.runtime.max_turns,
                max_context_messages: self.loaded.config.runtime.max_context_messages,
                budget_limit: self.loaded.config.runtime.budget_limit,
                max_concurrency: self.loaded.config.runtime.max_concurrency,
                max_context_tokens: self.loaded.config.runtime.max_context_tokens,
                max_iterations: self.loaded.config.runtime.max_iterations,
                tool_call_parser: self.loaded.config.provider.tool_call_parser.clone(),
                reasoning_effort: self.loaded.config.runtime.reasoning_effort,
                cache: self.loaded.config.runtime.cache.clone(),
                guardrails: self
                    .loaded
                    .config
                    .runtime
                    .guardrails
                    .as_ref()
                    .map(crate::guardrails::GuardrailConfig::from),
                thinking: self.loaded.config.runtime.thinking_budget.map(|budget| {
                    genesis_provider::ThinkingConfig {
                        budget_tokens: Some(budget),
                    }
                }),
                response_format: self.response_format.clone(),
                core_tools: self.loaded.config.runtime.core_tools.clone(),
                routing: self.loaded.config.routing.clone(),
                ..AgentLoopConfig::default()
            },
            hook_runner.clone(),
            history,
        );

        // Set up tool provider routing if configured
        if let Some(tp) = &self.loaded.config.tool_provider {
            let tp_cb = tp.circuit_breaker.as_ref();
            let tool_client = genesis_provider::client_from_config_with_circuit_breaker(
                &tp.backend,
                &tp.model,
                tp.base_url.as_deref(),
                tp.api_key_env.as_deref(),
                tp_cb.map(|c| c.failure_threshold),
                tp_cb.map(|c| c.cooldown_secs),
            )
            .await?;
            agent.set_tool_client(tool_client);
            debug!(
                tool_provider_backend = %tp.backend,
                tool_model = %tp.model,
                "multi-provider routing enabled"
            );
        }

        // Set up fallback providers for automatic failover
        if !self.loaded.config.fallback_providers.is_empty() {
            let mut fallbacks = Vec::new();
            for fp in &self.loaded.config.fallback_providers {
                let fb_cb = fp.circuit_breaker.as_ref();
                let fb_client = genesis_provider::client_from_config_with_circuit_breaker(
                    &fp.backend,
                    &fp.model,
                    fp.base_url.as_deref(),
                    fp.api_key_env.as_deref(),
                    fb_cb.map(|c| c.failure_threshold),
                    fb_cb.map(|c| c.cooldown_secs),
                )
                .await?;
                fallbacks.push(fb_client);
            }
            agent.set_fallback_clients(fallbacks);
            debug!(
                fallback_count = self.loaded.config.fallback_providers.len(),
                "provider failover enabled"
            );
        }

        // Attach subagent spawner so agent can spawn parallel workstreams
        agent.set_subagent_spawner(Arc::new(ExecutionSubagentSpawner {
            loaded: Arc::new(self.loaded.clone()),
            tool_runtime: subagent_tool_runtime,
            hook_runner,
            hooks: Arc::clone(&hooks),
            model_override: self.model_override.clone(),
        }));

        // Set up response cache if configured
        if self
            .loaded
            .config
            .runtime
            .cache
            .as_ref()
            .is_some_and(|c| c.enabled)
        {
            let cache = genesis_storage::ResponseCacheStore::new(db_path);
            agent.set_response_cache(cache);
        }

        // Attach audit logging hooks
        agent.set_hooks(hooks);

        Ok(agent)
    }

    async fn run_turn_with_runner<F, Fut>(
        &self,
        input: SessionTurnInput<'_>,
        runner: F,
    ) -> Result<SessionTurnOutcome, SessionExecutionError>
    where
        F: FnOnce(Vec<ChatMessage>) -> Fut,
        Fut: Future<Output = Result<ExecutedTurn, SessionExecutionError>>,
    {
        let created_session =
            self.ensure_session(input.session_id, input.session_platform, input.title)?;
        let history = self.load_history(input.session_id)?;
        debug!(history_messages = history.len(), "starting turn execution");
        let executed = runner(history).await?;
        let store = self.session_store();
        persist_new_messages(&store, input.session_id, &executed.emitted_messages)?;
        // Auto-generate title from prompt if this is a new session without one
        if created_session && input.title.is_none() {
            let title = generate_session_title(input.prompt);
            if let Err(e) = store.update_title(input.session_id, &title) {
                warn!(error = %e, "failed to set auto-generated session title");
            }
        }
        // Persist token usage
        if let Err(e) = store.add_usage(
            input.session_id,
            executed.result.total_input_tokens,
            executed.result.total_output_tokens,
        ) {
            warn!(error = %e, "failed to persist token usage");
        }
        // Inject skill creation nudge after complex turns
        maybe_inject_skill_nudge(&store, input.session_id, &executed.result);

        info!(
            created_session,
            emitted_messages = executed.emitted_messages.len(),
            turns_used = executed.result.turns_used,
            tool_calls_made = executed.result.tool_calls_made,
            finished_naturally = executed.result.finished_naturally,
            input_tokens = executed.result.total_input_tokens,
            output_tokens = executed.result.total_output_tokens,
            "completed turn execution"
        );

        Ok(SessionTurnOutcome {
            session_id: input.session_id.to_owned(),
            created_session,
            result: executed.result,
        })
    }

    async fn run_turn_streaming_with_runner<F, Fut, G>(
        &self,
        input: SessionTurnInput<'_>,
        on_chunk: G,
        runner: F,
    ) -> Result<SessionTurnOutcome, SessionExecutionError>
    where
        F: FnOnce(Vec<ChatMessage>, G) -> Fut,
        Fut: Future<Output = Result<ExecutedTurn, SessionExecutionError>>,
        G: FnMut(crate::agent_loop::StreamEvent<'_>),
    {
        let created_session =
            self.ensure_session(input.session_id, input.session_platform, input.title)?;
        let history = self.load_history(input.session_id)?;
        debug!(
            history_messages = history.len(),
            "starting streaming turn execution"
        );
        let executed = runner(history, on_chunk).await?;
        let store = self.session_store();
        persist_new_messages(&store, input.session_id, &executed.emitted_messages)?;
        // Auto-generate title from prompt if this is a new session without one
        if created_session && input.title.is_none() {
            let title = generate_session_title(input.prompt);
            if let Err(e) = store.update_title(input.session_id, &title) {
                warn!(error = %e, "failed to set auto-generated session title");
            }
        }
        // Persist token usage
        if let Err(e) = store.add_usage(
            input.session_id,
            executed.result.total_input_tokens,
            executed.result.total_output_tokens,
        ) {
            warn!(error = %e, "failed to persist token usage");
        }
        // Inject skill creation nudge after complex turns
        maybe_inject_skill_nudge(&store, input.session_id, &executed.result);

        info!(
            created_session,
            emitted_messages = executed.emitted_messages.len(),
            turns_used = executed.result.turns_used,
            tool_calls_made = executed.result.tool_calls_made,
            finished_naturally = executed.result.finished_naturally,
            input_tokens = executed.result.total_input_tokens,
            output_tokens = executed.result.total_output_tokens,
            "completed streaming turn execution"
        );

        Ok(SessionTurnOutcome {
            session_id: input.session_id.to_owned(),
            created_session,
            result: executed.result,
        })
    }

    /// Execute a multi-step workflow, running each step as an independent agent turn.
    ///
    /// Each step's output is captured and made available to subsequent steps via
    /// `{{step_name}}` template variables. The workflow's `{{input}}` variable is
    /// replaced with the provided `input` string.
    pub async fn run_workflow(
        &self,
        workflow: &crate::workflow::WorkflowDefinition,
        input: &str,
        session_id: &str,
    ) -> Result<crate::workflow::WorkflowResult, SessionExecutionError> {
        use crate::workflow::{render_prompt, StepResult, WorkflowResult};
        use std::collections::HashMap;

        let span = info_span!("workflow.run", workflow = %workflow.name, session_id = session_id);
        let _guard = span.enter();

        info!(steps = workflow.steps.len(), "starting workflow execution");

        let mut step_outputs: HashMap<String, String> = HashMap::new();
        let mut step_results: Vec<StepResult> = Vec::new();
        let mut total_input_tokens: u32 = 0;
        let mut total_output_tokens: u32 = 0;
        let mut final_output = String::new();

        for (i, step) in workflow.steps.iter().enumerate() {
            let rendered_prompt = render_prompt(&step.prompt, input, &step_outputs);
            info!(
                step = i + 1,
                step_name = %step.name,
                "executing workflow step"
            );

            // Per-step model overrides can be added later; currently uses the
            // service-level model for all steps.

            let step_session_id = format!("{session_id}__wf__{}", step.name);
            let turn_input = SessionTurnInput {
                session_id: &step_session_id,
                session_platform: "workflow",
                delivery_platform: DeliveryPlatform::Cli,
                prompt: &rendered_prompt,
                title: Some(&format!("Workflow: {} / {}", workflow.name, step.name)),
                images: vec![],
            };

            let outcome = self.run_turn(turn_input).await?;

            let step_output = outcome.result.response; // move, not clone
            total_input_tokens += outcome.result.total_input_tokens;
            total_output_tokens += outcome.result.total_output_tokens;

            step_outputs.insert(step.name.clone(), step_output.clone());
            final_output = step_output.clone();
            step_results.push(StepResult {
                step_name: step.name.clone(),
                output: step_output, // move last use
                input_tokens: outcome.result.total_input_tokens,
                output_tokens: outcome.result.total_output_tokens,
            });

            info!(
                step = i + 1,
                step_name = %step.name,
                input_tokens = outcome.result.total_input_tokens,
                output_tokens = outcome.result.total_output_tokens,
                "completed workflow step"
            );

            if step.terminal {
                info!(
                    step_name = %step.name,
                    "terminal step reached, ending workflow early"
                );
                break;
            }
        }

        let result = WorkflowResult {
            workflow_name: workflow.name.clone(),
            step_results,
            final_output,
            total_input_tokens,
            total_output_tokens,
        };

        info!(
            steps_completed = result.steps_completed(),
            total_input_tokens = result.total_input_tokens,
            total_output_tokens = result.total_output_tokens,
            "workflow execution complete"
        );

        Ok(result)
    }

    /// Run an evaluation suite against the agent, collecting per-case results.
    ///
    /// Each test case is executed as an independent agent turn. The response
    /// is evaluated against the case's criteria, and results are aggregated
    /// into an `EvalReport`.
    pub async fn run_eval(
        &self,
        suite: &crate::eval::EvalSuite,
    ) -> Result<crate::eval::EvalReport, SessionExecutionError> {
        use crate::eval::{build_report, evaluate_response, EvalResult};

        let started_at = chrono::Utc::now().to_rfc3339();
        let start_instant = std::time::Instant::now();
        let model = match &self.model_override {
            Some((b, m)) => format!("{b}/{m}"),
            None => format!(
                "{}/{}",
                self.loaded.config.provider.backend, self.loaded.config.provider.model
            ),
        };

        info!(
            suite = %suite.name,
            cases = suite.cases.len(),
            model = %model,
            "starting evaluation run"
        );

        let mut results = Vec::new();

        for (i, case) in suite.cases.iter().enumerate() {
            let case_start = std::time::Instant::now();
            let eval_session_id = format!("eval__{}__{}", suite.name, case.id);

            info!(
                case = i + 1,
                case_id = %case.id,
                "running eval case"
            );

            let turn_input = SessionTurnInput {
                session_id: &eval_session_id,
                session_platform: "eval",
                delivery_platform: DeliveryPlatform::Cli,
                prompt: &case.prompt,
                title: Some(&format!("Eval: {} / {}", suite.name, case.id)),
                images: vec![],
            };

            let result = match self.run_turn(turn_input).await {
                Ok(outcome) => {
                    let response = outcome.result.response.clone();
                    let (passed, score, checks) = evaluate_response(
                        &response,
                        &case.criteria,
                        outcome.result.turns_used,
                        outcome.result.tool_calls_made,
                    );

                    EvalResult {
                        case_id: case.id.clone(),
                        passed,
                        score,
                        response,
                        duration_ms: case_start.elapsed().as_millis() as u64,
                        input_tokens: outcome.result.total_input_tokens,
                        output_tokens: outcome.result.total_output_tokens,
                        turns_used: outcome.result.turns_used,
                        tool_calls: outcome.result.tool_calls_made,
                        checks,
                        error: None,
                    }
                }
                Err(e) => {
                    warn!(case_id = %case.id, error = %e, "eval case failed");
                    EvalResult {
                        case_id: case.id.clone(),
                        passed: false,
                        score: 0.0,
                        response: String::new(),
                        duration_ms: case_start.elapsed().as_millis() as u64,
                        input_tokens: 0,
                        output_tokens: 0,
                        turns_used: 0,
                        tool_calls: 0,
                        checks: vec![],
                        error: Some(e.to_string()),
                    }
                }
            };

            info!(
                case_id = %case.id,
                passed = result.passed,
                score = result.score,
                duration_ms = result.duration_ms,
                "eval case complete"
            );

            results.push(result);
        }

        let completed_at = chrono::Utc::now().to_rfc3339();
        let total_duration_ms = start_instant.elapsed().as_millis() as u64;

        let report = build_report(
            suite,
            &model,
            &started_at,
            &completed_at,
            total_duration_ms,
            results,
        );

        info!(
            suite = %suite.name,
            passed = report.passed,
            failed = report.failed,
            pass_rate = report.pass_rate,
            avg_score = report.avg_score,
            total_duration_ms = report.total_duration_ms,
            "evaluation run complete"
        );

        Ok(report)
    }
}

/// Spawns child agent loops as background tokio tasks.
///
/// When the parent agent calls `spawn_subagent`, the agent loop detects
/// the output metadata and calls `SubagentSpawner::spawn`, which creates
/// a new `AgentLoop` and runs it in the background.
struct ExecutionSubagentSpawner {
    loaded: Arc<LoadedConfig>,
    tool_runtime: Arc<ToolRuntime>,
    hook_runner: crate::hooks::HookRunner,
    hooks: Arc<dyn crate::agent_loop::AgentHooks>,
    model_override: Option<(String, String)>,
}

impl SubagentSpawner for ExecutionSubagentSpawner {
    fn spawn(&self, child_session_id: &str, subagent_id: &str, task: &str) {
        let loaded = Arc::clone(&self.loaded);
        let tool_runtime = self.tool_runtime.with_session_id(child_session_id);
        let hook_runner = self.hook_runner.clone();
        let hooks = Arc::clone(&self.hooks);
        let model_override = self.model_override.clone();
        let child_session_id = child_session_id.to_owned();
        let subagent_id = subagent_id.to_owned();
        let task = task.to_owned();

        tokio::spawn(async move {
            let span = info_span!(
                "subagent.run",
                subagent_id = subagent_id.as_str(),
                child_session_id = child_session_id.as_str(),
            );

            async {
                let db_path = &loaded.config.storage.database_path;
                let subagent_store = SubagentStore::new(db_path);

                // Mark as running
                if let Err(e) = subagent_store.set_running(&subagent_id) {
                    error!(error = %e, "failed to mark subagent as running");
                    return;
                }

                info!("subagent starting");

                // Build the child agent loop
                let system_prompt = format!(
                    "You are a subagent — a focused worker spawned by a parent agent to handle a specific task. \
                     Complete the task below thoroughly and concisely. You have access to the same tools as the parent agent.\n\n\
                     ## Your Task\n{task}"
                );

                let (backend, model) = match &model_override {
                    Some((b, m)) => (b.as_str(), m.as_str()),
                    None => (
                        loaded.config.provider.backend.as_str(),
                        loaded.config.provider.model.as_str(),
                    ),
                };
                let client = match client_from_config(
                    backend,
                    model,
                    loaded.config.provider.base_url.as_deref(),
                    loaded.config.provider.api_key_env.as_deref(),
                )
                .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        error!(error = %e, "failed to create chat client for subagent");
                        let _ = subagent_store.set_failed(&subagent_id, &e.to_string());
                        return;
                    }
                };

                let mut agent = AgentLoop::new(
                    client,
                    tool_runtime,
                    AgentLoopConfig {
                        system_prompt: Some(system_prompt),
                        max_turns: 10, // Subagents get fewer turns to stay focused
                        ..AgentLoopConfig::default()
                    },
                    hook_runner,
                );
                agent.set_hooks(hooks);

                // Run the subagent turn
                match agent.run_turn(&task).await {
                    Ok(result) => {
                        info!(
                            turns_used = result.turns_used,
                            tool_calls_made = result.tool_calls_made,
                            "subagent completed successfully"
                        );

                        // Persist the subagent's messages
                        let session_store = SessionStore::new(db_path);
                        let emitted = agent.messages()[1..].to_vec(); // skip system prompt
                        if let Err(e) = persist_new_messages(&session_store, &child_session_id, &emitted) {
                            warn!(error = %e, "failed to persist subagent messages");
                        }

                        let _ = subagent_store.set_completed(&subagent_id, &result.response);
                    }
                    Err(e) => {
                        error!(error = %e, "subagent execution failed");
                        let _ = subagent_store.set_failed(&subagent_id, &e.to_string());
                    }
                }
            }
            .instrument(span)
            .await
        });
    }
}

pub fn persist_new_messages(
    store: &SessionStore,
    session_id: &str,
    messages: &[ChatMessage],
) -> Result<(), SessionExecutionError> {
    if messages.is_empty() {
        warn!(session_id, "no new messages to persist");
        return Ok(());
    }

    for message in messages {
        let tool_calls_json = message
            .tool_calls
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        let provider_metadata_json = message
            .provider_metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        store.append_message(
            session_id,
            &message.role,
            message.content_text(),
            message.tool_call_id.as_deref(),
            tool_calls_json.as_deref(),
            provider_metadata_json.as_deref(),
        )?;
    }

    debug!(
        session_id,
        persisted_messages = messages.len(),
        "persisted new messages"
    );

    Ok(())
}

pub fn restore_chat_history(
    messages: Vec<StoredMessage>,
) -> Result<Vec<ChatMessage>, SessionExecutionError> {
    messages
        .into_iter()
        .map(|message| {
            let tool_calls = message
                .tool_calls_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?;

            Ok(ChatMessage {
                role: message.role,
                content: message.content.map(MessageContent::Text),
                thinking: None,
                tool_calls,
                tool_call_id: message.tool_call_id,
                name: None,
                provider_metadata: message
                    .provider_metadata
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?,
            })
        })
        .collect()
}

/// Generate a short title from the first user prompt.
///
/// Minimum tool calls in a turn before injecting a skill creation nudge.
const SKILL_NUDGE_TOOL_THRESHOLD: usize = 5;
/// Minimum turns used before injecting a skill creation nudge.
const SKILL_NUDGE_TURN_THRESHOLD: usize = 3;

/// After a complex turn (many tool calls, multiple turns), inject a system
/// message nudging the agent to create a skill from the pattern. The nudge
/// is persisted so it appears when the next turn loads history.
fn maybe_inject_skill_nudge(store: &SessionStore, session_id: &str, result: &AgentResult) {
    if result.tool_calls_made >= SKILL_NUDGE_TOOL_THRESHOLD
        && result.turns_used >= SKILL_NUDGE_TURN_THRESHOLD
        && result.finished_naturally
    {
        debug!(
            tool_calls_made = result.tool_calls_made,
            turns_used = result.turns_used,
            "injecting skill creation nudge"
        );
        if let Err(e) = store.append_message(
            session_id,
            "system",
            Some(SKILL_CREATION_NUDGE),
            None,
            None,
            None,
        ) {
            warn!(error = %e, "failed to persist skill creation nudge");
        }
    }
}

/// Takes up to 60 characters, truncated at a word boundary, with "..." appended
/// if truncated. Strips leading/trailing whitespace and collapses internal
/// whitespace.
fn generate_session_title(prompt: &str) -> String {
    const MAX_LEN: usize = 60;

    let normalized = prompt.split_whitespace().fold(String::new(), |mut acc, w| {
        if !acc.is_empty() {
            acc.push(' ');
        }
        acc.push_str(w);
        acc
    });

    let truncated: String = normalized.chars().take(MAX_LEN).collect();

    if truncated.len() == normalized.len() {
        return normalized;
    }

    let end = truncated.rfind(' ').unwrap_or(truncated.len());
    format!("{}...", &truncated[..end])
}

/// Convert a DeliveryPlatform to its string representation.
pub fn delivery_platform_str(platform: &DeliveryPlatform) -> &'static str {
    match platform {
        DeliveryPlatform::Cli => "cli",
        DeliveryPlatform::Telegram => "telegram",
        DeliveryPlatform::Discord => "discord",
        DeliveryPlatform::Slack => "slack",
        DeliveryPlatform::HomeAssistant => "homeassistant",
        DeliveryPlatform::WhatsApp => "whatsapp",
        DeliveryPlatform::Signal => "signal",
        DeliveryPlatform::Api => "api",
    }
}

pub fn delivery_platform_from_str(raw: &str) -> DeliveryPlatform {
    match raw.trim().to_ascii_lowercase().as_str() {
        "telegram" => DeliveryPlatform::Telegram,
        "discord" => DeliveryPlatform::Discord,
        "slack" => DeliveryPlatform::Slack,
        "homeassistant" | "home_assistant" | "home-assistant" => DeliveryPlatform::HomeAssistant,
        "whatsapp" => DeliveryPlatform::WhatsApp,
        "signal" => DeliveryPlatform::Signal,
        "api" => DeliveryPlatform::Api,
        _ => DeliveryPlatform::Cli,
    }
}

/// Build sandbox components from config, returning None if the terminal config
/// is not a sandbox backend or if the backend prerequisites are not met.
fn create_sandbox_components(loaded: &LoadedConfig) -> Option<SandboxComponents> {
    let terminal = loaded.config.runtime.terminal.as_ref()?;

    let (backend, base_config): (Arc<dyn SandboxBackend>, SandboxConfig) = match terminal {
        TerminalConfig::Singularity {
            image,
            cpu,
            memory_mb,
            persistent,
            bind,
            working_dir,
        } => {
            let sb = match SingularitySandbox::new() {
                Ok(sb) => sb,
                Err(e) => {
                    warn!(error = %e, backend = "singularity", "sandbox backend unavailable");
                    return None;
                }
            };
            let config = SandboxConfig {
                task_id: String::new(), // filled per-turn
                image: image.clone(),
                cpu: *cpu,
                memory_mb: *memory_mb,
                disk_mb: 0,
                persistent: *persistent,
                working_dir: working_dir.clone(),
                snapshot_data: None,
                backend_specific: BackendSpecific::Singularity { bind: bind.clone() },
            };
            (Arc::new(sb), config)
        }
        TerminalConfig::Modal {
            image,
            cpu,
            memory_mb,
            disk_mb,
            persistent,
            gpu,
            app,
            working_dir,
        } => {
            let data_dir = loaded
                .config
                .storage
                .data_dir
                .to_string_lossy()
                .into_owned();
            let sb = match ModalSandbox::new(&data_dir) {
                Ok(sb) => sb,
                Err(e) => {
                    warn!(error = %e, backend = "modal", "sandbox backend unavailable");
                    return None;
                }
            };
            let config = SandboxConfig {
                task_id: String::new(),
                image: image.clone().unwrap_or_else(|| "python:3.11".to_string()),
                cpu: *cpu,
                memory_mb: *memory_mb,
                disk_mb: *disk_mb,
                persistent: *persistent,
                working_dir: working_dir.clone(),
                snapshot_data: None,
                backend_specific: BackendSpecific::Modal {
                    gpu: gpu.clone(),
                    app: app.clone(),
                },
            };
            (Arc::new(sb), config)
        }
        TerminalConfig::Daytona {
            image,
            cpu,
            memory_mb,
            disk_mb,
            persistent,
            target,
            api_url,
            working_dir,
        } => {
            let sb = match DaytonaSandbox::new() {
                Ok(sb) => sb,
                Err(e) => {
                    warn!(error = %e, backend = "daytona", "sandbox backend unavailable");
                    return None;
                }
            };
            let config = SandboxConfig {
                task_id: String::new(),
                image: image
                    .clone()
                    .unwrap_or_else(|| "nikolaik/python-nodejs:python3.11-nodejs20".to_string()),
                cpu: *cpu,
                memory_mb: *memory_mb,
                disk_mb: *disk_mb,
                persistent: *persistent,
                working_dir: working_dir.clone(),
                snapshot_data: None,
                backend_specific: BackendSpecific::Daytona {
                    target: target.clone(),
                    api_url: api_url.clone(),
                },
            };
            (Arc::new(sb), config)
        }
        _ => return None,
    };

    let store = SandboxStore::new(&loaded.config.storage.database_path);
    let manager = Arc::new(SandboxManager::new(store, 300));

    Some(SandboxComponents {
        manager,
        backend,
        base_config,
    })
}

/// Convert a genesis_config::TerminalConfig to a genesis_tools::TerminalBackend.
fn terminal_config_to_backend(config: &TerminalConfig) -> genesis_tools::TerminalBackend {
    match config {
        TerminalConfig::Docker {
            container,
            user,
            working_dir,
        } => genesis_tools::TerminalBackend::Docker {
            container: container.clone(),
            user: user.clone(),
            working_dir: working_dir.clone(),
        },
        TerminalConfig::Ssh {
            host,
            user,
            port,
            identity_file,
        } => genesis_tools::TerminalBackend::Ssh {
            host: host.clone(),
            user: user.clone(),
            port: *port,
            identity_file: identity_file.clone(),
        },
        TerminalConfig::Singularity {
            image,
            cpu,
            memory_mb,
            persistent,
            bind,
            working_dir,
        } => genesis_tools::TerminalBackend::Singularity {
            image: image.clone(),
            cpu: *cpu,
            memory_mb: *memory_mb,
            persistent: *persistent,
            bind: bind.clone(),
            working_dir: working_dir.clone(),
        },
        TerminalConfig::Modal {
            image,
            cpu,
            memory_mb,
            disk_mb,
            persistent,
            gpu,
            app,
            working_dir,
        } => genesis_tools::TerminalBackend::Modal {
            image: image.clone(),
            cpu: *cpu,
            memory_mb: *memory_mb,
            disk_mb: *disk_mb,
            persistent: *persistent,
            gpu: gpu.clone(),
            app: app.clone(),
            working_dir: working_dir.clone(),
        },
        TerminalConfig::Daytona {
            image,
            cpu,
            memory_mb,
            disk_mb,
            persistent,
            target,
            api_url,
            working_dir,
        } => genesis_tools::TerminalBackend::Daytona {
            image: image.clone(),
            cpu: *cpu,
            memory_mb: *memory_mb,
            disk_mb: *disk_mb,
            persistent: *persistent,
            target: target.clone(),
            api_url: api_url.clone(),
            working_dir: working_dir.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        delivery_platform_from_str, generate_session_title, maybe_inject_skill_nudge,
        persist_new_messages, restore_chat_history, ExecutedTurn, SessionExecutionService,
        SessionTurnInput,
    };
    use crate::agent_loop::AgentResult;
    use crate::tests::test_loaded_config;
    use genesis_config::{
        AppPaths, GenesisConfig, LoadedConfig, ProviderConfig, RuntimeConfig, StorageConfig,
    };
    use genesis_provider::ChatMessage;
    use genesis_provider::MessageContent;
    use genesis_storage::{bootstrap, SessionStore, StoredMessage};
    use genesis_types::DeliveryPlatform;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn restore_chat_history_round_trips_tool_calls() {
        let messages = restore_chat_history(vec![StoredMessage {
            id: 1,
            session_id: "session-1".to_owned(),
            role: "assistant".to_owned(),
            content: Some("hello".to_owned()),
            tool_call_id: Some("tool-1".to_owned()),
            tool_calls_json: Some(
                r#"[{"id":"tool-1","type":"function","function":{"name":"echo","arguments":"{\"message\":\"hi\"}"}}]"#
                    .to_owned(),
            ),
            mirror: false,
            mirror_source: None,
            provider_metadata: None,
            created_at: "2026-03-08 12:00:00".to_owned(),
        }])
        .expect("history should restore");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("tool-1"));
        assert_eq!(
            messages[0]
                .tool_calls
                .as_ref()
                .expect("tool calls should restore")[0]
                .function
                .name,
            "echo"
        );
    }

    #[test]
    fn persist_new_messages_writes_tool_calls_json() {
        let dir = tempdir().expect("tempdir should exist");
        let database_path = dir.path().join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-1", "cli", None)
            .expect("session should be created");

        let tool_calls = serde_json::from_str(
            r#"[{"id":"tool-1","type":"function","function":{"name":"echo","arguments":"{\"message\":\"hi\"}"}}]"#,
        )
        .expect("tool calls should parse");
        let messages = vec![ChatMessage {
            role: "assistant".to_owned(),
            content: Some(MessageContent::Text("hello".to_owned())),
            thinking: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
            provider_metadata: None,
        }];

        persist_new_messages(&store, "session-1", &messages).expect("messages should persist");

        let stored = store
            .load_messages("session-1")
            .expect("messages should load");
        assert_eq!(stored.len(), 1);
        assert!(stored[0]
            .tool_calls_json
            .as_deref()
            .expect("tool calls json should exist")
            .contains("\"echo\""));
    }

    #[test]
    fn delivery_platform_from_str_maps_known_destinations() {
        assert_eq!(
            delivery_platform_from_str("telegram"),
            DeliveryPlatform::Telegram
        );
        assert_eq!(
            delivery_platform_from_str("discord"),
            DeliveryPlatform::Discord
        );
        assert_eq!(
            delivery_platform_from_str("home-assistant"),
            DeliveryPlatform::HomeAssistant
        );
        assert_eq!(delivery_platform_from_str("unknown"), DeliveryPlatform::Cli);
    }

    #[tokio::test]
    async fn run_turn_with_runner_loads_history_and_persists_emitted_messages() {
        let dir = tempdir().expect("tempdir should exist");
        let data_dir = dir.path().join("data");
        let database_path = data_dir.join("genesis.db");
        bootstrap(&database_path).expect("bootstrap should succeed");

        let store = SessionStore::new(&database_path);
        store
            .create_session("session-1", "cli", None)
            .expect("session should be created");
        store
            .append_message("session-1", "user", Some("prior context"), None, None, None)
            .expect("prior message should persist");

        let loaded = test_loaded_config(data_dir, database_path.clone());
        let service = SessionExecutionService::new(&loaded);

        let outcome = service
            .run_turn_with_runner(
                SessionTurnInput {
                    session_id: "session-1",
                    session_platform: "cli",
                    delivery_platform: DeliveryPlatform::Cli,
                    prompt: "new prompt",
                    title: None,
                    images: Vec::new(),
                },
                |history| async move {
                    assert_eq!(history.len(), 1);
                    assert_eq!(history[0].content_text(), Some("prior context"));

                    Ok(ExecutedTurn {
                        result: AgentResult {
                            response: "done".to_owned(),
                            turns_used: 1,
                            tool_calls_made: 0,
                            finished_naturally: true,
                            total_input_tokens: 0,
                            total_output_tokens: 0,
                            estimated_cost: None,
                            pending_clarification: None,
                        },
                        emitted_messages: vec![
                            ChatMessage::user("new prompt"),
                            ChatMessage::assistant("done"),
                        ],
                    })
                },
            )
            .await
            .expect("execution should succeed");

        assert!(!outcome.created_session);
        assert_eq!(outcome.result.response, "done");

        let messages = store
            .load_messages("session-1")
            .expect("messages should load");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].content.as_deref(), Some("new prompt"));
        assert_eq!(messages[2].content.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn run_turn_with_runner_creates_missing_session() {
        let dir = tempdir().expect("tempdir should exist");
        let data_dir = dir.path().join("data");
        let database_path = data_dir.join("genesis.db");
        let loaded = test_loaded_config(data_dir, database_path.clone());
        let service = SessionExecutionService::new(&loaded);

        let outcome = service
            .run_turn_with_runner(
                SessionTurnInput {
                    session_id: "session-new",
                    session_platform: "api",
                    delivery_platform: DeliveryPlatform::Cli,
                    prompt: "hello",
                    title: Some("scheduled"),
                    images: Vec::new(),
                },
                |history| async move {
                    assert!(history.is_empty());

                    Ok(ExecutedTurn {
                        result: AgentResult {
                            response: "ok".to_owned(),
                            turns_used: 1,
                            tool_calls_made: 0,
                            finished_naturally: true,
                            total_input_tokens: 0,
                            total_output_tokens: 0,
                            estimated_cost: None,
                            pending_clarification: None,
                        },
                        emitted_messages: vec![ChatMessage::assistant("ok")],
                    })
                },
            )
            .await
            .expect("execution should succeed");

        assert!(outcome.created_session);

        let store = SessionStore::new(&database_path);
        let session = store
            .get_session("session-new")
            .expect("lookup should succeed")
            .expect("session should exist");
        assert_eq!(session.platform, "api");
    }

    #[test]
    fn generate_title_short_prompt_unchanged() {
        assert_eq!(generate_session_title("Hello world"), "Hello world");
    }

    #[test]
    fn generate_title_long_prompt_truncated_at_word_boundary() {
        let prompt = "This is a very long prompt that exceeds the sixty character limit and should be truncated";
        let title = generate_session_title(prompt);
        assert!(title.len() <= 63); // 60 + "..."
        assert!(title.ends_with("..."));
        // Should not cut in the middle of a word
        assert!(!title.contains("lim"));
    }

    #[test]
    fn generate_title_normalizes_whitespace() {
        assert_eq!(generate_session_title("  hello   world  "), "hello world");
    }

    #[test]
    fn generate_title_empty_prompt() {
        assert_eq!(generate_session_title(""), "");
    }

    #[test]
    fn skill_nudge_injects_after_complex_turn() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        bootstrap(&db_path).expect("bootstrap");
        let store = SessionStore::new(&db_path);
        store.create_session("s1", "cli", None).unwrap();

        let result = AgentResult {
            response: "done".to_owned(),
            turns_used: 4,
            tool_calls_made: 6,
            finished_naturally: true,
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost: None,
            pending_clarification: None,
        };

        maybe_inject_skill_nudge(&store, "s1", &result);

        let messages = store.load_messages("s1").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "system");
        assert!(messages[0]
            .content
            .as_deref()
            .unwrap()
            .contains("skill_create"));
    }

    #[test]
    fn skill_nudge_skips_simple_turn() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        bootstrap(&db_path).expect("bootstrap");
        let store = SessionStore::new(&db_path);
        store.create_session("s1", "cli", None).unwrap();

        let result = AgentResult {
            response: "done".to_owned(),
            turns_used: 1,
            tool_calls_made: 2,
            finished_naturally: true,
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost: None,
            pending_clarification: None,
        };

        maybe_inject_skill_nudge(&store, "s1", &result);

        let messages = store.load_messages("s1").unwrap();
        assert!(messages.is_empty(), "no nudge for simple turns");
    }

    #[test]
    fn skill_nudge_skips_unfinished_turn() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        bootstrap(&db_path).expect("bootstrap");
        let store = SessionStore::new(&db_path);
        store.create_session("s1", "cli", None).unwrap();

        let result = AgentResult {
            response: "hit turn limit".to_owned(),
            turns_used: 5,
            tool_calls_made: 10,
            finished_naturally: false,
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost: None,
            pending_clarification: None,
        };

        maybe_inject_skill_nudge(&store, "s1", &result);

        let messages = store.load_messages("s1").unwrap();
        assert!(messages.is_empty(), "no nudge for unfinished turns");
    }

    #[test]
    fn personality_override_takes_precedence() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let db_path = data_dir.join("genesis.db");
        let loaded = LoadedConfig {
            config: GenesisConfig {
                schema_version: 1,
                profile: "operator".to_owned(),
                provider: ProviderConfig {
                    backend: "openai".to_owned(),
                    model: "gpt-4.1-mini".to_owned(),
                    base_url: Some("http://localhost:8000/v1".to_owned()),
                    api_key_env: None,
                    extra_body: None,
                    tool_call_parser: None,
                    circuit_breaker: None,
                },
                tool_provider: None,
                fallback_providers: Vec::new(),
                mcp_servers: std::collections::HashMap::new(),
                storage: StorageConfig {
                    data_dir: data_dir.clone(),
                    database_path: db_path.clone(),
                },
                runtime: RuntimeConfig {
                    max_concurrency: 4,
                    allow_destructive_tools: false,
                    max_turns: 20,
                    max_context_messages: None,
                    budget_limit: None,
                    terminal: None,
                    thinking_budget: None,
                    max_context_tokens: None,
                    max_iterations: None,
                    context_security: genesis_config::ContextSecurityPolicy::default(),
                    reasoning_effort: None,
                    cache: None,
                    tool_filter: None,
                    guardrails: None,
                    core_tools: None,
                    batch: None,
                    tool_policy_path: None,
                },
                gateway: None,
                toolsets: std::collections::HashMap::new(),
                personality: Some("default".to_owned()),
                embedding: None,
                display: genesis_config::DisplayConfig::default(),
                tui: genesis_config::TuiConfig::default(),
                telemetry: None,
                routing: None,
            },
            paths: AppPaths {
                config_path: PathBuf::from("/tmp/genesis/config.yaml"),
                data_dir,
                database_path: db_path,
            },
        };

        let mut service = SessionExecutionService::new(&loaded);
        service.set_personality_override("pirate".to_owned());
        // The personality_override field should be set
        assert_eq!(service.personality_override.as_deref(), Some("pirate"));
    }

    #[test]
    fn terminal_config_to_backend_singularity() {
        use super::terminal_config_to_backend;

        let config = genesis_config::TerminalConfig::Singularity {
            image: "docker://ubuntu:22.04".to_owned(),
            cpu: 2.0,
            memory_mb: 8192,
            persistent: true,
            bind: Some(vec!["/data:/data".to_owned()]),
            working_dir: Some("/workspace".to_owned()),
        };
        let backend = terminal_config_to_backend(&config);
        match backend {
            genesis_tools::TerminalBackend::Singularity {
                image,
                cpu,
                memory_mb,
                persistent,
                ..
            } => {
                assert_eq!(image, "docker://ubuntu:22.04");
                assert_eq!(cpu, 2.0);
                assert_eq!(memory_mb, 8192);
                assert!(persistent);
            }
            _ => panic!("expected Singularity"),
        }
    }

    #[test]
    fn terminal_config_to_backend_modal() {
        use super::terminal_config_to_backend;

        let config = genesis_config::TerminalConfig::Modal {
            image: Some("python:3.11".to_owned()),
            cpu: 1.0,
            memory_mb: 5120,
            disk_mb: 51200,
            persistent: true,
            gpu: Some("T4".to_owned()),
            app: None,
            working_dir: None,
        };
        let backend = terminal_config_to_backend(&config);
        assert!(matches!(
            backend,
            genesis_tools::TerminalBackend::Modal { .. }
        ));
    }

    #[test]
    fn terminal_config_to_backend_daytona() {
        use super::terminal_config_to_backend;

        let config = genesis_config::TerminalConfig::Daytona {
            image: Some("ubuntu:22.04".to_owned()),
            cpu: 1.0,
            memory_mb: 5120,
            disk_mb: 10240,
            persistent: true,
            target: None,
            api_url: None,
            working_dir: None,
        };
        let backend = terminal_config_to_backend(&config);
        assert!(matches!(
            backend,
            genesis_tools::TerminalBackend::Daytona { .. }
        ));
    }
}
