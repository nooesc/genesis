use genesis_config::LoadedConfig;
use genesis_provider::{client_from_config, ChatMessage, ProviderError};
use genesis_storage::{bootstrap, SessionStore, StorageError, StoredMessage};
use genesis_types::DeliveryPlatform;
use thiserror::Error;

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
        bootstrap(&self.loaded.config.storage.database_path)?;

        let store = self.session_store();
        if store.get_session(session_id)?.is_some() {
            return Ok(false);
        }

        store.create_session(session_id, platform, title)?;
        Ok(true)
    }

    pub fn load_history(&self, session_id: &str) -> Result<Vec<ChatMessage>, SessionExecutionError> {
        bootstrap(&self.loaded.config.storage.database_path)?;
        let store = self.session_store();
        let messages = store.load_messages(session_id)?;
        restore_chat_history(messages)
    }

    pub async fn run_turn(
        &self,
        input: SessionTurnInput<'_>,
    ) -> Result<SessionTurnOutcome, SessionExecutionError> {
        let created_session =
            self.ensure_session(input.session_id, input.session_platform, input.title)?;
        let history = self.load_history(input.session_id)?;
        let mut agent = self.build_agent_loop(
            input.session_id.to_owned(),
            input.delivery_platform,
            history,
        )?;
        let start_index = agent.messages().len();
        let result = agent.run_turn(input.prompt).await?;
        let store = self.session_store();
        persist_new_messages(&store, input.session_id, &agent.messages()[start_index..])?;

        Ok(SessionTurnOutcome {
            session_id: input.session_id.to_owned(),
            created_session,
            result,
        })
    }

    pub async fn run_turn_streaming<F>(
        &self,
        input: SessionTurnInput<'_>,
        on_chunk: F,
    ) -> Result<SessionTurnOutcome, SessionExecutionError>
    where
        F: FnMut(&str),
    {
        let created_session =
            self.ensure_session(input.session_id, input.session_platform, input.title)?;
        let history = self.load_history(input.session_id)?;
        let mut agent = self.build_agent_loop(
            input.session_id.to_owned(),
            input.delivery_platform,
            history,
        )?;
        let start_index = agent.messages().len();
        let result = agent.run_turn_streaming(input.prompt, on_chunk).await?;
        let store = self.session_store();
        persist_new_messages(&store, input.session_id, &agent.messages()[start_index..])?;

        Ok(SessionTurnOutcome {
            session_id: input.session_id.to_owned(),
            created_session,
            result,
        })
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
}

pub fn persist_new_messages(
    store: &SessionStore,
    session_id: &str,
    messages: &[ChatMessage],
) -> Result<(), SessionExecutionError> {
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
    use super::{delivery_platform_from_str, persist_new_messages, restore_chat_history};
    use genesis_provider::ChatMessage;
    use genesis_storage::{bootstrap, SessionStore, StoredMessage};
    use genesis_types::DeliveryPlatform;
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
}
