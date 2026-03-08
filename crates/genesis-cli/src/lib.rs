use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, Timelike};
use clap::{Parser, Subcommand};
use genesis_config::{load, LoadedConfig};
use genesis_core::agent_loop::AgentError;
use genesis_core::execution::{
    delivery_platform_from_str, SessionExecutionError, SessionExecutionService, SessionTurnInput,
};
use genesis_core::prompt::{agent_name, load_context_file};
use genesis_core::scheduler::{check_due_schedules, CronTime};
use genesis_core::run_doctor;
use genesis_provider::ProviderError;
use genesis_storage::{
    bootstrap, ScheduleStore, SessionStore, SessionSummary, SkillStore, StorageError,
    StoredSchedule, SubagentStore, UserModelStore,
};
use genesis_gateway::{AppState, build_router};
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
    #[command(about = "Initialize Genesis (creates config, bootstraps storage, verifies provider)")]
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
pub enum ModelCommand {
    #[command(about = "Show the active provider and model")]
    Show,
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
}

#[derive(Debug, Subcommand)]
pub enum ContextCommand {
    #[command(about = "Show the current project's context file")]
    Show,
    #[command(about = "Initialize a .genesis/context.md template in the current directory")]
    Init,
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
        Command::Init {
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
    }
}

async fn run_chat(
    config_path: Option<PathBuf>,
    session_id: Option<String>,
    resume: Option<String>,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;
    let service = SessionExecutionService::new(&loaded);
    let store = SessionStore::new(&loaded.config.storage.database_path);

    let (session_id, is_resumed) = match resume {
        Some(resume_id) => {
            let session = store
                .get_session(&resume_id)?
                .ok_or_else(|| CliError::SessionNotFound(resume_id.clone()))?;
            (session.id, true)
        }
        None => (session_id.unwrap_or_else(default_session_id), false),
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

    loop {
        let input = read_user_input("you> ")?;
        let trimmed = input.trim();

        if trimmed.is_empty() {
            continue;
        }

        if is_exit_command(trimmed) {
            break;
        }

        let mut streamed = false;
        let outcome = service
            .run_turn_streaming(
                SessionTurnInput {
                    session_id: &session_id,
                    session_platform: "cli",
                    delivery_platform: DeliveryPlatform::Cli,
                    prompt: trimmed,
                    title: None,
                },
                |chunk| {
                if !streamed {
                    print!("eve> ");
                    streamed = true;
                }
                print!("{chunk}");
                let _ = io::stdout().flush();
                },
            )
            .await?;
        if streamed {
            println!();
        } else {
            println!("eve> {}", outcome.result.response);
        }
    }

    Ok(format!("chat session saved as {session_id}"))
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

    let api_key = std::env::var("GENESIS_API_KEY").ok();
    let state = std::sync::Arc::new(AppState { loaded, api_key });
    let router = build_router(state);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        CliError::Io(e)
    })?;

    println!("genesis gateway listening on {addr}");
    axum::serve(listener, router).await.map_err(|e| {
        CliError::Io(e)
    })?;

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

fn run_init(
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

    if json {
        let info = serde_json::json!({
            "profile": loaded.config.profile,
            "provider": {
                "backend": loaded.config.provider.backend,
                "model": loaded.config.provider.model,
            },
            "tools": tool_count,
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
            "genesis info\n\
             profile:    {}\n\
             provider:   {} / {}\n\
             tools:      {tool_count}\n\
             sessions:   {session_count}\n\
             skills:     {skill_count}\n\
             user traits: {trait_count}\n\
             schedules:  {schedule_count}\n\
             config:     {}\n\
             database:   {}",
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
        lines.push(format!(
            "{}\t{}\t{}",
            session.id, session.platform, session.created_at
        ));
    }
    lines.join("\n")
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
        context_template,
        cron_time_from_datetime, default_schedule_id, default_schedule_session_id,
        default_session_id, delivery_platform_from_str, format_schedule_list, format_session_list,
        format_session_messages, format_skill, format_skill_list, format_subagent,
        format_subagent_list, is_exit_command, run, BootstrapCommand, Cli, Command,
        ContextCommand, ModelCommand, ScheduleCommand, SessionsCommand, SkillsCommand,
        StorageCommand, SubagentsCommand,
    };
    use chrono::{LocalResult, TimeZone};
    use clap::Parser;
    use genesis_storage::{SessionSummary, StoredSchedule, StoredSkill};
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

        assert!(output.contains("genesis info"));
        assert!(output.contains("profile:"));
        assert!(output.contains("provider:"));
        assert!(output.contains("tools:"));
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
}
