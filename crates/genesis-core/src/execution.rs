use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use genesis_config::LoadedConfig;
use genesis_provider::{client_from_config, ChatMessage, ProviderError};
use genesis_storage::{
    bootstrap, format_user_traits, SessionStore, StorageError, StoredMessage, SubagentStore,
    UserModelStore,
};
use genesis_types::DeliveryPlatform;
use thiserror::Error;
use tracing::{debug, error, info, info_span, warn, Instrument};

use genesis_mcp::McpManager;

use crate::agent_loop::{AgentError, AgentLoop, AgentLoopConfig, AgentResult, SubagentSpawner};
use crate::prompt::{build_system_prompt_complete, load_context_file};
use crate::skills::load_skills_prompt;
use crate::{build_default_tool_runtime, build_execution_context_from_loaded};

pub struct SessionExecutionService<'a> {
    loaded: &'a LoadedConfig,
    mcp: Option<Arc<McpManager>>,
}

#[derive(Debug, Clone)]
pub struct SessionTurnInput<'a> {
    pub session_id: &'a str,
    pub session_platform: &'a str,
    pub delivery_platform: DeliveryPlatform,
    pub prompt: &'a str,
    pub title: Option<&'a str>,
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
}

impl<'a> SessionExecutionService<'a> {
    pub fn new(loaded: &'a LoadedConfig) -> Self {
        Self { loaded, mcp: None }
    }

    /// Create a service with MCP servers connected.
    pub async fn with_mcp(loaded: &'a LoadedConfig) -> Self {
        let mcp = if !loaded.config.mcp_servers.is_empty() {
            let configs: Vec<genesis_mcp::McpServerConfig> = loaded
                .config
                .mcp_servers
                .iter()
                .filter_map(|(name, cfg)| {
                    let command = cfg.command.as_ref()?;
                    Some(genesis_mcp::McpServerConfig {
                        name: name.clone(),
                        command: command.clone(),
                        args: cfg.args.clone().unwrap_or_default(),
                        env: cfg.env.clone().unwrap_or_default(),
                        connect_timeout: std::time::Duration::from_secs(
                            cfg.connect_timeout.unwrap_or(60),
                        ),
                        call_timeout: std::time::Duration::from_secs(
                            cfg.timeout.unwrap_or(120),
                        ),
                    })
                })
                .collect();

            if configs.is_empty() {
                None
            } else {
                let manager = McpManager::connect_all(configs).await;
                if manager.tool_count().await > 0 {
                    info!(
                        servers = manager.server_count().await,
                        tools = manager.tool_count().await,
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

        Self { loaded, mcp }
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

    pub fn load_history(&self, session_id: &str) -> Result<Vec<ChatMessage>, SessionExecutionError> {
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
        input: SessionTurnInput<'_>,
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

        let outcome = self.run_turn_with_runner(input, |history| async move {
            let mut agent = self.build_agent_loop(session_id, platform, history)?;
            let start_index = agent.messages().len();
            let result = agent.run_turn(&prompt).await?;
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
        input: SessionTurnInput<'_>,
        on_chunk: F,
    ) -> Result<SessionTurnOutcome, SessionExecutionError>
    where
        F: FnMut(&str),
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

        let outcome = self.run_turn_streaming_with_runner(input, on_chunk, |history, on_chunk| async move {
            let mut agent = self.build_agent_loop(session_id, platform, history)?;
            let start_index = agent.messages().len();
            let result = agent.run_turn_streaming(&prompt, on_chunk).await?;
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

    fn session_store(&self) -> SessionStore {
        SessionStore::new(&self.loaded.config.storage.database_path)
    }

    fn build_agent_loop(
        &self,
        session_id: String,
        platform: DeliveryPlatform,
        history: Vec<ChatMessage>,
    ) -> Result<AgentLoop, SessionExecutionError> {
        let execution_context =
            build_execution_context_from_loaded(self.loaded, session_id, platform);
        let mut tool_runtime = build_default_tool_runtime(&execution_context);

        // Attach MCP manager if we connected any servers at service creation
        if let Some(mcp) = &self.mcp {
            tool_runtime.set_mcp(Arc::clone(mcp));
        }

        // Load skills, user model, and project context for prompt personalization
        let db_path = &self.loaded.config.storage.database_path;
        let skills_section = load_skills_prompt(db_path);
        let user_model_section = self.load_user_model_section();
        let context_section = load_context_file(std::path::Path::new("."));

        let system_prompt = build_system_prompt_complete(
            &execution_context.plan.profile,
            &tool_runtime.definitions(),
            None,
            skills_section.as_deref(),
            user_model_section.as_deref(),
            context_section.as_deref(),
        );
        let client = client_from_config(
            &self.loaded.config.provider.backend,
            &self.loaded.config.provider.model,
            self.loaded.config.provider.base_url.as_deref(),
            self.loaded.config.provider.api_key_env.as_deref(),
        )?;
        debug!(
            provider_backend = %self.loaded.config.provider.backend,
            model = %self.loaded.config.provider.model,
            "built agent loop dependencies"
        );

        let mut agent = AgentLoop::with_history(
            client,
            tool_runtime,
            AgentLoopConfig {
                system_prompt: Some(system_prompt),
                ..AgentLoopConfig::default()
            },
            history,
        );

        // Set up tool provider routing if configured
        if let Some(tp) = &self.loaded.config.tool_provider {
            let tool_client = client_from_config(
                &tp.backend,
                &tp.model,
                tp.base_url.as_deref(),
                tp.api_key_env.as_deref(),
            )?;
            agent.set_tool_client(tool_client);
            debug!(
                tool_provider_backend = %tp.backend,
                tool_model = %tp.model,
                "multi-provider routing enabled"
            );
        }

        // Attach subagent spawner so agent can spawn parallel workstreams
        agent.set_subagent_spawner(Arc::new(ExecutionSubagentSpawner {
            loaded: Arc::new(self.loaded.clone()),
        }));

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
        // Persist token usage
        if let Err(e) = store.add_usage(
            input.session_id,
            executed.result.total_input_tokens,
            executed.result.total_output_tokens,
        ) {
            warn!(error = %e, "failed to persist token usage");
        }
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
        G: FnMut(&str),
    {
        let created_session =
            self.ensure_session(input.session_id, input.session_platform, input.title)?;
        let history = self.load_history(input.session_id)?;
        debug!(history_messages = history.len(), "starting streaming turn execution");
        let executed = runner(history, on_chunk).await?;
        let store = self.session_store();
        persist_new_messages(&store, input.session_id, &executed.emitted_messages)?;
        // Persist token usage
        if let Err(e) = store.add_usage(
            input.session_id,
            executed.result.total_input_tokens,
            executed.result.total_output_tokens,
        ) {
            warn!(error = %e, "failed to persist token usage");
        }
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
}

/// Spawns child agent loops as background tokio tasks.
///
/// When the parent agent calls `spawn_subagent`, the agent loop detects
/// the output metadata and calls `SubagentSpawner::spawn`, which creates
/// a new `AgentLoop` and runs it in the background.
struct ExecutionSubagentSpawner {
    loaded: Arc<LoadedConfig>,
}

impl SubagentSpawner for ExecutionSubagentSpawner {
    fn spawn(&self, child_session_id: &str, subagent_id: &str, task: &str) {
        let loaded = Arc::clone(&self.loaded);
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
                let execution_context = build_execution_context_from_loaded(
                    &loaded,
                    child_session_id.clone(),
                    DeliveryPlatform::Cli,
                );
                let tool_runtime = build_default_tool_runtime(&execution_context);

                let system_prompt = format!(
                    "You are a subagent — a focused worker spawned by a parent agent to handle a specific task. \
                     Complete the task below thoroughly and concisely. You have access to the same tools as the parent agent.\n\n\
                     ## Your Task\n{task}"
                );

                let client = match client_from_config(
                    &loaded.config.provider.backend,
                    &loaded.config.provider.model,
                    loaded.config.provider.base_url.as_deref(),
                    loaded.config.provider.api_key_env.as_deref(),
                ) {
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
                );

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

        store.append_message(
            session_id,
            &message.role,
            message.content.as_deref(),
            message.tool_call_id.as_deref(),
            tool_calls_json.as_deref(),
        )?;
    }

    debug!(session_id, persisted_messages = messages.len(), "persisted new messages");

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
                content: message.content,
                tool_calls,
                tool_call_id: message.tool_call_id,
                name: None,
            })
        })
        .collect()
}

pub fn delivery_platform_from_str(raw: &str) -> DeliveryPlatform {
    match raw.trim().to_ascii_lowercase().as_str() {
        "telegram" => DeliveryPlatform::Telegram,
        "discord" => DeliveryPlatform::Discord,
        "slack" => DeliveryPlatform::Slack,
        "homeassistant" | "home_assistant" | "home-assistant" => {
            DeliveryPlatform::HomeAssistant
        }
        "whatsapp" => DeliveryPlatform::WhatsApp,
        _ => DeliveryPlatform::Cli,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        delivery_platform_from_str, persist_new_messages, restore_chat_history, ExecutedTurn,
        SessionExecutionService, SessionTurnInput,
    };
    use crate::agent_loop::AgentResult;
    use genesis_config::{
        AppPaths, GenesisConfig, LoadedConfig, ProviderConfig, RuntimeConfig, StorageConfig,
    };
    use genesis_provider::ChatMessage;
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
            content: Some("hello".to_owned()),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
        }];

        persist_new_messages(&store, "session-1", &messages)
            .expect("messages should persist");

        let stored = store
            .load_messages("session-1")
            .expect("messages should load");
        assert_eq!(stored.len(), 1);
        assert!(
            stored[0]
                .tool_calls_json
                .as_deref()
                .expect("tool calls json should exist")
                .contains("\"echo\"")
        );
    }

    #[test]
    fn delivery_platform_from_str_maps_known_destinations() {
        assert_eq!(delivery_platform_from_str("telegram"), DeliveryPlatform::Telegram);
        assert_eq!(delivery_platform_from_str("discord"), DeliveryPlatform::Discord);
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
            .append_message("session-1", "user", Some("prior context"), None, None)
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
                },
                |history| async move {
                    assert_eq!(history.len(), 1);
                    assert_eq!(history[0].content.as_deref(), Some("prior context"));

                    Ok(ExecutedTurn {
                        result: AgentResult {
                            response: "done".to_owned(),
                            turns_used: 1,
                            tool_calls_made: 0,
                            finished_naturally: true,
                            total_input_tokens: 0,
                            total_output_tokens: 0,
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

    fn test_loaded_config(data_dir: PathBuf, database_path: PathBuf) -> LoadedConfig {
        LoadedConfig {
            config: GenesisConfig {
                schema_version: 1,
                profile: "operator".to_owned(),
                provider: ProviderConfig {
                    backend: "openai".to_owned(),
                    model: "gpt-4.1-mini".to_owned(),
                    base_url: Some("http://localhost:8000/v1".to_owned()),
                    api_key_env: None,
                },
                tool_provider: None,
                mcp_servers: std::collections::HashMap::new(),
                storage: StorageConfig {
                    data_dir: data_dir.clone(),
                    database_path: database_path.clone(),
                },
                runtime: RuntimeConfig {
                    max_concurrency: 4,
                    allow_destructive_tools: false,
                },
            },
            paths: AppPaths {
                config_path: PathBuf::from("/tmp/genesis/config.yaml"),
                data_dir,
                database_path,
            },
        }
    }
}
