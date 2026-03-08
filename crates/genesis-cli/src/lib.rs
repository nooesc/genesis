use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use genesis_config::load;
use genesis_core::agent_loop::{AgentError, AgentLoop, AgentLoopConfig};
use genesis_core::prompt::{agent_name, build_system_prompt};
use genesis_core::{build_default_tool_runtime, build_execution_context_from_loaded, run_doctor};
use genesis_provider::{client_from_config, ChatMessage, ProviderError};
use genesis_storage::{bootstrap, SessionStore, SessionSummary, StorageError, StoredMessage};
use genesis_types::DeliveryPlatform;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(name = "genesis", version, about = "Rust-native Genesis bootstrap CLI")]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true, help = "Render machine-readable JSON output")]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Start an interactive Eve chat session")]
    Chat {
        #[arg(long, help = "Override the generated session id")]
        session_id: Option<String>,
        #[arg(long, help = "Resume an existing session instead of creating a new one")]
        resume: Option<String>,
    },
    #[command(about = "Inspect local config and storage readiness")]
    Doctor {
        #[arg(long, help = "Create the SQLite schema if it does not exist yet")]
        bootstrap_storage: bool,
    },
    #[command(subcommand, about = "Inspect resolved configuration")]
    Config(ConfigCommand),
    #[command(subcommand, about = "Inspect and bootstrap storage paths")]
    Storage(StorageCommand),
    #[command(subcommand, about = "Inspect recent saved sessions")]
    Sessions(SessionsCommand),
    #[command(subcommand, about = "Print starter assets for first-time setup")]
    Bootstrap(BootstrapCommand),
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Print the resolved config path")]
    Path,
    #[command(about = "Print the resolved configuration")]
    Show,
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    #[command(about = "Print the resolved sqlite database path")]
    Path,
    #[command(about = "Create the sqlite schema and print the resulting health report")]
    Bootstrap,
}

#[derive(Debug, Subcommand)]
pub enum BootstrapCommand {
    #[command(about = "Print a starter config using the resolved defaults")]
    Config,
}

#[derive(Debug, Subcommand)]
pub enum SessionsCommand {
    #[command(about = "List recent sessions")]
    List,
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Config(#[from] genesis_config::ConfigError),
    #[error(transparent)]
    Doctor(#[from] genesis_core::DoctorError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("session `{0}` was not found")]
    SessionNotFound(String),
    #[error("failed to encode json output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to encode yaml output: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

pub async fn run(cli: Cli) -> Result<String, CliError> {
    match cli.command {
        Command::Chat { session_id, resume } => run_chat(cli.config, session_id, resume).await,
        Command::Doctor { bootstrap_storage } => {
            let report = run_doctor(cli.config.as_deref(), bootstrap_storage)?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&report)?)
            } else {
                Ok(format_doctor_report(&report))
            }
        }
        Command::Config(ConfigCommand::Path) => {
            let loaded = load(cli.config.as_deref())?;
            Ok(loaded.paths.config_path.display().to_string())
        }
        Command::Config(ConfigCommand::Show) => {
            let loaded = load(cli.config.as_deref())?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&loaded.config)?)
            } else {
                Ok(serde_yaml::to_string(&loaded.config)?)
            }
        }
        Command::Storage(StorageCommand::Path) => {
            let loaded = load(cli.config.as_deref())?;
            Ok(loaded.config.storage.database_path.display().to_string())
        }
        Command::Storage(StorageCommand::Bootstrap) => {
            let report = run_doctor(cli.config.as_deref(), true)?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&report)?)
            } else {
                Ok(format_bootstrap_report(&report))
            }
        }
        Command::Sessions(SessionsCommand::List) => {
            let loaded = load(cli.config.as_deref())?;
            let store = SessionStore::new(&loaded.config.storage.database_path);
            let sessions = store.list_recent_sessions(20)?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&sessions)?)
            } else {
                Ok(format_session_list(&sessions))
            }
        }
        Command::Bootstrap(BootstrapCommand::Config) => {
            let loaded = load(cli.config.as_deref())?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&loaded.config)?)
            } else {
                Ok(serde_yaml::to_string(&loaded.config)?)
            }
        }
    }
}

async fn run_chat(
    config_path: Option<PathBuf>,
    session_id: Option<String>,
    resume: Option<String>,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;
    let store = SessionStore::new(&loaded.config.storage.database_path);

    let (session_id, existing_messages, is_resumed) = match resume {
        Some(resume_id) => {
            let session = store
                .get_session(&resume_id)?
                .ok_or_else(|| CliError::SessionNotFound(resume_id.clone()))?;
            let messages = store.load_messages(&resume_id)?;
            (session.id, restore_chat_history(messages)?, true)
        }
        None => (session_id.unwrap_or_else(default_session_id), Vec::new(), false),
    };

    let execution_context =
        build_execution_context_from_loaded(&loaded, session_id.clone(), DeliveryPlatform::Cli);
    let tool_runtime = build_default_tool_runtime(&execution_context);
    let system_prompt = build_system_prompt(
        &execution_context.plan.profile,
        &tool_runtime.definitions(),
        None,
    );

    let client = client_from_config(
        &loaded.config.provider.backend,
        &loaded.config.provider.model,
        loaded.config.provider.base_url.as_deref(),
        loaded.config.provider.api_key_env.as_deref(),
    )?;
    let mut agent = AgentLoop::with_history(
        client,
        tool_runtime,
        AgentLoopConfig {
            system_prompt: Some(system_prompt),
            ..AgentLoopConfig::default()
        },
        existing_messages,
    );

    if !is_resumed {
        store.create_session(&session_id, "cli", None)?;
    }

    if is_resumed {
        println!(
            "Resuming session `{session_id}` with {}. Type `exit` or `quit` to leave.",
            agent_name()
        );
    } else {
        println!(
            "Starting session `{session_id}` with {}. Type `exit` or `quit` to leave.",
            agent_name()
        );
    }

    loop {
        let input = read_user_input("you> ")?;
        let trimmed = input.trim();

        if trimmed.is_empty() {
            continue;
        }

        if is_exit_command(trimmed) {
            break;
        }

        let start_index = agent.messages().len();
        let mut streamed = false;
        let result = agent
            .run_turn_streaming(trimmed, |chunk| {
                if !streamed {
                    print!("eve> ");
                    streamed = true;
                }
                print!("{chunk}");
                let _ = io::stdout().flush();
            })
            .await?;
        persist_new_messages(&store, &session_id, &agent.messages()[start_index..])?;
        if streamed {
            println!();
        } else {
            println!("eve> {}", result.response);
        }
    }

    Ok(format!("chat session saved as {session_id}"))
}

fn read_user_input(prompt: &str) -> Result<String, CliError> {
    print!("{prompt}");
    io::stdout().flush()?;

    let mut input = String::new();
    let bytes_read = io::stdin().read_line(&mut input)?;
    if bytes_read == 0 {
        return Ok("exit".to_owned());
    }

    Ok(input)
}

fn persist_new_messages(
    store: &SessionStore,
    session_id: &str,
    messages: &[ChatMessage],
) -> Result<(), CliError> {
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

fn restore_chat_history(messages: Vec<StoredMessage>) -> Result<Vec<ChatMessage>, CliError> {
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

fn default_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("cli-{timestamp}")
}

fn is_exit_command(input: &str) -> bool {
    matches!(input, "exit" | "quit" | "/exit" | "/quit")
}

fn format_session_list(sessions: &[SessionSummary]) -> String {
    if sessions.is_empty() {
        return "no saved sessions".to_owned();
    }

    let mut lines = vec!["genesis sessions".to_owned()];
    for session in sessions {
        lines.push(format!(
            "{}\t{}\t{}",
            session.id, session.platform, session.created_at
        ));
    }
    lines.join("\n")
}

fn format_doctor_report(report: &genesis_core::DoctorReport) -> String {
    let mut lines = vec![
        "genesis doctor".to_owned(),
        format!("profile: {}", report.profile),
        format!("provider: {} / {}", report.provider_backend, report.model),
        format!("config: {}", report.config_path),
        format!("data: {}", report.data_dir),
        format!("database: {}", report.database_path),
    ];

    for check in &report.checks {
        lines.push(format!(
            "- [{}] {}: {}",
            status_marker(&check.status),
            check.name,
            check.detail
        ));
    }

    lines.push(format!(
        "next-event-preview: {}",
        serde_json::to_string(&report.next_event_preview)
            .expect("runtime event preview should always serialize")
    ));

    lines.join("\n")
}

fn status_marker(status: &genesis_core::CheckStatus) -> &'static str {
    match status {
        genesis_core::CheckStatus::Pass => "ok",
        genesis_core::CheckStatus::Warn => "warn",
        genesis_core::CheckStatus::Fail => "fail",
    }
}

fn format_bootstrap_report(report: &genesis_core::DoctorReport) -> String {
    let mut lines = vec![
        "genesis storage bootstrap".to_owned(),
        format!("database: {}", report.database_path),
        format!("provider: {} / {}", report.provider_backend, report.model),
    ];

    for check in &report.checks {
        lines.push(format!(
            "- [{}] {}: {}",
            status_marker(&check.status),
            check.name,
            check.detail
        ));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        default_session_id, format_session_list, is_exit_command, restore_chat_history, run,
        BootstrapCommand, Cli, Command, SessionsCommand, StorageCommand,
    };
    use clap::Parser;
    use genesis_storage::{SessionSummary, StoredMessage};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_chat_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "chat",
            "--session-id",
            "session-42",
            "--resume",
            "session-1",
        ])
            .expect("chat command should parse");

        match cli.command {
            Command::Chat { session_id, resume } => {
                assert_eq!(session_id.as_deref(), Some("session-42"));
                assert_eq!(resume.as_deref(), Some("session-1"));
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn restores_chat_history_from_stored_messages() {
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
    fn exit_command_detection_covers_common_forms() {
        assert!(is_exit_command("exit"));
        assert!(is_exit_command("quit"));
        assert!(is_exit_command("/exit"));
        assert!(!is_exit_command("hello"));
    }

    #[test]
    fn default_session_id_uses_cli_prefix() {
        assert!(default_session_id().starts_with("cli-"));
    }

    #[tokio::test]
    async fn parses_storage_path_command() {
        let cli = Cli::try_parse_from(["genesis", "storage", "path"])
            .expect("storage path command should parse");

        match cli.command {
            Command::Storage(StorageCommand::Path) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn parses_bootstrap_config_command() {
        let cli = Cli::try_parse_from(["genesis", "bootstrap", "config"])
            .expect("bootstrap config command should parse");

        match cli.command {
            Command::Bootstrap(BootstrapCommand::Config) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn formats_session_list_for_humans() {
        let output = format_session_list(&[SessionSummary {
            id: "session-1".to_owned(),
            title: None,
            platform: "cli".to_owned(),
            created_at: "2026-03-08 12:00:00".to_owned(),
            updated_at: "2026-03-08 12:05:00".to_owned(),
        }]);

        assert!(output.contains("genesis sessions"));
        assert!(output.contains("session-1\tcli\t2026-03-08 12:00:00"));
    }

    #[tokio::test]
    async fn parses_sessions_list_command() {
        let cli = Cli::try_parse_from(["genesis", "sessions", "list"])
            .expect("sessions list command should parse");

        match cli.command {
            Command::Sessions(SessionsCommand::List) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bootstrap_config_renders_yaml_from_defaults() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
profile: bootstrapper
provider:
  backend: openrouter
  model: nous/hermes
"#,
        )
        .expect("config should be written");

        let output = run(Cli {
            config: Some(config_path),
            json: false,
            command: Command::Bootstrap(BootstrapCommand::Config),
        })
        .await
        .expect("bootstrap config command should succeed");

        assert!(output.contains("profile: bootstrapper"));
        assert!(output.contains("backend: openrouter"));
        assert!(output.contains("model: nous/hermes"));
    }

    #[tokio::test]
    async fn storage_bootstrap_renders_text_report() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        let database_path = dir.path().join("state").join("genesis.db");
        fs::write(
            &config_path,
            format!(
                r#"
storage:
  database_path: {}
"#,
                database_path.display()
            ),
        )
        .expect("config should be written");

        let output = run(Cli {
            config: Some(config_path),
            json: false,
            command: Command::Storage(StorageCommand::Bootstrap),
        })
        .await
        .expect("storage bootstrap should succeed");

        assert!(output.contains("genesis storage bootstrap"));
        assert!(output.contains(&database_path.display().to_string()));
        assert!(output.contains("[ok] storage"));
    }
}
