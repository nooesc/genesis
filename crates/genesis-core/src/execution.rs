use std::future::Future;

use genesis_config::LoadedConfig;
use genesis_provider::{client_from_config, ChatMessage, ProviderError};
use genesis_storage::{bootstrap, SessionStore, StorageError, StoredMessage};
use genesis_types::DeliveryPlatform;
use thiserror::Error;
use tracing::{debug, info, info_span, warn};

use crate::agent_loop::{AgentError, AgentLoop, AgentLoopConfig, AgentResult};
use crate::prompt::build_system_prompt;
use crate::{build_default_tool_runtime, build_execution_context_from_loaded};

pub struct SessionExecutionService<'a> {
    loaded: &'a LoadedConfig,
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
        Self { loaded }
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
        let _span = info_span!(
            "session.run_turn",
            session_id = input.session_id,
            session_platform = input.session_platform
        )
        .entered();
        let session_id = input.session_id.to_owned();
        let platform = input.delivery_platform.clone();
        let prompt = input.prompt.to_owned();

        self.run_turn_with_runner(input, |history| async move {
            let mut agent = self.build_agent_loop(session_id, platform, history)?;
            let start_index = agent.messages().len();
            let result = agent.run_turn(&prompt).await?;
            Ok(ExecutedTurn {
                result,
                emitted_messages: agent.messages()[start_index..].to_vec(),
            })
        })
        .await
    }

    pub async fn run_turn_streaming<F>(
        &self,
        input: SessionTurnInput<'_>,
        on_chunk: F,
    ) -> Result<SessionTurnOutcome, SessionExecutionError>
    where
        F: FnMut(&str),
    {
        let _span = info_span!(
            "session.run_turn_streaming",
            session_id = input.session_id,
            session_platform = input.session_platform
        )
        .entered();
        let session_id = input.session_id.to_owned();
        let platform = input.delivery_platform.clone();
        let prompt = input.prompt.to_owned();

        self.run_turn_streaming_with_runner(input, on_chunk, |history, on_chunk| async move {
            let mut agent = self.build_agent_loop(session_id, platform, history)?;
            let start_index = agent.messages().len();
            let result = agent.run_turn_streaming(&prompt, on_chunk).await?;
            Ok(ExecutedTurn {
                result,
                emitted_messages: agent.messages()[start_index..].to_vec(),
            })
        })
        .await
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
        let tool_runtime = build_default_tool_runtime(&execution_context);
        let system_prompt = build_system_prompt(
            &execution_context.plan.profile,
            &tool_runtime.definitions(),
            None,
        );
        let client = client_from_config(
            &self.loaded.config.provider.backend,
            &self.loaded.config.provider.model,
            self.loaded.config.provider.base_url.as_deref(),
            self.loaded.config.provider.api_key_env.as_deref(),
        )?;
        debug!(
            provider_backend = self.loaded.config.provider.backend,
            model = self.loaded.config.provider.model,
            "built agent loop dependencies"
        );

        Ok(AgentLoop::with_history(
            client,
            tool_runtime,
            AgentLoopConfig {
                system_prompt: Some(system_prompt),
                ..AgentLoopConfig::default()
            },
            history,
        ))
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
        info!(
            created_session,
            emitted_messages = executed.emitted_messages.len(),
            turns_used = executed.result.turns_used,
            tool_calls_made = executed.result.tool_calls_made,
            finished_naturally = executed.result.finished_naturally,
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
        info!(
            created_session,
            emitted_messages = executed.emitted_messages.len(),
            turns_used = executed.result.turns_used,
            tool_calls_made = executed.result.tool_calls_made,
            finished_naturally = executed.result.finished_naturally,
            "completed streaming turn execution"
        );

        Ok(SessionTurnOutcome {
            session_id: input.session_id.to_owned(),
            created_session,
            result: executed.result,
        })
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
