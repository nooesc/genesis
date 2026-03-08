use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use genesis_config::load;
use genesis_core::agent_loop::{AgentError, AgentLoop, AgentLoopConfig};
use genesis_core::prompt::{agent_name, build_system_prompt};
use genesis_core::{build_default_tool_runtime, build_execution_context_from_loaded, run_doctor};
use genesis_provider::{client_from_config, ChatMessage, ProviderError};
use genesis_storage::{bootstrap, SessionStore, StorageError};
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
    #[error("failed to encode json output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to encode yaml output: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

pub async fn run(cli: Cli) -> Result<String, CliError> {
    match cli.command {
        Command::Chat { session_id } => run_chat(cli.config, session_id).await,
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
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;

    let session_id = session_id.unwrap_or_else(default_session_id);
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
    let mut agent = AgentLoop::new(
        client,
        tool_runtime,
        AgentLoopConfig {
            system_prompt: Some(system_prompt),
            ..AgentLoopConfig::default()
        },
    );

    let store = SessionStore::new(&loaded.config.storage.database_path);
    store.create_session(&session_id, "cli", None)?;

    println!(
        "Starting session `{session_id}` with {}. Type `exit` or `quit` to leave.",
        agent_name()
    );

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
        let result = agent.run_turn(trimmed).await?;
        persist_new_messages(&store, &session_id, &agent.messages()[start_index..])?;
        println!("eve> {}", result.response);
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
    use super::{default_session_id, is_exit_command, run, BootstrapCommand, Cli, Command, StorageCommand};
    use clap::Parser;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_chat_command() {
        let cli = Cli::try_parse_from(["genesis", "chat", "--session-id", "session-42"])
            .expect("chat command should parse");

        match cli.command {
            Command::Chat { session_id } => {
                assert_eq!(session_id.as_deref(), Some("session-42"));
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
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
