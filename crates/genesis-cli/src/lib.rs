use std::io::{self, Write};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, Timelike};
use clap::{Parser, Subcommand};
use genesis_config::{load, LoadedConfig};
use genesis_core::agent_loop::{AgentError, StreamEvent};
use genesis_core::execution::{
    delivery_platform_from_str, SessionExecutionError, SessionExecutionService, SessionTurnInput,
};
use genesis_core::prompt::{agent_name, load_context_file};
use genesis_core::scheduler::{check_due_schedules, CronTime};
use genesis_core::run_doctor;
use genesis_provider::ProviderError;
use genesis_storage::{
    bootstrap, InsightsData, MemoryStore, ScheduleStore, SessionStore, SessionSummary, SkillStore,
    StorageError, StoredSchedule, SubagentStore, UsageStats, UserModelStore,
};
use genesis_gateway::{AppState, build_router};
use genesis_types::DeliveryPlatform;
use rustyline::completion::{Completer, Pair};
use rustyline::hint::Hinter;
use rustyline::highlight::Highlighter;
use rustyline::validate::Validator;
use thiserror::Error;

/// Slash-command completer for the interactive chat TUI.
/// Provides tab-completion and inline hints for `/` commands.
struct SlashCompleter {
    commands: Vec<&'static str>,
}

impl SlashCompleter {
    fn new() -> Self {
        Self {
            commands: vec![
                "/help", "/history", "/export", "/tokens", "/session",
                "/new", "/undo", "/retry", "/fork", "/search",
                "/memories", "/compress", "/tools", "/skills", "/model",
                "/clear",
            ],
        }
    }
}

impl Completer for SlashCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if line.starts_with('/') && !line[1..].contains(' ') {
            let matches: Vec<Pair> = self
                .commands
                .iter()
                .filter(|cmd| cmd.starts_with(line))
                .map(|cmd| Pair {
                    display: cmd.to_string(),
                    replacement: cmd.to_string(),
                })
                .collect();
            Ok((0, matches))
        } else {
            Ok((pos, vec![]))
        }
    }
}

impl Hinter for SlashCompleter {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &rustyline::Context<'_>) -> Option<String> {
        if line.starts_with('/') && !line[1..].contains(' ') && pos == line.len() {
            self.commands
                .iter()
                .find(|cmd| cmd.starts_with(line) && cmd.len() > line.len())
                .map(|cmd| cmd[line.len()..].to_owned())
        } else {
            None
        }
    }
}

impl Highlighter for SlashCompleter {}
impl Validator for SlashCompleter {}
impl rustyline::Helper for SlashCompleter {}

/// Interactive approval handler for CLI mode. Prompts the user via stdin
/// when a tool requires explicit confirmation (e.g., send_message).
struct CliApprovalHandler;

impl genesis_tools::ApprovalHandler for CliApprovalHandler {
    fn request_approval(&self, tool_name: &str, arguments: &std::collections::BTreeMap<String, String>) -> bool {
        eprintln!("\n[Tool approval required] {tool_name}");
        for (key, value) in arguments {
            let display = match value.char_indices().nth(100) {
                Some((i, _)) => format!("{}...", &value[..i]),
                None => value.clone(),
            };
            eprintln!("  {key}: {display}");
        }
        eprint!("Allow this tool call? [y/N] ");
        let _ = io::stderr().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return false;
        }
        matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
    }
}

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
        #[arg(short, long, help = "Send an initial prompt before entering interactive mode")]
        prompt: Option<String>,
        #[arg(long, help = "Override the system prompt / agent identity")]
        system: Option<String>,
        #[arg(long, help = "Resume the most recent session")]
        last: bool,
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
    #[command(subcommand, about = "Inspect and manage saved skills")]
    Skills(SkillsCommand),
    #[command(subcommand, about = "Inspect and manage project context files")]
    Context(ContextCommand),
    #[command(about = "List all available tools")]
    Tools,
    #[command(about = "Show system overview (profile, model, tools, sessions, skills)")]
    Info,
    #[command(subcommand, about = "Manage scheduled prompts")]
    Schedule(ScheduleCommand),
    #[command(about = "Start the HTTP API server")]
    Serve {
        #[arg(long, default_value = "0.0.0.0", help = "Host to bind")]
        host: String,
        #[arg(long, default_value = "3000", help = "Port to listen on")]
        port: u16,
    },
    #[command(subcommand, about = "Manage the active LLM provider and model")]
    Model(ModelCommand),
    #[command(subcommand, about = "Inspect spawned subagents")]
    Subagents(SubagentsCommand),
    #[command(about = "Run a self-reflection nudge to consolidate learning")]
    Nudge,
    #[command(about = "Show usage insights and analytics")]
    Insights {
        #[arg(long, default_value = "30", help = "Number of days to analyze (default: 30)")]
        days: u32,
    },
    #[command(about = "Initialize Genesis — interactive setup wizard (or pass flags for non-interactive)")]
    Init {
        #[arg(long, help = "LLM provider backend (e.g. openai, openrouter, anthropic)")]
        backend: Option<String>,
        #[arg(long, help = "Model name (e.g. gpt-4.1-mini, claude-sonnet-4-6)")]
        model: Option<String>,
        #[arg(long, help = "Base URL for the provider API")]
        base_url: Option<String>,
        #[arg(long, help = "Environment variable holding the API key")]
        api_key_env: Option<String>,
    },
    #[command(about = "Run the interactive setup wizard (alias for `init`)", hide = true)]
    Setup {
        #[arg(long, help = "LLM provider backend (e.g. openai, openrouter, anthropic)")]
        backend: Option<String>,
        #[arg(long, help = "Model name (e.g. gpt-4.1-mini, claude-sonnet-4-6)")]
        model: Option<String>,
        #[arg(long, help = "Base URL for the provider API")]
        base_url: Option<String>,
        #[arg(long, help = "Environment variable holding the API key")]
        api_key_env: Option<String>,
    },
    #[command(subcommand, about = "Print starter assets for first-time setup")]
    Bootstrap(BootstrapCommand),
    #[command(about = "Run a single prompt non-interactively and print the response")]
    Run {
        /// The prompt to send to the agent.
        prompt: String,
        #[arg(long, help = "Session ID to use (creates a new session if not found)")]
        session_id: Option<String>,
        #[arg(long, help = "Print raw response without metadata")]
        raw: bool,
        #[arg(long, help = "Override the system prompt / agent identity")]
        system: Option<String>,
        #[arg(long, help = "Stream output as it arrives (default: wait for full response)")]
        stream: bool,
        #[arg(short = 'i', long = "image", help = "Attach an image file or URL to the prompt (can be repeated)")]
        images: Vec<String>,
    },
    #[command(about = "Show status dashboard of all Genesis components")]
    Status,
    #[command(about = "Generate agent training trajectories from a JSONL prompt file")]
    Batch {
        #[arg(long, help = "Input JSONL file where each line is {\"prompt\": ..., \"tags\": [...]}")]
        input: String,
        #[arg(long, help = "Output directory for saved trajectory files")]
        output: String,
        #[arg(long, help = "Override the model used for generation")]
        model: Option<String>,
        #[arg(long, help = "Override max turns per prompt")]
        max_turns: Option<usize>,
        #[arg(long, help = "Maximum number of prompts to run concurrently")]
        concurrency: Option<usize>,
    },
    #[command(about = "Compress a trajectory JSON file for training/export")]
    Compress {
        #[arg(long, help = "Input trajectory JSON file")]
        input: String,
        #[arg(long, help = "Optional output file path; writes to stdout when omitted")]
        output: Option<String>,
        #[arg(long, help = "Compression level: light, medium, or heavy")]
        level: Option<String>,
        #[arg(long, help = "Output format: json, sharegpt, or chatml")]
        format: Option<String>,
    },
    #[command(about = "Update Genesis to the latest version from source")]
    Update,
    #[command(subcommand, about = "Inspect and manage stored memories")]
    Memory(MemoryCommand),
    #[command(subcommand, about = "Inspect configured MCP servers")]
    Mcp(McpCommand),
    #[command(about = "Benchmark provider latency with a simple completion request")]
    Benchmark {
        #[arg(long, default_value = "3", help = "Number of requests to run")]
        runs: usize,
        #[arg(long, help = "Also benchmark the tool provider if configured")]
        tool_provider: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    #[command(about = "List configured MCP servers")]
    List,
    #[command(about = "Test connectivity to all configured MCP servers")]
    Test,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    #[command(about = "List stored memories")]
    List {
        #[arg(long, default_value = "50", help = "Maximum number of memories to show")]
        limit: usize,
    },
    #[command(about = "Search memories using full-text search")]
    Search {
        /// Search query
        query: String,
        #[arg(long, default_value = "10", help = "Maximum results to return")]
        limit: usize,
    },
    #[command(about = "Delete a memory by ID")]
    Delete {
        /// Memory ID to delete
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Print the resolved config path")]
    Path,
    #[command(about = "Print the resolved configuration")]
    Show,
    #[command(about = "Open the config file in $EDITOR")]
    Edit,
    #[command(about = "Set a config value (dot-notation: provider.model, runtime.max_turns, etc.)")]
    Set {
        /// Config key in dot-notation (e.g. provider.model, runtime.max_turns)
        key: String,
        /// Value to set
        value: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    #[command(about = "Print the resolved sqlite database path")]
    Path,
    #[command(about = "Create the sqlite schema and print the resulting health report")]
    Bootstrap,
}

#[derive(Debug, Subcommand)]
pub enum ModelCommand {
    #[command(about = "Show the active provider and model")]
    Show,
    #[command(about = "List popular models grouped by provider")]
    List {
        #[arg(long, help = "Filter by provider backend (e.g. openai, anthropic, google)")]
        backend: Option<String>,
    },
    #[command(about = "Switch the active model (persisted to config file)")]
    Set {
        #[arg(help = "Model name (e.g. gpt-4.1-mini, claude-sonnet-4-6)")]
        model: String,
        #[arg(long, help = "Provider backend (e.g. openai, openrouter, anthropic)")]
        backend: Option<String>,
        #[arg(long, help = "Base URL override for the provider API")]
        base_url: Option<String>,
        #[arg(long, help = "Environment variable holding the API key")]
        api_key_env: Option<String>,
    },
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
    #[command(about = "Show messages from a session")]
    Show {
        #[arg(help = "Session ID to display")]
        id: String,
        #[arg(long, default_value = "50", help = "Max messages to display")]
        limit: usize,
    },
    #[command(about = "Export a session as JSON or Markdown")]
    Export {
        #[arg(help = "Session ID to export")]
        id: String,
        #[arg(long, default_value = "json", help = "Output format: json or md")]
        format: String,
    },
    #[command(about = "Search across all sessions")]
    Search {
        #[arg(help = "Search query")]
        query: String,
    },
    #[command(about = "Delete a session and its messages")]
    Delete {
        #[arg(help = "Session ID to delete")]
        id: String,
    },
    #[command(about = "Show aggregate usage statistics across all sessions")]
    Stats,
    #[command(about = "Delete sessions older than a given number of days")]
    Purge {
        #[arg(long, help = "Delete sessions older than N days (e.g. 30)")]
        older_than: u32,
    },
    #[command(about = "Rename a session (set its title)")]
    Rename {
        #[arg(help = "Session ID to rename")]
        id: String,
        #[arg(help = "New title for the session")]
        title: String,
    },
    #[command(about = "Import a conversation from a file (ShareGPT JSON or JSONL)")]
    Import {
        #[arg(help = "Path to the file to import")]
        file: String,
        #[arg(long, help = "Import format: 'sharegpt' or 'jsonl' (auto-detected from extension if omitted)")]
        format: Option<String>,
        #[arg(long, help = "Optional title for the imported session")]
        title: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    #[command(about = "List saved skills")]
    List,
    #[command(about = "Show one skill")]
    Show {
        #[arg(help = "Skill name to display")]
        name: String,
    },
    #[command(about = "Delete a skill by name")]
    Delete {
        #[arg(help = "Skill name to delete")]
        name: String,
    },
    #[command(about = "Export all skills as JSON")]
    Export,
    #[command(about = "Import skills from a JSON file")]
    Import {
        #[arg(help = "Path to JSON file containing skills array")]
        file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ContextCommand {
    #[command(about = "Show the current project's context file")]
    Show,
    #[command(about = "Initialize a .genesis/context.md template in the current directory")]
    Init,
    #[command(about = "Open the context file in $EDITOR")]
    Edit,
}

#[derive(Debug, Subcommand)]
pub enum SubagentsCommand {
    #[command(about = "List subagents for a parent session")]
    List {
        #[arg(help = "Parent session ID")]
        session_id: String,
    },
    #[command(about = "Show subagent details")]
    Show {
        #[arg(help = "Subagent ID")]
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCommand {
    #[command(about = "Create a schedule")]
    Create {
        #[arg(long, help = "Cron expression for the schedule")]
        cron: String,
        #[arg(long, help = "Destination for schedule delivery")]
        destination: String,
        #[arg(long, help = "Prompt to execute when the schedule triggers")]
        prompt: String,
    },
    #[command(about = "List schedules")]
    List,
    #[command(about = "Run the background scheduler daemon")]
    Run,
    #[command(about = "Delete a schedule by id")]
    Delete {
        #[arg(help = "Schedule id to delete")]
        id: String,
    },
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
    Execution(#[from] SessionExecutionError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("session `{0}` was not found")]
    SessionNotFound(String),
    #[error("schedule `{0}` was not found")]
    ScheduleNotFound(String),
    #[error("skill `{0}` was not found")]
    SkillNotFound(String),
    #[error("subagent `{0}` was not found")]
    SubagentNotFound(String),
    #[error("failed to encode json output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to encode yaml output: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("{0}")]
    Other(String),
}

pub async fn run(cli: Cli) -> Result<String, CliError> {
    match cli.command {
        Command::Chat { session_id, resume, prompt, system, last } => {
            run_chat(cli.config, session_id, resume, prompt, system, last).await
        }
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
        Command::Config(ConfigCommand::Edit) => {
            let loaded = load(cli.config.as_deref())?;
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_owned());
            let config_path = loaded.paths.config_path.display().to_string();
            let status = std::process::Command::new(&editor)
                .arg(&config_path)
                .status()
                .map_err(|e| CliError::Other(format!("failed to launch {editor}: {e}")))?;
            if status.success() {
                Ok(format!("config saved: {config_path}"))
            } else {
                Err(CliError::Other(format!("{editor} exited with status {status}")))
            }
        }
        Command::Config(ConfigCommand::Set { key, value }) => {
            let loaded = load(cli.config.as_deref())?;
            genesis_config::set_value_in_file(&loaded.paths.config_path, &key, &value)?;
            Ok(format!("set {key} = {value}"))
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
        Command::Sessions(sessions_command) => {
            let loaded = load(cli.config.as_deref())?;
            let store = SessionStore::new(&loaded.config.storage.database_path);

            match sessions_command {
                SessionsCommand::List => {
                    let sessions = store.list_recent_sessions(20)?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&sessions)?)
                    } else {
                        Ok(format_session_list(&sessions))
                    }
                }
                SessionsCommand::Show { id, limit } => {
                    let session = store
                        .get_session(&id)?
                        .ok_or_else(|| CliError::SessionNotFound(id.clone()))?;
                    let messages = store.load_messages(&id)?;
                    let display_messages = if messages.len() > limit {
                        &messages[messages.len() - limit..]
                    } else {
                        &messages
                    };

                    if cli.json {
                        Ok(serde_json::to_string_pretty(&display_messages)?)
                    } else {
                        Ok(format_session_messages(&session.id, display_messages))
                    }
                }
                SessionsCommand::Export { id, format } => {
                    let _session = store
                        .get_session(&id)?
                        .ok_or_else(|| CliError::SessionNotFound(id.clone()))?;
                    let messages = store.load_messages(&id)?;

                    match format.as_str() {
                        "json" => Ok(serde_json::to_string_pretty(&messages)?),
                        "md" | "markdown" => Ok(export_session_markdown(&id, &messages)),
                        other => Err(CliError::Other(format!(
                            "unknown export format '{other}', expected 'json' or 'md'"
                        ))),
                    }
                }
                SessionsCommand::Search { query } => {
                    let results = store.search_sessions(&query)?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&results)?)
                    } else if results.is_empty() {
                        Ok(format!("No sessions found matching '{query}'"))
                    } else {
                        Ok(format_session_list(&results))
                    }
                }
                SessionsCommand::Delete { id } => {
                    let deleted = store.delete_session(&id)?;
                    if deleted {
                        Ok(format!("Deleted session {id}"))
                    } else {
                        Err(CliError::SessionNotFound(id))
                    }
                }
                SessionsCommand::Stats => {
                    let stats = store.usage_stats()?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&stats)?)
                    } else {
                        Ok(format_usage_stats(&stats))
                    }
                }
                SessionsCommand::Purge { older_than } => {
                    let deleted = store.purge_older_than(older_than)?;
                    Ok(format!("Purged {deleted} session(s) older than {older_than} days"))
                }
                SessionsCommand::Rename { id, title } => {
                    if store.set_title(&id, &title)? {
                        Ok(format!("Renamed session {id} to \"{title}\""))
                    } else {
                        Err(CliError::SessionNotFound(id))
                    }
                }
                SessionsCommand::Import { file, format, title } => {
                    run_session_import(
                        &store,
                        &file,
                        format.as_deref(),
                        title.as_deref(),
                    )
                }
            }
        }
        Command::Skills(skills_command) => {
            let loaded = load(cli.config.as_deref())?;
            bootstrap(&loaded.config.storage.database_path)?;
            let store = SkillStore::new(&loaded.config.storage.database_path);

            match skills_command {
                SkillsCommand::List => {
                    let skills = store.list_all()?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&skills)?)
                    } else {
                        Ok(format_skill_list(&skills))
                    }
                }
                SkillsCommand::Show { name } => {
                    let skill = store
                        .get(&name)?
                        .ok_or_else(|| CliError::SkillNotFound(name.clone()))?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&skill)?)
                    } else {
                        Ok(format_skill(&skill))
                    }
                }
                SkillsCommand::Delete { name } => {
                    if !store.delete(&name)? {
                        return Err(CliError::SkillNotFound(name));
                    }

                    Ok(format!("deleted skill {}", name))
                }
                SkillsCommand::Export => {
                    let skills = store.list_all()?;
                    Ok(serde_json::to_string_pretty(&skills)?)
                }
                SkillsCommand::Import { file } => {
                    let contents = std::fs::read_to_string(&file)
                        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", file.display())))?;
                    let skills: Vec<genesis_storage::StoredSkill> = serde_json::from_str(&contents)
                        .map_err(|e| CliError::Other(format!("invalid JSON: {e}")))?;

                    let mut imported = 0;
                    for skill in &skills {
                        let tags: Vec<&str> = skill.tags.iter().map(|s| s.as_str()).collect();
                        store.upsert(
                            &skill.name,
                            &skill.description,
                            &skill.instructions,
                            skill.trigger_hint.as_deref(),
                            &tags,
                        )?;
                        imported += 1;
                    }

                    Ok(format!("imported {imported} skill(s) from {}", file.display()))
                }
            }
        }
        Command::Context(context_command) => run_context(context_command),
        Command::Subagents(subagents_command) => {
            let loaded = load(cli.config.as_deref())?;
            bootstrap(&loaded.config.storage.database_path)?;
            let store = SubagentStore::new(&loaded.config.storage.database_path);

            match subagents_command {
                SubagentsCommand::List { session_id } => {
                    let subs = store.list_by_parent(&session_id)?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&subs)?)
                    } else {
                        Ok(format_subagent_list(&subs))
                    }
                }
                SubagentsCommand::Show { id } => {
                    let sub = store
                        .get(&id)?
                        .ok_or_else(|| CliError::SubagentNotFound(id.clone()))?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&sub)?)
                    } else {
                        Ok(format_subagent(&sub))
                    }
                }
            }
        }
        Command::Tools => run_tools(cli.config, cli.json),
        Command::Info => run_info(cli.config, cli.json),
        Command::Schedule(schedule_command) => {
            let loaded = load(cli.config.as_deref())?;
            bootstrap(&loaded.config.storage.database_path)?;
            let store = ScheduleStore::new(&loaded.config.storage.database_path);

            match schedule_command {
                ScheduleCommand::Create {
                    cron,
                    destination,
                    prompt,
                } => {
                    let schedule = store.create(
                        &default_schedule_id(),
                        &cron,
                        &destination,
                        &prompt,
                    )?;

                    if cli.json {
                        Ok(serde_json::to_string_pretty(&schedule)?)
                    } else {
                        Ok(format_created_schedule(&schedule))
                    }
                }
                ScheduleCommand::List => {
                    let schedules = store.list_all()?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&schedules)?)
                    } else {
                        Ok(format_schedule_list(&schedules))
                    }
                }
                ScheduleCommand::Run => run_schedule_daemon(&loaded).await,
                ScheduleCommand::Delete { id } => {
                    if !store.delete(&id)? {
                        return Err(CliError::ScheduleNotFound(id));
                    }

                    Ok(format!("deleted schedule {id}"))
                }
            }
        }
        Command::Model(model_command) => run_model(cli.config, model_command, cli.json),
        Command::Serve { host, port } => run_serve(cli.config, &host, port).await,
        Command::Nudge => run_nudge(cli.config).await,
        Command::Insights { days } => {
            let loaded = load(cli.config.as_deref())?;
            bootstrap(&loaded.config.storage.database_path)?;
            let store = SessionStore::new(&loaded.config.storage.database_path);
            let insights = store.insights(days)?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&insights)?)
            } else {
                Ok(format_insights(&insights, &loaded.config.provider.model))
            }
        }
        Command::Init {
            backend,
            model,
            base_url,
            api_key_env,
        }
        | Command::Setup {
            backend,
            model,
            base_url,
            api_key_env,
        } => run_init(cli.config, backend, model, base_url, api_key_env),
        Command::Bootstrap(BootstrapCommand::Config) => {
            let loaded = load(cli.config.as_deref())?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&loaded.config)?)
            } else {
                Ok(serde_yaml::to_string(&loaded.config)?)
            }
        }
        Command::Run { prompt, session_id, raw, system, stream, images } => {
            run_oneshot(cli.config, &prompt, session_id, raw, cli.json, system, stream, &images).await
        }
        Command::Status => {
            let loaded = load(cli.config.as_deref())?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&build_status_json(&loaded))?)
            } else {
                Ok(build_status_text(&loaded))
            }
        }
        Command::Batch {
            input,
            output,
            model,
            max_turns,
            concurrency,
        } => run_batch(cli.config, input, output, model, max_turns, concurrency).await,
        Command::Compress {
            input,
            output,
            level,
            format,
        } => run_compress(input, output, level, format),
        Command::Update => run_update().await,
        Command::Memory(memory_command) => {
            let loaded = load(cli.config.as_deref())?;
            bootstrap(&loaded.config.storage.database_path)?;
            let store = MemoryStore::new(&loaded.config.storage.database_path);
            match memory_command {
                MemoryCommand::List { limit } => {
                    let memories = store.list(limit)?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&memories)?)
                    } else {
                        Ok(format_memory_list(&memories))
                    }
                }
                MemoryCommand::Search { query, limit } => {
                    let memories = store.search(&query, limit)?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&memories)?)
                    } else if memories.is_empty() {
                        Ok(format!("no memories matching \"{query}\""))
                    } else {
                        Ok(format_memory_list(&memories))
                    }
                }
                MemoryCommand::Delete { id } => {
                    if store.delete(&id)? {
                        Ok(format!("deleted memory {id}"))
                    } else {
                        Err(CliError::Other(format!("memory not found: {id}")))
                    }
                }
            }
        }
        Command::Mcp(mcp_command) => run_mcp(cli.config, mcp_command, cli.json).await,
        Command::Benchmark { runs, tool_provider } => {
            run_benchmark(cli.config, runs, tool_provider, cli.json).await
        }
    }
}

async fn run_chat(
    config_path: Option<PathBuf>,
    session_id: Option<String>,
    resume: Option<String>,
    initial_prompt: Option<String>,
    system_override: Option<String>,
    last: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;
    let mut service = SessionExecutionService::new(&loaded);
    service.set_approval_handler(std::sync::Arc::new(CliApprovalHandler));
    if let Some(ref sys) = system_override {
        service.set_system_prompt_override(sys.clone());
    }
    let store = SessionStore::new(&loaded.config.storage.database_path);

    let (session_id, is_resumed) = if last {
        let sessions = store.list_recent_sessions(1)?;
        match sessions.first() {
            Some(s) => (s.id.clone(), true),
            None => return Err(CliError::Other("no previous sessions found".to_owned())),
        }
    } else {
        match resume {
            Some(resume_id) => {
                let session = store
                    .get_session(&resume_id)?
                    .ok_or_else(|| CliError::SessionNotFound(resume_id.clone()))?;
                (session.id, true)
            }
            None => (session_id.unwrap_or_else(default_session_id), false),
        }
    };

    if !is_resumed {
        service.ensure_session(&session_id, "cli", None)?;
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

    let mut rl = rustyline::Editor::new()
        .map_err(|e| CliError::Other(format!("readline init failed: {e}")))?;
    rl.set_helper(Some(SlashCompleter::new()));

    let model = &loaded.config.provider.model;
    let mut session_id = session_id;

    // Process initial prompt if provided
    if let Some(initial) = initial_prompt {
        println!("you> {initial}");
        run_streaming_turn(&service, &session_id, &initial, model).await?;
    }

    loop {
        let input = match read_multiline_input(&mut rl, "you> ", "  .. ") {
            Some(input) => input,
            None => break, // EOF or ctrl-c
        };
        let trimmed = input.trim();

        if trimmed.is_empty() {
            continue;
        }

        if is_exit_command(trimmed) {
            break;
        }

        // Handle /new — start a fresh session
        if trimmed == "/new" {
            session_id = default_session_id();
            service.ensure_session(&session_id, "cli", None)?;
            println!("Started new session: {session_id}");
            continue;
        }

        // Handle /retry — undo last turn and re-send the user message
        if trimmed == "/retry" {
            let messages = store.load_messages(&session_id).unwrap_or_default();
            match messages.iter().rposition(|m| m.role == "user") {
                Some(idx) => {
                    let prompt_text = messages[idx].content.clone().unwrap_or_default();
                    if prompt_text.is_empty() {
                        println!("Last user message has no text content to retry.");
                    } else {
                        let to_remove = messages.len() - idx;
                        let _ = store.delete_last_n_messages(&session_id, to_remove);
                        println!("Retrying: {prompt_text}");
                        run_streaming_turn(&service, &session_id, &prompt_text, model).await?;
                    }
                }
                None => println!("No user message to retry."),
            }
            continue;
        }

        // Handle /fork — branch the conversation into a new session
        if trimmed == "/fork" {
            let new_id = default_session_id();
            match store.fork_session(&session_id, &new_id) {
                Ok(_) => {
                    let old_id = session_id.clone();
                    session_id = new_id;
                    println!("Forked from {old_id} → new session: {session_id}");
                }
                Err(e) => println!("Fork failed: {e}"),
            }
            continue;
        }

        // Handle in-chat slash commands
        if let Some(handled) = handle_chat_command(trimmed, &session_id, &store) {
            println!("{handled}");
            continue;
        }

        run_streaming_turn(&service, &session_id, trimmed, model).await?;
    }

    Ok(format!("chat session saved as {session_id}"))
}

/// Run a single prompt non-interactively and return the response.
async fn run_oneshot(
    config_path: Option<PathBuf>,
    prompt: &str,
    session_id: Option<String>,
    raw: bool,
    json: bool,
    system_override: Option<String>,
    stream: bool,
    image_paths: &[String],
) -> Result<String, CliError> {
    // Support piping: `echo "prompt" | genesis run -`
    let prompt = if prompt == "-" {
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map_err(|e| CliError::Other(format!("stdin read error: {e}")))?;
        buf.trim().to_owned()
    } else {
        prompt.to_owned()
    };

    // Resolve image paths/URLs to ImageUrl objects
    let images = resolve_image_inputs(image_paths)?;

    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;
    let mut service = SessionExecutionService::new(&loaded);
    service.set_approval_handler(std::sync::Arc::new(CliApprovalHandler));
    if let Some(sys) = system_override {
        service.set_system_prompt_override(sys);
    }

    let session_id = session_id.unwrap_or_else(default_session_id);
    service.ensure_session(&session_id, "cli", None)?;

    if stream && !json {
        // Streaming mode — print output as it arrives
        run_streaming_turn(&service, &session_id, &prompt, &loaded.config.provider.model).await?;
        return Ok(String::new());
    }

    let outcome = service
        .run_turn(SessionTurnInput {
            session_id: &session_id,
            session_platform: "cli",
            delivery_platform: DeliveryPlatform::Cli,
            prompt: &prompt,
            title: None,
            images,
        })
        .await?;

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "session_id": outcome.session_id,
            "response": outcome.result.response,
            "turns_used": outcome.result.turns_used,
            "tool_calls_made": outcome.result.tool_calls_made,
            "total_input_tokens": outcome.result.total_input_tokens,
            "total_output_tokens": outcome.result.total_output_tokens,
        }))?);
    }

    if raw {
        Ok(outcome.result.response)
    } else {
        let mut output = outcome.result.response.clone();
        let r = &outcome.result;
        if r.total_input_tokens > 0 || r.total_output_tokens > 0 {
            let cost_str = match r.estimated_cost {
                Some(c) if c > 0.0 => {
                    if c < 0.01 {
                        format!(", ~${c:.4}")
                    } else {
                        format!(", ~${c:.2}")
                    }
                }
                _ => genesis_provider::pricing::estimate_cost(
                    &loaded.config.provider.model,
                    r.total_input_tokens,
                    r.total_output_tokens,
                )
                .map(|c| format!(", ~{c}"))
                .unwrap_or_default(),
            };
            output.push_str(&format!(
                "\n\n[{} in / {} out tokens, {} turns, {} tool calls{cost_str}]",
                r.total_input_tokens, r.total_output_tokens, r.turns_used, r.tool_calls_made
            ));
        }
        Ok(output)
    }
}

async fn run_schedule_daemon(loaded: &LoadedConfig) -> Result<String, CliError> {
    println!(
        "starting genesis scheduler daemon for provider {} / {}",
        loaded.config.provider.backend, loaded.config.provider.model
    );
    let service = SessionExecutionService::new(loaded);

    loop {
        let store = ScheduleStore::new(&loaded.config.storage.database_path);
        let schedules = store.list_enabled()?;
        let due = check_due_schedules(&schedules, &cron_time_from_datetime(Local::now()));

        for schedule in due {
            let session_id = default_schedule_session_id();
            let outcome = service
                .run_turn(SessionTurnInput {
                    session_id: &session_id,
                    session_platform: &schedule.destination,
                    delivery_platform: delivery_platform_from_str(&schedule.destination),
                    prompt: &schedule.prompt,
                    title: Some(schedule.id.as_str()),
                    images: Vec::new(),
                })
                .await?;

            println!(
                "fired {} -> session {} [{}]: {}",
                schedule.id,
                outcome.session_id,
                schedule.destination,
                outcome.result.response
            );
        }

        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn run_serve(
    config_path: Option<PathBuf>,
    host: &str,
    port: u16,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;

    // Connect MCP servers if configured
    let mcp = if !loaded.config.mcp_servers.is_empty() {
        let service = genesis_core::execution::SessionExecutionService::with_mcp(&loaded).await;
        service.mcp_manager()
    } else {
        None
    };

    let api_key = std::env::var("GENESIS_API_KEY").ok();
    let state = std::sync::Arc::new(AppState { loaded, api_key, mcp });
    let router = build_router(state);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        CliError::Io(e)
    })?;

    println!("genesis gateway listening on {addr}");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            println!("\nshutting down gateway...");
        })
        .await
        .map_err(|e| CliError::Io(e))?;

    Ok("server stopped".to_owned())
}

async fn run_nudge(config_path: Option<PathBuf>) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;

    let session_id = format!(
        "nudge-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let response = genesis_core::nudge::run_nudge(&loaded, &session_id).await?;
    Ok(format!("Nudge complete (session: {session_id}):\n\n{response}"))
}

#[derive(Debug)]
struct BatchInputLine {
    prompt: String,
    tags: Vec<String>,
}

async fn run_batch(
    config_path: Option<PathBuf>,
    input: String,
    output: String,
    model_override: Option<String>,
    max_turns: Option<usize>,
    concurrency: Option<usize>,
) -> Result<String, CliError> {
    let loaded = std::sync::Arc::new(load(config_path.as_deref())?);
    bootstrap(&loaded.config.storage.database_path)?;

    let input_file = std::fs::File::open(&input)
        .map_err(|e| CliError::Other(format!("failed to open {input}: {e}")))?;
    let reader = std::io::BufReader::new(input_file);
    let mut items = Vec::new();

    for (line_no, line) in std::io::BufRead::lines(reader).enumerate() {
        let line = line.map_err(|e| {
            CliError::Other(format!("failed to read line {} from {}: {e}", line_no + 1, input))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let item = parse_batch_input_line(&line).map_err(|e| {
            CliError::Other(format!("invalid JSONL at {} line {}: {}", input, line_no + 1, e))
        })?;
        items.push(item);
    }

    if items.is_empty() {
        return Ok(format!("no prompts found in {input}"));
    }

    std::fs::create_dir_all(&output).map_err(|e| {
        CliError::Other(format!("failed to create output directory {}: {e}", output))
    })?;

    let total = items.len();
    let limit = concurrency
        .unwrap_or(loaded.config.runtime.max_concurrency)
        .max(1);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(limit));
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    let mut seen_hashes = HashSet::new();
    let mut skipped = 0usize;

    for item in items.into_iter() {
        let prompt_hash = sha256_hex(&item.prompt);
        let output_path = batch_output_path(&output, &prompt_hash);

        if !seen_hashes.insert(prompt_hash.clone()) || output_path.exists() {
            skipped += 1;
            let done = completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            eprintln!("[{done}/{total}] skip {prompt_hash}");
            continue;
        }

        let permit = semaphore.clone().acquire_owned().await.map_err(|e| {
            CliError::Other(format!("failed to acquire concurrency permit: {e}"))
        })?;
        let loaded = loaded.clone();
        let output_dir = output.clone();
        let model_override = model_override.clone();
        let completed = completed.clone();

        tasks.spawn(async move {
            let _permit = permit;
            let result = run_batch_item(
                &loaded,
                &prompt_hash,
                &item,
                &output_dir,
                model_override.as_deref(),
                max_turns,
            )
            .await;

            let done = completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            eprintln!("[{done}/{total}] {prompt_hash}");
            result
        });
    }

    let mut succeeded = 0usize;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(())) => succeeded += 1,
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                return Err(CliError::Other(format!("batch task join error: {error}")));
            }
        }
    }

    Ok(format!(
        "generated {succeeded}/{total} trajectories in {} (skipped {skipped})",
        output
    ))
}

async fn run_batch_item(
    loaded: &genesis_config::LoadedConfig,
    session_id: &str,
    item: &BatchInputLine,
    output_dir: &str,
    model_override: Option<&str>,
    max_turns: Option<usize>,
) -> Result<(), CliError> {
    let session_store = SessionStore::new(&loaded.config.storage.database_path);
    let _ = session_store.create_session(session_id, "batch", None);

    let execution_context = genesis_core::build_execution_context_from_loaded(
        loaded,
        session_id.to_owned(),
        DeliveryPlatform::Cli,
    );
    let tool_runtime = genesis_core::build_default_tool_runtime(&execution_context);
    let skills_section = genesis_core::skills::load_skills_prompt_for_prompt(
        &loaded.config.storage.database_path,
        &item.prompt,
    );
    let context_section = load_context_file(std::path::Path::new("."));
    let system_prompt = genesis_core::prompt::build_system_prompt_complete(
        &execution_context.plan.profile,
        &tool_runtime.definitions(),
        None,
        skills_section.as_deref(),
        None,
        context_section.as_deref(),
    );

    let model = model_override.unwrap_or(&loaded.config.provider.model);
    let client = genesis_provider::client_from_config(
        &loaded.config.provider.backend,
        model,
        loaded.config.provider.base_url.as_deref(),
        loaded.config.provider.api_key_env.as_deref(),
    )?;

    let mut agent = genesis_core::agent_loop::AgentLoop::new(
        client,
        tool_runtime,
        genesis_core::agent_loop::AgentLoopConfig {
            system_prompt: Some(system_prompt),
            max_turns: max_turns.unwrap_or(loaded.config.runtime.max_turns),
            max_context_messages: loaded.config.runtime.max_context_messages,
            budget_limit: loaded.config.runtime.budget_limit,
            max_concurrency: loaded.config.runtime.max_concurrency,
            enable_trajectory: true,
            trajectory_dir: Some(output_dir.to_owned()),
            session_id: Some(session_id.to_owned()),
            ..genesis_core::agent_loop::AgentLoopConfig::default()
        },
    );

    if let Some(tp) = &loaded.config.tool_provider {
        let tool_client = genesis_provider::client_from_config(
            &tp.backend,
            &tp.model,
            tp.base_url.as_deref(),
            tp.api_key_env.as_deref(),
        )?;
        agent.set_tool_client(tool_client);
    }

    for tag in &item.tags {
        agent.trajectory_mut().add_tag(tag);
    }

    let _ = agent.run_turn(&item.prompt).await?;
    Ok(())
}

fn parse_batch_input_line(line: &str) -> Result<BatchInputLine, String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| e.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "expected JSON object".to_owned())?;

    let prompt = object
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing string field `prompt`".to_owned())?
        .to_owned();

    let tags = object
        .get("tags")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| "field `tags` must be an array of strings".to_owned())?
                .iter()
                .map(|tag| {
                    tag.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "field `tags` must be an array of strings".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok(BatchInputLine { prompt, tags })
}

fn batch_output_path(output_dir: &str, prompt_hash: &str) -> PathBuf {
    std::path::Path::new(output_dir).join(format!("{prompt_hash}.json"))
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(input.as_bytes());
    format!("{hash:x}")
}

fn run_compress(
    input: String,
    output: Option<String>,
    level: Option<String>,
    format: Option<String>,
) -> Result<String, CliError> {
    let level = parse_compression_level(level.as_deref())?;
    let format = parse_compression_format(format.as_deref())?;

    let raw = std::fs::read_to_string(&input)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", input)))?;
    let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
        .map_err(|e| CliError::Other(format!("invalid trajectory JSON in {}: {e}", input)))?;

    let compressed = genesis_core::compress::compress(&trajectory, level);
    let rendered = match format {
        CompressionFormat::Json => serde_json::to_string_pretty(&compressed)?,
        CompressionFormat::ShareGpt => {
            serde_json::to_string_pretty(&genesis_core::compress::to_sharegpt(&compressed))?
        }
        CompressionFormat::ChatMl => genesis_core::compress::to_chatml(&compressed),
    };

    match output {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        CliError::Other(format!(
                            "failed to create parent directory for {}: {e}",
                            path
                        ))
                    })?;
                }
            }
            std::fs::write(&path, rendered)
                .map_err(|e| CliError::Other(format!("failed to write {}: {e}", path)))?;
            Ok(format!("wrote compressed trajectory to {path}"))
        }
        None => Ok(rendered),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompressionFormat {
    Json,
    ShareGpt,
    ChatMl,
}

fn parse_compression_level(
    raw: Option<&str>,
) -> Result<genesis_core::compress::CompressionLevel, CliError> {
    match raw.unwrap_or("medium").trim().to_ascii_lowercase().as_str() {
        "light" => Ok(genesis_core::compress::CompressionLevel::Light),
        "medium" => Ok(genesis_core::compress::CompressionLevel::Medium),
        "heavy" => Ok(genesis_core::compress::CompressionLevel::Heavy),
        other => Err(CliError::Other(format!(
            "unknown compression level '{other}', expected light, medium, or heavy"
        ))),
    }
}

fn parse_compression_format(raw: Option<&str>) -> Result<CompressionFormat, CliError> {
    match raw.unwrap_or("json").trim().to_ascii_lowercase().as_str() {
        "json" => Ok(CompressionFormat::Json),
        "sharegpt" => Ok(CompressionFormat::ShareGpt),
        "chatml" => Ok(CompressionFormat::ChatMl),
        other => Err(CliError::Other(format!(
            "unknown compression format '{other}', expected json, sharegpt, or chatml"
        ))),
    }
}

fn run_init(
    config_path: Option<PathBuf>,
    backend: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
) -> Result<String, CliError> {
    use std::io::IsTerminal;

    // If no flags provided and stdin is a TTY, run interactive wizard
    let is_interactive = backend.is_none()
        && model.is_none()
        && base_url.is_none()
        && api_key_env.is_none()
        && io::stdin().is_terminal();

    if is_interactive {
        return run_init_wizard(config_path);
    }

    run_init_non_interactive(config_path, backend, model, base_url, api_key_env)
}

/// Interactive setup wizard — prompts the user to choose a provider, model,
/// and verify their API key. Invoked when `genesis init` is run with no flags.
fn run_init_wizard(config_path: Option<PathBuf>) -> Result<String, CliError> {
    use genesis_config::{render_example_yaml, update_provider_in_file, AppPaths};

    eprintln!();
    eprintln!("  Welcome to Genesis setup!");
    eprintln!("  ========================");
    eprintln!();

    // Step 1: Choose provider
    let providers: Vec<(&str, &str, &str)> = vec![
        ("openai", "OpenAI", "OPENAI_API_KEY"),
        ("anthropic", "Anthropic (Claude)", "ANTHROPIC_API_KEY"),
        ("google", "Google (Gemini)", "GEMINI_API_KEY"),
        ("openrouter", "OpenRouter (200+ models)", "OPENROUTER_API_KEY"),
        ("local", "Local / Self-hosted (vLLM, Ollama, etc.)", ""),
        ("compatible", "Custom OpenAI-compatible endpoint", ""),
    ];

    eprintln!("  Choose your LLM provider:");
    eprintln!();
    for (i, (_, label, _)) in providers.iter().enumerate() {
        eprintln!("    {}. {}", i + 1, label);
    }
    eprintln!();

    let provider_idx = prompt_choice("  Provider", providers.len())?;
    let (backend, _provider_label, default_key_env) = providers[provider_idx];
    eprintln!();

    // Step 2: Choose model
    let models = known_models();
    let backend_models: Vec<_> = models
        .iter()
        .filter(|(p, _, _)| p.eq_ignore_ascii_case(backend))
        .collect();

    let chosen_model = if backend_models.is_empty() {
        // No known models for this backend — ask for free-form input
        eprintln!("  Enter the model name:");
        prompt_line("  Model")?
    } else {
        eprintln!("  Choose a model:");
        eprintln!();
        for (i, (_, model_id, desc)) in backend_models.iter().enumerate() {
            eprintln!("    {}. {} — {}", i + 1, model_id, desc);
        }
        eprintln!("    {}. Enter a custom model name", backend_models.len() + 1);
        eprintln!();

        let model_idx = prompt_choice("  Model", backend_models.len() + 1)?;
        if model_idx < backend_models.len() {
            backend_models[model_idx].1.to_owned()
        } else {
            prompt_line("  Custom model name")?
        }
    };
    eprintln!();

    // Step 3: Base URL (only for local/compatible)
    let base_url = if backend == "local" || backend == "compatible" {
        eprintln!("  Enter the API base URL (e.g. http://localhost:11434/v1):");
        let url = prompt_line("  Base URL")?;
        eprintln!();
        Some(url)
    } else {
        None
    };

    // Step 4: API key
    let api_key_env = if !default_key_env.is_empty() {
        eprintln!(
            "  API key env var [default: {}]:",
            default_key_env
        );
        let input = prompt_line_or_default("  Env var", default_key_env)?;
        eprintln!();

        // Check if the key is actually set
        if std::env::var(&input).is_ok() {
            eprintln!("  [ok] ${} is set", input);
        } else {
            eprintln!(
                "  [!!] ${} is NOT set — set it before chatting:",
                input
            );
            eprintln!("       export {}=your-api-key-here", input);
        }
        eprintln!();

        Some(input)
    } else {
        None
    };

    // Now run the actual init with the chosen values
    let mut steps = Vec::new();
    steps.push(String::new());

    let paths = AppPaths::resolve(config_path.as_deref())?;

    // Create config if needed
    if !paths.config_path.exists() {
        if let Some(parent) = paths.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(CliError::Io)?;
        }
        let yaml = render_example_yaml(config_path.as_deref())?;
        std::fs::write(&paths.config_path, &yaml).map_err(CliError::Io)?;
        steps.push(format!(
            "  [+] Created config: {}",
            paths.config_path.display()
        ));
    } else {
        steps.push(format!(
            "  [ok] Config exists: {}",
            paths.config_path.display()
        ));
    }

    // Apply choices
    update_provider_in_file(
        &paths.config_path,
        Some(backend),
        Some(&chosen_model),
        base_url.as_ref().map(|u| Some(u.as_str())),
        api_key_env.as_ref().map(|k| Some(k.as_str())),
    )?;
    steps.push(format!(
        "  [+] Provider: {} / {}",
        backend, chosen_model
    ));

    // Bootstrap storage
    std::fs::create_dir_all(&paths.data_dir).map_err(CliError::Io)?;
    let storage_result = bootstrap(&paths.database_path)?;
    steps.push(format!(
        "  [+] Storage ready: {} (schema v{})",
        paths.database_path.display(),
        storage_result.schema_version
    ));

    let tool_count = genesis_core::default_tool_count();
    steps.push(String::new());
    steps.push(format!(
        "  Genesis is ready! {} tools available.",
        tool_count
    ));
    steps.push("  Run `genesis chat` to start talking to Eve.".to_owned());

    Ok(steps.join("\n"))
}

/// Non-interactive init — used when CLI flags are provided or stdin is not a TTY.
fn run_init_non_interactive(
    config_path: Option<PathBuf>,
    backend: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
) -> Result<String, CliError> {
    use genesis_config::{render_example_yaml, update_provider_in_file, AppPaths};

    let paths = AppPaths::resolve(config_path.as_deref())?;
    let mut steps = Vec::new();

    // Step 1: Create config file if it doesn't exist
    let config_existed = paths.config_path.exists();
    if !config_existed {
        // Create parent directory
        if let Some(parent) = paths.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CliError::Io(e))?;
        }

        // Write default config
        let yaml = render_example_yaml(config_path.as_deref())?;
        std::fs::write(&paths.config_path, &yaml).map_err(|e| CliError::Io(e))?;
        steps.push(format!(
            "[+] Created config: {}",
            paths.config_path.display()
        ));
    } else {
        steps.push(format!(
            "[ok] Config exists: {}",
            paths.config_path.display()
        ));
    }

    // Step 2: Apply provider overrides if specified
    if backend.is_some() || model.is_some() || base_url.is_some() || api_key_env.is_some() {
        update_provider_in_file(
            &paths.config_path,
            backend.as_deref(),
            model.as_deref(),
            base_url.as_ref().map(|u| Some(u.as_str())),
            api_key_env.as_ref().map(|k| Some(k.as_str())),
        )?;

        let mut parts = Vec::new();
        if let Some(ref b) = backend {
            parts.push(format!("backend={b}"));
        }
        if let Some(ref m) = model {
            parts.push(format!("model={m}"));
        }
        steps.push(format!("[+] Updated provider: {}", parts.join(", ")));
    }

    // Step 3: Bootstrap storage
    std::fs::create_dir_all(&paths.data_dir).map_err(|e| CliError::Io(e))?;
    let storage_result = bootstrap(&paths.database_path)?;
    steps.push(format!(
        "[+] Storage ready: {} (schema v{})",
        paths.database_path.display(),
        storage_result.schema_version
    ));

    // Step 4: Load and verify config
    let loaded = load(config_path.as_deref())?;
    steps.push(format!(
        "[ok] Profile: {}, Provider: {} / {}",
        loaded.config.profile, loaded.config.provider.backend, loaded.config.provider.model
    ));

    // Step 5: Check API key
    let api_key_var = loaded
        .config
        .provider
        .api_key_env
        .as_deref()
        .unwrap_or("OPENAI_API_KEY");
    if std::env::var(api_key_var).is_ok() {
        steps.push(format!("[ok] API key: ${api_key_var} is set"));
    } else {
        steps.push(format!(
            "[!!] API key: ${api_key_var} is NOT set — set it to start chatting"
        ));
    }

    // Summary
    let tool_count = genesis_core::default_tool_count();
    steps.push(String::new());
    steps.push(format!("Genesis is ready! {} tools available.", tool_count));
    steps.push("Run `genesis chat` to start talking to Eve.".to_owned());

    Ok(steps.join("\n"))
}

/// Prompt the user to enter a number (1-based) and return 0-based index.
fn prompt_choice(label: &str, max: usize) -> Result<usize, CliError> {
    loop {
        eprint!("{} [1-{}]: ", label, max);
        let _ = io::stderr().flush();

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(CliError::Io)?;

        match input.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= max => return Ok(n - 1),
            _ => eprintln!("  Please enter a number between 1 and {}.", max),
        }
    }
}

/// Prompt the user for a free-form line of input.
fn prompt_line(label: &str) -> Result<String, CliError> {
    loop {
        eprint!("{}: ", label);
        let _ = io::stderr().flush();

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(CliError::Io)?;

        let trimmed = input.trim().to_owned();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
        eprintln!("  Please enter a value.");
    }
}

/// Prompt the user for a line of input with a default value.
fn prompt_line_or_default(label: &str, default: &str) -> Result<String, CliError> {
    eprint!("{} [{}]: ", label, default);
    let _ = io::stderr().flush();

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(CliError::Io)?;

    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

async fn run_update() -> Result<String, CliError> {
    use std::process::Command as StdCommand;

    let exe = std::env::current_exe().map_err(CliError::Io)?;
    let repo_dir = exe
        .ancestors()
        .find(|p| p.join(".git").exists() || p.join("Cargo.toml").exists())
        .map(|p| p.to_path_buf())
        .ok_or_else(|| {
            CliError::Other(
                "cannot locate genesis source repo — update requires a source install".into(),
            )
        })?;

    let mut steps = Vec::new();

    // Step 1: git pull
    steps.push("[*] Pulling latest changes…".to_owned());
    let pull = StdCommand::new("git")
        .args(["pull", "--rebase", "origin", "main"])
        .current_dir(&repo_dir)
        .output()
        .map_err(CliError::Io)?;

    let pull_out = String::from_utf8_lossy(&pull.stdout);
    let pull_err = String::from_utf8_lossy(&pull.stderr);

    if !pull.status.success() {
        return Err(CliError::Other(format!(
            "git pull failed:\n{pull_out}{pull_err}"
        )));
    }
    steps.push(format!("    {}", pull_out.trim()));

    // Step 2: cargo build --release
    steps.push("[*] Building release binary…".to_owned());
    let build = StdCommand::new("cargo")
        .args(["build", "--release"])
        .current_dir(&repo_dir)
        .output()
        .map_err(CliError::Io)?;

    if !build.status.success() {
        let build_err = String::from_utf8_lossy(&build.stderr);
        return Err(CliError::Other(format!(
            "cargo build failed:\n{build_err}"
        )));
    }
    steps.push("[ok] Build succeeded.".to_owned());

    // Step 3: Report new version
    let version_out = StdCommand::new(repo_dir.join("target/release/genesis"))
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".to_owned());
    steps.push(format!("[ok] Updated to: {}", version_out.trim()));

    Ok(steps.join("\n"))
}

async fn run_mcp(
    config_path: Option<PathBuf>,
    command: McpCommand,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;

    match command {
        McpCommand::List => {
            let servers = &loaded.config.mcp_servers;
            if servers.is_empty() {
                return Ok("no MCP servers configured".to_owned());
            }

            if json {
                return Ok(serde_json::to_string_pretty(servers)?);
            }

            let mut lines = Vec::new();
            for (name, cfg) in servers {
                let transport = if cfg.command.is_some() {
                    "stdio"
                } else if cfg.url.is_some() {
                    "http"
                } else {
                    "unknown"
                };

                let endpoint = cfg
                    .command
                    .as_deref()
                    .or(cfg.url.as_deref())
                    .unwrap_or("-");

                let timeout = cfg.timeout.unwrap_or(120);
                let connect_timeout = cfg.connect_timeout.unwrap_or(60);

                lines.push(format!(
                    "{name}  [{transport}]  {endpoint}  timeout={timeout}s connect={connect_timeout}s"
                ));
            }
            Ok(lines.join("\n"))
        }
        McpCommand::Test => {
            let servers = &loaded.config.mcp_servers;
            if servers.is_empty() {
                return Ok("no MCP servers configured".to_owned());
            }

            let configs = genesis_mcp::build_server_configs(servers);
            if configs.is_empty() {
                return Ok("no valid MCP server configs found".to_owned());
            }

            let mut lines = Vec::new();
            let manager = genesis_mcp::McpManager::connect_all(configs).await;
            let server_count = manager.server_count().await;
            let tool_count = manager.tool_count().await;

            if server_count == 0 {
                lines.push("no MCP servers responded".to_owned());
            } else {
                lines.push(format!(
                    "{server_count} server(s) connected, {tool_count} tool(s) available"
                ));

                let tool_defs = manager.tool_definitions().await;
                for tool in &tool_defs {
                    lines.push(format!("  - {}: {}", tool.name, tool.description));
                }
            }

            Ok(lines.join("\n"))
        }
    }
}

async fn run_benchmark(
    config_path: Option<PathBuf>,
    runs: usize,
    include_tool_provider: bool,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;

    let mut providers = vec![(
        "primary",
        loaded.config.provider.backend.clone(),
        loaded.config.provider.model.clone(),
        genesis_provider::client_from_config(
            &loaded.config.provider.backend,
            &loaded.config.provider.model,
            loaded.config.provider.base_url.as_deref(),
            loaded.config.provider.api_key_env.as_deref(),
        )?,
    )];

    if include_tool_provider {
        if let Some(tp) = &loaded.config.tool_provider {
            providers.push((
                "tool",
                tp.backend.clone(),
                tp.model.clone(),
                genesis_provider::client_from_config(
                    &tp.backend,
                    &tp.model,
                    tp.base_url.as_deref(),
                    tp.api_key_env.as_deref(),
                )?,
            ));
        }
    }

    let test_prompt = "Say exactly: ping";
    let runs = runs.max(1).min(20);
    let mut results = Vec::new();

    for (label, backend, model, client) in &providers {
        eprintln!("benchmarking {label} ({backend}/{model}) × {runs}...");

        let mut latencies = Vec::with_capacity(runs);
        let mut ttft_times = Vec::new(); // time to first token (streaming)
        let mut errors = 0;

        for i in 0..runs {
            let request = genesis_provider::ChatCompletionRequest::new(
                "",
                vec![genesis_provider::ChatMessage::user(test_prompt)],
            );

            let start = std::time::Instant::now();
            match client.complete(request).await {
                Ok(response) => {
                    let elapsed = start.elapsed();
                    latencies.push(elapsed);

                    let tokens = response.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0);
                    eprintln!(
                        "  run {}: {:.0}ms ({tokens} tokens)",
                        i + 1,
                        elapsed.as_secs_f64() * 1000.0,
                    );
                }
                Err(e) => {
                    errors += 1;
                    eprintln!("  run {}: ERROR — {e}", i + 1);
                }
            }

            // Also do a streaming TTFT test on the first run.
            if i == 0 {
                let request = genesis_provider::ChatCompletionRequest::new(
                    "",
                    vec![genesis_provider::ChatMessage::user(test_prompt)],
                );
                let stream_start = std::time::Instant::now();
                if let Ok(mut stream) = client.complete_stream(request).await {
                    use futures_util::TryStreamExt;
                    if let Some(_chunk) = stream.try_next().await.ok().flatten() {
                        ttft_times.push(stream_start.elapsed());
                    }
                }
            }
        }

        let successful = latencies.len();
        let (min, max, avg, p50) = if !latencies.is_empty() {
            latencies.sort();
            let min = latencies[0];
            let max = latencies[latencies.len() - 1];
            let total: Duration = latencies.iter().sum();
            let avg = total / successful as u32;
            let p50 = latencies[successful / 2];
            (min, max, avg, p50)
        } else {
            (Duration::ZERO, Duration::ZERO, Duration::ZERO, Duration::ZERO)
        };

        results.push(serde_json::json!({
            "label": label,
            "backend": backend,
            "model": model,
            "runs": runs,
            "successful": successful,
            "errors": errors,
            "min_ms": min.as_millis(),
            "max_ms": max.as_millis(),
            "avg_ms": avg.as_millis(),
            "p50_ms": p50.as_millis(),
            "ttft_ms": ttft_times.first().map(|d| d.as_millis()),
        }));
    }

    if json {
        return Ok(serde_json::to_string_pretty(&results)?);
    }

    let mut lines = Vec::new();
    for r in &results {
        lines.push(format!(
            "\n{} ({}/{})",
            r["label"].as_str().unwrap_or("-"),
            r["backend"].as_str().unwrap_or("-"),
            r["model"].as_str().unwrap_or("-"),
        ));
        lines.push(format!(
            "  {} successful / {} errors",
            r["successful"], r["errors"]
        ));
        lines.push(format!(
            "  avg: {}ms  p50: {}ms  min: {}ms  max: {}ms",
            r["avg_ms"], r["p50_ms"], r["min_ms"], r["max_ms"]
        ));
        if let Some(ttft) = r["ttft_ms"].as_u64() {
            lines.push(format!("  ttft (time to first token): {ttft}ms"));
        }
    }

    Ok(lines.join("\n"))
}

fn run_info(config_path: Option<PathBuf>, json: bool) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let db_path = &loaded.config.storage.database_path;

    let tool_count = genesis_core::default_tool_count();

    let session_count = SessionStore::new(db_path)
        .count_sessions()
        .unwrap_or(0);

    let skill_count = SkillStore::new(db_path)
        .list_all()
        .map(|s| s.len())
        .unwrap_or(0);

    let trait_count = UserModelStore::new(db_path)
        .list_all()
        .map(|t| t.len())
        .unwrap_or(0);

    let schedule_count = ScheduleStore::new(db_path)
        .list_all()
        .map(|s| s.len())
        .unwrap_or(0);

    let mcp_server_count = loaded.config.mcp_servers.len();
    let version = env!("CARGO_PKG_VERSION");

    if json {
        let info = serde_json::json!({
            "version": version,
            "profile": loaded.config.profile,
            "provider": {
                "backend": loaded.config.provider.backend,
                "model": loaded.config.provider.model,
            },
            "tools": tool_count,
            "mcp_servers": mcp_server_count,
            "sessions": session_count,
            "skills": skill_count,
            "user_traits": trait_count,
            "schedules": schedule_count,
            "config_path": loaded.paths.config_path.display().to_string(),
            "database_path": db_path.display().to_string(),
        });
        Ok(serde_json::to_string_pretty(&info)?)
    } else {
        Ok(format!(
            "genesis v{version}\n\
             profile:     {}\n\
             provider:    {} / {}\n\
             tools:       {tool_count}\n\
             mcp servers: {mcp_server_count}\n\
             sessions:    {session_count}\n\
             skills:      {skill_count}\n\
             user traits: {trait_count}\n\
             schedules:   {schedule_count}\n\
             config:      {}\n\
             database:    {}",
            loaded.config.profile,
            loaded.config.provider.backend,
            loaded.config.provider.model,
            loaded.paths.config_path.display(),
            db_path.display(),
        ))
    }
}

fn run_tools(config_path: Option<PathBuf>, json: bool) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let ctx = genesis_core::build_execution_context_from_loaded(
        &loaded,
        "tools-list".to_owned(),
        DeliveryPlatform::Cli,
    );
    let runtime = genesis_core::build_default_tool_runtime(&ctx);
    let defs = runtime.definitions();

    if json {
        Ok(serde_json::to_string_pretty(&defs)?)
    } else {
        let mut lines = vec![format!("genesis tools ({} registered)", defs.len())];
        for def in &defs {
            lines.push(format!("  {:<20} {}", def.name, def.description));
        }
        Ok(lines.join("\n"))
    }
}

fn run_context(command: ContextCommand) -> Result<String, CliError> {
    let current_dir = std::env::current_dir()?;

    match command {
        ContextCommand::Show => Ok(match load_context_file(&current_dir) {
            Some(contents) => contents,
            None => "no context file found in current directory".to_owned(),
        }),
        ContextCommand::Init => {
            let context_dir = current_dir.join(".genesis");
            let context_path = context_dir.join("context.md");

            if context_path.exists() {
                return Ok(format!(
                    "context file already exists: {}",
                    context_path.display()
                ));
            }

            std::fs::create_dir_all(&context_dir)?;
            std::fs::write(&context_path, context_template())?;

            Ok(format!("created context file: {}", context_path.display()))
        }
        ContextCommand::Edit => {
            let context_dir = current_dir.join(".genesis");
            let context_path = context_dir.join("context.md");

            if !context_path.exists() {
                // Create with template if it doesn't exist
                std::fs::create_dir_all(&context_dir)?;
                std::fs::write(&context_path, context_template())?;
            }

            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_owned());
            let path_str = context_path.display().to_string();
            let status = std::process::Command::new(&editor)
                .arg(&path_str)
                .status()
                .map_err(|e| CliError::Other(format!("failed to launch {editor}: {e}")))?;
            if status.success() {
                Ok(format!("context saved: {path_str}"))
            } else {
                Err(CliError::Other(format!("{editor} exited with status {status}")))
            }
        }
    }
}

fn run_model(
    config_path: Option<PathBuf>,
    command: ModelCommand,
    json: bool,
) -> Result<String, CliError> {
    match command {
        ModelCommand::Show => {
            let loaded = load(config_path.as_deref())?;
            if json {
                Ok(serde_json::to_string_pretty(&loaded.config.provider)?)
            } else {
                let mut lines = vec![
                    format!("backend: {}", loaded.config.provider.backend),
                    format!("model: {}", loaded.config.provider.model),
                ];
                if let Some(url) = &loaded.config.provider.base_url {
                    lines.push(format!("base_url: {url}"));
                }
                if let Some(key_env) = &loaded.config.provider.api_key_env {
                    lines.push(format!("api_key_env: {key_env}"));
                }
                Ok(lines.join("\n"))
            }
        }
        ModelCommand::List { backend } => {
            let models = known_models();
            if json {
                let filtered: Vec<_> = if let Some(ref b) = backend {
                    models
                        .iter()
                        .filter(|(provider, _, _)| provider.eq_ignore_ascii_case(b))
                        .collect()
                } else {
                    models.iter().collect()
                };
                let json_models: Vec<_> = filtered
                    .iter()
                    .map(|(provider, model, desc)| {
                        serde_json::json!({
                            "provider": provider,
                            "model": model,
                            "description": desc,
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&json_models)?)
            } else {
                let loaded = load(config_path.as_deref()).ok();
                let active_model = loaded.as_ref().map(|l| l.config.provider.model.as_str());
                let mut current_provider = String::new();
                let mut lines = Vec::new();
                for (provider, model, desc) in &models {
                    if let Some(ref b) = backend {
                        if !provider.eq_ignore_ascii_case(b) {
                            continue;
                        }
                    }
                    if *provider != current_provider {
                        if !current_provider.is_empty() {
                            lines.push(String::new());
                        }
                        lines.push(format!("[{provider}]"));
                        current_provider = provider.to_string();
                    }
                    let marker = if active_model == Some(model) {
                        " *"
                    } else {
                        ""
                    };
                    lines.push(format!("  {model}{marker}  — {desc}"));
                }
                if lines.is_empty() {
                    Ok("No models found for the specified backend.".to_owned())
                } else {
                    lines.push(String::new());
                    lines.push("* = currently active".to_owned());
                    Ok(lines.join("\n"))
                }
            }
        }
        ModelCommand::Set {
            model,
            backend,
            base_url,
            api_key_env,
        } => {
            let loaded = load(config_path.as_deref())?;
            let config_file = config_path
                .unwrap_or_else(|| loaded.paths.config_path.clone());

            genesis_config::update_provider_in_file(
                &config_file,
                backend.as_deref(),
                Some(&model),
                base_url.as_ref().map(|u| Some(u.as_str())),
                api_key_env.as_ref().map(|k| Some(k.as_str())),
            )?;

            let updated = load(Some(&config_file))?;
            if json {
                Ok(serde_json::to_string_pretty(&updated.config.provider)?)
            } else {
                Ok(format!(
                    "model set to {} / {}\nconfig: {}",
                    updated.config.provider.backend,
                    updated.config.provider.model,
                    config_file.display()
                ))
            }
        }
    }
}

/// Well-known models grouped by provider.
/// Returns (provider, model_id, short_description).
fn known_models() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        // Anthropic
        ("anthropic", "claude-opus-4-6", "Most capable, complex reasoning"),
        ("anthropic", "claude-sonnet-4-6", "Balanced speed and capability"),
        ("anthropic", "claude-haiku-4-5-20251001", "Fastest, lightweight tasks"),
        // OpenAI
        ("openai", "gpt-4.1", "Flagship GPT model"),
        ("openai", "gpt-4.1-mini", "Fast and affordable"),
        ("openai", "gpt-4.1-nano", "Fastest, simplest tasks"),
        ("openai", "o3", "Advanced reasoning"),
        ("openai", "o4-mini", "Fast reasoning"),
        // Google
        ("google", "gemini-2.5-pro", "Best for complex tasks"),
        ("google", "gemini-2.5-flash", "Fast and versatile"),
        // OpenRouter (aggregator — any model)
        ("openrouter", "anthropic/claude-sonnet-4-6", "Claude via OpenRouter"),
        ("openrouter", "openai/gpt-4.1", "GPT-4.1 via OpenRouter"),
        ("openrouter", "google/gemini-2.5-pro", "Gemini via OpenRouter"),
        ("openrouter", "deepseek/deepseek-r1", "DeepSeek R1 reasoning"),
        ("openrouter", "meta-llama/llama-4-maverick", "Llama 4 Maverick"),
    ]
}

/// Handle in-chat slash commands. Returns Some(output) if handled.
fn handle_chat_command(input: &str, session_id: &str, store: &SessionStore) -> Option<String> {
    let cmd = input.strip_prefix('/')?;
    let (name, _args) = cmd.split_once(' ').unwrap_or((cmd, ""));

    match name {
        "help" | "h" => Some(
            "/help       - Show this help\n\
             /history    - Show recent conversation history\n\
             /export     - Export session as Markdown\n\
             /tokens     - Show session token usage\n\
             /session    - Show current session ID\n\
             /new        - Start a new session\n\
             /undo       - Undo last turn (remove last user-assistant exchange)\n\
             /retry      - Undo last turn and re-send the user message\n\
             /fork       - Branch conversation into a new session\n\
             /search <q> - Search past sessions for a query\n\
             /memories   - Show stored memories\n\
             /compress   - Trim old messages, keeping recent context\n\
             /tools      - List available tools\n\
             /skills     - List saved skills\n\
             /model      - Show active model\n\
             /clear      - Clear the screen\n\
             Use \\ at end of line for multi-line input"
                .to_owned(),
        ),
        "history" => {
            let messages = store.load_messages(session_id).ok()?;
            let recent = if messages.len() > 10 {
                &messages[messages.len() - 10..]
            } else {
                &messages
            };
            Some(format_session_messages(session_id, recent))
        }
        "export" => {
            let messages = store.load_messages(session_id).ok()?;
            Some(export_session_markdown(session_id, &messages))
        }
        "tokens" | "usage" => {
            let session = store.get_session(session_id).ok()??;
            let total = session.total_input_tokens + session.total_output_tokens;
            Some(format!(
                "Session: {}\nInput tokens:  {}\nOutput tokens: {}\nTotal tokens:  {}",
                session_id, session.total_input_tokens, session.total_output_tokens, total
            ))
        }
        "session" => Some(format!("Current session: {session_id}")),
        "compress" => {
            let messages = store.load_messages(session_id).ok()?;
            let total = messages.len();
            if total <= 10 {
                return Some(format!(
                    "Session has only {total} message(s). Nothing to compress."
                ));
            }
            // Keep the 10 most recent messages
            let keep = 10;
            match store.truncate_messages(session_id, keep) {
                Ok(deleted) => {
                    // Inject a summary note as the first message
                    let _ = store.append_message(
                        session_id,
                        "system",
                        Some(&format!(
                            "[Context compressed] {deleted} older messages were removed. \
                             {keep} recent messages retained."
                        )),
                        None,
                        None,
                    );
                    Some(format!(
                        "Compressed: removed {deleted} old messages, kept {keep} recent."
                    ))
                }
                Err(e) => Some(format!("Compression failed: {e}")),
            }
        }
        "search" => {
            let query = _args.trim();
            if query.is_empty() {
                return Some("Usage: /search <query>".to_owned());
            }
            match store.search_sessions(query) {
                Ok(results) if results.is_empty() => {
                    Some(format!("No sessions found matching '{query}'."))
                }
                Ok(results) => {
                    let mut lines = vec![format!("{} session(s) matching '{query}':", results.len())];
                    for s in results.iter().take(10) {
                        let title = s.title.as_deref().unwrap_or("(untitled)");
                        lines.push(format!("  {} - {} [{}]", s.id, title, s.updated_at));
                    }
                    if results.len() > 10 {
                        lines.push(format!("  ... and {} more", results.len() - 10));
                    }
                    Some(lines.join("\n"))
                }
                Err(_) => Some(format!("No sessions found matching '{query}'.")),
            }
        }
        "memories" => {
            let memory_store = MemoryStore::new(store.database_path());
            match memory_store.list(20) {
                Ok(memories) if memories.is_empty() => {
                    Some("No stored memories.".to_owned())
                }
                Ok(memories) => {
                    let mut lines = vec![format!("{} memory/memories:", memories.len())];
                    for m in &memories {
                        lines.push(format!("  [{}] {}", m.kind, m.content));
                    }
                    Some(lines.join("\n"))
                }
                Err(_) => Some("No stored memories.".to_owned()),
            }
        }
        "clear" => {
            // ANSI clear screen
            print!("\x1b[2J\x1b[H");
            let _ = io::stdout().flush();
            Some(String::new())
        }
        "tools" => {
            let registry = genesis_tools::default_registry();
            let defs = registry.definitions();
            let mut lines: Vec<String> = defs
                .iter()
                .map(|d| format!("  {} - {}", d.name, d.description))
                .collect();
            lines.sort();
            Some(format!("Available tools ({}):\n{}", defs.len(), lines.join("\n")))
        }
        "skills" => {
            Some("Use `genesis skills list` to see saved skills.".to_owned())
        }
        "model" => {
            Some("Use `genesis model show` to see the active model.".to_owned())
        }
        "undo" => {
            // Remove the last user-assistant exchange (user message + all
            // subsequent assistant/tool messages until the next user or start).
            let messages = store.load_messages(session_id).ok()?;
            if messages.is_empty() {
                return Some("Nothing to undo.".to_owned());
            }
            // Walk backwards to find the last user message, then count all
            // messages from it to the end — that's the "turn" to remove.
            let mut last_user_idx = None;
            for (i, msg) in messages.iter().enumerate().rev() {
                if msg.role == "user" {
                    last_user_idx = Some(i);
                    break;
                }
            }
            let idx = match last_user_idx {
                Some(i) => i,
                None => return Some("No user messages to undo.".to_owned()),
            };
            let to_remove = messages.len() - idx;
            match store.delete_last_n_messages(session_id, to_remove) {
                Ok(n) => Some(format!("Undid last turn ({n} messages removed).")),
                Err(e) => Some(format!("Undo failed: {e}")),
            }
        }
        _ => Some(format!("Unknown command: /{name}. Type /help for available commands.")),
    }
}

/// Read user input with readline support (history, line editing).
/// Returns `None` on EOF (ctrl-d) or interrupt (ctrl-c).
/// Read multi-line input from the user. Lines ending with `\` are joined with
/// a newline and the next line is read with a continuation prompt.
fn read_multiline_input(
    rl: &mut rustyline::Editor<SlashCompleter, rustyline::history::DefaultHistory>,
    prompt: &str,
    continuation: &str,
) -> Option<String> {
    let first = read_user_input(rl, prompt)?;
    if !first.ends_with('\\') {
        return Some(first);
    }

    let mut buf = String::new();
    buf.push_str(first.trim_end_matches('\\'));
    buf.push('\n');

    loop {
        let line = read_user_input(rl, continuation)?;
        if line.ends_with('\\') {
            buf.push_str(line.trim_end_matches('\\'));
            buf.push('\n');
        } else {
            buf.push_str(&line);
            break;
        }
    }

    // Add the full multi-line input as a single history entry
    if !buf.trim().is_empty() {
        let _ = rl.add_history_entry(&buf);
    }

    Some(buf)
}

fn read_user_input(rl: &mut rustyline::Editor<SlashCompleter, rustyline::history::DefaultHistory>, prompt: &str) -> Option<String> {
    match rl.readline(prompt) {
        Ok(line) => {
            if !line.trim().is_empty() {
                let _ = rl.add_history_entry(&line);
            }
            Some(line)
        }
        Err(rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof) => {
            None
        }
        Err(_) => None,
    }
}

/// Resolve image inputs (file paths or URLs) to ImageUrl objects.
///
/// Supports:
/// - URLs (http/https) — passed through directly
/// - Local file paths — read and encoded as base64 data URIs
fn resolve_image_inputs(inputs: &[String]) -> Result<Vec<genesis_provider::ImageUrl>, CliError> {
    use base64::Engine;

    let mut images = Vec::new();
    for input in inputs {
        if input.starts_with("http://") || input.starts_with("https://") {
            images.push(genesis_provider::ImageUrl {
                url: input.clone(),
                detail: None,
            });
        } else {
            // Local file path — read and encode as data URI
            let path = std::path::Path::new(input);
            let data = std::fs::read(path)
                .map_err(|e| CliError::Other(format!("failed to read image {input}: {e}")))?;

            let mime = match path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|s| s.to_lowercase())
                .as_deref()
            {
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("gif") => "image/gif",
                Some("webp") => "image/webp",
                Some("svg") => "image/svg+xml",
                _ => "image/png", // default
            };

            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
            images.push(genesis_provider::ImageUrl {
                url: format!("data:{mime};base64,{encoded}"),
                detail: None,
            });
        }
    }
    Ok(images)
}

fn default_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("cli-{timestamp}")
}

fn default_schedule_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("sched-{timestamp}")
}

fn default_schedule_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("sched-run-{timestamp}")
}

/// Run a single streaming turn with Ctrl+C cancellation support.
///
/// Wraps `run_turn_streaming` with `tokio::select!` against `ctrl_c()` so
/// pressing Ctrl+C during an LLM response cancels the operation and returns
/// to the input prompt instead of killing the process.
async fn run_streaming_turn(
    service: &SessionExecutionService<'_>,
    session_id: &str,
    prompt: &str,
    model: &str,
) -> Result<(), CliError> {
    use std::sync::atomic::{AtomicBool, Ordering};

    let streamed = AtomicBool::new(false);
    let turn_future = service.run_turn_streaming(
        SessionTurnInput {
            session_id,
            session_platform: "cli",
            delivery_platform: DeliveryPlatform::Cli,
            prompt,
            title: None,
            images: Vec::new(),
        },
        |event| match event {
            StreamEvent::Chunk(chunk) => {
                if !streamed.swap(true, Ordering::Relaxed) {
                    print!("eve> ");
                }
                print!("{chunk}");
                let _ = io::stdout().flush();
            }
            StreamEvent::ToolCallStart { name } => {
                if streamed.load(Ordering::Relaxed) {
                    println!();
                }
                println!("     [calling {name}...]");
                streamed.store(false, Ordering::Relaxed);
            }
            StreamEvent::ToolCallEnd { .. } => {}
            StreamEvent::ClarificationNeeded { question } => {
                println!("\neve> {question}");
            }
        },
    );

    tokio::select! {
        result = turn_future => {
            let outcome = result?;
            if outcome.result.pending_clarification.is_some() {
                // Clarification was already printed via stream event
            } else if streamed.load(Ordering::Relaxed) {
                println!();
            } else {
                println!("eve> {}", outcome.result.response);
            }
            let r = &outcome.result;
            if r.total_input_tokens > 0 || r.total_output_tokens > 0 {
                let cost_str = match r.estimated_cost {
                    Some(c) if c > 0.0 => {
                        if c < 0.01 {
                            format!(", ~${c:.4}")
                        } else {
                            format!(", ~${c:.2}")
                        }
                    }
                    _ => genesis_provider::pricing::estimate_cost(
                        model,
                        r.total_input_tokens,
                        r.total_output_tokens,
                    )
                    .map(|c| format!(", ~{c}"))
                    .unwrap_or_default(),
                };
                println!(
                    "     [{} in / {} out tokens, {} turns, {} tool calls{cost_str}]",
                    r.total_input_tokens, r.total_output_tokens, r.turns_used, r.tool_calls_made
                );
            }
        }
        _ = tokio::signal::ctrl_c() => {
            if streamed.load(Ordering::Relaxed) {
                println!();
            }
            println!("     [interrupted]");
        }
    }

    Ok(())
}

fn is_exit_command(input: &str) -> bool {
    matches!(input, "exit" | "quit" | "/exit" | "/quit")
}

fn cron_time_from_datetime<Tz: chrono::TimeZone>(now: DateTime<Tz>) -> CronTime {
    CronTime {
        minute: now.minute(),
        hour: now.hour(),
        day_of_month: now.day(),
        month: now.month(),
        day_of_week: now.weekday().num_days_from_sunday(),
    }
}

fn context_template() -> &'static str {
    "# Project Context

## Purpose
- Describe what this project does.

## Constraints
- Note any technical, product, or operational constraints.

## Working Agreements
- Capture coding standards, review expectations, and collaboration rules.

## Priorities
- Explain what matters most right now.
"
}

fn format_session_list(sessions: &[SessionSummary]) -> String {
    if sessions.is_empty() {
        return "no saved sessions".to_owned();
    }

    let mut lines = vec!["genesis sessions".to_owned()];
    for session in sessions {
        let tokens = session.total_input_tokens + session.total_output_tokens;
        let token_info = if tokens > 0 {
            format!("  {}tok", tokens)
        } else {
            String::new()
        };
        let title_info = session
            .title
            .as_deref()
            .map(|t| format!("  \"{t}\""))
            .unwrap_or_default();
        lines.push(format!(
            "{}  {}  {}{title_info}{token_info}",
            session.id, session.platform, session.created_at
        ));
    }
    lines.join("\n")
}

fn format_usage_stats(stats: &UsageStats) -> String {
    let total_tokens = stats.total_input_tokens + stats.total_output_tokens;
    let mut lines = vec!["genesis usage stats".to_owned()];
    lines.push(format!("  sessions:      {}", stats.total_sessions));
    lines.push(format!("  input tokens:  {}", stats.total_input_tokens));
    lines.push(format!("  output tokens: {}", stats.total_output_tokens));
    lines.push(format!("  total tokens:  {total_tokens}"));
    lines.join("\n")
}

fn format_insights(data: &InsightsData, model: &str) -> String {
    let total_tokens = data.total_input_tokens + data.total_output_tokens;
    let avg_per_session = if data.sessions_count > 0 {
        total_tokens / data.sessions_count
    } else {
        0
    };

    let mut lines = vec![format!(
        "genesis insights (last {} days)",
        data.period_days
    )];
    lines.push(format!("  model:           {model}"));
    lines.push(format!("  sessions:        {}", data.sessions_count));
    lines.push(format!("  input tokens:    {}", data.total_input_tokens));
    lines.push(format!("  output tokens:   {}", data.total_output_tokens));
    lines.push(format!("  total tokens:    {total_tokens}"));
    lines.push(format!("  avg per session: {avg_per_session}"));

    if let Some(cost) = genesis_provider::pricing::estimate_cost(
        model,
        data.total_input_tokens as u32,
        data.total_output_tokens as u32,
    ) {
        lines.push(format!("  estimated cost:  ~{cost}"));
    }

    if !data.platform_breakdown.is_empty() {
        lines.push(String::new());
        lines.push("  platforms:".to_owned());
        for (platform, count) in &data.platform_breakdown {
            lines.push(format!("    {platform}: {count} sessions"));
        }
    }

    if !data.sessions_per_day.is_empty() {
        lines.push(String::new());
        lines.push("  activity:".to_owned());
        let max_count = data
            .sessions_per_day
            .iter()
            .map(|(_, c)| *c)
            .max()
            .unwrap_or(1);
        for (day, count) in &data.sessions_per_day {
            let bar_len = if max_count > 0 {
                ((*count as f64 / max_count as f64) * 20.0) as usize
            } else {
                0
            };
            let bar: String = std::iter::repeat_n('#', bar_len).collect();
            lines.push(format!("    {day}  {bar} ({count})"));
        }
    }

    lines.join("\n")
}

fn format_memory_list(memories: &[genesis_storage::StoredMemory]) -> String {
    if memories.is_empty() {
        return "no stored memories".to_owned();
    }

    let mut lines = vec![format!("memories ({})", memories.len())];
    for m in memories {
        let content_preview = if m.content.len() > 80 {
            format!("{}...", &m.content[..77])
        } else {
            m.content.clone()
        };
        lines.push(format!("[{}] {} ({})", m.kind, content_preview, m.created_at));
    }
    lines.join("\n")
}

fn build_status_text(loaded: &LoadedConfig) -> String {
    let mut lines = vec!["genesis status".to_owned()];
    lines.push(String::new());

    // Config
    let config_exists = loaded.paths.config_path.exists();
    lines.push(format!(
        "  config:    {} ({})",
        loaded.paths.config_path.display(),
        if config_exists { "found" } else { "not found" }
    ));

    // Provider
    lines.push(format!(
        "  provider:  {} / {}",
        loaded.config.provider.backend, loaded.config.provider.model
    ));

    // Database
    let db_exists = loaded.config.storage.database_path.exists();
    lines.push(format!(
        "  database:  {} ({})",
        loaded.config.storage.database_path.display(),
        if db_exists { "found" } else { "not found" }
    ));

    // Counts (best-effort)
    if db_exists {
        if let Ok(_) = bootstrap(&loaded.config.storage.database_path) {
            let session_store = SessionStore::new(&loaded.config.storage.database_path);
            let skill_store = genesis_storage::SkillStore::new(&loaded.config.storage.database_path);
            let schedule_store =
                genesis_storage::ScheduleStore::new(&loaded.config.storage.database_path);
            let memory_store =
                MemoryStore::new(&loaded.config.storage.database_path);

            if let Ok(stats) = session_store.usage_stats() {
                lines.push(format!("  sessions:  {}", stats.total_sessions));
                let total_tokens = stats.total_input_tokens + stats.total_output_tokens;
                lines.push(format!("  tokens:    {total_tokens} total"));
            }
            if let Ok(skills) = skill_store.list_all() {
                lines.push(format!("  skills:    {}", skills.len()));
            }
            if let Ok(schedules) = schedule_store.list_all() {
                lines.push(format!("  schedules: {}", schedules.len()));
            }
            if let Ok(memories) = memory_store.list(usize::MAX) {
                lines.push(format!("  memories:  {}", memories.len()));
            }
        }
    }

    // MCP servers
    let mcp_count = loaded.config.mcp_servers.len();
    lines.push(format!("  mcp:       {mcp_count} server(s) configured"));

    // Runtime
    lines.push(format!(
        "  runtime:   max_turns={}, destructive={}",
        loaded.config.runtime.max_turns, loaded.config.runtime.allow_destructive_tools
    ));

    lines.join("\n")
}

fn build_status_json(loaded: &LoadedConfig) -> serde_json::Value {
    let mut data = serde_json::json!({
        "config_path": loaded.paths.config_path.display().to_string(),
        "config_exists": loaded.paths.config_path.exists(),
        "database_path": loaded.config.storage.database_path.display().to_string(),
        "database_exists": loaded.config.storage.database_path.exists(),
        "provider_backend": loaded.config.provider.backend,
        "provider_model": loaded.config.provider.model,
        "mcp_servers": loaded.config.mcp_servers.len(),
        "max_turns": loaded.config.runtime.max_turns,
        "allow_destructive_tools": loaded.config.runtime.allow_destructive_tools,
    });

    if loaded.config.storage.database_path.exists() {
        if let Ok(_) = bootstrap(&loaded.config.storage.database_path) {
            let session_store = SessionStore::new(&loaded.config.storage.database_path);
            if let Ok(stats) = session_store.usage_stats() {
                data["total_sessions"] = serde_json::json!(stats.total_sessions);
                data["total_tokens"] =
                    serde_json::json!(stats.total_input_tokens + stats.total_output_tokens);
            }
        }
    }

    data
}

fn format_session_messages(session_id: &str, messages: &[genesis_storage::StoredMessage]) -> String {
    if messages.is_empty() {
        return format!("session {session_id}: no messages");
    }

    let mut lines = vec![format!("session {session_id} ({} messages)", messages.len())];
    for msg in messages {
        let content = msg.content.as_deref().unwrap_or("[no content]");
        let truncated = if content.len() > 200 {
            format!("{}...", &content[..200])
        } else {
            content.to_owned()
        };
        lines.push(format!("[{}] {}: {}", msg.created_at, msg.role, truncated));
    }
    lines.join("\n")
}

fn export_session_markdown(session_id: &str, messages: &[genesis_storage::StoredMessage]) -> String {
    let mut lines = vec![format!("# Session {session_id}\n")];
    for msg in messages {
        let role = match msg.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "system" => "System",
            "tool" => "Tool Result",
            other => other,
        };
        lines.push(format!("## {role}\n"));
        if let Some(content) = &msg.content {
            lines.push(content.clone());
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

fn run_session_import(
    store: &genesis_storage::SessionStore,
    file: &str,
    format: Option<&str>,
    title: Option<&str>,
) -> Result<String, CliError> {
    let path = std::path::Path::new(file);
    let detected_format = match format {
        Some(f) => f.to_owned(),
        None => match path.extension().and_then(|e| e.to_str()) {
            Some("json") => "sharegpt".to_owned(),
            Some("jsonl") => "jsonl".to_owned(),
            _ => {
                return Err(CliError::Other(
                    "cannot auto-detect format: use --format sharegpt or --format jsonl".to_owned(),
                ))
            }
        },
    };

    let contents = std::fs::read_to_string(path)
        .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;

    let messages: Vec<(String, String)> = match detected_format.as_str() {
        "sharegpt" => {
            let entries: Vec<serde_json::Value> = serde_json::from_str(&contents)
                .map_err(|e| CliError::Other(format!("invalid ShareGPT JSON: {e}")))?;
            entries
                .into_iter()
                .filter_map(|entry| {
                    let from = entry.get("from")?.as_str()?;
                    let value = entry.get("value")?.as_str()?;
                    let role = match from {
                        "human" => "user",
                        "gpt" => "assistant",
                        _ => return None, // skip system, thought, etc.
                    };
                    Some((role.to_owned(), value.to_owned()))
                })
                .collect()
        }
        "jsonl" => {
            contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    let entry: serde_json::Value = serde_json::from_str(line)
                        .map_err(|e| CliError::Other(format!("invalid JSONL line: {e}")))?;
                    let role = entry
                        .get("role")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| CliError::Other("JSONL line missing 'role' field".to_owned()))?
                        .to_owned();
                    let content = entry
                        .get("content")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| CliError::Other("JSONL line missing 'content' field".to_owned()))?
                        .to_owned();
                    Ok((role, content))
                })
                .collect::<Result<Vec<_>, CliError>>()?
        }
        other => {
            return Err(CliError::Other(format!(
                "unknown import format '{other}', expected 'sharegpt' or 'jsonl'"
            )));
        }
    };

    let count = messages.len();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let session_id = format!("import-{timestamp}");

    store.import_session(&session_id, title, messages)?;

    Ok(format!("Imported {count} messages into session {session_id}"))
}

fn format_skill_list(skills: &[genesis_storage::StoredSkill]) -> String {
    if skills.is_empty() {
        return "no saved skills".to_owned();
    }

    let mut lines = vec!["genesis skills".to_owned()];
    for skill in skills {
        lines.push(format!(
            "{}\tv{}\t{}",
            skill.name, skill.version, skill.description
        ));
    }
    lines.join("\n")
}

fn format_skill(skill: &genesis_storage::StoredSkill) -> String {
    let mut lines = vec![
        format!("skill: {}", skill.name),
        format!("description: {}", skill.description),
        format!("version: {}", skill.version),
        format!("created_at: {}", skill.created_at),
        format!("updated_at: {}", skill.updated_at),
    ];

    if let Some(trigger_hint) = &skill.trigger_hint {
        lines.push(format!("trigger_hint: {trigger_hint}"));
    }

    if !skill.tags.is_empty() {
        lines.push(format!("tags: {}", skill.tags.join(", ")));
    }

    lines.push("instructions:".to_owned());
    lines.push(skill.instructions.clone());
    lines.join("\n")
}

fn format_subagent_list(subs: &[genesis_storage::StoredSubagent]) -> String {
    if subs.is_empty() {
        return "no subagents found".to_owned();
    }

    let mut lines = vec!["genesis subagents".to_owned()];
    for sub in subs {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            sub.id, sub.name, sub.status, sub.created_at
        ));
    }
    lines.join("\n")
}

fn format_subagent(sub: &genesis_storage::StoredSubagent) -> String {
    let mut lines = vec![
        format!("subagent: {}", sub.id),
        format!("name: {}", sub.name),
        format!("status: {}", sub.status),
        format!("parent_session: {}", sub.parent_session_id),
        format!("child_session: {}", sub.child_session_id),
        format!("task: {}", sub.task),
        format!("created_at: {}", sub.created_at),
    ];
    if let Some(ref result) = sub.result {
        lines.push(format!("result: {result}"));
    }
    if let Some(ref error) = sub.error {
        lines.push(format!("error: {error}"));
    }
    if let Some(ref completed_at) = sub.completed_at {
        lines.push(format!("completed_at: {completed_at}"));
    }
    lines.join("\n")
}

fn format_created_schedule(schedule: &StoredSchedule) -> String {
    format!(
        "created schedule {}\ncron: {}\ndestination: {}\nprompt: {}\ncreated_at: {}",
        schedule.id,
        schedule.cron_expression,
        schedule.destination,
        schedule.prompt,
        schedule.created_at
    )
}

fn format_schedule_list(schedules: &[StoredSchedule]) -> String {
    if schedules.is_empty() {
        return "no saved schedules".to_owned();
    }

    let mut lines = vec!["genesis schedules".to_owned()];
    for schedule in schedules {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            schedule.id, schedule.destination, schedule.cron_expression, schedule.created_at
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
        batch_output_path, context_template, parse_batch_input_line,
        parse_compression_format, parse_compression_level, sha256_hex,
        cron_time_from_datetime, default_schedule_id, default_schedule_session_id,
        default_session_id, delivery_platform_from_str, export_session_markdown,
        format_insights, format_memory_list, format_schedule_list, format_session_list,
        format_usage_stats, format_session_messages, format_skill, format_skill_list,
        format_subagent, format_subagent_list, handle_chat_command, is_exit_command, known_models,
        run, BootstrapCommand, Cli, Command, ConfigCommand, ContextCommand, McpCommand,
        MemoryCommand, ModelCommand, ScheduleCommand, SessionsCommand, SkillsCommand,
        StorageCommand, SubagentsCommand,
    };
    use chrono::{LocalResult, TimeZone};
    use clap::Parser;
    use genesis_storage::{InsightsData, SessionSummary, StoredSchedule, StoredSkill, UsageStats};
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
            Command::Chat { session_id, resume, prompt, system, last } => {
                assert_eq!(session_id.as_deref(), Some("session-42"));
                assert_eq!(resume.as_deref(), Some("session-1"));
                assert!(prompt.is_none());
                assert!(system.is_none());
                assert!(!last);
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

    #[test]
    fn default_schedule_id_uses_sched_prefix() {
        assert!(default_schedule_id().starts_with("sched-"));
    }

    #[test]
    fn default_schedule_session_id_uses_sched_run_prefix() {
        assert!(default_schedule_session_id().starts_with("sched-run-"));
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
            total_input_tokens: 0,
            total_output_tokens: 0,
            parent_session_id: None,
            created_at: "2026-03-08 12:00:00".to_owned(),
            updated_at: "2026-03-08 12:05:00".to_owned(),
        }]);

        assert!(output.contains("genesis sessions"));
        assert!(output.contains("session-1  cli  2026-03-08 12:00:00"));
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
    async fn parses_skills_list_command() {
        let cli = Cli::try_parse_from(["genesis", "skills", "list"])
            .expect("skills list command should parse");

        match cli.command {
            Command::Skills(SkillsCommand::List) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_context_show_command() {
        let cli = Cli::try_parse_from(["genesis", "context", "show"])
            .expect("context show command should parse");

        match cli.command {
            Command::Context(ContextCommand::Show) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_context_init_command() {
        let cli = Cli::try_parse_from(["genesis", "context", "init"])
            .expect("context init command should parse");

        match cli.command {
            Command::Context(ContextCommand::Init) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn context_template_includes_expected_sections() {
        let template = context_template();
        assert!(template.contains("# Project Context"));
        assert!(template.contains("## Purpose"));
        assert!(template.contains("## Constraints"));
        assert!(template.contains("## Working Agreements"));
        assert!(template.contains("## Priorities"));
    }

    #[test]
    fn parses_skills_show_command() {
        let cli = Cli::try_parse_from(["genesis", "skills", "show", "memory_store"])
            .expect("skills show command should parse");

        match cli.command {
            Command::Skills(SkillsCommand::Show { name }) => {
                assert_eq!(name, "memory_store");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_skills_delete_command() {
        let cli = Cli::try_parse_from(["genesis", "skills", "delete", "memory_store"])
            .expect("skills delete command should parse");

        match cli.command {
            Command::Skills(SkillsCommand::Delete { name }) => {
                assert_eq!(name, "memory_store");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn formats_skill_list_for_humans() {
        let output = format_skill_list(&[StoredSkill {
            name: "memory_store".to_owned(),
            description: "Persist a reusable skill".to_owned(),
            instructions: "Remember facts.".to_owned(),
            trigger_hint: Some("when learning new facts".to_owned()),
            tags: vec!["memory".to_owned(), "learning".to_owned()],
            version: 2,
            created_at: "2026-03-08 12:00:00".to_owned(),
            updated_at: "2026-03-08 12:05:00".to_owned(),
        }]);

        assert!(output.contains("genesis skills"));
        assert!(output.contains("memory_store\tv2\tPersist a reusable skill"));
    }

    #[test]
    fn formats_skill_show_for_humans() {
        let output = format_skill(&StoredSkill {
            name: "memory_store".to_owned(),
            description: "Persist a reusable skill".to_owned(),
            instructions: "Remember facts.".to_owned(),
            trigger_hint: Some("when learning new facts".to_owned()),
            tags: vec!["memory".to_owned(), "learning".to_owned()],
            version: 2,
            created_at: "2026-03-08 12:00:00".to_owned(),
            updated_at: "2026-03-08 12:05:00".to_owned(),
        });

        assert!(output.contains("skill: memory_store"));
        assert!(output.contains("description: Persist a reusable skill"));
        assert!(output.contains("trigger_hint: when learning new facts"));
        assert!(output.contains("tags: memory, learning"));
        assert!(output.contains("instructions:\nRemember facts."));
    }

    #[test]
    fn formats_schedule_list_for_humans() {
        let output = format_schedule_list(&[StoredSchedule {
            id: "sched-123".to_owned(),
            cron_expression: "*/5 * * * *".to_owned(),
            destination: "cli".to_owned(),
            prompt: "check status".to_owned(),
            enabled: true,
            created_at: "2026-03-08 12:00:00".to_owned(),
        }]);

        assert!(output.contains("genesis schedules"));
        assert!(output.contains("sched-123\tcli\t*/5 * * * *\t2026-03-08 12:00:00"));
    }

    #[tokio::test]
    async fn parses_schedule_create_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "schedule",
            "create",
            "--cron",
            "*/5 * * * *",
            "--destination",
            "cli",
            "--prompt",
            "check status",
        ])
        .expect("schedule create command should parse");

        match cli.command {
            Command::Schedule(ScheduleCommand::Create {
                cron,
                destination,
                prompt,
            }) => {
                assert_eq!(cron, "*/5 * * * *");
                assert_eq!(destination, "cli");
                assert_eq!(prompt, "check status");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn parses_schedule_list_command() {
        let cli = Cli::try_parse_from(["genesis", "schedule", "list"])
            .expect("schedule list command should parse");

        match cli.command {
            Command::Schedule(ScheduleCommand::List) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn parses_schedule_run_command() {
        let cli = Cli::try_parse_from(["genesis", "schedule", "run"])
            .expect("schedule run command should parse");

        match cli.command {
            Command::Schedule(ScheduleCommand::Run) => {}
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn parses_schedule_delete_command() {
        let cli = Cli::try_parse_from(["genesis", "schedule", "delete", "sched-123"])
            .expect("schedule delete command should parse");

        match cli.command {
            Command::Schedule(ScheduleCommand::Delete { id }) => {
                assert_eq!(id, "sched-123");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn cron_time_from_datetime_maps_fields_correctly() {
        let dt = match chrono::FixedOffset::east_opt(0)
            .expect("offset should build")
            .with_ymd_and_hms(2026, 3, 8, 14, 5, 0)
        {
            LocalResult::Single(value) => value,
            other => panic!("unexpected datetime construction result: {other:?}"),
        };

        let cron_time = cron_time_from_datetime(dt);
        assert_eq!(cron_time.minute, 5);
        assert_eq!(cron_time.hour, 14);
        assert_eq!(cron_time.day_of_month, 8);
        assert_eq!(cron_time.month, 3);
        assert_eq!(cron_time.day_of_week, 0);
    }

    #[test]
    fn platform_from_destination_defaults_to_cli() {
        assert!(matches!(
            delivery_platform_from_str("cli"),
            genesis_types::DeliveryPlatform::Cli
        ));
        assert!(matches!(
            delivery_platform_from_str("slack"),
            genesis_types::DeliveryPlatform::Slack
        ));
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

    #[test]
    fn parses_serve_command_with_defaults() {
        let cli = Cli::try_parse_from(["genesis", "serve"])
            .expect("serve command should parse");

        match cli.command {
            Command::Serve { host, port } => {
                assert_eq!(host, "0.0.0.0");
                assert_eq!(port, 3000);
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_serve_command_with_custom_host_port() {
        let cli = Cli::try_parse_from(["genesis", "serve", "--host", "127.0.0.1", "--port", "8080"])
            .expect("serve command should parse");

        match cli.command {
            Command::Serve { host, port } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, 8080);
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_model_show_command() {
        let cli = Cli::try_parse_from(["genesis", "model", "show"])
            .expect("model show command should parse");

        assert!(matches!(cli.command, Command::Model(ModelCommand::Show)));
    }

    #[test]
    fn parses_model_set_command_minimal() {
        let cli = Cli::try_parse_from(["genesis", "model", "set", "gpt-5"])
            .expect("model set command should parse");

        match cli.command {
            Command::Model(ModelCommand::Set { model, backend, .. }) => {
                assert_eq!(model, "gpt-5");
                assert!(backend.is_none());
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_model_set_command_with_backend() {
        let cli = Cli::try_parse_from([
            "genesis", "model", "set", "claude-sonnet-4-6", "--backend", "anthropic",
        ])
        .expect("model set command should parse");

        match cli.command {
            Command::Model(ModelCommand::Set { model, backend, .. }) => {
                assert_eq!(model, "claude-sonnet-4-6");
                assert_eq!(backend.as_deref(), Some("anthropic"));
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_show_renders_current_provider() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            "provider:\n  backend: openrouter\n  model: nous/hermes-3\n",
        )
        .expect("config should be written");

        let output = run(Cli {
            config: Some(config_path),
            json: false,
            command: Command::Model(ModelCommand::Show),
        })
        .await
        .expect("model show should succeed");

        assert!(output.contains("backend: openrouter"));
        assert!(output.contains("model: nous/hermes-3"));
    }

    #[tokio::test]
    async fn model_set_updates_config_file() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            "provider:\n  backend: openai\n  model: gpt-4.1-mini\n",
        )
        .expect("config should be written");

        let output = run(Cli {
            config: Some(config_path.clone()),
            json: false,
            command: Command::Model(ModelCommand::Set {
                model: "gpt-5".to_owned(),
                backend: Some("openai".to_owned()),
                base_url: None,
                api_key_env: None,
            }),
        })
        .await
        .expect("model set should succeed");

        assert!(output.contains("model set to openai / gpt-5"));

        // Verify persisted
        let reloaded = genesis_config::load(Some(&config_path)).expect("reload");
        assert_eq!(reloaded.config.provider.model, "gpt-5");
    }

    #[tokio::test]
    async fn model_show_json_output() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            "provider:\n  backend: anthropic\n  model: claude-sonnet-4-6\n",
        )
        .expect("config should be written");

        let output = run(Cli {
            config: Some(config_path),
            json: true,
            command: Command::Model(ModelCommand::Show),
        })
        .await
        .expect("model show json should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid json");
        assert_eq!(parsed["backend"], "anthropic");
        assert_eq!(parsed["model"], "claude-sonnet-4-6");
    }

    #[tokio::test]
    async fn model_list_shows_providers() {
        let output = run(Cli {
            config: None,
            json: false,
            command: Command::Model(ModelCommand::List { backend: None }),
        })
        .await
        .expect("model list should succeed");

        assert!(output.contains("[anthropic]"));
        assert!(output.contains("[openai]"));
        assert!(output.contains("claude-sonnet-4-6"));
        assert!(output.contains("gpt-4.1"));
    }

    #[tokio::test]
    async fn model_list_filters_by_backend() {
        let output = run(Cli {
            config: None,
            json: false,
            command: Command::Model(ModelCommand::List {
                backend: Some("openai".to_owned()),
            }),
        })
        .await
        .expect("model list filtered should succeed");

        assert!(output.contains("[openai]"));
        assert!(!output.contains("[anthropic]"));
        assert!(output.contains("gpt-4.1"));
    }

    #[tokio::test]
    async fn model_list_json_output() {
        let output = run(Cli {
            config: None,
            json: true,
            command: Command::Model(ModelCommand::List { backend: None }),
        })
        .await
        .expect("model list json should succeed");

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&output).expect("output should be valid json array");
        assert!(!parsed.is_empty());
        assert!(parsed[0]["provider"].is_string());
        assert!(parsed[0]["model"].is_string());
    }

    #[test]
    fn known_models_not_empty() {
        let models = known_models();
        assert!(models.len() >= 10);
    }

    #[test]
    fn parses_tools_command() {
        let cli = Cli::try_parse_from(["genesis", "tools"])
            .expect("tools command should parse");
        assert!(matches!(cli.command, Command::Tools));
    }

    #[tokio::test]
    async fn tools_command_lists_registered_tools() {
        let output = run(Cli {
            config: None,
            json: false,
            command: Command::Tools,
        })
        .await
        .expect("tools command should succeed");

        assert!(output.contains("genesis tools"));
        assert!(output.contains("echo"));
        assert!(output.contains("user_observe"));
    }

    #[test]
    fn parses_sessions_show_command() {
        let cli = Cli::try_parse_from(["genesis", "sessions", "show", "session-42"])
            .expect("sessions show command should parse");

        match cli.command {
            Command::Sessions(SessionsCommand::Show { id, limit }) => {
                assert_eq!(id, "session-42");
                assert_eq!(limit, 50);
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_sessions_export_command() {
        let cli = Cli::try_parse_from(["genesis", "sessions", "export", "session-42"])
            .expect("sessions export command should parse");

        match cli.command {
            Command::Sessions(SessionsCommand::Export { id, format }) => {
                assert_eq!(id, "session-42");
                assert_eq!(format, "json"); // default
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_sessions_export_markdown() {
        let cli = Cli::try_parse_from(["genesis", "sessions", "export", "s-1", "--format", "md"])
            .expect("sessions export md should parse");

        match cli.command {
            Command::Sessions(SessionsCommand::Export { id, format }) => {
                assert_eq!(id, "s-1");
                assert_eq!(format, "md");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn export_session_markdown_formats_conversation() {
        let messages = vec![
            genesis_storage::StoredMessage {
                id: 1,
                session_id: "s-1".to_owned(),
                role: "user".to_owned(),
                content: Some("hello".to_owned()),
                tool_call_id: None,
                tool_calls_json: None,
                created_at: "2026-03-08 12:00:00".to_owned(),
            },
            genesis_storage::StoredMessage {
                id: 2,
                session_id: "s-1".to_owned(),
                role: "assistant".to_owned(),
                content: Some("hi there".to_owned()),
                tool_call_id: None,
                tool_calls_json: None,
                created_at: "2026-03-08 12:00:01".to_owned(),
            },
        ];

        let output = export_session_markdown("s-1", &messages);
        assert!(output.contains("# Session s-1"));
        assert!(output.contains("## User"));
        assert!(output.contains("hello"));
        assert!(output.contains("## Assistant"));
        assert!(output.contains("hi there"));
    }

    #[test]
    fn parses_sessions_search_command() {
        let cli =
            Cli::try_parse_from(["genesis", "sessions", "search", "hello world"])
                .expect("sessions search should parse");

        match cli.command {
            Command::Sessions(SessionsCommand::Search { query }) => {
                assert_eq!(query, "hello world");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_sessions_delete_command() {
        let cli = Cli::try_parse_from(["genesis", "sessions", "delete", "session-42"])
            .expect("sessions delete should parse");

        match cli.command {
            Command::Sessions(SessionsCommand::Delete { id }) => {
                assert_eq!(id, "session-42");
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn chat_command_help_returns_command_list() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).expect("bootstrap");
        let store = genesis_storage::SessionStore::new(&db);
        store.create_session("s-1", "cli", None).expect("create session");

        let result = handle_chat_command("/help", "s-1", &store);
        assert!(result.is_some());
        let output = result.unwrap();
        assert!(output.contains("/history"));
        assert!(output.contains("/tokens"));
    }

    #[test]
    fn chat_command_session_shows_id() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).expect("bootstrap");
        let store = genesis_storage::SessionStore::new(&db);

        let result = handle_chat_command("/session", "s-1", &store);
        assert_eq!(result.as_deref(), Some("Current session: s-1"));
    }

    #[test]
    fn chat_command_returns_none_for_non_slash() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).expect("bootstrap");
        let store = genesis_storage::SessionStore::new(&db);

        assert!(handle_chat_command("hello world", "s-1", &store).is_none());
    }

    #[test]
    fn chat_command_tokens_shows_usage() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).expect("bootstrap");
        let store = genesis_storage::SessionStore::new(&db);
        store.create_session("s-tok", "cli", None).expect("create session");
        store.add_usage("s-tok", 100, 50).expect("add usage");

        let result = handle_chat_command("/tokens", "s-tok", &store);
        assert!(result.is_some());
        let output = result.unwrap();
        assert!(output.contains("100"));
        assert!(output.contains("50"));
        assert!(output.contains("150"));
    }

    #[test]
    fn format_session_messages_renders_history() {
        let messages = vec![genesis_storage::StoredMessage {
            id: 1,
            session_id: "s-1".to_owned(),
            role: "user".to_owned(),
            content: Some("hello".to_owned()),
            tool_call_id: None,
            tool_calls_json: None,
            created_at: "2026-03-08 12:00:00".to_owned(),
        }];

        let output = format_session_messages("s-1", &messages);
        assert!(output.contains("session s-1"));
        assert!(output.contains("[2026-03-08 12:00:00] user: hello"));
    }

    #[test]
    fn format_session_messages_truncates_long_content() {
        let long_content = "a".repeat(300);
        let messages = vec![genesis_storage::StoredMessage {
            id: 1,
            session_id: "s-1".to_owned(),
            role: "assistant".to_owned(),
            content: Some(long_content),
            tool_call_id: None,
            tool_calls_json: None,
            created_at: "2026-03-08 12:00:00".to_owned(),
        }];

        let output = format_session_messages("s-1", &messages);
        assert!(output.contains("..."));
        assert!(output.len() < 400);
    }

    #[test]
    fn parses_info_command() {
        let cli = Cli::try_parse_from(["genesis", "info"])
            .expect("info command should parse");
        assert!(matches!(cli.command, Command::Info));
    }

    #[test]
    fn parses_nudge_command() {
        let cli = Cli::try_parse_from(["genesis", "nudge"])
            .expect("nudge command should parse");
        assert!(matches!(cli.command, Command::Nudge));
    }

    #[test]
    fn parses_update_command() {
        let cli = Cli::try_parse_from(["genesis", "update"])
            .expect("update command should parse");
        assert!(matches!(cli.command, Command::Update));
    }

    #[test]
    fn parses_mcp_list_command() {
        let cli = Cli::try_parse_from(["genesis", "mcp", "list"])
            .expect("mcp list command should parse");
        assert!(matches!(cli.command, Command::Mcp(McpCommand::List)));
    }

    #[test]
    fn parses_mcp_test_command() {
        let cli = Cli::try_parse_from(["genesis", "mcp", "test"])
            .expect("mcp test command should parse");
        assert!(matches!(cli.command, Command::Mcp(McpCommand::Test)));
    }

    #[test]
    fn parses_run_command() {
        let cli = Cli::try_parse_from(["genesis", "run", "hello world"])
            .expect("run command should parse");
        match cli.command {
            Command::Run { prompt, session_id, raw, system, stream, .. } => {
                assert_eq!(prompt, "hello world");
                assert!(session_id.is_none());
                assert!(!raw);
                assert!(system.is_none());
                assert!(!stream);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_run_command_with_flags() {
        let cli = Cli::try_parse_from([
            "genesis", "run", "--raw", "--session-id", "my-session", "what is 2+2",
        ])
        .expect("run command with flags should parse");
        match cli.command {
            Command::Run { prompt, session_id, raw, system, stream, .. } => {
                assert_eq!(prompt, "what is 2+2");
                assert_eq!(session_id.as_deref(), Some("my-session"));
                assert!(raw);
                assert!(system.is_none());
                assert!(!stream);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_chat_with_system_override() {
        let cli = Cli::try_parse_from([
            "genesis", "chat", "--system", "You are a pirate.",
        ])
        .expect("chat with --system should parse");
        match cli.command {
            Command::Chat { system, .. } => {
                assert_eq!(system.as_deref(), Some("You are a pirate."));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_run_with_system_override() {
        let cli = Cli::try_parse_from([
            "genesis", "run", "--system", "You are a calculator.", "what is 2+2",
        ])
        .expect("run with --system should parse");
        match cli.command {
            Command::Run { prompt, system, .. } => {
                assert_eq!(prompt, "what is 2+2");
                assert_eq!(system.as_deref(), Some("You are a calculator."));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_run_with_stream() {
        let cli = Cli::try_parse_from([
            "genesis", "run", "--stream", "tell me a story",
        ])
        .expect("run with --stream should parse");
        match cli.command {
            Command::Run { prompt, stream, raw, .. } => {
                assert_eq!(prompt, "tell me a story");
                assert!(stream);
                assert!(!raw);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_init_command() {
        let cli = Cli::try_parse_from(["genesis", "init"])
            .expect("init command should parse");
        assert!(matches!(cli.command, Command::Init { .. }));
    }

    #[test]
    fn parses_init_with_options() {
        let cli = Cli::try_parse_from([
            "genesis",
            "init",
            "--backend",
            "openrouter",
            "--model",
            "anthropic/claude-sonnet-4-6",
        ])
        .expect("init with options should parse");
        match cli.command {
            Command::Init { backend, model, .. } => {
                assert_eq!(backend.as_deref(), Some("openrouter"));
                assert_eq!(model.as_deref(), Some("anthropic/claude-sonnet-4-6"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn init_command_creates_config_and_storage() {
        let dir = tempdir().expect("tempdir");
        let config_path = dir.path().join("config.yaml");

        let output = run(Cli {
            config: Some(config_path.clone()),
            json: false,
            command: Command::Init {
                backend: None,
                model: None,
                base_url: None,
                api_key_env: None,
            },
        })
        .await
        .expect("init should succeed");

        assert!(output.contains("Created config"));
        assert!(output.contains("Storage ready"));
        assert!(output.contains("genesis chat"));
        assert!(config_path.exists());
    }

    #[tokio::test]
    async fn info_command_shows_system_overview() {
        let output = run(Cli {
            config: None,
            json: false,
            command: Command::Info,
        })
        .await
        .expect("info command should succeed");

        assert!(output.contains("genesis v"));
        assert!(output.contains("profile:"));
        assert!(output.contains("provider:"));
        assert!(output.contains("tools:"));
        assert!(output.contains("mcp servers:"));
    }

    #[tokio::test]
    async fn info_command_json_output() {
        let output = run(Cli {
            config: None,
            json: true,
            command: Command::Info,
        })
        .await
        .expect("info json should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid json");
        assert!(parsed["tools"].as_u64().unwrap() > 0);
        assert!(parsed["profile"].is_string());
    }

    #[test]
    fn parses_subagents_list_command() {
        let cli = Cli::try_parse_from(["genesis", "subagents", "list", "session-42"])
            .expect("subagents list should parse");
        match cli.command {
            Command::Subagents(SubagentsCommand::List { session_id }) => {
                assert_eq!(session_id, "session-42");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_subagents_show_command() {
        let cli = Cli::try_parse_from(["genesis", "subagents", "show", "sub-1"])
            .expect("subagents show should parse");
        match cli.command {
            Command::Subagents(SubagentsCommand::Show { id }) => {
                assert_eq!(id, "sub-1");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn formats_subagent_list_for_humans() {
        use genesis_storage::StoredSubagent;
        let output = format_subagent_list(&[StoredSubagent {
            id: "sub-1".to_owned(),
            parent_session_id: "p-1".to_owned(),
            child_session_id: "c-1".to_owned(),
            name: "researcher".to_owned(),
            task: "find docs".to_owned(),
            status: "completed".to_owned(),
            result: Some("found them".to_owned()),
            error: None,
            created_at: "2026-03-08".to_owned(),
            completed_at: Some("2026-03-08".to_owned()),
        }]);
        assert!(output.contains("genesis subagents"));
        assert!(output.contains("sub-1\tresearcher\tcompleted"));
    }

    #[test]
    fn formats_subagent_show_for_humans() {
        use genesis_storage::StoredSubagent;
        let output = format_subagent(&StoredSubagent {
            id: "sub-1".to_owned(),
            parent_session_id: "p-1".to_owned(),
            child_session_id: "c-1".to_owned(),
            name: "researcher".to_owned(),
            task: "find docs".to_owned(),
            status: "failed".to_owned(),
            result: None,
            error: Some("timeout".to_owned()),
            created_at: "2026-03-08".to_owned(),
            completed_at: Some("2026-03-08".to_owned()),
        });
        assert!(output.contains("subagent: sub-1"));
        assert!(output.contains("name: researcher"));
        assert!(output.contains("error: timeout"));
    }

    #[test]
    fn parses_config_edit_command() {
        let cli = Cli::try_parse_from(["genesis", "config", "edit"])
            .expect("config edit should parse");
        assert!(matches!(cli.command, Command::Config(ConfigCommand::Edit)));
    }

    #[test]
    fn parses_sessions_stats_command() {
        let cli = Cli::try_parse_from(["genesis", "sessions", "stats"])
            .expect("sessions stats should parse");
        assert!(matches!(
            cli.command,
            Command::Sessions(SessionsCommand::Stats)
        ));
    }

    #[test]
    fn format_usage_stats_displays_token_counts() {
        let stats = UsageStats {
            total_sessions: 42,
            total_input_tokens: 100_000,
            total_output_tokens: 50_000,
        };
        let output = format_usage_stats(&stats);
        assert!(output.contains("sessions:      42"));
        assert!(output.contains("input tokens:  100000"));
        assert!(output.contains("output tokens: 50000"));
        assert!(output.contains("total tokens:  150000"));
    }

    #[test]
    fn parses_sessions_rename_command() {
        let cli = Cli::try_parse_from([
            "genesis", "sessions", "rename", "session-42", "My Project",
        ])
        .expect("sessions rename should parse");
        match cli.command {
            Command::Sessions(SessionsCommand::Rename { id, title }) => {
                assert_eq!(id, "session-42");
                assert_eq!(title, "My Project");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_chat_last_flag() {
        let cli = Cli::try_parse_from(["genesis", "chat", "--last"])
            .expect("chat --last should parse");
        match cli.command {
            Command::Chat { last, .. } => assert!(last),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_sessions_purge_command() {
        let cli = Cli::try_parse_from([
            "genesis", "sessions", "purge", "--older-than", "30",
        ])
        .expect("sessions purge should parse");
        match cli.command {
            Command::Sessions(SessionsCommand::Purge { older_than }) => {
                assert_eq!(older_than, 30);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn chat_help_includes_multiline_hint() {
        let result = handle_chat_command("/help", "s1", &stub_session_store());
        let help = result.expect("help should return something");
        assert!(help.contains("/history"));
        assert!(help.contains("multi-line"));
    }

    fn stub_session_store() -> genesis_storage::SessionStore {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).expect("bootstrap");
        genesis_storage::SessionStore::new(&db)
    }

    #[test]
    fn parses_context_edit_command() {
        let cli = Cli::try_parse_from(["genesis", "context", "edit"])
            .expect("context edit should parse");
        assert!(matches!(
            cli.command,
            Command::Context(ContextCommand::Edit)
        ));
    }

    #[test]
    fn parses_skills_export_command() {
        let cli = Cli::try_parse_from(["genesis", "skills", "export"])
            .expect("skills export should parse");
        assert!(matches!(
            cli.command,
            Command::Skills(SkillsCommand::Export)
        ));
    }

    #[test]
    fn parses_skills_import_command() {
        let cli = Cli::try_parse_from(["genesis", "skills", "import", "skills.json"])
            .expect("skills import should parse");
        match cli.command {
            Command::Skills(SkillsCommand::Import { file }) => {
                assert_eq!(file.to_str().unwrap(), "skills.json");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_config_set_command() {
        let cli = Cli::try_parse_from([
            "genesis", "config", "set", "provider.model", "gpt-5",
        ])
        .expect("config set should parse");
        match cli.command {
            Command::Config(ConfigCommand::Set { key, value }) => {
                assert_eq!(key, "provider.model");
                assert_eq!(value, "gpt-5");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_insights_command() {
        let cli = Cli::try_parse_from(["genesis", "insights"])
            .expect("insights should parse");
        match cli.command {
            Command::Insights { days } => assert_eq!(days, 30),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_insights_with_custom_days() {
        let cli = Cli::try_parse_from(["genesis", "insights", "--days", "7"])
            .expect("insights with days should parse");
        match cli.command {
            Command::Insights { days } => assert_eq!(days, 7),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_batch_command_minimal() {
        let cli = Cli::try_parse_from([
            "genesis",
            "batch",
            "--input",
            "prompts.jsonl",
            "--output",
            "trajectories",
        ])
        .expect("batch should parse");

        match cli.command {
            Command::Batch {
                input,
                output,
                model,
                max_turns,
                concurrency,
            } => {
                assert_eq!(input, "prompts.jsonl");
                assert_eq!(output, "trajectories");
                assert!(model.is_none());
                assert!(max_turns.is_none());
                assert!(concurrency.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_batch_command_with_overrides() {
        let cli = Cli::try_parse_from([
            "genesis",
            "batch",
            "--input",
            "prompts.jsonl",
            "--output",
            "trajectories",
            "--model",
            "claude-sonnet-4-6",
            "--max-turns",
            "12",
            "--concurrency",
            "8",
        ])
        .expect("batch with overrides should parse");

        match cli.command {
            Command::Batch {
                input,
                output,
                model,
                max_turns,
                concurrency,
            } => {
                assert_eq!(input, "prompts.jsonl");
                assert_eq!(output, "trajectories");
                assert_eq!(model.as_deref(), Some("claude-sonnet-4-6"));
                assert_eq!(max_turns, Some(12));
                assert_eq!(concurrency, Some(8));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn batch_input_line_defaults_tags() {
        let parsed =
            parse_batch_input_line(r#"{"prompt":"hello"}"#).expect("json should parse");
        assert_eq!(parsed.prompt, "hello");
        assert!(parsed.tags.is_empty());
    }

    #[test]
    fn sha256_hex_matches_known_value() {
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn batch_output_path_uses_hash_filename() {
        let path = batch_output_path("out", "abc123");
        assert_eq!(path, std::path::Path::new("out").join("abc123.json"));
    }

    #[test]
    fn parses_compress_command_minimal() {
        let cli = Cli::try_parse_from([
            "genesis",
            "compress",
            "--input",
            "trajectory.json",
        ])
        .expect("compress should parse");

        match cli.command {
            Command::Compress {
                input,
                output,
                level,
                format,
            } => {
                assert_eq!(input, "trajectory.json");
                assert!(output.is_none());
                assert!(level.is_none());
                assert!(format.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_compress_command_with_options() {
        let cli = Cli::try_parse_from([
            "genesis",
            "compress",
            "--input",
            "trajectory.json",
            "--output",
            "out/sharegpt.json",
            "--level",
            "heavy",
            "--format",
            "sharegpt",
        ])
        .expect("compress with options should parse");

        match cli.command {
            Command::Compress {
                input,
                output,
                level,
                format,
            } => {
                assert_eq!(input, "trajectory.json");
                assert_eq!(output.as_deref(), Some("out/sharegpt.json"));
                assert_eq!(level.as_deref(), Some("heavy"));
                assert_eq!(format.as_deref(), Some("sharegpt"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parse_compression_level_defaults_to_medium() {
        assert!(matches!(
            parse_compression_level(None).expect("default level should parse"),
            genesis_core::compress::CompressionLevel::Medium
        ));
    }

    #[test]
    fn parse_compression_format_defaults_to_json() {
        assert!(matches!(
            parse_compression_format(None).expect("default format should parse"),
            super::CompressionFormat::Json
        ));
    }

    #[test]
    fn format_insights_displays_summary() {
        let data = InsightsData {
            period_days: 30,
            sessions_count: 10,
            total_input_tokens: 5000,
            total_output_tokens: 3000,
            sessions_per_day: vec![
                ("2026-03-07".to_owned(), 3),
                ("2026-03-08".to_owned(), 7),
            ],
            platform_breakdown: vec![
                ("cli".to_owned(), 8),
                ("api".to_owned(), 2),
            ],
        };
        let output = format_insights(&data, "gpt-4.1-mini");
        assert!(output.contains("sessions:        10"));
        assert!(output.contains("input tokens:    5000"));
        assert!(output.contains("total tokens:    8000"));
        assert!(output.contains("avg per session: 800"));
        assert!(output.contains("cli: 8 sessions"));
        assert!(output.contains("2026-03-08"));
    }

    #[test]
    fn chat_help_includes_new_command() {
        let result = handle_chat_command("/help", "s1", &stub_session_store());
        let help = result.expect("help should return something");
        assert!(help.contains("/new"));
        assert!(help.contains("/tools"));
    }

    #[test]
    fn chat_tools_lists_available_tools() {
        let result = handle_chat_command("/tools", "s1", &stub_session_store());
        let output = result.expect("should return tool list");
        assert!(output.contains("echo"));
        assert!(output.contains("patch"));
        assert!(output.contains("todo"));
    }

    #[test]
    fn chat_undo_removes_last_turn() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).expect("bootstrap");
        let store = genesis_storage::SessionStore::new(&db);
        store.create_session("s-undo", "cli", None).expect("create");
        store.append_message("s-undo", "system", Some("You are Eve."), None, None).unwrap();
        store.append_message("s-undo", "user", Some("Hello"), None, None).unwrap();
        store.append_message("s-undo", "assistant", Some("Hi!"), None, None).unwrap();
        store.append_message("s-undo", "user", Some("How are you?"), None, None).unwrap();
        store.append_message("s-undo", "assistant", Some("Great!"), None, None).unwrap();

        let result = handle_chat_command("/undo", "s-undo", &store);
        assert!(result.is_some());
        let output = result.unwrap();
        assert!(output.contains("2 messages removed"), "got: {output}");

        // Should have system + first user + first assistant = 3 messages left
        let remaining = store.load_messages("s-undo").unwrap();
        assert_eq!(remaining.len(), 3);
        assert_eq!(remaining[2].role, "assistant");
    }

    #[test]
    fn chat_undo_removes_tool_call_turn() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).expect("bootstrap");
        let store = genesis_storage::SessionStore::new(&db);
        store.create_session("s-undo2", "cli", None).expect("create");
        store.append_message("s-undo2", "user", Some("search for X"), None, None).unwrap();
        // assistant with tool call, tool result, then final assistant response
        store.append_message("s-undo2", "assistant", None, Some(r#"[{"id":"t1","type":"function","function":{"name":"web_search","arguments":"{}"}}]"#), None).unwrap();
        store.append_message("s-undo2", "tool", Some("result"), None, None).unwrap();
        store.append_message("s-undo2", "assistant", Some("Here's what I found"), None, None).unwrap();

        let result = handle_chat_command("/undo", "s-undo2", &store);
        let output = result.unwrap();
        assert!(output.contains("4 messages removed"), "got: {output}");

        let remaining = store.load_messages("s-undo2").unwrap();
        assert_eq!(remaining.len(), 0);
    }

    #[test]
    fn chat_undo_empty_session() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).expect("bootstrap");
        let store = genesis_storage::SessionStore::new(&db);
        store.create_session("s-empty", "cli", None).expect("create");

        let result = handle_chat_command("/undo", "s-empty", &store);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Nothing to undo"));
    }

    #[test]
    fn chat_help_includes_undo() {
        let result = handle_chat_command("/help", "s1", &stub_session_store());
        let help = result.expect("help should return something");
        assert!(help.contains("/undo"));
    }

    #[test]
    fn chat_help_includes_search_and_memories() {
        let result = handle_chat_command("/help", "s1", &stub_session_store());
        let help = result.expect("help should return something");
        assert!(help.contains("/search"));
        assert!(help.contains("/memories"));
    }

    #[test]
    fn chat_search_requires_query() {
        let store = stub_session_store();
        let result = handle_chat_command("/search", "s1", &store);
        let output = result.expect("should return something");
        assert!(output.contains("Usage"));
    }

    #[test]
    fn chat_search_with_query_runs() {
        let store = stub_session_store();
        let result = handle_chat_command("/search test query", "s1", &store);
        let output = result.expect("should return something");
        // Either finds results or says no results found
        assert!(output.contains("session") || output.contains("No sessions"));
    }

    #[test]
    fn chat_memories_empty() {
        let store = stub_session_store();
        let result = handle_chat_command("/memories", "s1", &store);
        let output = result.expect("should return something");
        assert!(output.contains("No stored memories") || output.contains("memory"));
    }

    #[test]
    fn chat_usage_is_alias_for_tokens() {
        let store = stub_session_store();
        let result1 = handle_chat_command("/tokens", "s1", &store);
        let result2 = handle_chat_command("/usage", "s1", &store);
        // Both should return something (even if session doesn't exist = None)
        assert_eq!(result1.is_some(), result2.is_some());
    }

    #[test]
    fn parses_memory_list_command() {
        let cli = Cli::try_parse_from(["genesis", "memory", "list"])
            .expect("memory list should parse");
        match cli.command {
            Command::Memory(MemoryCommand::List { limit }) => assert_eq!(limit, 50),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_memory_search_command() {
        let cli = Cli::try_parse_from(["genesis", "memory", "search", "rust programming"])
            .expect("memory search should parse");
        match cli.command {
            Command::Memory(MemoryCommand::Search { query, limit }) => {
                assert_eq!(query, "rust programming");
                assert_eq!(limit, 10);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_memory_delete_command() {
        let cli = Cli::try_parse_from(["genesis", "memory", "delete", "mem-123"])
            .expect("memory delete should parse");
        match cli.command {
            Command::Memory(MemoryCommand::Delete { id }) => {
                assert_eq!(id, "mem-123");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn format_memory_list_shows_memories() {
        let memories = vec![genesis_storage::StoredMemory {
            id: "m1".to_owned(),
            session_id: Some("s1".to_owned()),
            kind: "user_preference".to_owned(),
            content: "likes rust".to_owned(),
            created_at: "2026-03-08 12:00:00".to_owned(),
        }];
        let output = format_memory_list(&memories);
        assert!(output.contains("[user_preference]"));
        assert!(output.contains("likes rust"));
        assert!(output.contains("2026-03-08"));
    }

    #[test]
    fn format_memory_list_empty() {
        let output = format_memory_list(&[]);
        assert_eq!(output, "no stored memories");
    }

    #[test]
    fn parses_status_command() {
        let cli = Cli::try_parse_from(["genesis", "status"])
            .expect("status should parse");
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn parses_sessions_import_command() {
        let cli = Cli::try_parse_from(["genesis", "sessions", "import", "chat.json"])
            .expect("sessions import should parse");
        match cli.command {
            Command::Sessions(SessionsCommand::Import { file, format, title }) => {
                assert_eq!(file, "chat.json");
                assert!(format.is_none());
                assert!(title.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_sessions_import_with_options() {
        let cli = Cli::try_parse_from([
            "genesis", "sessions", "import", "data.jsonl",
            "--format", "jsonl",
            "--title", "My Chat",
        ])
        .expect("sessions import with options should parse");
        match cli.command {
            Command::Sessions(SessionsCommand::Import { file, format, title }) => {
                assert_eq!(file, "data.jsonl");
                assert_eq!(format.as_deref(), Some("jsonl"));
                assert_eq!(title.as_deref(), Some("My Chat"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_sessions_import_sharegpt_format() {
        let cli = Cli::try_parse_from([
            "genesis", "sessions", "import", "conversation.json",
            "--format", "sharegpt",
            "--title", "Imported Conversation",
        ])
        .expect("sessions import sharegpt should parse");
        match cli.command {
            Command::Sessions(SessionsCommand::Import { file, format, title }) => {
                assert_eq!(file, "conversation.json");
                assert_eq!(format.as_deref(), Some("sharegpt"));
                assert_eq!(title.as_deref(), Some("Imported Conversation"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
