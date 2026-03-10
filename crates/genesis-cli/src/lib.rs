mod clipboard;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, Timelike};
use clap::{CommandFactory, Parser, Subcommand};
use genesis_config::{load, LoadedConfig};
use genesis_core::agent_loop::{AgentError, StreamEvent};
use genesis_core::replay::{load_and_report, ReplayEventCounts, ReplayReport};
use genesis_core::execution::{
    delivery_platform_from_str, SessionExecutionError, SessionExecutionService, SessionTurnInput,
};
use genesis_core::prompt::{agent_name, load_context_file};
use genesis_core::scheduler::{check_due_schedules, CronTime};
use genesis_core::run_doctor;
use genesis_provider::ProviderError;
use genesis_storage::{
    bootstrap, InsightsData, MemoryStore, PairingStore, ScheduleStore, SessionStore,
    SessionSummary, SkillStore, StorageError, StoredSchedule, SubagentStore, UsageStats,
    UserModelStore,
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
                "/new", "/undo", "/retry", "/fork", "/resume", "/search",
                "/memories", "/compress", "/tools", "/skills", "/model",
                "/personality", "/system", "/cache", "/stats", "/tag",
                "/title", "/tree", "/audit", "/analytics", "/template", "/workflow", "/bus", "/eval", "/clear",
                "/paste",
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
        #[arg(long, help = "Run in an isolated git worktree (requires git repo)")]
        worktree: bool,
        #[arg(long, help = "Attach the clipboard image to the first message")]
        clipboard: bool,
    },
    #[command(about = "Inspect local config and storage readiness")]
    Doctor {
        #[arg(long, help = "Create the SQLite schema if it does not exist yet")]
        bootstrap_storage: bool,
        #[arg(long, help = "Verify API connectivity with a test request")]
        verify: bool,
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
        #[arg(long, help = "Toolset distribution name (e.g. full, development, research, safe, minimal, creative, ops, home-assistant, coding-agent, random)")]
        toolset: Option<String>,
        #[arg(long, help = "Discard generated trajectories whose quality score is below this threshold (0.0-1.0)")]
        quality_filter: Option<f64>,
        #[arg(long, help = "Automatically tag generated trajectories based on content analysis")]
        auto_tag: bool,
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
        #[arg(long, help = "Use the training compressor that protects first/last turns and summarizes the middle")]
        training: bool,
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
    #[command(subcommand, about = "Inspect offline trajectory replay reports")]
    Eval(EvalCommand),
    #[command(subcommand, about = "Manage DM pairing authorization for messaging platforms")]
    Pairing(PairingCommand),
    #[command(subcommand, about = "List and inspect toolset distributions for batch training")]
    Toolset(ToolsetCommand),
    #[command(subcommand, about = "List and preview agent personalities")]
    Personality(PersonalityCommand),
    #[command(subcommand, about = "Run and manage multi-step workflows")]
    Workflow(WorkflowCommand),
    /// Sign in to an LLM provider (e.g. OpenAI Codex via ChatGPT)
    #[command(about = "Sign in to an LLM provider via OAuth device code flow")]
    Login,

    /// Sign out and clear stored authentication credentials
    #[command(about = "Sign out and clear stored OAuth credentials")]
    Logout,

    #[command(about = "Generate shell completions for bash, zsh, fish, elvish, or powershell")]
    Completions {
        #[arg(help = "Shell to generate completions for (bash, zsh, fish, elvish, powershell)")]
        shell: clap_complete::Shell,
    },

    #[command(about = "Uninstall Genesis — remove binary, data, and config files")]
    Uninstall {
        #[arg(long, help = "Also remove the data directory (database, trajectories, etc.)")]
        remove_data: bool,
        #[arg(long, help = "Also remove the config directory (config.yaml, auth, etc.)")]
        remove_config: bool,
        #[arg(long, help = "Remove everything without prompting for confirmation")]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    #[command(about = "Execute a workflow from a YAML definition file")]
    Run {
        #[arg(help = "Path to the workflow YAML file")]
        file: String,
        #[arg(help = "Initial input for the workflow")]
        input: String,
        #[arg(long, help = "Session ID to use (default: auto-generated)")]
        session_id: Option<String>,
    },
    #[command(about = "Validate a workflow YAML file without executing it")]
    Validate {
        #[arg(help = "Path to the workflow YAML file")]
        file: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PersonalityCommand {
    #[command(about = "List all available personalities")]
    List,
    #[command(about = "Show details for a specific personality")]
    Show {
        #[arg(help = "Name of the personality")]
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum EvalCommand {
    #[command(about = "Build a replay report for one trajectory JSON file")]
    Report {
        #[arg(help = "Path to a trajectory JSON file")]
        file: String,
    },
    #[command(about = "Aggregate replay reports across a directory of trajectory JSON files")]
    Summarize {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories for trajectory JSON files")]
        recursive: bool,
        #[arg(long, help = "Only include trajectories for this model")]
        model: Option<String>,
        #[arg(long, help = "Only include trajectories tagged with this value")]
        tag: Option<String>,
        #[arg(long, help = "Only include trajectories that used this tool")]
        tool: Option<String>,
        #[arg(long, help = "Only include trajectories whose outcome is failure")]
        failures_only: bool,
        #[arg(long, help = "Only include trajectories that contain replay warnings")]
        warnings_only: bool,
        #[arg(long, help = "Only include trajectories with at least this many replay warnings")]
        min_warnings: Option<usize>,
    },
    #[command(about = "Compare two trajectory replay reports")]
    Compare {
        #[arg(help = "Left-hand trajectory JSON file")]
        left: String,
        #[arg(help = "Right-hand trajectory JSON file")]
        right: String,
    },
    #[command(about = "Export a directory of trajectories as ChatML JSONL")]
    ExportChatml {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories for trajectory JSON files")]
        recursive: bool,
    },
    #[command(about = "Export a directory of trajectories as ShareGPT JSONL")]
    ExportSharegpt {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories for trajectory JSON files")]
        recursive: bool,
    },
    #[command(about = "Import ChatML JSONL and create trajectory JSON files")]
    ImportChatml {
        #[arg(help = "Path to a ChatML JSONL file")]
        file: String,
        #[arg(long, help = "Directory to write trajectory JSON files into")]
        output: String,
    },
    #[command(about = "Import ShareGPT JSONL and create trajectory JSON files")]
    ImportSharegpt {
        #[arg(help = "Path to a ShareGPT JSONL file")]
        file: String,
        #[arg(long, help = "Directory to write trajectory JSON files into")]
        output: String,
    },
    #[command(about = "Merge trajectory directories into a single output directory")]
    Merge {
        #[arg(help = "Source directories containing trajectory JSON files")]
        sources: Vec<String>,
        #[arg(long, help = "Output directory to merge into")]
        output: String,
        #[arg(long, help = "Deduplicate by session_id, keeping first occurrence")]
        dedup: bool,
    },
    #[command(about = "Convert between trajectory JSON, ChatML JSONL, and ShareGPT JSONL")]
    Convert {
        #[arg(long, help = "Input file to convert")]
        input: String,
        #[arg(long, help = "Output file to write")]
        output: String,
        #[arg(long, help = "Target format: json, chatml, or sharegpt")]
        format: String,
    },
    #[command(about = "Compute dataset statistics for a directory of trajectories")]
    Stats {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories for trajectory JSON files")]
        recursive: bool,
        #[arg(long, help = "Only include trajectories for this model")]
        model: Option<String>,
        #[arg(long, help = "Only include trajectories tagged with this value")]
        tag: Option<String>,
        #[arg(long, help = "Only include trajectories that used this tool")]
        tool: Option<String>,
        #[arg(long, help = "Only include trajectories whose outcome is failure")]
        failures_only: bool,
    },
    #[command(about = "Score trajectory quality for training data filtering")]
    Quality {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
        #[arg(long, help = "Minimum quality score to pass (0.0-1.0, default: show all)")]
        min_score: Option<f64>,
        #[arg(long, help = "Sort by score ascending (worst first) instead of descending")]
        worst_first: bool,
    },
    #[command(about = "Automatically tag trajectory files using genesis_core::tagger::auto_tag")]
    AutoTag {
        #[arg(long, help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
        #[arg(long, help = "Only print the tags that would be added without writing files")]
        dry_run: bool,
    },
    #[command(about = "Show tag frequency distribution across trajectory files")]
    TagStats {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
    },
    #[command(about = "Find near-duplicate trajectories by system prompt and first user message")]
    Deduplicate {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
        #[arg(long, help = "Delete duplicate files, keeping the first file in each group")]
        remove: bool,
    },
    #[command(about = "Filter trajectories by criteria and copy matching files to output")]
    Filter {
        #[arg(help = "Source directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Output directory for matching trajectories")]
        output: String,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
        #[arg(long, help = "Only include trajectories for this model")]
        model: Option<String>,
        #[arg(long, help = "Only include trajectories with this tag")]
        tag: Option<String>,
        #[arg(long, help = "Minimum quality score (0.0-1.0)")]
        min_quality: Option<f64>,
        #[arg(long, help = "Maximum quality score (0.0-1.0)")]
        max_quality: Option<f64>,
        #[arg(long, help = "Only include successful trajectories")]
        success_only: bool,
        #[arg(long, help = "Only include failed trajectories")]
        failure_only: bool,
        #[arg(long, help = "Minimum number of steps")]
        min_steps: Option<usize>,
        #[arg(long, help = "Maximum number of steps")]
        max_steps: Option<usize>,
        #[arg(long, help = "Only include trajectories that used this tool")]
        tool: Option<String>,
    },
    #[command(about = "Split a trajectory directory into train/test sets")]
    Split {
        #[arg(help = "Source directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Output directory for train set")]
        train: String,
        #[arg(long, help = "Output directory for test set")]
        test: String,
        #[arg(long, default_value = "0.8", help = "Fraction of data for training (0.0-1.0)")]
        ratio: f64,
        #[arg(long, help = "Random seed for reproducibility")]
        seed: Option<u64>,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
    },
    #[command(about = "Build or show a dataset manifest (dataset.json) for a trajectory directory")]
    Manifest {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Dataset name")]
        name: Option<String>,
        #[arg(long, help = "Dataset description")]
        description: Option<String>,
        #[arg(long, help = "Write the manifest to dataset.json in the directory")]
        save: bool,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
    },
    #[command(about = "Validate trajectory files for structural integrity")]
    Validate {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
        #[arg(long, help = "Delete invalid files")]
        remove: bool,
    },
    #[command(about = "Run a multi-step data pipeline: validate → auto-tag → filter → export")]
    Pipeline {
        #[arg(help = "Source directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Output directory for processed trajectories")]
        output: String,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
        #[arg(long, help = "Remove invalid trajectories during validation")]
        validate: bool,
        #[arg(long, help = "Apply auto-tagging")]
        auto_tag: bool,
        #[arg(long, help = "Minimum quality score filter (0.0-1.0)")]
        min_quality: Option<f64>,
        #[arg(long, help = "Only include successful trajectories")]
        success_only: bool,
        #[arg(long, help = "Only include trajectories with this tag")]
        tag: Option<String>,
        #[arg(long, help = "Only include trajectories for this model")]
        model: Option<String>,
        #[arg(long, help = "Export format: json (default), chatml, or sharegpt")]
        format: Option<String>,
        #[arg(long, help = "Build dataset.json manifest in output dir")]
        manifest: bool,
        #[arg(long, help = "Maximum number of trajectories to include")]
        limit: Option<usize>,
        #[arg(long, help = "Random seed for sampling when limit is set")]
        seed: Option<u64>,
    },
    #[command(about = "Random sample of trajectories from a directory")]
    Sample {
        #[arg(help = "Source directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Output directory for sampled trajectories")]
        output: String,
        #[arg(long, help = "Number of trajectories to sample")]
        count: usize,
        #[arg(long, help = "Random seed for reproducibility")]
        seed: Option<u64>,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    #[command(about = "List configured MCP servers")]
    List,
    #[command(about = "Test connectivity to all configured MCP servers")]
    Test,
    #[command(about = "Run Genesis as an MCP server on stdio (for Claude Desktop, etc.)")]
    Serve,
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
    #[command(about = "Validate the config file and check for common issues")]
    Validate,
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
    #[command(about = "Scan a directory of SKILL.md files and show available skills")]
    Scan {
        #[arg(help = "Directory containing skill subdirectories with SKILL.md files")]
        dir: String,
    },
    #[command(about = "Search skills by name, description, or tags")]
    Search {
        #[arg(help = "Search query")]
        query: String,
        #[arg(long, help = "Also search SKILL.md files in this directory")]
        dir: Option<String>,
    },
    #[command(about = "Install a skill from a SKILL.md directory into the database")]
    InstallLocal {
        #[arg(help = "Path to a directory containing a SKILL.md file")]
        path: String,
    },
    #[command(subcommand, about = "Browse, install, and manage skills from registries")]
    Hub(HubCommand),
}

#[derive(Debug, Subcommand)]
pub enum HubCommand {
    #[command(about = "Browse available skills from all sources")]
    Browse {
        #[arg(long, default_value = "1", help = "Page number")]
        page: usize,
        #[arg(long, default_value = "20", help = "Results per page")]
        size: usize,
        #[arg(long, help = "Filter by source name")]
        source: Option<String>,
    },
    #[command(about = "Search skills across registries")]
    Search {
        #[arg(help = "Search query")]
        query: String,
        #[arg(long, help = "Filter by source name")]
        source: Option<String>,
        #[arg(long, default_value = "20", help = "Maximum results to return")]
        limit: usize,
    },
    #[command(about = "Inspect a skill without installing (preview + security scan)")]
    Inspect {
        #[arg(help = "Skill name to inspect")]
        name: String,
    },
    #[command(about = "Install a skill from a registry (quarantine, scan, install)")]
    Install {
        #[arg(help = "Skill name to install")]
        name: String,
        #[arg(long, help = "Force install even if security scan reports issues")]
        force: bool,
    },
    #[command(about = "Uninstall a hub-installed skill")]
    Uninstall {
        #[arg(help = "Skill name to uninstall")]
        name: String,
    },
    #[command(about = "Re-run security scans and integrity checks on installed skills")]
    Audit,
    #[command(about = "List installed hub skills")]
    Installed,
    #[command(subcommand, about = "Manage custom GitHub repo sources (taps)")]
    Tap(TapCommand),
}

#[derive(Debug, Subcommand)]
pub enum TapCommand {
    #[command(about = "List configured taps")]
    List,
    #[command(about = "Add a GitHub repository as a skill source")]
    Add {
        #[arg(help = "Tap name (e.g. 'community')")]
        name: String,
        #[arg(help = "GitHub owner/repo (e.g. 'nooesc/genesis-skills')")]
        repo: String,
        #[arg(long, default_value = "skills", help = "Path within the repo where skills live")]
        path: String,
    },
    #[command(about = "Remove a tap by name")]
    Remove {
        #[arg(help = "Tap name to remove")]
        name: String,
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

#[derive(Debug, Subcommand)]
pub enum PairingCommand {
    #[command(about = "List approved (paired) users")]
    List {
        #[arg(long, help = "Filter by platform (telegram, discord, slack, whatsapp)")]
        platform: Option<String>,
    },
    #[command(about = "List pending pairing requests")]
    Pending {
        #[arg(long, help = "Filter by platform")]
        platform: Option<String>,
    },
    #[command(about = "Approve a pairing code")]
    Approve {
        #[arg(help = "Platform name (telegram, discord, slack, whatsapp)")]
        platform: String,
        #[arg(help = "The pairing code to approve")]
        code: String,
    },
    #[command(about = "Revoke an approved user's access")]
    Revoke {
        #[arg(help = "Platform name")]
        platform: String,
        #[arg(help = "User ID to revoke")]
        user_id: String,
    },
    #[command(about = "Clear all pending pairing codes")]
    ClearPending {
        #[arg(long, help = "Only clear pending codes for this platform")]
        platform: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ToolsetCommand {
    #[command(about = "List available toolset distributions")]
    List,
    #[command(about = "Show details of a specific toolset distribution")]
    Show {
        #[arg(help = "Distribution name")]
        name: String,
    },
    #[command(about = "Sample a distribution and show which tools would be selected")]
    Sample {
        #[arg(help = "Distribution name")]
        name: String,
        #[arg(long, help = "Random seed for reproducible sampling")]
        seed: Option<u64>,
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
    #[error("{0}")]
    Auth(#[from] genesis_auth::AuthError),
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
    #[error("failed to load replay report: {0}")]
    Replay(String),
    #[error("failed to encode json output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to encode yaml output: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("{0}")]
    Other(String),
}

pub async fn run(cli: Cli) -> Result<String, CliError> {
    match cli.command {
        Command::Chat { session_id, resume, prompt, system, last, worktree, clipboard } => {
            run_chat(cli.config, session_id, resume, prompt, system, last, worktree, clipboard).await
        }
        Command::Doctor { bootstrap_storage, verify } => {
            let report = run_doctor(cli.config.as_deref(), bootstrap_storage)?;
            let mut output = if cli.json {
                serde_json::to_string_pretty(&report)?
            } else {
                format_doctor_report(&report)
            };

            // Optional API connectivity verification
            if verify {
                output.push_str("\n\nAPI connectivity:\n");
                let loaded = load(cli.config.as_deref())?;
                match verify_api_connectivity(&loaded).await {
                    Ok(latency_ms) => {
                        output.push_str(&format!(
                            "  [ok] {} / {} responded in {}ms",
                            loaded.config.provider.backend,
                            loaded.config.provider.model,
                            latency_ms
                        ));
                    }
                    Err(e) => {
                        output.push_str(&format!(
                            "  [FAIL] {} / {}: {}",
                            loaded.config.provider.backend,
                            loaded.config.provider.model,
                            e
                        ));
                    }
                }
            }

            Ok(output)
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
        Command::Config(ConfigCommand::Validate) => {
            let loaded = load(cli.config.as_deref())?;
            let mut issues: Vec<String> = Vec::new();
            let mut warnings: Vec<String> = Vec::new();

            // Check provider
            let valid_backends = ["openai", "anthropic", "google", "openrouter", "custom"];
            if !valid_backends.contains(&loaded.config.provider.backend.as_str()) {
                warnings.push(format!(
                    "Unknown provider backend '{}' (known: {})",
                    loaded.config.provider.backend,
                    valid_backends.join(", ")
                ));
            }
            if loaded.config.provider.model.is_empty() {
                issues.push("Provider model is empty.".to_owned());
            }

            // Check API key resolution
            let api_key_env = loaded.config.provider.api_key_env.as_deref()
                .unwrap_or(match loaded.config.provider.backend.as_str() {
                    "anthropic" => "ANTHROPIC_API_KEY",
                    "google" => "GOOGLE_API_KEY",
                    _ => "OPENAI_API_KEY",
                });
            if std::env::var(api_key_env).is_err() {
                warnings.push(format!(
                    "API key env var '{}' is not set.", api_key_env
                ));
            }

            // Check storage paths
            if let Some(parent) = loaded.config.storage.database_path.parent() {
                if !parent.exists() {
                    warnings.push(format!(
                        "Database directory does not exist: {}",
                        parent.display()
                    ));
                }
            }

            // Check runtime config
            if loaded.config.runtime.max_turns == 0 {
                issues.push("runtime.max_turns is 0 — agent cannot run.".to_owned());
            }
            if loaded.config.runtime.max_concurrency == 0 {
                issues.push("runtime.max_concurrency is 0 — tools cannot run.".to_owned());
            }

            // Check fallback providers
            for (i, fp) in loaded.config.fallback_providers.iter().enumerate() {
                if fp.model.is_empty() {
                    issues.push(format!("Fallback provider {} has an empty model.", i + 1));
                }
            }

            // Check MCP servers
            for (name, mcp) in &loaded.config.mcp_servers {
                if mcp.command.is_none() && mcp.url.is_none() {
                    issues.push(format!(
                        "MCP server '{}' has no command or URL configured.", name
                    ));
                }
            }

            let config_path = loaded.paths.config_path.display();
            let mut output = format!("Config: {config_path}\n");
            if issues.is_empty() && warnings.is_empty() {
                output.push_str("All checks passed.");
            } else {
                for issue in &issues {
                    output.push_str(&format!("  ERROR: {issue}\n"));
                }
                for warning in &warnings {
                    output.push_str(&format!("  WARN:  {warning}\n"));
                }
                if issues.is_empty() {
                    output.push_str("No errors found.");
                } else {
                    output.push_str(&format!("{} error(s) found.", issues.len()));
                }
            }
            Ok(output)
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
        Command::Eval(eval_command) => match eval_command {
            EvalCommand::Report { file } => {
                let report = load_and_report(&file)
                    .map_err(|e| CliError::Replay(e.to_string()))?;
                if cli.json {
                    Ok(serde_json::to_string_pretty(&report)?)
                } else {
                    Ok(format_replay_report(&report))
                }
            }
            EvalCommand::Summarize {
                dir,
                recursive,
                model,
                tag,
                tool,
                failures_only,
                warnings_only,
                min_warnings,
            } => {
                let summary = summarize_replay_reports(
                    &dir,
                    recursive,
                    model.as_deref(),
                    tag.as_deref(),
                    tool.as_deref(),
                    failures_only,
                    warnings_only,
                    min_warnings,
                )
                .map_err(|e| CliError::Replay(e.to_string()))?;
                if cli.json {
                    Ok(serde_json::to_string_pretty(&eval_summary_to_json(&summary))?)
                } else {
                    Ok(format_eval_summary(&summary))
                }
            }
            EvalCommand::Compare { left, right } => {
                let comparison = compare_replay_reports(&left, &right)?;
                if cli.json {
                    Ok(serde_json::to_string_pretty(&eval_comparison_to_json(
                        &comparison,
                    ))?)
                } else {
                    Ok(format_eval_comparison(&comparison))
                }
            }
            EvalCommand::ExportChatml { dir, recursive } => {
                run_eval_export_chatml(&dir, recursive)
            }
            EvalCommand::ExportSharegpt { dir, recursive } => {
                run_eval_export_sharegpt(&dir, recursive)
            }
            EvalCommand::ImportChatml { file, output } => {
                run_eval_import_chatml(&file, &output)
            }
            EvalCommand::ImportSharegpt { file, output } => {
                run_eval_import_sharegpt(&file, &output)
            }
            EvalCommand::Merge { sources, output, dedup } => {
                run_eval_merge(&sources, &output, dedup)
            }
            EvalCommand::Convert { input, output, format } => {
                run_eval_convert(&input, &output, &format)
            }
            EvalCommand::Stats {
                dir,
                recursive,
                model,
                tag,
                tool,
                failures_only,
            } => {
                let stats = compute_eval_stats(
                    &dir,
                    recursive,
                    model.as_deref(),
                    tag.as_deref(),
                    tool.as_deref(),
                    failures_only,
                )?;
                if cli.json {
                    Ok(serde_json::to_string_pretty(&eval_stats_to_json(&stats))?)
                } else {
                    Ok(format_eval_stats(&stats))
                }
            }
            EvalCommand::Quality {
                dir,
                recursive,
                min_score,
                worst_first,
            } => {
                run_eval_quality(&dir, recursive, min_score, worst_first, cli.json)
            }
            EvalCommand::AutoTag { dir, recursive, dry_run } => {
                run_eval_auto_tag(&dir, recursive, dry_run, cli.json)
            }
            EvalCommand::TagStats { dir, recursive } => {
                run_eval_tag_stats(&dir, recursive, cli.json)
            }
            EvalCommand::Deduplicate { dir, recursive, remove } => {
                run_eval_deduplicate(&dir, recursive, remove, cli.json)
            }
            EvalCommand::Filter {
                dir,
                output,
                recursive,
                model,
                tag,
                min_quality,
                max_quality,
                success_only,
                failure_only,
                min_steps,
                max_steps,
                tool,
            } => run_eval_filter(
                &dir, &output, recursive, model.as_deref(), tag.as_deref(),
                min_quality, max_quality, success_only, failure_only,
                min_steps, max_steps, tool.as_deref(),
            ),
            EvalCommand::Split {
                dir,
                train,
                test,
                ratio,
                seed,
                recursive,
            } => run_eval_split(&dir, &train, &test, ratio, seed, recursive),
            EvalCommand::Manifest {
                dir,
                name,
                description,
                save,
                recursive,
            } => run_eval_manifest(
                &dir,
                name.as_deref().unwrap_or("unnamed"),
                description.as_deref().unwrap_or(""),
                save,
                recursive,
                cli.json,
            ),
            EvalCommand::Pipeline {
                dir,
                output,
                recursive,
                validate,
                auto_tag,
                min_quality,
                success_only,
                tag,
                model,
                format,
                manifest,
                limit,
                seed,
            } => run_eval_pipeline(
                &dir, &output, recursive, validate, auto_tag,
                min_quality, success_only, tag.as_deref(), model.as_deref(),
                format.as_deref(), manifest, limit, seed,
            ),
            EvalCommand::Validate {
                dir,
                recursive,
                remove,
            } => run_eval_validate(&dir, recursive, remove),
            EvalCommand::Sample {
                dir,
                output,
                count,
                seed,
                recursive,
            } => run_eval_sample(&dir, &output, count, seed, recursive),
        },
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
                SkillsCommand::Scan { dir } => {
                    run_skills_scan(&dir, cli.json)
                }
                SkillsCommand::Search { query, dir } => {
                    run_skills_search(&store, &query, dir.as_deref(), cli.json)
                }
                SkillsCommand::InstallLocal { path } => {
                    run_skills_install_local(&store, &path)
                }
                SkillsCommand::Hub(hub_command) => {
                    run_skills_hub(hub_command, &loaded, cli.json)
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
        } => run_init(cli.config, backend, model, base_url, api_key_env).await,
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
            toolset,
            quality_filter,
            auto_tag,
        } => {
            run_batch(
                cli.config,
                input,
                output,
                model,
                max_turns,
                concurrency,
                toolset,
                quality_filter,
                auto_tag,
            )
            .await
        }
        Command::Compress {
            input,
            output,
            level,
            format,
            training,
        } => run_compress(input, output, level, format, training),
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
        Command::Pairing(pairing_command) => {
            run_pairing(cli.config, pairing_command, cli.json).await
        }
        Command::Toolset(toolset_command) => run_toolset(toolset_command, cli.json),
        Command::Personality(personality_command) => run_personality(personality_command, cli.json),
        Command::Workflow(WorkflowCommand::Validate { file }) => {
            let yaml = fs::read_to_string(&file)
                .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;
            let workflow = genesis_core::workflow::parse_workflow(&yaml)
                .map_err(|e| CliError::Other(format!("invalid workflow YAML: {e}")))?;
            let issues = genesis_core::workflow::validate_workflow(&workflow);
            if issues.is_empty() {
                Ok(format!("Workflow '{}' is valid ({} steps)", workflow.name, workflow.steps.len()))
            } else {
                Err(CliError::Other(format!("Validation errors:\n{}", issues.join("\n"))))
            }
        }
        Command::Workflow(WorkflowCommand::Run { file, input, session_id }) => {
            let yaml = fs::read_to_string(&file)
                .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;
            let workflow = genesis_core::workflow::parse_workflow(&yaml)
                .map_err(|e| CliError::Other(format!("invalid workflow YAML: {e}")))?;

            let loaded = load(cli.config.as_deref())?;
            let session_id = session_id.unwrap_or_else(|| {
                let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
                format!("workflow-{}-{ts}", workflow.name)
            });

            let svc = genesis_core::execution::SessionExecutionService::new(&loaded);
            let result = svc.run_workflow(&workflow, &input, &session_id).await
                .map_err(|e| CliError::Other(format!("workflow failed: {e}")))?;

            if cli.json {
                Ok(serde_json::to_string_pretty(&result)?)
            } else {
                let mut output = format!("Workflow '{}' completed ({} steps)\n", result.workflow_name, result.steps_completed());
                for step in &result.step_results {
                    output.push_str(&format!("\n--- {} ---\n{}\n", step.step_name, step.output));
                }
                output.push_str(&format!(
                    "\nTokens: {} in / {} out",
                    result.total_input_tokens, result.total_output_tokens
                ));
                Ok(output)
            }
        }
        Command::Login => run_login(cli.config).await,
        Command::Logout => run_logout(),
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "genesis", &mut io::stdout());
            Ok(String::new())
        }
        Command::Uninstall { remove_data, remove_config, force } => {
            run_uninstall(cli.config.as_deref(), remove_data, remove_config, force)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AggregatedToolUsage {
    name: String,
    call_count: usize,
    result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EvalSummary {
    directory: String,
    recursive: bool,
    model_filter: Option<String>,
    tag_filter: Option<String>,
    tool_filter: Option<String>,
    failures_only: bool,
    warnings_only: bool,
    min_warnings: Option<usize>,
    files_processed: usize,
    total_events: usize,
    event_counts: ReplayEventCounts,
    warnings: usize,
    success_count: usize,
    failure_count: usize,
    abandoned_count: usize,
    missing_outcome_count: usize,
    top_warning_messages: Vec<(String, usize)>,
    top_failure_reasons: Vec<(String, usize)>,
    models: Vec<(String, usize)>,
    tags: Vec<(String, usize)>,
    tools: Vec<AggregatedToolUsage>,
}

#[derive(Debug, Clone, PartialEq)]
struct EvalStats {
    directory: String,
    recursive: bool,
    model_filter: Option<String>,
    tag_filter: Option<String>,
    tool_filter: Option<String>,
    failures_only: bool,
    total_trajectories: usize,
    total_turns: usize,
    average_turns_per_trajectory: f64,
    min_turns: usize,
    max_turns: usize,
    p50_turns: usize,
    p90_turns: usize,
    p99_turns: usize,
    average_tool_calls_per_trajectory: f64,
    tool_usage: Vec<AggregatedToolUsage>,
    model_distribution: Vec<(String, usize)>,
    tag_distribution: Vec<(String, usize)>,
    outcome_distribution: Vec<(String, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplayEventDelta {
    user: i64,
    assistant: i64,
    tool_call: i64,
    tool_result: i64,
    system: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolUsageDelta {
    name: String,
    left_call_count: usize,
    right_call_count: usize,
    left_result_count: usize,
    right_result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EvalComparison {
    left_path: String,
    right_path: String,
    left_session_id: String,
    right_session_id: String,
    left_model: String,
    right_model: String,
    left_total_events: usize,
    right_total_events: usize,
    left_warning_count: usize,
    right_warning_count: usize,
    event_delta: ReplayEventDelta,
    tools: Vec<ToolUsageDelta>,
    left_only_tags: Vec<String>,
    right_only_tags: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn summarize_replay_reports(
    dir: &str,
    recursive: bool,
    model_filter: Option<&str>,
    tag_filter: Option<&str>,
    tool_filter: Option<&str>,
    failures_only: bool,
    warnings_only: bool,
    min_warnings: Option<usize>,
) -> Result<EvalSummary, CliError> {
    let reports = load_filtered_replay_reports(
        dir,
        recursive,
        model_filter,
        tag_filter,
        tool_filter,
        failures_only,
        warnings_only,
        min_warnings,
    )?;

    let mut model_counts = BTreeMap::<String, usize>::new();
    let mut tag_counts = BTreeMap::<String, usize>::new();
    let mut tool_counts = BTreeMap::<String, AggregatedToolUsage>::new();
    let mut event_counts = ReplayEventCounts::default();
    let mut total_events = 0usize;
    let mut warnings = 0usize;
    let mut success_count = 0usize;
    let mut failure_count = 0usize;
    let mut abandoned_count = 0usize;
    let mut missing_outcome_count = 0usize;
    let mut warning_counts = BTreeMap::<String, usize>::new();
    let mut failure_reasons = BTreeMap::<String, usize>::new();

    for report in &reports {
        total_events += report.total_events;
        warnings += report.warnings.len();
        event_counts.user += report.event_counts.user;
        event_counts.assistant += report.event_counts.assistant;
        event_counts.tool_call += report.event_counts.tool_call;
        event_counts.tool_result += report.event_counts.tool_result;
        event_counts.system += report.event_counts.system;

        *model_counts.entry(report.model.clone()).or_default() += 1;
        for tag in &report.tags {
            *tag_counts.entry(tag.clone()).or_default() += 1;
        }
        for tool in &report.tool_usage {
            let entry = tool_counts
                .entry(tool.name.clone())
                .or_insert_with(|| AggregatedToolUsage {
                    name: tool.name.clone(),
                    call_count: 0,
                    result_count: 0,
                });
            entry.call_count += tool.call_count;
            entry.result_count += tool.result_count;
        }
        for warning in &report.warnings {
            *warning_counts.entry(warning.message.clone()).or_default() += 1;
        }

        match &report.outcome {
            Some(genesis_core::trajectory::TrajectoryOutcome::Success) => success_count += 1,
            Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. }) => {
                failure_count += 1;
                if let Some(genesis_core::trajectory::TrajectoryOutcome::Failure { reason }) =
                    &report.outcome
                {
                    *failure_reasons.entry(reason.clone()).or_default() += 1;
                }
            }
            Some(genesis_core::trajectory::TrajectoryOutcome::Abandoned) => {
                abandoned_count += 1
            }
            None => missing_outcome_count += 1,
        }
    }

    let mut tools = tool_counts.into_values().collect::<Vec<_>>();
    tools.sort_by(|left, right| {
        right
            .call_count
            .cmp(&left.call_count)
            .then(right.result_count.cmp(&left.result_count))
            .then(left.name.cmp(&right.name))
    });
    let mut top_warning_messages = warning_counts.into_iter().collect::<Vec<_>>();
    top_warning_messages.sort_by(|left, right| {
        right.1.cmp(&left.1).then(left.0.cmp(&right.0))
    });
    top_warning_messages.truncate(5);

    let mut top_failure_reasons = failure_reasons.into_iter().collect::<Vec<_>>();
    top_failure_reasons.sort_by(|left, right| {
        right.1.cmp(&left.1).then(left.0.cmp(&right.0))
    });
    top_failure_reasons.truncate(5);

    Ok(EvalSummary {
        directory: dir.to_owned(),
        recursive,
        model_filter: model_filter.map(str::to_owned),
        tag_filter: tag_filter.map(str::to_owned),
        tool_filter: tool_filter.map(str::to_owned),
        failures_only,
        warnings_only,
        min_warnings,
        files_processed: reports.len(),
        total_events,
        event_counts,
        warnings,
        success_count,
        failure_count,
        abandoned_count,
        missing_outcome_count,
        top_warning_messages,
        top_failure_reasons,
        models: model_counts.into_iter().collect(),
        tags: tag_counts.into_iter().collect(),
        tools,
    })
}

#[allow(clippy::too_many_arguments)]
fn load_filtered_replay_reports(
    dir: &str,
    recursive: bool,
    model_filter: Option<&str>,
    tag_filter: Option<&str>,
    tool_filter: Option<&str>,
    failures_only: bool,
    warnings_only: bool,
    min_warnings: Option<usize>,
) -> Result<Vec<ReplayReport>, CliError> {
    let mut reports = Vec::new();

    for path in collect_eval_files(PathBuf::from(dir), recursive)? {
        let report = load_and_report(&path).map_err(|e| CliError::Replay(e.to_string()))?;
        if let Some(model_filter) = model_filter {
            if report.model != model_filter {
                continue;
            }
        }
        if let Some(tag_filter) = tag_filter {
            if !report.tags.iter().any(|tag| tag == tag_filter) {
                continue;
            }
        }
        if let Some(tool_filter) = tool_filter {
            if !report.tool_usage.iter().any(|tool| tool.name == tool_filter) {
                continue;
            }
        }
        if failures_only
            && !matches!(
                report.outcome,
                Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. })
            )
        {
            continue;
        }
        if warnings_only && report.warnings.is_empty() {
            continue;
        }
        if let Some(min_warnings) = min_warnings {
            if report.warnings.len() < min_warnings {
                continue;
            }
        }
        reports.push(report);
    }

    Ok(reports)
}

fn compute_eval_stats(
    dir: &str,
    recursive: bool,
    model_filter: Option<&str>,
    tag_filter: Option<&str>,
    tool_filter: Option<&str>,
    failures_only: bool,
) -> Result<EvalStats, CliError> {
    let reports = load_filtered_replay_reports(
        dir,
        recursive,
        model_filter,
        tag_filter,
        tool_filter,
        failures_only,
        false,
        None,
    )?;

    let total_trajectories = reports.len();
    let mut turn_counts = reports.iter().map(|r| r.total_events).collect::<Vec<_>>();
    turn_counts.sort_unstable();

    let total_turns = turn_counts.iter().sum::<usize>();
    let min_turns = turn_counts.first().copied().unwrap_or(0);
    let max_turns = turn_counts.last().copied().unwrap_or(0);
    let average_turns_per_trajectory = if total_trajectories == 0 {
        0.0
    } else {
        total_turns as f64 / total_trajectories as f64
    };

    let total_tool_calls = reports
        .iter()
        .map(|report| report.event_counts.tool_call)
        .sum::<usize>();
    let average_tool_calls_per_trajectory = if total_trajectories == 0 {
        0.0
    } else {
        total_tool_calls as f64 / total_trajectories as f64
    };

    let mut tool_usage = BTreeMap::<String, AggregatedToolUsage>::new();
    let mut model_distribution = BTreeMap::<String, usize>::new();
    let mut tag_distribution = BTreeMap::<String, usize>::new();
    let mut outcome_distribution = BTreeMap::<String, usize>::new();

    for report in &reports {
        *model_distribution.entry(report.model.clone()).or_default() += 1;
        for tag in &report.tags {
            *tag_distribution.entry(tag.clone()).or_default() += 1;
        }

        let outcome = match &report.outcome {
            Some(genesis_core::trajectory::TrajectoryOutcome::Success) => "success",
            Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. }) => "failure",
            Some(genesis_core::trajectory::TrajectoryOutcome::Abandoned) => "abandoned",
            None => "missing",
        };
        *outcome_distribution.entry(outcome.to_owned()).or_default() += 1;

        for tool in &report.tool_usage {
            let entry = tool_usage
                .entry(tool.name.clone())
                .or_insert_with(|| AggregatedToolUsage {
                    name: tool.name.clone(),
                    call_count: 0,
                    result_count: 0,
                });
            entry.call_count += tool.call_count;
            entry.result_count += tool.result_count;
        }
    }

    let mut tool_usage = tool_usage.into_values().collect::<Vec<_>>();
    tool_usage.sort_by(|left, right| {
        right
            .call_count
            .cmp(&left.call_count)
            .then(right.result_count.cmp(&left.result_count))
            .then(left.name.cmp(&right.name))
    });

    Ok(EvalStats {
        directory: dir.to_owned(),
        recursive,
        model_filter: model_filter.map(str::to_owned),
        tag_filter: tag_filter.map(str::to_owned),
        tool_filter: tool_filter.map(str::to_owned),
        failures_only,
        total_trajectories,
        total_turns,
        average_turns_per_trajectory,
        min_turns,
        max_turns,
        p50_turns: percentile(&turn_counts, 50.0),
        p90_turns: percentile(&turn_counts, 90.0),
        p99_turns: percentile(&turn_counts, 99.0),
        average_tool_calls_per_trajectory,
        tool_usage,
        model_distribution: model_distribution.into_iter().collect(),
        tag_distribution: tag_distribution.into_iter().collect(),
        outcome_distribution: outcome_distribution.into_iter().collect(),
    })
}

fn collect_eval_files(dir: PathBuf, recursive: bool) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();

    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                files.extend(collect_eval_files(path, true)?);
            }
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

fn compare_replay_reports(left: &str, right: &str) -> Result<EvalComparison, CliError> {
    let left_report = load_and_report(left).map_err(|e| CliError::Replay(e.to_string()))?;
    let right_report = load_and_report(right).map_err(|e| CliError::Replay(e.to_string()))?;

    let mut tool_deltas = BTreeMap::<String, ToolUsageDelta>::new();
    for tool in &left_report.tool_usage {
        tool_deltas.insert(
            tool.name.clone(),
            ToolUsageDelta {
                name: tool.name.clone(),
                left_call_count: tool.call_count,
                right_call_count: 0,
                left_result_count: tool.result_count,
                right_result_count: 0,
            },
        );
    }
    for tool in &right_report.tool_usage {
        let entry = tool_deltas
            .entry(tool.name.clone())
            .or_insert_with(|| ToolUsageDelta {
                name: tool.name.clone(),
                left_call_count: 0,
                right_call_count: 0,
                left_result_count: 0,
                right_result_count: 0,
            });
        entry.right_call_count = tool.call_count;
        entry.right_result_count = tool.result_count;
    }

    let left_tags = left_report.tags.iter().cloned().collect::<HashSet<_>>();
    let right_tags = right_report.tags.iter().cloned().collect::<HashSet<_>>();

    let mut tools = tool_deltas.into_values().collect::<Vec<_>>();
    tools.sort_by(|left, right| left.name.cmp(&right.name));

    let mut left_only_tags = left_tags
        .difference(&right_tags)
        .cloned()
        .collect::<Vec<_>>();
    left_only_tags.sort();

    let mut right_only_tags = right_tags
        .difference(&left_tags)
        .cloned()
        .collect::<Vec<_>>();
    right_only_tags.sort();

    Ok(EvalComparison {
        left_path: left.to_owned(),
        right_path: right.to_owned(),
        left_session_id: left_report.session_id,
        right_session_id: right_report.session_id,
        left_model: left_report.model,
        right_model: right_report.model,
        left_total_events: left_report.total_events,
        right_total_events: right_report.total_events,
        left_warning_count: left_report.warnings.len(),
        right_warning_count: right_report.warnings.len(),
        event_delta: ReplayEventDelta {
            user: right_report.event_counts.user as i64 - left_report.event_counts.user as i64,
            assistant: right_report.event_counts.assistant as i64
                - left_report.event_counts.assistant as i64,
            tool_call: right_report.event_counts.tool_call as i64
                - left_report.event_counts.tool_call as i64,
            tool_result: right_report.event_counts.tool_result as i64
                - left_report.event_counts.tool_result as i64,
            system: right_report.event_counts.system as i64
                - left_report.event_counts.system as i64,
        },
        tools,
        left_only_tags,
        right_only_tags,
    })
}

fn format_replay_report(report: &ReplayReport) -> String {
    let mut output = String::new();
    output.push_str("genesis eval report\n");
    output.push_str(&format!("session:      {}\n", report.session_id));
    output.push_str(&format!("model:        {}\n", report.model));
    output.push_str(&format!("started:      {}\n", report.started_at));
    output.push_str(&format!(
        "completed:    {}\n",
        report.completed_at.as_deref().unwrap_or("<missing>")
    ));
    output.push_str(&format!(
        "outcome:      {}\n",
        match &report.outcome {
            Some(genesis_core::trajectory::TrajectoryOutcome::Success) => "success",
            Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. }) => "failure",
            Some(genesis_core::trajectory::TrajectoryOutcome::Abandoned) => "abandoned",
            None => "<missing>",
        }
    ));
    output.push_str(&format!("tags:         {}\n", report.tags.join(", ")));
    output.push_str(&format!("events:       {}\n", report.total_events));
    output.push_str(&format!(
        "event counts: user={} assistant={} tool_call={} tool_result={} system={}\n",
        report.event_counts.user,
        report.event_counts.assistant,
        report.event_counts.tool_call,
        report.event_counts.tool_result,
        report.event_counts.system
    ));
    output.push_str(&format!("warnings:     {}\n", report.warnings.len()));

    if !report.tool_usage.is_empty() {
        output.push_str("tools:\n");
        for tool in &report.tool_usage {
            output.push_str(&format!(
                "  - {}\tcall={} result={}\n",
                tool.name, tool.call_count, tool.result_count
            ));
        }
    }

    if !report.warnings.is_empty() {
        output.push_str("replay warnings:\n");
        for warning in &report.warnings {
            output.push_str(&format!("  - {}\n", warning.message));
        }
    }

    output
}

fn format_eval_summary(summary: &EvalSummary) -> String {
    let mut output = String::new();
    output.push_str("genesis eval summarize\n");
    output.push_str(&format!("directory:       {}\n", summary.directory));
    output.push_str(&format!("recursive:       {}\n", summary.recursive));
    output.push_str(&format!(
        "model filter:    {}\n",
        summary.model_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!(
        "tag filter:      {}\n",
        summary.tag_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!(
        "tool filter:     {}\n",
        summary.tool_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!("failures only:   {}\n", summary.failures_only));
    output.push_str(&format!("warnings only:   {}\n", summary.warnings_only));
    output.push_str(&format!(
        "min warnings:    {}\n",
        summary
            .min_warnings
            .map(|count| count.to_string())
            .unwrap_or_else(|| "<none>".to_owned())
    ));
    output.push_str(&format!("files:           {}\n", summary.files_processed));
    output.push_str(&format!("total events:    {}\n", summary.total_events));
    output.push_str(&format!(
        "event counts:    user={} assistant={} tool_call={} tool_result={} system={}\n",
        summary.event_counts.user,
        summary.event_counts.assistant,
        summary.event_counts.tool_call,
        summary.event_counts.tool_result,
        summary.event_counts.system
    ));
    output.push_str(&format!("warnings:        {}\n", summary.warnings));
    output.push_str(&format!(
        "outcomes:        success={} failure={} abandoned={} missing={}\n",
        summary.success_count,
        summary.failure_count,
        summary.abandoned_count,
        summary.missing_outcome_count
    ));

    if !summary.top_warning_messages.is_empty() {
        output.push_str("top warnings:\n");
        for (message, count) in &summary.top_warning_messages {
            output.push_str(&format!("  - {count}x {message}\n"));
        }
    }

    if !summary.top_failure_reasons.is_empty() {
        output.push_str("top failure reasons:\n");
        for (reason, count) in &summary.top_failure_reasons {
            output.push_str(&format!("  - {count}x {reason}\n"));
        }
    }

    if !summary.models.is_empty() {
        output.push_str("models:\n");
        for (model, count) in &summary.models {
            output.push_str(&format!("  - {model}: {count}\n"));
        }
    }

    if !summary.tags.is_empty() {
        output.push_str("tags:\n");
        for (tag, count) in &summary.tags {
            output.push_str(&format!("  - {tag}: {count}\n"));
        }
    }

    if !summary.tools.is_empty() {
        output.push_str("tools:\n");
        for tool in &summary.tools {
            output.push_str(&format!(
                "  - {}\tcall={} result={}\n",
                tool.name, tool.call_count, tool.result_count
            ));
        }
    }

    output
}

fn format_eval_comparison(comparison: &EvalComparison) -> String {
    let mut output = String::new();
    output.push_str("genesis eval compare\n");
    output.push_str(&format!("left:            {}\n", comparison.left_path));
    output.push_str(&format!("right:           {}\n", comparison.right_path));
    output.push_str(&format!(
        "sessions:        {} vs {}\n",
        comparison.left_session_id, comparison.right_session_id
    ));
    output.push_str(&format!(
        "models:          {} vs {}\n",
        comparison.left_model, comparison.right_model
    ));
    output.push_str(&format!(
        "total events:    {} -> {}\n",
        comparison.left_total_events, comparison.right_total_events
    ));
    output.push_str(&format!(
        "warnings:        {} -> {}\n",
        comparison.left_warning_count, comparison.right_warning_count
    ));
    output.push_str(&format!(
        "event delta:     user={:+} assistant={:+} tool_call={:+} tool_result={:+} system={:+}\n",
        comparison.event_delta.user,
        comparison.event_delta.assistant,
        comparison.event_delta.tool_call,
        comparison.event_delta.tool_result,
        comparison.event_delta.system
    ));

    if !comparison.left_only_tags.is_empty() || !comparison.right_only_tags.is_empty() {
        output.push_str("tag differences:\n");
        if !comparison.left_only_tags.is_empty() {
            output.push_str(&format!(
                "  - left only: {}\n",
                comparison.left_only_tags.join(", ")
            ));
        }
        if !comparison.right_only_tags.is_empty() {
            output.push_str(&format!(
                "  - right only: {}\n",
                comparison.right_only_tags.join(", ")
            ));
        }
    }

    if !comparison.tools.is_empty() {
        output.push_str("tool deltas:\n");
        for tool in &comparison.tools {
            output.push_str(&format!(
                "  - {}\tcall {} -> {}\tresult {} -> {}\n",
                tool.name,
                tool.left_call_count,
                tool.right_call_count,
                tool.left_result_count,
                tool.right_result_count
            ));
        }
    }

    output
}

fn format_eval_stats(stats: &EvalStats) -> String {
    let mut output = String::new();
    output.push_str("genesis eval stats\n");
    output.push_str(&format!("directory:                 {}\n", stats.directory));
    output.push_str(&format!("recursive:                 {}\n", stats.recursive));
    output.push_str(&format!(
        "model filter:              {}\n",
        stats.model_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!(
        "tag filter:                {}\n",
        stats.tag_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!(
        "tool filter:               {}\n",
        stats.tool_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!("failures only:             {}\n", stats.failures_only));
    output.push_str(&format!("total trajectories:        {}\n", stats.total_trajectories));
    output.push_str(&format!("total turns:               {}\n", stats.total_turns));
    output.push_str(&format!(
        "avg turns / trajectory:    {:.2}\n",
        stats.average_turns_per_trajectory
    ));
    output.push_str(&format!("min turns:                 {}\n", stats.min_turns));
    output.push_str(&format!("max turns:                 {}\n", stats.max_turns));
    output.push_str(&format!("p50 turns:                 {}\n", stats.p50_turns));
    output.push_str(&format!("p90 turns:                 {}\n", stats.p90_turns));
    output.push_str(&format!("p99 turns:                 {}\n", stats.p99_turns));
    output.push_str(&format!(
        "avg tool calls / traj:     {:.2}\n",
        stats.average_tool_calls_per_trajectory
    ));

    if !stats.tool_usage.is_empty() {
        output.push_str("tool usage:\n");
        for tool in &stats.tool_usage {
            output.push_str(&format!(
                "  - {}\tcall={} result={}\n",
                tool.name, tool.call_count, tool.result_count
            ));
        }
    }
    if !stats.model_distribution.is_empty() {
        output.push_str("model distribution:\n");
        for (model, count) in &stats.model_distribution {
            output.push_str(&format!("  - {model}: {count}\n"));
        }
    }
    if !stats.tag_distribution.is_empty() {
        output.push_str("tag distribution:\n");
        for (tag, count) in &stats.tag_distribution {
            output.push_str(&format!("  - {tag}: {count}\n"));
        }
    }
    if !stats.outcome_distribution.is_empty() {
        output.push_str("outcome distribution:\n");
        for (outcome, count) in &stats.outcome_distribution {
            output.push_str(&format!("  - {outcome}: {count}\n"));
        }
    }

    output
}

fn eval_summary_to_json(summary: &EvalSummary) -> serde_json::Value {
    serde_json::json!({
        "directory": summary.directory,
        "recursive": summary.recursive,
        "model_filter": summary.model_filter,
        "tag_filter": summary.tag_filter,
        "tool_filter": summary.tool_filter,
        "failures_only": summary.failures_only,
        "warnings_only": summary.warnings_only,
        "min_warnings": summary.min_warnings,
        "files_processed": summary.files_processed,
        "total_events": summary.total_events,
        "event_counts": {
            "user": summary.event_counts.user,
            "assistant": summary.event_counts.assistant,
            "tool_call": summary.event_counts.tool_call,
            "tool_result": summary.event_counts.tool_result,
            "system": summary.event_counts.system,
        },
        "warnings": summary.warnings,
        "success_count": summary.success_count,
        "failure_count": summary.failure_count,
        "abandoned_count": summary.abandoned_count,
        "missing_outcome_count": summary.missing_outcome_count,
        "top_warning_messages": summary.top_warning_messages.iter().map(|(message, count)| {
            serde_json::json!({
                "message": message,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "top_failure_reasons": summary.top_failure_reasons.iter().map(|(reason, count)| {
            serde_json::json!({
                "reason": reason,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "models": summary.models.iter().map(|(model, count)| {
            serde_json::json!({
                "model": model,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "tags": summary.tags.iter().map(|(tag, count)| {
            serde_json::json!({
                "tag": tag,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "tools": summary.tools.iter().map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "call_count": tool.call_count,
                "result_count": tool.result_count,
            })
        }).collect::<Vec<_>>(),
    })
}

fn eval_comparison_to_json(comparison: &EvalComparison) -> serde_json::Value {
    serde_json::json!({
        "left_path": comparison.left_path,
        "right_path": comparison.right_path,
        "left_session_id": comparison.left_session_id,
        "right_session_id": comparison.right_session_id,
        "left_model": comparison.left_model,
        "right_model": comparison.right_model,
        "left_total_events": comparison.left_total_events,
        "right_total_events": comparison.right_total_events,
        "left_warning_count": comparison.left_warning_count,
        "right_warning_count": comparison.right_warning_count,
        "event_delta": {
            "user": comparison.event_delta.user,
            "assistant": comparison.event_delta.assistant,
            "tool_call": comparison.event_delta.tool_call,
            "tool_result": comparison.event_delta.tool_result,
            "system": comparison.event_delta.system,
        },
        "tools": comparison.tools.iter().map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "left_call_count": tool.left_call_count,
                "right_call_count": tool.right_call_count,
                "left_result_count": tool.left_result_count,
                "right_result_count": tool.right_result_count,
            })
        }).collect::<Vec<_>>(),
        "left_only_tags": comparison.left_only_tags,
        "right_only_tags": comparison.right_only_tags,
    })
}

fn eval_stats_to_json(stats: &EvalStats) -> serde_json::Value {
    serde_json::json!({
        "directory": stats.directory,
        "recursive": stats.recursive,
        "model_filter": stats.model_filter,
        "tag_filter": stats.tag_filter,
        "tool_filter": stats.tool_filter,
        "failures_only": stats.failures_only,
        "total_trajectories": stats.total_trajectories,
        "total_turns": stats.total_turns,
        "average_turns_per_trajectory": stats.average_turns_per_trajectory,
        "min_turns": stats.min_turns,
        "max_turns": stats.max_turns,
        "p50_turns": stats.p50_turns,
        "p90_turns": stats.p90_turns,
        "p99_turns": stats.p99_turns,
        "average_tool_calls_per_trajectory": stats.average_tool_calls_per_trajectory,
        "tool_usage": stats.tool_usage.iter().map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "call_count": tool.call_count,
                "result_count": tool.result_count,
            })
        }).collect::<Vec<_>>(),
        "model_distribution": stats.model_distribution.iter().map(|(model, count)| {
            serde_json::json!({
                "model": model,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "tag_distribution": stats.tag_distribution.iter().map(|(tag, count)| {
            serde_json::json!({
                "tag": tag,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "outcome_distribution": stats.outcome_distribution.iter().map(|(outcome, count)| {
            serde_json::json!({
                "outcome": outcome,
                "count": count,
            })
        }).collect::<Vec<_>>(),
    })
}

fn percentile(values: &[usize], pct: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    let rank = ((pct / 100.0) * (values.len().saturating_sub(1) as f64)).ceil() as usize;
    values[rank.min(values.len() - 1)]
}

fn is_production_profile(profile: &str) -> bool {
    matches!(
        profile.to_ascii_lowercase().as_str(),
        "prod" | "production"
    )
}

fn parse_bool_env(name: &str) -> Option<Result<bool, CliError>> {
    std::env::var(name).ok().map(|value| {
        value.parse::<bool>().map_err(|_| {
            CliError::Other(format!("invalid value for {name}: {value} (expected true or false)"))
        })
    })
}

fn is_production_environment() -> bool {
    std::env::var("GENESIS_ENV")
        .ok()
        .map(|value| is_production_profile(&value))
        .unwrap_or(false)
}

fn mcp_startup_strict(loaded: &LoadedConfig) -> Result<bool, CliError> {
    if let Some(result) = parse_bool_env("GENESIS_MCP_STRICT_STARTUP") {
        return result;
    }

    Ok(is_production_environment() || is_production_profile(&loaded.config.profile))
}

fn resolve_api_key_required(_profile: &str) -> Result<bool, CliError> {
    if let Some(result) = parse_bool_env("GENESIS_API_KEY_REQUIRED") {
        return result;
    }

    Ok(false)
}

fn parse_trusted_proxies() -> Result<Vec<std::net::IpAddr>, CliError> {
    match std::env::var("GENESIS_TRUSTED_PROXIES") {
        Ok(value) => value
            .split(',')
            .map(|entry| entry.trim())
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                entry.parse::<std::net::IpAddr>().map_err(|_| {
                    CliError::Other(format!(
                        "invalid value for GENESIS_TRUSTED_PROXIES: {entry}"
                    ))
                })
            })
            .collect(),
        Err(_) => Ok(Vec::new()),
    }
}

async fn build_session_service<'a>(
    loaded: &'a LoadedConfig,
    strict_startup: bool,
    approval_handler: bool,
) -> Result<SessionExecutionService<'a>, CliError> {
    let mut service = SessionExecutionService::with_mcp(loaded, strict_startup).await?;
    if approval_handler {
        service.set_approval_handler(std::sync::Arc::new(CliApprovalHandler));
    }
    Ok(service)
}

#[allow(clippy::too_many_arguments)]
async fn run_chat(
    config_path: Option<PathBuf>,
    session_id: Option<String>,
    resume: Option<String>,
    initial_prompt: Option<String>,
    system_override: Option<String>,
    last: bool,
    worktree: bool,
    clipboard: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;
    let strict_startup = mcp_startup_strict(&loaded)?;
    let mut service = build_session_service(&loaded, strict_startup, true).await?;
    if let Some(ref sys) = system_override {
        service.set_system_prompt_override(sys.clone());
    }

    // Set up git worktree isolation if requested.
    let _worktree_guard = if worktree {
        let guard = create_worktree()?;
        service.set_default_working_dir(guard.path.clone());
        println!("Working in isolated worktree: {}", guard.path);
        Some(guard)
    } else {
        None
    };

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

    // Load persistent readline history
    let history_path = loaded.config.storage.data_dir.join("chat_history.txt");
    let _ = rl.load_history(&history_path);

    let model = &loaded.config.provider.model;
    let mut session_id = session_id;

    // Extract clipboard image if --clipboard was passed
    let mut pending_clipboard_images: Vec<genesis_provider::ImageUrl> = Vec::new();
    if clipboard {
        match extract_clipboard_as_image_url(&loaded.config.storage.data_dir) {
            Ok(img) => {
                println!("     [clipboard image attached]");
                pending_clipboard_images.push(img);
            }
            Err(e) => {
                println!("     [clipboard: {e}]");
            }
        }
    } else if clipboard::has_clipboard_image().unwrap_or(false) {
        println!("     [clipboard image detected — use /paste to attach it]");
    }

    // Process initial prompt if provided
    if let Some(initial) = initial_prompt {
        println!("you> {initial}");
        let images = std::mem::take(&mut pending_clipboard_images);
        run_streaming_turn(&service, &session_id, &initial, model, images).await?;
    }

    while let Some(input) = read_multiline_input(&mut rl, "you> ", "  .. ") {
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
                        run_streaming_turn(&service, &session_id, &prompt_text, model, Vec::new()).await?;
                    }
                }
                None => println!("No user message to retry."),
            }
            continue;
        }

        // Handle /resume <session_id> — switch to a different session
        if trimmed.starts_with("/resume") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            if parts.len() < 2 || parts[1].trim().is_empty() {
                // No arg — list recent sessions to pick from
                match store.list_recent_sessions(10) {
                    Ok(sessions) if !sessions.is_empty() => {
                        println!("Recent sessions:");
                        for s in &sessions {
                            let title = s.title.as_deref().unwrap_or("(untitled)");
                            println!("  {} — {} ({})", s.id, title, s.platform);
                        }
                        println!("\nUsage: /resume <session_id>");
                    }
                    Ok(_) => println!("No sessions found."),
                    Err(e) => println!("Failed to list sessions: {e}"),
                }
            } else {
                let target_id = parts[1].trim();
                match store.get_session(target_id) {
                    Ok(Some(_)) => {
                        let old_id = session_id.clone();
                        session_id = target_id.to_owned();
                        let msgs = store.load_messages(&session_id).unwrap_or_default();
                        println!("Switched from {old_id} → {session_id} ({} messages)", msgs.len());
                    }
                    Ok(None) => println!("Session '{target_id}' not found."),
                    Err(e) => println!("Error looking up session: {e}"),
                }
            }
            continue;
        }

        // Handle /system — view or set system prompt override
        if trimmed.starts_with("/system") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if arg.is_empty() {
                println!("Current system prompt: (use the default agent prompt)");
                println!("Override with: /system <prompt text>");
                println!("Clear override: /system reset");
            } else if arg == "reset" {
                service.clear_system_prompt_override();
                println!("System prompt override cleared. Using default.");
            } else {
                service.set_system_prompt_override(arg.to_owned());
                println!("System prompt override set. Takes effect on next turn.");
            }
            continue;
        }

        // Handle /personality — list or set personality
        // Handle /template — apply an agent template (personality + system prompt + guidelines)
        if trimmed.starts_with("/template") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if arg.is_empty() {
                let templates = genesis_core::templates::list_templates();
                println!("Available agent templates:");
                for t in templates {
                    println!("  {:12} - {} [personality: {}]", t.name, t.description, t.personality);
                }
                println!("\nApply with: /template <name>");
            } else {
                match genesis_core::templates::get_template(arg) {
                    Some(t) => {
                        let prompt = genesis_core::templates::format_template_prompt(t);
                        service.set_personality_override(t.personality.to_owned());
                        service.set_system_prompt_override(prompt);
                        println!("Applied template '{}' (personality: {}).", t.name, t.personality);
                        println!("Guidelines:");
                        for g in t.guidelines {
                            println!("  - {g}");
                        }
                        println!("Takes effect on next turn.");
                    }
                    None => {
                        let names: Vec<&str> =
                            genesis_core::templates::list_templates()
                                .iter()
                                .map(|t| t.name)
                                .collect();
                        println!("Unknown template '{arg}'. Available: {}", names.join(", "));
                    }
                }
            }
            continue;
        }

        // Handle /workflow — run a YAML-defined multi-step workflow
        if trimmed.starts_with("/workflow") {
            let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
            let sub = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if sub.is_empty() || sub == "help" {
                println!(
                    "Usage:\n\
                     /workflow run <file.yaml> [input]  - Run a workflow from YAML file\n\
                     /workflow validate <file.yaml>     - Validate a workflow definition\n\
                     /workflow show <file.yaml>         - Show workflow steps\n\
                     \n\
                     Example workflow YAML:\n\
                     name: research_pipeline\n\
                     description: Research and summarize a topic\n\
                     steps:\n\
                       - name: research\n\
                         prompt: \"Research: {{{{input}}}}\"\n\
                       - name: summarize\n\
                         prompt: \"Summarize: {{{{research}}}}\"\n\
                         terminal: true"
                );
            } else if sub == "validate" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /workflow validate <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::workflow::parse_workflow(&yaml) {
                            Ok(wf) => {
                                let issues = genesis_core::workflow::validate_workflow(&wf);
                                if issues.is_empty() {
                                    println!("Workflow '{}' is valid ({} steps).", wf.name, wf.steps.len());
                                } else {
                                    println!("Workflow '{}' has {} issue(s):", wf.name, issues.len());
                                    for issue in &issues {
                                        println!("  - {issue}");
                                    }
                                }
                            }
                            Err(e) => println!("Failed to parse workflow: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else if sub == "show" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /workflow show <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::workflow::parse_workflow(&yaml) {
                            Ok(wf) => {
                                println!("Workflow: {} — {}", wf.name, wf.description);
                                for (i, step) in wf.steps.iter().enumerate() {
                                    let model_str = step.model.as_deref().unwrap_or("default");
                                    let terminal_str = if step.terminal { " [terminal]" } else { "" };
                                    println!("  Step {}: {} (model: {}){}", i + 1, step.name, model_str, terminal_str);
                                    // Show truncated prompt
                                    let prompt_preview = if step.prompt.len() > 80 {
                                        format!("{}...", &step.prompt[..80])
                                    } else {
                                        step.prompt.clone()
                                    };
                                    println!("    Prompt: {prompt_preview}");
                                }
                            }
                            Err(e) => println!("Failed to parse workflow: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else if sub == "run" {
                let rest = parts.get(2).map(|s| s.trim()).unwrap_or("");
                let (file, input_text) = rest.split_once(' ').unwrap_or((rest, ""));
                if file.is_empty() {
                    println!("Usage: /workflow run <file.yaml> [input text]");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::workflow::parse_workflow(&yaml) {
                            Ok(wf) => {
                                let issues = genesis_core::workflow::validate_workflow(&wf);
                                if !issues.is_empty() {
                                    println!("Workflow has validation issues:");
                                    for issue in &issues {
                                        println!("  - {issue}");
                                    }
                                    println!("Fix these before running.");
                                } else {
                                    println!("Running workflow '{}' ({} steps)...", wf.name, wf.steps.len());
                                    let wf_session_id = format!("{session_id}__wf__{}", wf.name);
                                    match service.run_workflow(&wf, input_text, &wf_session_id).await {
                                        Ok(result) => {
                                            println!("\nWorkflow '{}' complete!", result.workflow_name);
                                            println!("Steps completed: {}/{}", result.steps_completed(), wf.steps.len());
                                            println!("Total tokens: {} in / {} out", result.total_input_tokens, result.total_output_tokens);
                                            for sr in &result.step_results {
                                                println!("\n--- Step: {} ---", sr.step_name);
                                                // Show first 500 chars of output
                                                let preview = if sr.output.len() > 500 {
                                                    format!("{}...", &sr.output[..500])
                                                } else {
                                                    sr.output.clone()
                                                };
                                                println!("{preview}");
                                            }
                                            println!("\n--- Final Output ---");
                                            println!("{}", result.final_output);
                                        }
                                        Err(e) => println!("Workflow execution failed: {e}"),
                                    }
                                }
                            }
                            Err(e) => println!("Failed to parse workflow: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else {
                println!("Unknown workflow subcommand '{sub}'. Use /workflow help.");
            }
            continue;
        }

        // Handle /eval — run evaluation suites
        if trimmed.starts_with("/eval") {
            let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
            let sub = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if sub.is_empty() || sub == "help" {
                println!(
                    "Usage:\n\
                     /eval run <file.yaml>      - Run an evaluation suite\n\
                     /eval validate <file.yaml>  - Validate a suite definition\n\
                     /eval show <file.yaml>      - Show suite test cases\n\
                     \n\
                     Example eval suite YAML:\n\
                     name: basic_math\n\
                     cases:\n\
                       - id: addition\n\
                         prompt: \"What is 2 + 2?\"\n\
                         criteria:\n\
                           must_contain: [\"4\"]"
                );
            } else if sub == "validate" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /eval validate <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::eval::parse_suite(&yaml) {
                            Ok(suite) => {
                                let issues = genesis_core::eval::validate_suite(&suite);
                                if issues.is_empty() {
                                    println!("Suite '{}' is valid ({} cases).", suite.name, suite.cases.len());
                                } else {
                                    println!("Suite '{}' has {} issue(s):", suite.name, issues.len());
                                    for issue in &issues {
                                        println!("  - {issue}");
                                    }
                                }
                            }
                            Err(e) => println!("Failed to parse suite: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else if sub == "show" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /eval show <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::eval::parse_suite(&yaml) {
                            Ok(suite) => {
                                println!("Suite: {} v{} — {}", suite.name, suite.version, suite.description);
                                println!("Cases: {}", suite.cases.len());
                                for case in &suite.cases {
                                    let criteria_count = case.criteria.must_contain.len()
                                        + case.criteria.must_not_contain.len()
                                        + if case.criteria.exact_match.is_some() { 1 } else { 0 }
                                        + if case.criteria.regex_match.is_some() { 1 } else { 0 };
                                    println!(
                                        "  {} (difficulty: {}, {} criteria, tags: [{}])",
                                        case.id,
                                        case.difficulty,
                                        criteria_count,
                                        case.tags.join(", ")
                                    );
                                    let prompt_preview = if case.prompt.len() > 60 {
                                        format!("{}...", &case.prompt[..60])
                                    } else {
                                        case.prompt.clone()
                                    };
                                    println!("    Prompt: {prompt_preview}");
                                }
                            }
                            Err(e) => println!("Failed to parse suite: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else if sub == "run" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /eval run <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::eval::parse_suite(&yaml) {
                            Ok(suite) => {
                                let issues = genesis_core::eval::validate_suite(&suite);
                                if !issues.is_empty() {
                                    println!("Suite has validation issues:");
                                    for issue in &issues {
                                        println!("  - {issue}");
                                    }
                                    println!("Fix these before running.");
                                } else {
                                    println!("Running eval suite '{}' ({} cases)...", suite.name, suite.cases.len());
                                    match service.run_eval(&suite).await {
                                        Ok(report) => {
                                            println!("\n=== Eval Report: {} v{} ===", report.suite_name, report.suite_version);
                                            println!("Model: {}", report.model);
                                            println!("Duration: {}ms", report.total_duration_ms);
                                            println!("Results: {} passed, {} failed, {} errored ({:.0}% pass rate)",
                                                report.passed, report.failed, report.errored, report.pass_rate * 100.0);
                                            println!("Avg score: {:.2}", report.avg_score);
                                            println!("Tokens: {} in / {} out\n", report.total_input_tokens, report.total_output_tokens);

                                            for r in &report.results {
                                                let status = if r.passed { "PASS" } else if r.error.is_some() { "ERROR" } else { "FAIL" };
                                                println!("[{status}] {} — score: {:.2}, {}ms, {} turns",
                                                    r.case_id, r.score, r.duration_ms, r.turns_used);
                                                for check in &r.checks {
                                                    let mark = if check.passed { "✓" } else { "✗" };
                                                    println!("  {mark} {}: {}", check.criterion, check.detail);
                                                }
                                                if let Some(ref err) = r.error {
                                                    println!("  Error: {err}");
                                                }
                                            }

                                            if !report.tag_results.is_empty() {
                                                println!("\nBy tag:");
                                                for (tag, tr) in &report.tag_results {
                                                    println!("  {tag}: {}/{} passed ({:.0}%)", tr.passed, tr.total, tr.pass_rate * 100.0);
                                                }
                                            }
                                        }
                                        Err(e) => println!("Eval run failed: {e}"),
                                    }
                                }
                            }
                            Err(e) => println!("Failed to parse suite: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else {
                println!("Unknown eval subcommand '{sub}'. Use /eval help.");
            }
            continue;
        }

        if trimmed.starts_with("/personality") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if arg.is_empty() {
                let all = genesis_core::personality::list_personalities();
                println!("Available personalities:");
                for p in &all {
                    println!("  {:12} - {}", p.name, p.description);
                }
                println!("\nSet with: /personality <name>");
            } else {
                match genesis_core::personality::get_personality(arg) {
                    Some(p) => {
                        service.set_personality_override(p.name.to_owned());
                        println!("Personality set to '{}'. Takes effect on next turn.", p.name);
                    }
                    None => {
                        let names: Vec<&str> =
                            genesis_core::personality::list_personalities()
                                .iter()
                                .map(|p| p.name)
                                .collect();
                        println!("Unknown personality '{arg}'. Available: {}", names.join(", "));
                    }
                }
            }
            continue;
        }

        // Handle /model — show or switch model at runtime
        if trimmed.starts_with("/model") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if arg.is_empty() {
                println!("Active model: {}", model);
                println!("Set with: /model <backend>/<model>  (e.g. /model anthropic/claude-sonnet-4-20250514)");
            } else if let Some((backend, new_model)) = arg.split_once('/') {
                service.set_model_override(backend.to_owned(), new_model.to_owned());
                println!("Model switched to {backend}/{new_model}. Takes effect on next turn.");
            } else {
                // Assume same backend, just changing model name
                let backend = &loaded.config.provider.backend;
                service.set_model_override(backend.clone(), arg.to_owned());
                println!("Model switched to {backend}/{arg}. Takes effect on next turn.");
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

        // Handle /paste — attach clipboard image to the next message
        if trimmed == "/paste" {
            match extract_clipboard_as_image_url(&loaded.config.storage.data_dir) {
                Ok(img) => {
                    pending_clipboard_images.push(img);
                    println!("     [clipboard image attached — send a message to include it]");
                }
                Err(e) => println!("     [clipboard: {e}]"),
            }
            continue;
        }

        // Handle in-chat slash commands
        if let Some(handled) = handle_chat_command(trimmed, &session_id, &store) {
            println!("{handled}");
            continue;
        }

        let images = std::mem::take(&mut pending_clipboard_images);
        run_streaming_turn(&service, &session_id, trimmed, model, images).await?;
    }

    // Save readline history for next session
    let _ = rl.save_history(&history_path);

    Ok(format!("chat session saved as {session_id}"))
}

/// Run a single prompt non-interactively and return the response.
#[allow(clippy::too_many_arguments)]
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
    let strict_startup = mcp_startup_strict(&loaded)?;
    let mut service = build_session_service(&loaded, strict_startup, true).await?;
    if let Some(sys) = system_override {
        service.set_system_prompt_override(sys);
    }

    let session_id = session_id.unwrap_or_else(default_session_id);
    service.ensure_session(&session_id, "cli", None)?;

    if stream && !json {
        // Streaming mode — print output as it arrives
        run_streaming_turn(&service, &session_id, &prompt, &loaded.config.provider.model, images).await?;
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
    let strict_startup = mcp_startup_strict(loaded)?;
    let service = build_session_service(loaded, strict_startup, false).await?;

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

    let strict_startup = mcp_startup_strict(&loaded)?;
    let service = build_session_service(&loaded, strict_startup, false).await?;
    let mcp = service.mcp_manager();

    let api_key = std::env::var("GENESIS_API_KEY").ok();
    let api_key_required = resolve_api_key_required(&loaded.config.profile)?;
    let trusted_proxies = parse_trusted_proxies()?;
    // Env var overrides config file setting
    let rate_limit_rpm = std::env::var("GENESIS_RATE_LIMIT_RPM")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .or_else(|| loaded.config.gateway.as_ref().and_then(|g| g.rate_limit_rpm));
    let state = std::sync::Arc::new(AppState::new(
        loaded,
        api_key,
        api_key_required,
        mcp,
        rate_limit_rpm,
        trusted_proxies,
    ));
    let router = build_router(std::sync::Arc::clone(&state));

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        CliError::Io(e)
    })?;

    // Start background scheduler
    let db_path = state.loaded.config.storage.database_path.clone();
    let sched_loaded = std::sync::Arc::clone(&state);
    let executor = std::sync::Arc::new(GatewayScheduleExecutor {
        loaded: sched_loaded,
    });
    let scheduler = genesis_core::scheduler::SchedulerRuntime::new(db_path, executor);
    let sched_cancel = scheduler.cancellation_handle();
    let sched_handle = tokio::spawn(scheduler.run());

    println!("genesis gateway listening on {addr}");
    let shutdown_state = std::sync::Arc::clone(&state);
    let serve_result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let uptime = shutdown_state.started_at.elapsed().as_secs();
            let requests = shutdown_state.requests_total.load(std::sync::atomic::Ordering::Relaxed);
            let errors = shutdown_state.errors_total.load(std::sync::atomic::Ordering::Relaxed);
            let input_tokens = shutdown_state.input_tokens_total.load(std::sync::atomic::Ordering::Relaxed);
            let output_tokens = shutdown_state.output_tokens_total.load(std::sync::atomic::Ordering::Relaxed);
            println!("\nshutting down gateway...");
            println!(
                "  uptime: {}s | requests: {} | errors: {} | tokens: {} in / {} out",
                uptime, requests, errors, input_tokens, output_tokens
            );

            // Prune expired cache entries on shutdown
            let cache = genesis_storage::ResponseCacheStore::new(
                &shutdown_state.loaded.config.storage.database_path,
            );
            if let Ok(pruned) = cache.prune_expired() {
                if pruned > 0 {
                    println!("  pruned {pruned} expired cache entries");
                }
            }
        })
        .await;

    // Stop the scheduler
    sched_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Err(error) = sched_handle.await {
        return Err(CliError::Other(format!(
            "scheduler task failed during shutdown: {error}"
        )));
    }

    serve_result.map_err(CliError::Io)?;
    Ok("server stopped".to_owned())
}

/// Schedule executor that runs prompts through SessionExecutionService.
struct GatewayScheduleExecutor {
    loaded: std::sync::Arc<genesis_gateway::AppState>,
}

impl genesis_core::scheduler::ScheduleExecutor for GatewayScheduleExecutor {
    fn execute(
        &self,
        schedule: genesis_core::scheduler::DueSchedule,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            let mut service =
                genesis_core::execution::SessionExecutionService::new(&self.loaded.loaded);
            if let Some(mcp) = &self.loaded.mcp {
                service.set_mcp(std::sync::Arc::clone(mcp));
            }
            let session_id = format!("schedule-{}", schedule.id);
            let title = format!("Schedule: {}", schedule.id);
            let platform =
                genesis_core::execution::delivery_platform_from_str(&schedule.destination);
            let input = genesis_core::execution::SessionTurnInput {
                session_id: &session_id,
                session_platform: &schedule.destination,
                delivery_platform: platform,
                prompt: &schedule.prompt,
                images: Vec::new(),
                title: Some(&title),
            };
            service
                .run_turn(input)
                .await
                .map(|_| ())
                .map_err(|e| format!("{e}"))
        })
    }
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

#[allow(clippy::too_many_arguments)]
async fn run_batch(
    config_path: Option<PathBuf>,
    input: String,
    output: String,
    model_override: Option<String>,
    max_turns: Option<usize>,
    concurrency: Option<usize>,
    toolset: Option<String>,
    quality_filter: Option<f64>,
    auto_tag: bool,
) -> Result<String, CliError> {
    if let Some(score) = quality_filter {
        if !(0.0..=1.0).contains(&score) {
            return Err(CliError::Other(format!(
                "quality filter must be between 0.0 and 1.0, got {score}"
            )));
        }
    }

    let loaded = std::sync::Arc::new(load(config_path.as_deref())?);
    bootstrap(&loaded.config.storage.database_path)?;

    let distribution = match &toolset {
        Some(name) => {
            let dist = genesis_core::toolset::resolve_distribution(
                name,
                &loaded.config.toolsets,
            )
            .ok_or_else(|| {
                let mut available: Vec<String> = genesis_core::toolset::builtin_distribution_names()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                available.extend(loaded.config.toolsets.keys().cloned());
                CliError::Other(format!(
                    "unknown toolset distribution '{name}'. Available: {}",
                    available.join(", ")
                ))
            })?;
            Some(std::sync::Arc::new(dist))
        }
        None => None,
    };

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
        let distribution = distribution.clone();

        tasks.spawn(async move {
            let _permit = permit;
            let result = run_batch_item(
                &loaded,
                &prompt_hash,
                &item,
                &output_dir,
                model_override.as_deref(),
                max_turns,
                distribution.as_ref().map(|d| d.as_ref()),
                quality_filter,
                auto_tag,
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

#[allow(clippy::too_many_arguments)]
async fn run_batch_item(
    loaded: &genesis_config::LoadedConfig,
    session_id: &str,
    item: &BatchInputLine,
    output_dir: &str,
    model_override: Option<&str>,
    max_turns: Option<usize>,
    distribution: Option<&genesis_core::toolset::ToolsetDistribution>,
    quality_filter: Option<f64>,
    auto_tag: bool,
) -> Result<(), CliError> {
    let session_store = SessionStore::new(&loaded.config.storage.database_path);
    let _ = session_store.create_session(session_id, "batch", None);

    let execution_context = genesis_core::build_execution_context_from_loaded(
        loaded,
        session_id.to_owned(),
        DeliveryPlatform::Cli,
    );
    let mut tool_runtime = genesis_core::build_default_tool_runtime(&execution_context);

    // Apply toolset distribution filtering if specified.
    if let Some(dist) = distribution {
        let mut rng = rand::rng();
        let selected = dist.sample(&mut rng);
        tool_runtime.retain(&selected);
    }
    let skills_section = genesis_core::skills::load_skills_prompt_for_prompt(
        &loaded.config.storage.database_path,
        &item.prompt,
    );
    let context_section = load_context_file(
        std::path::Path::new("."),
        &loaded.config.runtime.context_security,
    );
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
    )
    .await?;

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
        genesis_core::hooks::HookRunner::default(),
    );

    if let Some(tp) = &loaded.config.tool_provider {
        let tool_client = genesis_provider::client_from_config(
            &tp.backend,
            &tp.model,
            tp.base_url.as_deref(),
            tp.api_key_env.as_deref(),
        )
        .await?;
        agent.set_tool_client(tool_client);
    }

    if !loaded.config.fallback_providers.is_empty() {
        let mut fallbacks = Vec::new();
        for fp in &loaded.config.fallback_providers {
            let fb_client = genesis_provider::client_from_config(
                &fp.backend,
                &fp.model,
                fp.base_url.as_deref(),
                fp.api_key_env.as_deref(),
            )
            .await?;
            fallbacks.push(fb_client);
        }
        agent.set_fallback_clients(fallbacks);
    }

    for tag in &item.tags {
        agent.trajectory_mut().add_tag(tag);
    }

    let _ = agent.run_turn(&item.prompt).await?;
    if auto_tag {
        apply_auto_tags(output_dir, session_id)?;
    }
    if let Some(min_quality) = quality_filter {
        discard_low_quality_trajectory(output_dir, session_id, min_quality)?;
    }
    Ok(())
}

fn discard_low_quality_trajectory(
    output_dir: &str,
    session_id: &str,
    min_quality: f64,
) -> Result<(), CliError> {
    let output_path = batch_output_path(output_dir, session_id);
    if !output_path.exists() {
        return Ok(());
    }

    let raw = std::fs::read_to_string(&output_path)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", output_path.display())))?;
    let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw).map_err(|e| {
        CliError::Other(format!(
            "invalid trajectory JSON in {}: {e}",
            output_path.display()
        ))
    })?;
    let quality = genesis_core::quality::score(&trajectory);
    if quality.overall < min_quality {
        std::fs::remove_file(&output_path).map_err(|e| {
            CliError::Other(format!(
                "failed to discard low-quality trajectory {}: {e}",
                output_path.display()
            ))
        })?;
    }

    Ok(())
}

fn apply_auto_tags(output_dir: &str, session_id: &str) -> Result<(), CliError> {
    let output_path = batch_output_path(output_dir, session_id);
    if !output_path.exists() {
        return Ok(());
    }

    let raw = std::fs::read_to_string(&output_path)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", output_path.display())))?;
    let mut trajectory: genesis_core::trajectory::Trajectory =
        serde_json::from_str(&raw).map_err(|e| {
            CliError::Other(format!(
                "invalid trajectory JSON in {}: {e}",
                output_path.display()
            ))
        })?;

    let auto_tags = genesis_core::tagger::auto_tag(&trajectory);
    let existing: HashSet<String> = trajectory.tags.iter().cloned().collect();
    for tag in auto_tags {
        if !existing.contains(&tag) {
            trajectory.tags.push(tag);
        }
    }
    trajectory.tags.sort();

    let updated = serde_json::to_string_pretty(&trajectory)?;
    std::fs::write(&output_path, updated).map_err(|e| {
        CliError::Other(format!(
            "failed to write auto-tagged trajectory {}: {e}",
            output_path.display()
        ))
    })?;

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
    training: bool,
) -> Result<String, CliError> {
    let level = parse_compression_level(level.as_deref())?;
    let format = parse_compression_format(format.as_deref())?;

    let raw = std::fs::read_to_string(&input)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", input)))?;
    let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
        .map_err(|e| CliError::Other(format!("invalid trajectory JSON in {}: {e}", input)))?;

    let compressed = if training {
        genesis_core::compress::TrajectoryCompressor::default()
            .compress_for_training(&trajectory)
    } else {
        genesis_core::compress::compress(&trajectory, level)
    };
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

fn run_eval_export_chatml(dir: &str, recursive: bool) -> Result<String, CliError> {
    let mut lines = Vec::new();

    for path in collect_eval_files(PathBuf::from(dir), recursive)? {
        let compressed = load_training_compressed_trajectory(&path)?;
        let line = serde_json::json!({
            "session_id": compressed.session_id,
            "model": compressed.model,
            "tags": compressed.tags,
            "outcome": compressed.outcome,
            "chatml": genesis_core::compress::to_chatml(&compressed),
        });
        lines.push(serde_json::to_string(&line)?);
    }

    Ok(lines.join("\n"))
}

fn run_eval_import_sharegpt(file: &str, output_dir: &str) -> Result<String, CliError> {
    let contents = std::fs::read_to_string(file)
        .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;
    std::fs::create_dir_all(output_dir)
        .map_err(|e| CliError::Other(format!("failed to create {output_dir}: {e}")))?;

    let mut imported = 0usize;
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| CliError::Other(format!("invalid JSONL line {}: {e}", index + 1)))?;
        let trajectory = trajectory_from_sharegpt_entry(&entry, index)?;
        let session_id = sanitize_session_id_for_filename(&trajectory.session_id);
        let output_path = std::path::Path::new(output_dir).join(format!("{session_id}.json"));
        let json = serde_json::to_string_pretty(&trajectory)?;
        std::fs::write(&output_path, json).map_err(|e| {
            CliError::Other(format!("failed to write {}: {e}", output_path.display()))
        })?;
        imported += 1;
    }

    Ok(format!("imported {imported} trajectories into {output_dir}"))
}

fn run_eval_merge(sources: &[String], output: &str, dedup: bool) -> Result<String, CliError> {
    std::fs::create_dir_all(output)
        .map_err(|e| CliError::Other(format!("failed to create {output}: {e}")))?;

    let mut copied = 0usize;
    let mut skipped = 0usize;
    let mut seen_ids: HashSet<String> = HashSet::new();

    for source in sources {
        let source_path = PathBuf::from(source);
        if !source_path.is_dir() {
            return Err(CliError::Other(format!("{source} is not a directory")));
        }

        for entry in std::fs::read_dir(&source_path).map_err(|e| {
            CliError::Other(format!("failed to read directory {source}: {e}"))
        })? {
            let entry = entry.map_err(|e| {
                CliError::Other(format!("failed to read entry in {source}: {e}"))
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let filename = path.file_name().unwrap().to_string_lossy().to_string();

            if dedup {
                let raw = std::fs::read_to_string(&path).map_err(|e| {
                    CliError::Other(format!("failed to read {}: {e}", path.display()))
                })?;
                if let Ok(traj) =
                    serde_json::from_str::<genesis_core::trajectory::Trajectory>(&raw)
                {
                    if !seen_ids.insert(traj.session_id.clone()) {
                        skipped += 1;
                        continue;
                    }
                }
            }

            let dest = std::path::Path::new(output).join(&filename);
            if dest.exists() {
                skipped += 1;
                continue;
            }

            std::fs::copy(&path, &dest).map_err(|e| {
                CliError::Other(format!(
                    "failed to copy {} -> {}: {e}",
                    path.display(),
                    dest.display()
                ))
            })?;
            copied += 1;
        }
    }

    Ok(format!(
        "merged {copied} trajectories into {output} (skipped {skipped})"
    ))
}

fn run_eval_export_sharegpt(dir: &str, recursive: bool) -> Result<String, CliError> {
    let mut lines = Vec::new();

    for path in collect_eval_files(PathBuf::from(dir), recursive)? {
        let compressed = load_training_compressed_trajectory(&path)?;
        let line = serde_json::json!({
            "session_id": compressed.session_id,
            "model": compressed.model,
            "tags": compressed.tags,
            "outcome": compressed.outcome,
            "sharegpt": genesis_core::compress::to_sharegpt(&compressed),
        });
        lines.push(serde_json::to_string(&line)?);
    }

    Ok(lines.join("\n"))
}

fn run_eval_import_chatml(file: &str, output_dir: &str) -> Result<String, CliError> {
    let contents = std::fs::read_to_string(file)
        .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;
    std::fs::create_dir_all(output_dir)
        .map_err(|e| CliError::Other(format!("failed to create {output_dir}: {e}")))?;

    let mut imported = 0usize;
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| CliError::Other(format!("invalid JSONL line {}: {e}", index + 1)))?;
        let trajectory = trajectory_from_chatml_entry(&entry, index)?;
        let session_id = sanitize_session_id_for_filename(&trajectory.session_id);
        let output_path = std::path::Path::new(output_dir).join(format!("{session_id}.json"));
        let json = serde_json::to_string_pretty(&trajectory)?;
        std::fs::write(&output_path, json).map_err(|e| {
            CliError::Other(format!("failed to write {}: {e}", output_path.display()))
        })?;
        imported += 1;
    }

    Ok(format!("imported {imported} trajectories into {output_dir}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvalFileFormat {
    TrajectoryJson,
    ChatmlJsonl,
    SharegptJsonl,
}

fn run_eval_convert(input: &str, output: &str, format: &str) -> Result<String, CliError> {
    let target = format.trim().to_ascii_lowercase();
    let input_format = detect_eval_input_format(input)?;

    let rendered = match (input_format, target.as_str()) {
        (EvalFileFormat::TrajectoryJson, "json") => {
            let raw = std::fs::read_to_string(input)
                .map_err(|e| CliError::Other(format!("failed to read {input}: {e}")))?;
            let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
                .map_err(|e| CliError::Other(format!("invalid trajectory JSON in {input}: {e}")))?;
            serde_json::to_string_pretty(&trajectory)?
        }
        (EvalFileFormat::TrajectoryJson, "chatml") => {
            let compressed =
                load_training_compressed_trajectory(std::path::Path::new(input))?;
            serde_json::to_string(&serde_json::json!({
                "session_id": compressed.session_id,
                "model": compressed.model,
                "tags": compressed.tags,
                "outcome": compressed.outcome,
                "chatml": genesis_core::compress::to_chatml(&compressed),
            }))?
        }
        (EvalFileFormat::TrajectoryJson, "sharegpt") => {
            let compressed =
                load_training_compressed_trajectory(std::path::Path::new(input))?;
            serde_json::to_string(&serde_json::json!({
                "session_id": compressed.session_id,
                "model": compressed.model,
                "tags": compressed.tags,
                "outcome": compressed.outcome,
                "sharegpt": genesis_core::compress::to_sharegpt(&compressed),
            }))?
        }
        (EvalFileFormat::ChatmlJsonl, "json") => {
            let entry = load_single_jsonl_entry(input)?;
            let trajectory = trajectory_from_chatml_entry(&entry, 0)?;
            serde_json::to_string_pretty(&trajectory)?
        }
        (EvalFileFormat::SharegptJsonl, "json") => {
            let entry = load_single_jsonl_entry(input)?;
            let trajectory = trajectory_from_sharegpt_entry(&entry, 0)?;
            serde_json::to_string_pretty(&trajectory)?
        }
        (_, other) => {
            return Err(CliError::Other(format!(
                "unsupported conversion to '{other}' from input {input}"
            )))
        }
    };

    if let Some(parent) = std::path::Path::new(output).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CliError::Other(format!(
                    "failed to create parent directory for {output}: {e}"
                ))
            })?;
        }
    }
    std::fs::write(output, rendered)
        .map_err(|e| CliError::Other(format!("failed to write {output}: {e}")))?;

    Ok(format!("converted {input} -> {output} ({target})"))
}

fn detect_eval_input_format(input: &str) -> Result<EvalFileFormat, CliError> {
    let path = std::path::Path::new(input);
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => Ok(EvalFileFormat::TrajectoryJson),
        Some("jsonl") => {
            let entry = load_single_jsonl_entry(input)?;
            if entry.get("chatml").is_some() {
                Ok(EvalFileFormat::ChatmlJsonl)
            } else if entry.get("sharegpt").is_some() {
                Ok(EvalFileFormat::SharegptJsonl)
            } else {
                Err(CliError::Other(format!(
                    "cannot detect JSONL format for {input}: expected 'chatml' or 'sharegpt' field"
                )))
            }
        }
        _ => Err(CliError::Other(format!(
            "cannot detect input format for {input}: expected .json or .jsonl"
        ))),
    }
}

fn load_single_jsonl_entry(input: &str) -> Result<serde_json::Value, CliError> {
    let contents = std::fs::read_to_string(input)
        .map_err(|e| CliError::Other(format!("failed to read {input}: {e}")))?;
    let lines = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();

    if lines.len() != 1 {
        return Err(CliError::Other(format!(
            "expected exactly 1 JSONL record in {input}, found {}",
            lines.len()
        )));
    }

    serde_json::from_str(lines[0])
        .map_err(|e| CliError::Other(format!("invalid JSONL in {input}: {e}")))
}

fn load_training_compressed_trajectory(
    path: &std::path::Path,
) -> Result<genesis_core::compress::CompressedTrajectory, CliError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
    let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw).map_err(|e| {
        CliError::Other(format!(
            "invalid trajectory JSON in {}: {e}",
            path.display()
        ))
    })?;
    Ok(
        genesis_core::compress::TrajectoryCompressor::default()
            .compress_for_training(&trajectory),
    )
}

fn trajectory_from_chatml_entry(
    entry: &serde_json::Value,
    index: usize,
) -> Result<genesis_core::trajectory::Trajectory, CliError> {
    let chatml = entry
        .get("chatml")
        .and_then(|value| value.as_str())
        .ok_or_else(|| CliError::Other(format!("JSONL line {} missing 'chatml' field", index + 1)))?;

    let session_id = entry
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("chatml-import-{}", index + 1));
    let model = entry
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or("imported-chatml")
        .to_owned();
    let tags = entry
        .get("tags")
        .and_then(|value| value.as_array())
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| tag.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let outcome = entry
        .get("outcome")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| CliError::Other(format!("invalid outcome on line {}: {e}", index + 1)))?;

    let now = chrono::Utc::now().to_rfc3339();
    let messages = parse_chatml_blocks(chatml)?;
    let steps = messages
        .into_iter()
        .enumerate()
        .map(|(step_index, (role, content))| genesis_core::trajectory::TrajectoryStep {
            step_index,
            timestamp: now.clone(),
            action_type: match role.as_str() {
                "system" => genesis_core::trajectory::ActionType::SystemMessage,
                "user" => genesis_core::trajectory::ActionType::UserMessage,
                "assistant" | "tool" => genesis_core::trajectory::ActionType::AssistantMessage,
                _ => genesis_core::trajectory::ActionType::AssistantMessage,
            },
            content,
            tool_name: None,
            tool_arguments: None,
            tool_result: None,
            tokens: None,
        })
        .collect::<Vec<_>>();

    Ok(genesis_core::trajectory::Trajectory {
        session_id,
        model,
        system_prompt_hash: sha256_hex(chatml),
        started_at: now.clone(),
        completed_at: Some(now),
        steps,
        outcome,
        tags,
    })
}

fn trajectory_from_sharegpt_entry(
    entry: &serde_json::Value,
    index: usize,
) -> Result<genesis_core::trajectory::Trajectory, CliError> {
    let sharegpt = entry
        .get("sharegpt")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            CliError::Other(format!("JSONL line {} missing 'sharegpt' field", index + 1))
        })?;

    let session_id = entry
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("sharegpt-import-{}", index + 1));
    let model = entry
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or("imported-sharegpt")
        .to_owned();
    let tags = entry
        .get("tags")
        .and_then(|value| value.as_array())
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| tag.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let outcome = entry
        .get("outcome")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| CliError::Other(format!("invalid outcome on line {}: {e}", index + 1)))?;

    let now = chrono::Utc::now().to_rfc3339();
    let steps = sharegpt
        .iter()
        .enumerate()
        .map(|(step_index, item)| {
            let from = item
                .get("from")
                .and_then(|value| value.as_str())
                .ok_or_else(|| CliError::Other("ShareGPT item missing 'from'".to_owned()))?;
            let value = item
                .get("value")
                .and_then(|value| value.as_str())
                .ok_or_else(|| CliError::Other("ShareGPT item missing 'value'".to_owned()))?;
            let action_type = match from {
                "human" => genesis_core::trajectory::ActionType::UserMessage,
                "gpt" | "thought" => genesis_core::trajectory::ActionType::AssistantMessage,
                _ => genesis_core::trajectory::ActionType::AssistantMessage,
            };

            Ok(genesis_core::trajectory::TrajectoryStep {
                step_index,
                timestamp: now.clone(),
                action_type,
                content: value.to_owned(),
                tool_name: None,
                tool_arguments: None,
                tool_result: None,
                tokens: None,
            })
        })
        .collect::<Result<Vec<_>, CliError>>()?;

    Ok(genesis_core::trajectory::Trajectory {
        session_id,
        model,
        system_prompt_hash: sha256_hex(&serde_json::to_string(sharegpt).unwrap_or_default()),
        started_at: now.clone(),
        completed_at: Some(now),
        steps,
        outcome,
        tags,
    })
}

fn parse_chatml_blocks(chatml: &str) -> Result<Vec<(String, String)>, CliError> {
    let mut messages = Vec::new();
    let mut rest = chatml;

    while let Some(start_idx) = rest.find("<|im_start|>") {
        rest = &rest[start_idx + "<|im_start|>".len()..];
        let end_idx = rest.find("<|im_end|>").ok_or_else(|| {
            CliError::Other("invalid ChatML: missing <|im_end|> marker".to_owned())
        })?;
        let block = &rest[..end_idx];
        rest = &rest[end_idx + "<|im_end|>".len()..];

        let Some((role, content)) = block.split_once('\n') else {
            return Err(CliError::Other(
                "invalid ChatML: block missing role/content separator".to_owned(),
            ));
        };

        messages.push((role.trim().to_owned(), content.to_owned()));
    }

    if messages.is_empty() {
        return Err(CliError::Other("invalid ChatML: no messages found".to_owned()));
    }

    Ok(messages)
}

fn sanitize_session_id_for_filename(session_id: &str) -> String {
    session_id
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

fn run_eval_quality(
    dir: &str,
    recursive: bool,
    min_score: Option<f64>,
    worst_first: bool,
    json: bool,
) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;

    if files.is_empty() {
        return Ok("No trajectory files found.".to_owned());
    }

    let mut scored: Vec<(String, genesis_core::quality::QualityScore)> = Vec::new();

    for path in &files {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
        let trajectory: genesis_core::trajectory::Trajectory =
            serde_json::from_str(&raw).map_err(|e| {
                CliError::Other(format!(
                    "invalid trajectory JSON in {}: {e}",
                    path.display()
                ))
            })?;

        let quality = genesis_core::quality::score(&trajectory);
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_owned();

        if let Some(threshold) = min_score {
            if quality.overall < threshold {
                continue;
            }
        }

        scored.push((name, quality));
    }

    // Sort by overall score
    if worst_first {
        scored.sort_by(|a, b| a.1.overall.partial_cmp(&b.1.overall).unwrap());
    } else {
        scored.sort_by(|a, b| b.1.overall.partial_cmp(&a.1.overall).unwrap());
    }

    if json {
        let entries: Vec<serde_json::Value> = scored
            .iter()
            .map(|(name, q)| {
                serde_json::json!({
                    "file": name,
                    "overall": (q.overall * 100.0).round() / 100.0,
                    "outcome": (q.dimensions.outcome * 100.0).round() / 100.0,
                    "signal_to_noise": (q.dimensions.signal_to_noise * 100.0).round() / 100.0,
                    "tool_diversity": (q.dimensions.tool_diversity * 100.0).round() / 100.0,
                    "depth": (q.dimensions.depth * 100.0).round() / 100.0,
                    "efficiency": (q.dimensions.efficiency * 100.0).round() / 100.0,
                    "completeness": (q.dimensions.completeness * 100.0).round() / 100.0,
                    "issues": q.issues,
                })
            })
            .collect();

        let total = files.len();
        let passed = scored.len();
        let avg_score = if scored.is_empty() {
            0.0
        } else {
            scored.iter().map(|(_, q)| q.overall).sum::<f64>() / scored.len() as f64
        };

        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "total_files": total,
            "scored": passed,
            "filtered_out": total - passed,
            "average_score": (avg_score * 100.0).round() / 100.0,
            "trajectories": entries,
        }))?)
    } else {
        let mut lines = Vec::new();

        let total = files.len();
        let passed = scored.len();
        let avg_score = if scored.is_empty() {
            0.0
        } else {
            scored.iter().map(|(_, q)| q.overall).sum::<f64>() / scored.len() as f64
        };

        lines.push(format!(
            "Quality report: {passed}/{total} trajectories{}",
            if let Some(t) = min_score {
                format!(" (min score: {t:.2})")
            } else {
                String::new()
            }
        ));
        lines.push(format!("Average score: {avg_score:.2}"));
        lines.push(String::new());
        lines.push(format!(
            "{:<40} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
            "FILE", "SCORE", "OUTCM", "S/N", "TOOLS", "DEPTH", "EFFIC", "COMPL"
        ));

        for (name, q) in &scored {
            let truncated_name = if name.len() > 38 {
                format!("{}...", &name[..35])
            } else {
                name.clone()
            };
            lines.push(format!(
                "{:<40} {:>5.2} {:>5.2} {:>5.2} {:>5.2} {:>5.2} {:>5.2} {:>5.2}",
                truncated_name,
                q.overall,
                q.dimensions.outcome,
                q.dimensions.signal_to_noise,
                q.dimensions.tool_diversity,
                q.dimensions.depth,
                q.dimensions.efficiency,
                q.dimensions.completeness,
            ));

            if !q.issues.is_empty() {
                for issue in &q.issues {
                    lines.push(format!("  -> {issue}"));
                }
            }
        }

        Ok(lines.join("\n"))
    }
}

fn run_eval_auto_tag(
    dir: &str,
    recursive: bool,
    dry_run: bool,
    json: bool,
) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut updated = Vec::<(String, Vec<String>)>::new();

    for path in files {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
        let mut trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
            .map_err(|e| {
                CliError::Other(format!(
                    "invalid trajectory JSON in {}: {e}",
                    path.display()
                ))
            })?;

        let suggested = genesis_core::tagger::auto_tag(&trajectory);
        let existing = trajectory.tags.iter().cloned().collect::<HashSet<_>>();
        let mut additions = suggested
            .into_iter()
            .filter(|tag| !existing.contains(tag))
            .collect::<Vec<_>>();
        additions.sort();

        if additions.is_empty() {
            continue;
        }

        if !dry_run {
            trajectory.tags.extend(additions.clone());
            trajectory.tags.sort();
            trajectory.tags.dedup();
            let serialized = serde_json::to_string_pretty(&trajectory)?;
            std::fs::write(&path, serialized).map_err(|e| {
                CliError::Other(format!("failed to write {}: {e}", path.display()))
            })?;
        }

        updated.push((path.display().to_string(), additions));
    }

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "directory": dir,
            "recursive": recursive,
            "dry_run": dry_run,
            "updated": updated.iter().map(|(file, tags)| {
                serde_json::json!({
                    "file": file,
                    "added_tags": tags,
                })
            }).collect::<Vec<_>>(),
            "files_changed": updated.len(),
        }))?);
    }

    if updated.is_empty() {
        return Ok("No new tags to apply.".to_owned());
    }

    let mut lines = Vec::new();
    lines.push("genesis eval auto-tag".to_owned());
    lines.push(format!("directory:    {dir}"));
    lines.push(format!("recursive:    {recursive}"));
    lines.push(format!("dry run:      {dry_run}"));
    lines.push(format!("files changed: {}", updated.len()));
    for (file, tags) in &updated {
        lines.push(format!("{file}: {}", tags.join(", ")));
    }

    Ok(lines.join("\n"))
}

fn run_eval_tag_stats(dir: &str, recursive: bool, json: bool) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut counts = BTreeMap::<String, usize>::new();

    for path in files {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
        let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
            .map_err(|e| {
                CliError::Other(format!(
                    "invalid trajectory JSON in {}: {e}",
                    path.display()
                ))
            })?;
        for tag in trajectory.tags {
            *counts.entry(tag).or_default() += 1;
        }
    }

    let mut tags = counts.into_iter().collect::<Vec<_>>();
    tags.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "directory": dir,
            "recursive": recursive,
            "tags": tags.iter().map(|(tag, count)| {
                serde_json::json!({
                    "tag": tag,
                    "count": count,
                })
            }).collect::<Vec<_>>(),
        }))?);
    }

    if tags.is_empty() {
        return Ok("No trajectory tags found.".to_owned());
    }

    let mut lines = Vec::new();
    lines.push("genesis eval tag-stats".to_owned());
    lines.push(format!("directory: {dir}"));
    lines.push(format!("recursive: {recursive}"));
    for (tag, count) in &tags {
        lines.push(format!("{tag}: {count}"));
    }
    Ok(lines.join("\n"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeduplicateGroup {
    key: String,
    files: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn run_eval_filter(
    dir: &str,
    output: &str,
    recursive: bool,
    model: Option<&str>,
    tag: Option<&str>,
    min_quality: Option<f64>,
    max_quality: Option<f64>,
    success_only: bool,
    failure_only: bool,
    min_steps: Option<usize>,
    max_steps: Option<usize>,
    tool: Option<&str>,
) -> Result<String, CliError> {
    std::fs::create_dir_all(output)
        .map_err(|e| CliError::Other(format!("failed to create {output}: {e}")))?;

    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut matched = 0usize;
    let mut total = 0usize;

    for path in files {
        total += 1;
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            CliError::Other(format!("failed to read {}: {e}", path.display()))
        })?;
        let traj: genesis_core::trajectory::Trajectory = match serde_json::from_str(&raw) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Apply filters
        if let Some(m) = model {
            if !traj.model.contains(m) {
                continue;
            }
        }
        if let Some(t) = tag {
            if !traj.tags.iter().any(|tag| tag == t) {
                continue;
            }
        }
        if success_only
            && !matches!(
                traj.outcome,
                Some(genesis_core::trajectory::TrajectoryOutcome::Success)
            )
        {
            continue;
        }
        if failure_only
            && !matches!(
                traj.outcome,
                Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. })
            )
        {
            continue;
        }
        if let Some(min) = min_steps {
            if traj.steps.len() < min {
                continue;
            }
        }
        if let Some(max) = max_steps {
            if traj.steps.len() > max {
                continue;
            }
        }
        if let Some(tool_name) = tool {
            let has_tool = traj.steps.iter().any(|s| {
                s.tool_name.as_deref() == Some(tool_name)
            });
            if !has_tool {
                continue;
            }
        }
        if min_quality.is_some() || max_quality.is_some() {
            let score = genesis_core::quality::score(&traj).overall;
            if let Some(min) = min_quality {
                if score < min {
                    continue;
                }
            }
            if let Some(max) = max_quality {
                if score > max {
                    continue;
                }
            }
        }

        // Passed all filters — copy to output
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        let dest = std::path::Path::new(output).join(&filename);
        std::fs::copy(&path, &dest).map_err(|e| {
            CliError::Other(format!(
                "failed to copy {} -> {}: {e}",
                path.display(),
                dest.display()
            ))
        })?;
        matched += 1;
    }

    Ok(format!(
        "filtered {matched}/{total} trajectories into {output}"
    ))
}

fn run_eval_split(
    dir: &str,
    train_dir: &str,
    test_dir: &str,
    ratio: f64,
    seed: Option<u64>,
    recursive: bool,
) -> Result<String, CliError> {
    if !(0.0..=1.0).contains(&ratio) {
        return Err(CliError::Other(format!(
            "ratio must be between 0.0 and 1.0, got {ratio}"
        )));
    }

    std::fs::create_dir_all(train_dir)
        .map_err(|e| CliError::Other(format!("failed to create {train_dir}: {e}")))?;
    std::fs::create_dir_all(test_dir)
        .map_err(|e| CliError::Other(format!("failed to create {test_dir}: {e}")))?;

    let mut files = collect_eval_files(PathBuf::from(dir), recursive)?;
    if files.is_empty() {
        return Ok("no trajectory files found".to_owned());
    }

    // Sort for deterministic ordering, then shuffle with seed
    files.sort();
    use rand::seq::SliceRandom;
    let mut rng = match seed {
        Some(s) => {
            use rand::SeedableRng;
            rand::rngs::StdRng::seed_from_u64(s)
        }
        None => {
            use rand::SeedableRng;
            rand::rngs::StdRng::from_os_rng()
        }
    };
    files.shuffle(&mut rng);

    let split_point = (files.len() as f64 * ratio).round() as usize;
    let (train_files, test_files) = files.split_at(split_point);

    for path in train_files {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        std::fs::copy(path, std::path::Path::new(train_dir).join(&filename)).map_err(|e| {
            CliError::Other(format!("failed to copy {}: {e}", path.display()))
        })?;
    }

    for path in test_files {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        std::fs::copy(path, std::path::Path::new(test_dir).join(&filename)).map_err(|e| {
            CliError::Other(format!("failed to copy {}: {e}", path.display()))
        })?;
    }

    Ok(format!(
        "split {} trajectories: {} train, {} test (ratio {ratio})",
        files.len(),
        train_files.len(),
        test_files.len()
    ))
}

fn run_eval_sample(
    dir: &str,
    output: &str,
    count: usize,
    seed: Option<u64>,
    recursive: bool,
) -> Result<String, CliError> {
    std::fs::create_dir_all(output)
        .map_err(|e| CliError::Other(format!("failed to create {output}: {e}")))?;

    let mut files = collect_eval_files(PathBuf::from(dir), recursive)?;
    if files.is_empty() {
        return Ok("no trajectory files found".to_owned());
    }

    let actual_count = count.min(files.len());

    files.sort();
    use rand::seq::SliceRandom;
    let mut rng = match seed {
        Some(s) => {
            use rand::SeedableRng;
            rand::rngs::StdRng::seed_from_u64(s)
        }
        None => {
            use rand::SeedableRng;
            rand::rngs::StdRng::from_os_rng()
        }
    };
    files.shuffle(&mut rng);

    for path in &files[..actual_count] {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        let dest = std::path::Path::new(output).join(&filename);
        std::fs::copy(path, &dest).map_err(|e| {
            CliError::Other(format!("failed to copy {}: {e}", path.display()))
        })?;
    }

    Ok(format!(
        "sampled {actual_count}/{} trajectories into {output}",
        files.len()
    ))
}

fn run_eval_manifest(
    dir: &str,
    name: &str,
    description: &str,
    save: bool,
    recursive: bool,
    json: bool,
) -> Result<String, CliError> {
    let manifest = genesis_core::dataset::build_manifest(
        name,
        description,
        std::path::Path::new(dir),
        recursive,
    )
    .map_err(|e| CliError::Other(format!("failed to build manifest: {e}")))?;

    if save {
        genesis_core::dataset::save_manifest(&manifest, std::path::Path::new(dir))
            .map_err(|e| CliError::Other(format!("failed to save manifest: {e}")))?;
    }

    if json || save {
        return Ok(serde_json::to_string_pretty(&manifest)?);
    }

    let mut lines = vec![
        format!("dataset: {}", manifest.name),
        format!("files: {}", manifest.file_count),
        format!("total steps: {}", manifest.total_steps),
        format!(
            "avg steps: {:.1}",
            manifest.statistics.avg_steps_per_trajectory
        ),
        format!(
            "step range: {}–{}",
            manifest.statistics.min_steps, manifest.statistics.max_steps
        ),
        format!("models: {}", manifest.models.join(", ")),
    ];

    if let Some(q) = manifest.statistics.avg_quality_score {
        lines.push(format!("avg quality: {q:.3}"));
    }

    if !manifest.statistics.outcome_counts.is_empty() {
        let outcomes: Vec<String> = manifest
            .statistics
            .outcome_counts
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        lines.push(format!("outcomes: {}", outcomes.join(", ")));
    }

    if save {
        lines.push(format!("saved to {dir}/dataset.json"));
    }

    Ok(lines.join("\n"))
}

#[allow(clippy::too_many_arguments)]
fn run_eval_pipeline(
    dir: &str,
    output: &str,
    recursive: bool,
    validate: bool,
    auto_tag: bool,
    min_quality: Option<f64>,
    success_only: bool,
    tag: Option<&str>,
    model: Option<&str>,
    format: Option<&str>,
    build_manifest: bool,
    limit: Option<usize>,
    seed: Option<u64>,
) -> Result<String, CliError> {
    std::fs::create_dir_all(output)
        .map_err(|e| CliError::Other(format!("failed to create {output}: {e}")))?;

    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut log = Vec::new();
    log.push(format!("pipeline: {} source files", files.len()));

    // Step 1: Load and optionally validate
    let mut trajectories: Vec<(std::path::PathBuf, genesis_core::trajectory::Trajectory)> =
        Vec::new();
    let mut invalid = 0usize;

    for path in &files {
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(_) => {
                invalid += 1;
                continue;
            }
        };
        match serde_json::from_str::<genesis_core::trajectory::Trajectory>(&raw) {
            Ok(mut traj) => {
                if validate
                    && (traj.session_id.is_empty()
                        || traj.model.is_empty()
                        || traj.steps.is_empty())
                {
                    invalid += 1;
                    continue;
                }

                // Step 2: Auto-tag
                if auto_tag {
                    let new_tags = genesis_core::tagger::auto_tag(&traj);
                    let existing: HashSet<String> = traj.tags.iter().cloned().collect();
                    for t in new_tags {
                        if !existing.contains(&t) {
                            traj.tags.push(t);
                        }
                    }
                    traj.tags.sort();
                }

                trajectories.push((path.clone(), traj));
            }
            Err(_) => {
                invalid += 1;
            }
        }
    }

    if validate {
        log.push(format!("validate: {} valid, {invalid} invalid", trajectories.len()));
    }
    if auto_tag {
        log.push(format!("auto-tag: applied to {} trajectories", trajectories.len()));
    }

    // Step 3: Filter
    let before_filter = trajectories.len();
    trajectories.retain(|(_, traj)| {
        if success_only
            && !matches!(
                traj.outcome,
                Some(genesis_core::trajectory::TrajectoryOutcome::Success)
            )
        {
            return false;
        }
        if let Some(t) = tag {
            if !traj.tags.iter().any(|tag| tag == t) {
                return false;
            }
        }
        if let Some(m) = model {
            if !traj.model.contains(m) {
                return false;
            }
        }
        if let Some(min_q) = min_quality {
            let score = genesis_core::quality::score(traj).overall;
            if score < min_q {
                return false;
            }
        }
        true
    });

    if trajectories.len() != before_filter {
        log.push(format!(
            "filter: {} → {} trajectories",
            before_filter,
            trajectories.len()
        ));
    }

    // Step 4: Sample/limit
    if let Some(max) = limit {
        if trajectories.len() > max {
            use rand::seq::SliceRandom;
            let mut rng = match seed {
                Some(s) => {
                    use rand::SeedableRng;
                    rand::rngs::StdRng::seed_from_u64(s)
                }
                None => {
                    use rand::SeedableRng;
                    rand::rngs::StdRng::from_os_rng()
                }
            };
            trajectories.shuffle(&mut rng);
            trajectories.truncate(max);
            log.push(format!("sample: limited to {max} trajectories"));
        }
    }

    // Step 5: Write output
    let output_format = format.unwrap_or("json");
    match output_format {
        "json" => {
            for (_, traj) in &trajectories {
                let filename = format!(
                    "{}.json",
                    sanitize_session_id_for_filename(&traj.session_id)
                );
                let dest = std::path::Path::new(output).join(&filename);
                let json = serde_json::to_string_pretty(traj)?;
                std::fs::write(&dest, json).map_err(|e| {
                    CliError::Other(format!("failed to write {}: {e}", dest.display()))
                })?;
            }
            log.push(format!("output: {} JSON files in {output}", trajectories.len()));
        }
        "chatml" | "sharegpt" => {
            let output_file = std::path::Path::new(output).join(format!("dataset.{output_format}.jsonl"));
            let mut lines = Vec::new();
            for (original_path, _) in &trajectories {
                let compressed = load_training_compressed_trajectory(original_path)?;
                let data = if output_format == "chatml" {
                    serde_json::json!({
                        "session_id": compressed.session_id,
                        "model": compressed.model,
                        "tags": compressed.tags,
                        "outcome": compressed.outcome,
                        "chatml": genesis_core::compress::to_chatml(&compressed),
                    })
                } else {
                    serde_json::json!({
                        "session_id": compressed.session_id,
                        "model": compressed.model,
                        "tags": compressed.tags,
                        "outcome": compressed.outcome,
                        "sharegpt": genesis_core::compress::to_sharegpt(&compressed),
                    })
                };
                lines.push(serde_json::to_string(&data)?);
            }
            std::fs::write(&output_file, lines.join("\n"))
                .map_err(|e| CliError::Other(format!("failed to write {}: {e}", output_file.display())))?;
            log.push(format!(
                "output: {} records as {output_format} JSONL",
                trajectories.len()
            ));
        }
        other => {
            return Err(CliError::Other(format!(
                "unknown format '{other}', expected json, chatml, or sharegpt"
            )));
        }
    }

    // Step 6: Build manifest
    if build_manifest && output_format == "json" {
        let manifest = genesis_core::dataset::build_manifest(
            "pipeline-output",
            &log.join("; "),
            std::path::Path::new(output),
            false,
        )
        .map_err(|e| CliError::Other(format!("failed to build manifest: {e}")))?;
        genesis_core::dataset::save_manifest(&manifest, std::path::Path::new(output))
            .map_err(|e| CliError::Other(format!("failed to save manifest: {e}")))?;
        log.push("manifest: saved dataset.json".to_owned());
    }

    Ok(log.join("\n"))
}

fn run_eval_validate(
    dir: &str,
    recursive: bool,
    remove: bool,
) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut valid = 0usize;
    let mut invalid = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for path in &files {
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("{}: read error: {e}", path.display()));
                invalid += 1;
                continue;
            }
        };

        match serde_json::from_str::<genesis_core::trajectory::Trajectory>(&raw) {
            Ok(traj) => {
                // Validate required fields
                let mut issues = Vec::new();
                if traj.session_id.is_empty() {
                    issues.push("empty session_id");
                }
                if traj.model.is_empty() {
                    issues.push("empty model");
                }
                if traj.started_at.is_empty() {
                    issues.push("empty started_at");
                }
                if traj.steps.is_empty() {
                    issues.push("no steps");
                }

                if issues.is_empty() {
                    valid += 1;
                } else {
                    errors.push(format!(
                        "{}: {}",
                        path.display(),
                        issues.join(", ")
                    ));
                    invalid += 1;
                    if remove {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            Err(e) => {
                errors.push(format!("{}: invalid JSON: {e}", path.display()));
                invalid += 1;
                if remove {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    let mut lines = vec![format!(
        "validated {} files: {valid} valid, {invalid} invalid",
        files.len()
    )];
    for err in errors.iter().take(20) {
        lines.push(format!("  {err}"));
    }
    if errors.len() > 20 {
        lines.push(format!("  ... and {} more", errors.len() - 20));
    }
    if remove && invalid > 0 {
        lines.push(format!("removed {invalid} invalid files"));
    }
    Ok(lines.join("\n"))
}

fn run_eval_deduplicate(
    dir: &str,
    recursive: bool,
    remove: bool,
    json: bool,
) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut grouped = BTreeMap::<String, Vec<PathBuf>>::new();

    for path in files {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
        let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
            .map_err(|e| {
                CliError::Other(format!(
                    "invalid trajectory JSON in {}: {e}",
                    path.display()
                ))
            })?;
        grouped
            .entry(deduplicate_key(&trajectory))
            .or_default()
            .push(path);
    }

    let mut groups = grouped
        .into_iter()
        .filter_map(|(key, mut files)| {
            if files.len() < 2 {
                return None;
            }
            files.sort();
            Some((key, files))
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| left.0.cmp(&right.0));

    let mut removed_files = 0usize;
    if remove {
        for (_, files) in &groups {
            for file in files.iter().skip(1) {
                std::fs::remove_file(file).map_err(|e| {
                    CliError::Other(format!("failed to remove {}: {e}", file.display()))
                })?;
                removed_files += 1;
            }
        }
    }

    let groups = groups
        .into_iter()
        .map(|(key, files)| DeduplicateGroup {
            key,
            files: files
                .into_iter()
                .map(|path| path.display().to_string())
                .collect(),
        })
        .collect::<Vec<_>>();

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "directory": dir,
            "recursive": recursive,
            "remove": remove,
            "duplicate_groups": groups.len(),
            "removed_files": removed_files,
            "groups": groups.iter().map(|group| {
                serde_json::json!({
                    "key": group.key,
                    "files": group.files,
                })
            }).collect::<Vec<_>>(),
        }))?);
    }

    if groups.is_empty() {
        return Ok("No duplicate trajectories found.".to_owned());
    }

    let mut lines = Vec::new();
    lines.push("genesis eval deduplicate".to_owned());
    lines.push(format!("directory:        {dir}"));
    lines.push(format!("recursive:        {recursive}"));
    lines.push(format!("remove:           {remove}"));
    lines.push(format!("duplicate groups: {}", groups.len()));
    lines.push(format!("removed files:    {removed_files}"));
    for group in &groups {
        lines.push(format!("group: {}", group.key));
        for file in &group.files {
            lines.push(format!("  - {file}"));
        }
    }

    Ok(lines.join("\n"))
}

fn deduplicate_key(trajectory: &genesis_core::trajectory::Trajectory) -> String {
    let first_user_message = trajectory
        .steps
        .iter()
        .find(|step| step.action_type == genesis_core::trajectory::ActionType::UserMessage)
        .map(|step| step.content.trim())
        .unwrap_or("");
    format!("{}::{}", trajectory.system_prompt_hash, first_user_message)
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

async fn run_login(
    config_path: Option<PathBuf>,
) -> Result<String, CliError> {
    use genesis_config::{update_provider_in_file, AppPaths};

    let auth_path = genesis_auth::default_auth_path()?;
    let paths = AppPaths::resolve(config_path.as_deref())?;

    // Check for existing valid credentials
    if let Ok(existing_store) = genesis_auth::store::read_store(&auth_path) {
        if genesis_auth::store::get_codex_state(&existing_store).is_some() {
            eprintln!("  Existing Codex credentials found in auth store.");
            eprint!("  Use existing credentials? [Y/n]: ");

            let mut input = String::new();
            std::io::stdin().read_line(&mut input).map_err(CliError::Io)?;
            let input = input.trim().to_lowercase();

            if input.is_empty() || input == "y" || input == "yes" {
                update_provider_in_file(
                    &paths.config_path,
                    Some("openai-codex"),
                    None,
                    Some(Some(genesis_auth::provider::CODEX_INFERENCE_URL)),
                    None,
                )?;
                return Ok(format!(
                    "\n  Login successful!\n  Config updated: {} (backend=openai-codex)",
                    paths.config_path.display()
                ));
            }
        }
    }

    // Check for Codex CLI migration
    if let Some(cli_tokens) = genesis_auth::codex::import_codex_cli_tokens() {
        eprintln!("  Found existing Codex CLI credentials (~/.codex/auth.json)");
        eprintln!("  Genesis will create its own independent session.");
        eprint!("  Import these credentials? [y/N]: ");

        let mut input = String::new();
        std::io::stdin().read_line(&mut input).map_err(CliError::Io)?;
        let input = input.trim().to_lowercase();

        if input == "y" || input == "yes" {
            genesis_auth::store::save_codex_tokens(&auth_path, cli_tokens, genesis_auth::store::CredentialSource::CodexMigration)?;
            update_provider_in_file(
                &paths.config_path,
                Some("openai-codex"),
                None,
                Some(Some(genesis_auth::provider::CODEX_INFERENCE_URL)),
                None,
            )?;
            return Ok(format!(
                "\n  Credentials imported!\n  Config updated: {} (backend=openai-codex)\n\n  Note: Genesis maintains its own session — won't affect Codex CLI.",
                paths.config_path.display()
            ));
        }
    }

    // Run device code flow
    eprintln!();
    eprintln!("  Signing in to OpenAI Codex...");
    eprintln!("  (Genesis creates its own session — won't affect Codex CLI or VS Code)");

    let creds = genesis_auth::codex::login(&auth_path).await?;

    update_provider_in_file(
        &paths.config_path,
        Some("openai-codex"),
        None,
        Some(Some(&creds.base_url)),
        None,
    )?;

    Ok(format!(
        "\n  Login successful!\n  Auth state: {}\n  Config updated: {} (backend=openai-codex)",
        auth_path.display(),
        paths.config_path.display()
    ))
}

fn run_logout() -> Result<String, CliError> {
    let auth_path = genesis_auth::default_auth_path()?;
    let removed = genesis_auth::store::clear_active_provider(&auth_path)?;

    match removed {
        Some(provider_id) => Ok(format!(
            "  Logged out from '{provider_id}'.\n  Credentials cleared from {}",
            auth_path.display()
        )),
        None => Ok("  No active authentication session to clear.".to_owned()),
    }
}

async fn run_init(
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
        return run_init_wizard(config_path).await;
    }

    run_init_non_interactive(config_path, backend, model, base_url, api_key_env)
}

/// Interactive setup wizard — prompts the user to choose a provider, model,
/// and verify their API key. Invoked when `genesis init` is run with no flags.
async fn run_init_wizard(config_path: Option<PathBuf>) -> Result<String, CliError> {
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
        ("openai-codex", "Sign in with ChatGPT (OAuth)", ""),
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

    // If user chose OAuth, delegate to login flow
    if backend == "openai-codex" {
        eprintln!("  Starting OAuth login flow...");
        eprintln!();

        // Ensure config and storage exist first
        let paths = AppPaths::resolve(config_path.as_deref())?;
        if !paths.config_path.exists() {
            if let Some(parent) = paths.config_path.parent() {
                std::fs::create_dir_all(parent).map_err(CliError::Io)?;
            }
            let yaml = render_example_yaml(config_path.as_deref())?;
            std::fs::write(&paths.config_path, &yaml).map_err(CliError::Io)?;
        }
        std::fs::create_dir_all(&paths.data_dir).map_err(CliError::Io)?;
        let _ = bootstrap(&paths.database_path)?;

        return run_login(config_path).await;
    }

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
            std::fs::create_dir_all(parent).map_err(CliError::Io)?;
        }

        // Write default config
        let yaml = render_example_yaml(config_path.as_deref())?;
        std::fs::write(&paths.config_path, &yaml).map_err(CliError::Io)?;
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
    std::fs::create_dir_all(&paths.data_dir).map_err(CliError::Io)?;
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

fn run_uninstall(
    config_override: Option<&Path>,
    remove_data: bool,
    remove_config: bool,
    force: bool,
) -> Result<String, CliError> {
    use std::io::IsTerminal;

    let exe_path = std::env::current_exe().map_err(CliError::Io)?;

    // Try loading the full config (respects storage.data_dir and GENESIS_DATA_DIR).
    // Fall back to platform-default paths if the config file is already gone or unreadable.
    let (config_dir, data_dir) = match load(config_override) {
        Ok(loaded) => {
            let cd = loaded.paths.config_path.parent().map(|p| p.to_path_buf());
            (cd, loaded.paths.data_dir)
        }
        Err(_) => {
            let paths = genesis_config::AppPaths::resolve(config_override)?;
            let cd = paths.config_path.parent().map(|p| p.to_path_buf());
            (cd, paths.data_dir)
        }
    };

    // Build the plan of what will be removed
    let mut plan: Vec<String> = Vec::new();

    if exe_path.exists() {
        plan.push(format!("  Binary:  {}", exe_path.display()));
    }
    if remove_data && data_dir.exists() {
        plan.push(format!("  Data:    {}", data_dir.display()));
    }
    if remove_config {
        if let Some(ref cd) = config_dir {
            if cd.exists() {
                plan.push(format!("  Config:  {}", cd.display()));
            }
        }
    }

    if plan.is_empty() {
        return Ok("Nothing to remove — Genesis does not appear to be installed.".to_owned());
    }

    let mut output = Vec::new();
    output.push("The following will be removed:".to_owned());
    output.extend(plan.iter().cloned());

    // Prompt for confirmation unless --force is set
    if !force {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "uninstall requires --force when stdin is not a terminal".into(),
            ));
        }

        eprintln!();
        for line in &output {
            eprintln!("{line}");
        }
        eprintln!();
        eprint!("Proceed with uninstall? [y/N] ");
        let _ = io::stderr().flush();

        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(CliError::Io)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            return Ok("Uninstall cancelled.".to_owned());
        }
    }

    // Perform removals
    let mut results = Vec::new();

    // Remove data directory first (if requested), since it's the most expendable
    if remove_data && data_dir.exists() {
        fs::remove_dir_all(&data_dir).map_err(|e| {
            CliError::Other(format!(
                "failed to remove data directory {}: {e}",
                data_dir.display()
            ))
        })?;
        results.push(format!("[ok] Removed data directory: {}", data_dir.display()));
    }

    // Remove config directory (if requested)
    if remove_config {
        if let Some(ref cd) = config_dir {
            if cd.exists() {
                fs::remove_dir_all(cd).map_err(|e| {
                    CliError::Other(format!(
                        "failed to remove config directory {}: {e}",
                        cd.display()
                    ))
                })?;
                results.push(format!("[ok] Removed config directory: {}", cd.display()));
            }
        }
    }

    // Remove the binary last so the process can finish writing output
    if exe_path.exists() {
        fs::remove_file(&exe_path).map_err(|e| {
            CliError::Other(format!(
                "failed to remove binary {}: {e}",
                exe_path.display()
            ))
        })?;
        results.push(format!("[ok] Removed binary: {}", exe_path.display()));
    }

    if results.is_empty() {
        Ok("Nothing was removed.".to_owned())
    } else {
        results.push(String::new());
        results.push("Genesis has been uninstalled.".to_owned());
        Ok(results.join("\n"))
    }
}

/// Bridge between the MCP server and the Genesis tool registry.
struct RegistryMcpBackend {
    registry: genesis_tools::ToolRegistry,
    context: genesis_tools::ToolContext,
}

impl genesis_mcp::McpToolBackend for RegistryMcpBackend {
    fn list_tools(&self) -> Vec<genesis_mcp::McpServerToolDef> {
        self.registry
            .definitions()
            .into_iter()
            .map(|def| genesis_mcp::McpServerToolDef {
                name: def.name,
                description: Some(def.description),
                input_schema: def.parameters.unwrap_or_else(|| {
                    serde_json::json!({"type": "object", "properties": {}})
                }),
            })
            .collect()
    }

    fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, String> {
        // Convert JSON arguments to BTreeMap<String, String>
        let mut args = std::collections::BTreeMap::new();
        if let Some(obj) = arguments.as_object() {
            for (k, v) in obj {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => continue,
                    other => other.to_string(),
                };
                args.insert(k.clone(), s);
            }
        }

        let call = genesis_tools::ToolCall {
            name: name.to_owned(),
            arguments: args,
        };

        match self.registry.execute(&call, &self.context) {
            Ok(output) => Ok(output.content),
            Err(e) => Err(e.to_string()),
        }
    }
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
        McpCommand::Serve => {
            // Run Genesis as an MCP server on stdio
            let registry = genesis_tools::default_registry();
            let context = genesis_tools::ToolContext {
                session_id: format!("mcp-server-{}", std::process::id()),
                profile: loaded.config.profile.clone(),
                data_dir: loaded.config.storage.data_dir.to_string_lossy().to_string(),
                allow_destructive_tools: false,
                terminal_backend: None,
                default_working_dir: None,
            };

            let backend = std::sync::Arc::new(RegistryMcpBackend { registry, context });
            let config = genesis_mcp::McpServeConfig::default();

            genesis_mcp::run_stdio_server(config, backend)
                .await
                .map_err(|e| CliError::Other(format!("MCP server error: {e}")))?;

            Ok("MCP server exited".to_owned())
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

async fn run_pairing(
    config_path: Option<PathBuf>,
    command: PairingCommand,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let store = PairingStore::new(&loaded.config.storage.database_path);

    match command {
        PairingCommand::List { platform } => {
            let users = store
                .list_approved(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "approved": users,
                    "count": users.len(),
                }))
                .unwrap());
            }

            if users.is_empty() {
                return Ok("No approved users.".to_owned());
            }

            let mut lines = vec![format!("{:<12} {:<20} {:<20} {}", "PLATFORM", "USER_ID", "NAME", "APPROVED_AT")];
            for u in &users {
                lines.push(format!("{:<12} {:<20} {:<20} {}", u.platform, u.user_id, u.user_name, u.approved_at));
            }
            lines.push(format!("\n{} approved user(s)", users.len()));
            Ok(lines.join("\n"))
        }
        PairingCommand::Pending { platform } => {
            let pending = store
                .list_pending(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "pending": pending,
                    "count": pending.len(),
                }))
                .unwrap());
            }

            if pending.is_empty() {
                return Ok("No pending pairing requests.".to_owned());
            }

            let mut lines = vec![format!("{:<12} {:<10} {:<20} {:<20} {}", "PLATFORM", "CODE", "USER_ID", "NAME", "CREATED_AT")];
            for p in &pending {
                lines.push(format!("{:<12} {:<10} {:<20} {:<20} {}", p.platform, p.code, p.user_id, p.user_name, p.created_at));
            }
            lines.push(format!("\n{} pending request(s)", pending.len()));
            Ok(lines.join("\n"))
        }
        PairingCommand::Approve { platform, code } => {
            let result = store
                .approve_code(&platform, &code)
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            match result {
                Some(user) => {
                    if json {
                        Ok(serde_json::to_string_pretty(&serde_json::json!({
                            "approved": true,
                            "user": user,
                        }))
                        .unwrap())
                    } else {
                        Ok(format!(
                            "Approved {} ({}) on {}",
                            user.user_name, user.user_id, user.platform
                        ))
                    }
                }
                None => Err(CliError::Other(
                    "Invalid or expired pairing code.".to_owned(),
                )),
            }
        }
        PairingCommand::Revoke { platform, user_id } => {
            let revoked = store
                .revoke(&platform, &user_id)
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "revoked": revoked,
                    "platform": platform,
                    "user_id": user_id,
                }))
                .unwrap());
            }

            if revoked {
                Ok(format!("Revoked access for {} on {}", user_id, platform))
            } else {
                Err(CliError::Other(format!(
                    "No approved user '{}' on platform '{}'",
                    user_id, platform
                )))
            }
        }
        PairingCommand::ClearPending { platform } => {
            let cleared = store
                .clear_pending(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "cleared": cleared,
                    "platform": platform,
                }))
                .unwrap());
            }

            match platform {
                Some(p) => Ok(format!("Cleared {} pending code(s) for {}", cleared, p)),
                None => Ok(format!("Cleared {} pending code(s) across all platforms", cleared)),
            }
        }
    }
}

fn run_toolset(command: ToolsetCommand, json: bool) -> Result<String, CliError> {
    use genesis_core::toolset;
    use rand::SeedableRng;

    match command {
        ToolsetCommand::List => {
            let names = toolset::builtin_distribution_names();
            if json {
                let distributions: Vec<serde_json::Value> = names
                    .iter()
                    .filter_map(|name| {
                        toolset::builtin_distribution(name).map(|d| {
                            serde_json::json!({
                                "name": d.name,
                                "description": d.description,
                                "tool_count": d.possible_tools().len(),
                            })
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&distributions).unwrap())
            } else {
                let mut lines = vec![format!("{:<18} {:<5} {}", "NAME", "TOOLS", "DESCRIPTION")];
                for name in &names {
                    if let Some(d) = toolset::builtin_distribution(name) {
                        lines.push(format!(
                            "{:<18} {:<5} {}",
                            d.name,
                            d.possible_tools().len(),
                            d.description
                        ));
                    }
                }
                Ok(lines.join("\n"))
            }
        }
        ToolsetCommand::Show { name } => {
            let dist = toolset::builtin_distribution(&name).ok_or_else(|| {
                CliError::Other(format!(
                    "unknown distribution '{name}'. Available: {}",
                    toolset::builtin_distribution_names().join(", ")
                ))
            })?;

            if json {
                Ok(serde_json::to_string_pretty(&dist).unwrap())
            } else {
                let mut lines = vec![
                    format!("Distribution: {}", dist.name),
                    format!("Description:  {}", dist.description),
                    format!("Tools ({}):", dist.tools.len()),
                ];
                for (tool, prob) in &dist.tools {
                    lines.push(format!("  {:<30} {:.0}%", tool, prob * 100.0));
                }
                Ok(lines.join("\n"))
            }
        }
        ToolsetCommand::Sample { name, seed } => {
            let dist = toolset::builtin_distribution(&name).ok_or_else(|| {
                CliError::Other(format!(
                    "unknown distribution '{name}'. Available: {}",
                    toolset::builtin_distribution_names().join(", ")
                ))
            })?;

            let mut rng = match seed {
                Some(s) => rand::rngs::StdRng::seed_from_u64(s),
                None => rand::rngs::StdRng::from_os_rng(),
            };
            let selected = dist.sample(&mut rng);
            let mut tools: Vec<&str> = selected.iter().map(|s| s.as_str()).collect();
            tools.sort();

            if json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "distribution": dist.name,
                    "seed": seed,
                    "selected_count": tools.len(),
                    "possible_count": dist.possible_tools().len(),
                    "selected": tools,
                }))
                .unwrap())
            } else {
                let mut lines = vec![format!(
                    "Sampled {} tools from '{}' ({} possible):",
                    tools.len(),
                    dist.name,
                    dist.possible_tools().len()
                )];
                for tool in &tools {
                    lines.push(format!("  {tool}"));
                }
                Ok(lines.join("\n"))
            }
        }
    }
}

fn run_personality(command: PersonalityCommand, json: bool) -> Result<String, CliError> {
    use genesis_core::personality;

    match command {
        PersonalityCommand::List => {
            let personalities = personality::list_personalities();
            if json {
                let items: Vec<serde_json::Value> = personalities
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "description": p.description,
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&items).unwrap())
            } else {
                let mut lines = vec![format!("{:<16} {}", "NAME", "DESCRIPTION")];
                for p in &personalities {
                    lines.push(format!("{:<16} {}", p.name, p.description));
                }
                Ok(lines.join("\n"))
            }
        }
        PersonalityCommand::Show { name } => {
            let p = personality::get_personality(&name).ok_or_else(|| {
                let available: Vec<String> = personality::list_personalities()
                    .iter()
                    .map(|p| p.name.to_owned())
                    .collect();
                CliError::Other(format!(
                    "unknown personality '{name}'. Available: {}",
                    available.join(", ")
                ))
            })?;

            if json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "name": p.name,
                    "description": p.description,
                    "system_prompt_prefix": p.system_prompt_prefix,
                }))
                .unwrap())
            } else {
                Ok(format!(
                    "Personality: {}\nDescription: {}\n\nSystem prompt prefix:\n{}",
                    p.name, p.description, p.system_prompt_prefix
                ))
            }
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
        )
        .await?,
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
                )
                .await?,
            ));
        }
    }

    let test_prompt = "Say exactly: ping";
    let runs = runs.clamp(1, 20);
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
        ContextCommand::Show => Ok(match load_context_file(&current_dir, &genesis_config::ContextSecurityPolicy::Warn) {
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

/// Verify API connectivity by sending a minimal completion request.
/// Returns round-trip latency in milliseconds on success.
async fn verify_api_connectivity(loaded: &LoadedConfig) -> Result<u128, String> {
    use genesis_provider::{ChatCompletionRequest, ChatMessage as ProviderMessage};

    let client = genesis_provider::client_from_config(
        &loaded.config.provider.backend,
        &loaded.config.provider.model,
        loaded.config.provider.base_url.as_deref(),
        loaded.config.provider.api_key_env.as_deref(),
    )
    .await
    .map_err(|e| format!("failed to create client: {e}"))?;

    let mut request = ChatCompletionRequest::new(
        &loaded.config.provider.model,
        vec![ProviderMessage::user("Say: ok")],
    );
    request.max_tokens = Some(5);

    let start = std::time::Instant::now();
    client
        .complete(request)
        .await
        .map_err(|e| format!("{e}"))?;

    Ok(start.elapsed().as_millis())
}

/// Well-known models grouped by provider.
/// Returns (provider, model_id, short_description).
/// Rough cost estimate based on typical per-million-token pricing.
/// Returns (estimated_cost_usd, pricing_note).
fn estimate_token_cost(input_tokens: u32, output_tokens: u32) -> Option<(f64, &'static str)> {
    if input_tokens == 0 && output_tokens == 0 {
        return None;
    }
    // Use GPT-4.1-mini pricing as a reasonable middle-ground estimate:
    // $0.40 / 1M input, $1.60 / 1M output
    let input_cost = (input_tokens as f64 / 1_000_000.0) * 0.40;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * 1.60;
    Some((input_cost + output_cost, "GPT-4.1-mini pricing"))
}

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
        // OpenAI Codex (ChatGPT subscription)
        ("openai-codex", "o3-pro", "Most capable reasoning"),
        ("openai-codex", "o3", "Advanced reasoning"),
        ("openai-codex", "gpt-4.1", "Flagship GPT model"),
        ("openai-codex", "o4-mini", "Fast reasoning"),
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
            "/help         - Show this help\n\
             /history      - Show recent conversation history\n\
             /export       - Export session as Markdown\n\
             /tokens       - Show session token usage\n\
             /session      - Show current session ID\n\
             /new          - Start a new session\n\
             /undo         - Undo last turn (remove last user-assistant exchange)\n\
             /retry        - Undo last turn and re-send the user message\n\
             /fork         - Branch conversation into a new session\n\
             /resume [id]  - Switch to another session (lists recent if no ID)\n\
             /search <q>   - Search past sessions for a query\n\
             /memories     - Show stored memories\n\
             /compress     - Trim old messages, keeping recent context\n\
             /tools        - List available tools\n\
             /skills       - List saved skills\n\
             /model [spec] - Show or switch model (e.g. /model anthropic/claude-sonnet-4-20250514)\n\
             /system [txt] - View or set system prompt override (reset to clear)\n\
             /personality  - List or set personality (e.g. /personality pirate)\n\
             /title [text] - View or set session title\n\
             /tree         - Show conversation branch tree\n\
             /tag [name]   - View, add, or remove (-name) session tags\n\
             /stats        - Show session statistics\n\
             /cache        - Show cache stats (clear, prune)\n\
             /audit        - View audit log (stats, purge <days>)\n\
             /analytics    - Tool and LLM usage analytics\n\
             /template     - Apply an agent template (archetype)\n\
             /workflow     - Run a YAML-defined multi-step workflow\n\
             /bus          - Agent message bus (stats, history)\n\
             /eval         - Run evaluation suites against the agent\n\
             /clear        - Clear the screen\n\
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
            // Rough cost estimate based on common pricing
            let cost_estimate = estimate_token_cost(
                session.total_input_tokens as u32,
                session.total_output_tokens as u32,
            );
            let mut output = format!(
                "Session: {}\nInput tokens:  {}\nOutput tokens: {}\nTotal tokens:  {}",
                session_id, session.total_input_tokens, session.total_output_tokens, total
            );
            if let Some((cost, note)) = cost_estimate {
                output.push_str(&format!("\nEst. cost:     ${:.4} ({note})", cost));
            }
            Some(output)
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
        "tree" => {
            // Show the conversation tree rooted at the current session
            let _session = store.get_session(session_id).ok()??;
            // Walk up to the root
            let mut root_id = session_id.to_owned();
            let mut visited = std::collections::HashSet::new();
            while let Ok(Some(s)) = store.get_session(&root_id) {
                if let Some(parent) = s.parent_session_id {
                    if visited.contains(&parent) {
                        break;
                    }
                    visited.insert(root_id.clone());
                    root_id = parent;
                } else {
                    break;
                }
            }
            // Print tree from root
            fn print_tree(
                store: &SessionStore,
                id: &str,
                current: &str,
                prefix: &str,
                is_last: bool,
                lines: &mut Vec<String>,
            ) {
                let connector = if prefix.is_empty() { "" } else if is_last { "└── " } else { "├── " };
                let title = store
                    .get_session(id)
                    .ok()
                    .flatten()
                    .and_then(|s| s.title)
                    .unwrap_or_else(|| "(untitled)".to_owned());
                let marker = if id == current { " ← you" } else { "" };
                lines.push(format!("{prefix}{connector}{id} — {title}{marker}"));
                if let Ok(children) = store.list_children(id) {
                    let child_prefix = if prefix.is_empty() {
                        "".to_owned()
                    } else if is_last {
                        format!("{prefix}    ")
                    } else {
                        format!("{prefix}│   ")
                    };
                    for (i, child) in children.iter().enumerate() {
                        let last = i == children.len() - 1;
                        print_tree(store, &child.id, current, &child_prefix, last, lines);
                    }
                }
            }
            let mut lines = Vec::new();
            print_tree(store, &root_id, session_id, "", false, &mut lines);
            if lines.len() <= 1 {
                Some("No conversation branches. Use /fork to create one.".to_owned())
            } else {
                Some(lines.join("\n"))
            }
        }
        "title" => {
            let arg = _args.trim();
            if arg.is_empty() {
                match store.get_session(session_id) {
                    Ok(Some(s)) => Some(format!(
                        "Title: {}",
                        s.title.as_deref().unwrap_or("(untitled)")
                    )),
                    _ => Some("Could not load session.".to_owned()),
                }
            } else {
                match store.update_title(session_id, arg) {
                    Ok(_) => Some(format!("Title set to: {arg}")),
                    Err(e) => Some(format!("Failed to set title: {e}")),
                }
            }
        }
        "tag" => {
            let arg = _args.trim();
            if arg.is_empty() {
                match store.get_tags(session_id) {
                    Ok(tags) if tags.is_empty() => Some("No tags on this session. Add with: /tag <name>".to_owned()),
                    Ok(tags) => Some(format!("Tags: {}\nRemove with: /tag -<name>", tags.join(", "))),
                    Err(e) => Some(format!("Failed to read tags: {e}")),
                }
            } else if let Some(to_remove) = arg.strip_prefix('-') {
                match store.remove_tag(session_id, to_remove.trim()) {
                    Ok(true) => Some(format!("Removed tag '{}'.", to_remove.trim())),
                    Ok(false) => Some(format!("Tag '{}' not found.", to_remove.trim())),
                    Err(e) => Some(format!("Failed to remove tag: {e}")),
                }
            } else {
                match store.add_tag(session_id, arg) {
                    Ok(_) => Some(format!("Added tag '{arg}'.")),
                    Err(e) => Some(format!("Failed to add tag: {e}")),
                }
            }
        }
        "stats" => {
            let messages = store.load_messages(session_id).ok()?;
            let session = store.get_session(session_id).ok()??;
            let user_msgs = messages.iter().filter(|m| m.role == "user").count();
            let assistant_msgs = messages.iter().filter(|m| m.role == "assistant").count();
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            let system_msgs = messages.iter().filter(|m| m.role == "system").count();
            let total_chars: usize = messages
                .iter()
                .filter_map(|m| m.content.as_ref())
                .map(|c| c.len())
                .sum();
            let total_tokens = session.total_input_tokens + session.total_output_tokens;
            let title = session.title.as_deref().unwrap_or("(untitled)");
            let mut lines = vec![
                format!("Session: {} — {}", session_id, title),
                format!("Platform: {}", session.platform),
                format!("Messages: {} total ({} user, {} assistant, {} tool, {} system)",
                    messages.len(), user_msgs, assistant_msgs, tool_msgs, system_msgs),
                format!("Characters: {}", total_chars),
                format!("Tokens: {} ({} in, {} out)",
                    total_tokens, session.total_input_tokens, session.total_output_tokens),
            ];
            if let Some(ref parent) = session.parent_session_id {
                lines.push(format!("Forked from: {parent}"));
            }
            lines.push(format!("Created: {}", session.created_at));
            lines.push(format!("Updated: {}", session.updated_at));
            Some(lines.join("\n"))
        }
        "cache" => {
            let cache_store = genesis_storage::ResponseCacheStore::new(store.database_path());
            let sub = _args.trim();
            if sub == "clear" {
                match cache_store.clear() {
                    Ok(n) => Some(format!("Cache cleared ({n} entries removed).")),
                    Err(e) => Some(format!("Failed to clear cache: {e}")),
                }
            } else if sub == "prune" {
                match cache_store.prune_expired() {
                    Ok(n) => Some(format!("Pruned {n} expired cache entries.")),
                    Err(e) => Some(format!("Failed to prune cache: {e}")),
                }
            } else {
                match cache_store.stats() {
                    Ok((entries, hits)) => Some(format!(
                        "Response cache: {entries} entries, {hits} total hits\n\
                         Commands: /cache clear, /cache prune"
                    )),
                    Err(e) => Some(format!("Cache stats unavailable: {e}")),
                }
            }
        }
        "analytics" => {
            let audit_store = genesis_storage::AuditLogStore::new(store.database_path());
            let sub = _args.trim();
            let days: u32 = if sub == "llm" || sub == "tools" { 30 } else { sub.parse().unwrap_or(30) };
            let is_llm = sub == "llm";

            if is_llm {
                match audit_store.llm_analytics(days) {
                    Ok(analytics) => {
                        if analytics.is_empty() {
                            Some("No LLM analytics data yet. Analytics are populated from audit logs.".into())
                        } else {
                            let mut lines = vec![format!("LLM usage (last {days} days):")];
                            for a in &analytics {
                                let total = a.total_input_tokens + a.total_output_tokens;
                                lines.push(format!(
                                    "  {} - {} calls, {} tokens ({} in / {} out)",
                                    a.model, a.call_count, total, a.total_input_tokens, a.total_output_tokens
                                ));
                            }
                            Some(lines.join("\n"))
                        }
                    }
                    Err(e) => Some(format!("LLM analytics unavailable: {e}")),
                }
            } else {
                match audit_store.tool_analytics(days) {
                    Ok(analytics) => {
                        if analytics.is_empty() {
                            Some("No tool analytics data yet. Analytics are populated from audit logs.".into())
                        } else {
                            let mut lines = vec![format!("Tool usage (last {days} days):")];
                            for a in &analytics {
                                let success_rate = if a.call_count > 0 {
                                    (a.success_count as f64 / a.call_count as f64) * 100.0
                                } else { 0.0 };
                                lines.push(format!(
                                    "  {:20} {:4} calls  {:.0}% success  {:.0}ms avg",
                                    a.tool_name, a.call_count, success_rate, a.avg_duration_ms
                                ));
                            }
                            lines.push("\nCommands: /analytics, /analytics llm".into());
                            Some(lines.join("\n"))
                        }
                    }
                    Err(e) => Some(format!("Tool analytics unavailable: {e}")),
                }
            }
        }
        "audit" => {
            let audit_store = genesis_storage::AuditLogStore::new(store.database_path());
            let sub = _args.trim();
            if sub == "stats" {
                match audit_store.stats() {
                    Ok(stats) => {
                        let total: i64 = stats.iter().map(|(_, c)| c).sum();
                        let mut lines = vec![format!("Audit log: {total} total entries")];
                        for (event_type, count) in &stats {
                            lines.push(format!("  {event_type}: {count}"));
                        }
                        lines.push("Commands: /audit, /audit stats, /audit purge <days>".into());
                        Some(lines.join("\n"))
                    }
                    Err(e) => Some(format!("Audit stats unavailable: {e}")),
                }
            } else if sub.starts_with("purge") {
                let days: u32 = sub.strip_prefix("purge").unwrap_or("90").trim().parse().unwrap_or(90);
                match audit_store.purge_older_than(days) {
                    Ok(n) => Some(format!("Purged {n} audit entries older than {days} days.")),
                    Err(e) => Some(format!("Audit purge failed: {e}")),
                }
            } else {
                // Show recent entries (default: 20)
                let limit: usize = sub.parse().unwrap_or(20);
                match audit_store.recent(limit) {
                    Ok(entries) => {
                        if entries.is_empty() {
                            Some("No audit log entries.".into())
                        } else {
                            let mut lines = vec![format!("Recent audit entries (showing {}):", entries.len())];
                            for entry in &entries {
                                let session = entry.session_id.as_deref().unwrap_or("-");
                                lines.push(format!(
                                    "  [{}] {} session={} {}",
                                    entry.created_at, entry.event_type, session,
                                    entry.details
                                ));
                            }
                            Some(lines.join("\n"))
                        }
                    }
                    Err(e) => Some(format!("Audit log unavailable: {e}")),
                }
            }
        }
        "bus" => {
            let bus_store = genesis_core::agent_bus::AgentBusStore::new(store.database_path());
            let sub = _args.trim();
            if sub == "stats" {
                match bus_store.channel_stats() {
                    Ok(stats) => {
                        if stats.is_empty() {
                            Some("No agent bus messages yet.".into())
                        } else {
                            let total: i64 = stats.iter().map(|(_, c)| c).sum();
                            let mut lines = vec![format!("Agent bus: {total} total messages")];
                            for (channel, count) in &stats {
                                lines.push(format!("  {channel}: {count} messages"));
                            }
                            lines.push("Commands: /bus stats, /bus history <channel>".into());
                            Some(lines.join("\n"))
                        }
                    }
                    Err(e) => Some(format!("Bus stats unavailable: {e}")),
                }
            } else if sub.starts_with("history") {
                let channel = sub.strip_prefix("history").unwrap_or("").trim();
                if channel.is_empty() {
                    Some("Usage: /bus history <channel>".into())
                } else {
                    match bus_store.channel_messages(channel, 20) {
                        Ok(messages) => {
                            if messages.is_empty() {
                                Some(format!("No messages on channel '{channel}'."))
                            } else {
                                let mut lines = vec![format!("Channel '{channel}' ({} messages):", messages.len())];
                                for msg in &messages {
                                    lines.push(format!(
                                        "  [{}] {} ({:?}): {}",
                                        msg.timestamp, msg.sender, msg.kind,
                                        if msg.payload.len() > 100 {
                                            format!("{}...", &msg.payload[..100])
                                        } else {
                                            msg.payload.clone()
                                        }
                                    ));
                                }
                                Some(lines.join("\n"))
                            }
                        }
                        Err(e) => Some(format!("Bus history unavailable: {e}")),
                    }
                }
            } else {
                match bus_store.channel_stats() {
                    Ok(stats) => {
                        let total: i64 = stats.iter().map(|(_, c)| c).sum();
                        let mut lines = vec![format!("Agent bus: {total} total messages, {} channels", stats.len())];
                        for (channel, count) in &stats {
                            lines.push(format!("  {channel}: {count} messages"));
                        }
                        lines.push("\nCommands: /bus stats, /bus history <channel>".into());
                        Some(lines.join("\n"))
                    }
                    Err(e) => Some(format!("Bus info unavailable: {e}")),
                }
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
        "model" => None, // handled in chat loop for mutable service access
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

/// Extract the clipboard image, save it to a temp file under `data_dir`, and
/// return an [`ImageUrl`] with the base64-encoded data URI.
fn extract_clipboard_as_image_url(
    data_dir: &Path,
) -> Result<genesis_provider::ImageUrl, clipboard::ClipboardError> {
    use base64::Engine;

    let clip_dir = data_dir.join("clipboard");
    std::fs::create_dir_all(&clip_dir).map_err(|e| {
        clipboard::ClipboardError::ExtractionFailed(format!(
            "failed to create clipboard dir: {e}"
        ))
    })?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let dest = clip_dir.join(format!("clip-{timestamp}.png"));

    clipboard::save_clipboard_image(&dest)?;

    let data = std::fs::read(&dest).map_err(clipboard::ClipboardError::Io)?;
    // Clean up the temp file — we only need the base64 data
    let _ = std::fs::remove_file(&dest);

    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
    Ok(genesis_provider::ImageUrl {
        url: format!("data:image/png;base64,{encoded}"),
        detail: None,
    })
}

fn default_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("cli-{timestamp}")
}

/// Guard that cleans up a git worktree when dropped.
/// The worktree is removed only if no uncommitted changes exist.
struct WorktreeGuard {
    path: String,
    branch: String,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        // Check for uncommitted changes before cleaning up
        let has_changes = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.path)
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(true);

        if has_changes {
            eprintln!(
                "Worktree has uncommitted changes, keeping at: {}\n\
                 Branch: {}\n\
                 To clean up: git worktree remove {}",
                self.path, self.branch, self.path
            );
        } else {
            // Remove the worktree
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", &self.path])
                .output();
            // Delete the temporary branch
            let _ = std::process::Command::new("git")
                .args(["branch", "-d", &self.branch])
                .output();
        }
    }
}

/// Creates a git worktree in a temporary directory for isolated agent work.
fn create_worktree() -> Result<WorktreeGuard, CliError> {
    // Verify we're in a git repo
    let toplevel = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| CliError::Other(format!("git not found: {e}")))?;
    if !toplevel.status.success() {
        return Err(CliError::Other(
            "--worktree requires a git repository".to_owned(),
        ));
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let branch_name = format!("genesis-worktree-{timestamp}");

    // Create a temp directory for the worktree
    let repo_root = String::from_utf8_lossy(&toplevel.stdout).trim().to_owned();
    let worktree_path = format!("{repo_root}/.git/genesis-worktrees/{branch_name}");

    // Ensure the worktrees directory exists
    let _ = std::fs::create_dir_all(format!("{repo_root}/.git/genesis-worktrees"));

    let output = std::process::Command::new("git")
        .args(["worktree", "add", "-b", &branch_name, &worktree_path])
        .output()
        .map_err(|e| CliError::Other(format!("failed to create worktree: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::Other(format!(
            "git worktree add failed: {stderr}"
        )));
    }

    Ok(WorktreeGuard {
        path: worktree_path,
        branch: branch_name,
    })
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
    images: Vec<genesis_provider::ImageUrl>,
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
            images,
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

    if !data.tool_usage.is_empty() {
        lines.push(String::new());
        lines.push("  top tools:".to_owned());
        for (name, count) in data.tool_usage.iter().take(10) {
            lines.push(format!("    {name}: {count} calls"));
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
    if db_exists && bootstrap(&loaded.config.storage.database_path).is_ok() {
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

    if loaded.config.storage.database_path.exists()
        && bootstrap(&loaded.config.storage.database_path).is_ok()
    {
        let session_store = SessionStore::new(&loaded.config.storage.database_path);
        if let Ok(stats) = session_store.usage_stats() {
            data["total_sessions"] = serde_json::json!(stats.total_sessions);
            data["total_tokens"] =
                serde_json::json!(stats.total_input_tokens + stats.total_output_tokens);
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

fn run_skills_scan(dir: &str, json: bool) -> Result<String, CliError> {
    let entries = genesis_core::skill_manifest::scan_skills_dir(std::path::Path::new(dir))
        .map_err(|e| CliError::Other(format!("failed to scan skills directory: {e}")))?;

    if json {
        return Ok(serde_json::to_string_pretty(&entries)?);
    }

    if entries.is_empty() {
        return Ok(format!("no SKILL.md files found in {dir}"));
    }

    let mut lines = vec![format!("found {} skill(s) in {dir}", entries.len())];
    for entry in &entries {
        let tags = if entry.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", entry.tags.join(", "))
        };
        lines.push(format!(
            "  {} v{}: {}{}",
            entry.name, entry.version, entry.description, tags
        ));
    }
    Ok(lines.join("\n"))
}

fn run_skills_search(
    store: &SkillStore,
    query: &str,
    dir: Option<&str>,
    json: bool,
) -> Result<String, CliError> {
    // Search stored skills
    let stored = store.find_matching(query, 50).unwrap_or_default();

    // Optionally search SKILL.md files on disk
    let disk_entries = if let Some(dir) = dir {
        let all = genesis_core::skill_manifest::scan_skills_dir(std::path::Path::new(dir))
            .unwrap_or_default();
        genesis_core::skill_manifest::search_entries(&all, query)
    } else {
        vec![]
    };

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "stored": stored,
            "disk": disk_entries,
        }))?);
    }

    let mut lines = Vec::new();
    if !stored.is_empty() {
        lines.push(format!("stored skills matching \"{}\":", query));
        for s in &stored {
            lines.push(format!("  {} v{}: {}", s.name, s.version, s.description));
        }
    }
    if !disk_entries.is_empty() {
        lines.push(format!("SKILL.md files matching \"{}\":", query));
        for e in &disk_entries {
            lines.push(format!(
                "  {} v{}: {} ({})",
                e.name,
                e.version,
                e.description,
                e.path.display()
            ));
        }
    }
    if lines.is_empty() {
        return Ok(format!("no skills matching \"{query}\""));
    }
    Ok(lines.join("\n"))
}

fn run_skills_install_local(store: &SkillStore, path: &str) -> Result<String, CliError> {
    let skill_file = std::path::Path::new(path).join("SKILL.md");
    let parsed = genesis_core::skill_manifest::parse_skill_file(&skill_file)
        .map_err(|e| CliError::Other(format!("failed to parse SKILL.md: {e}")))?;

    let tags: Vec<&str> = parsed.frontmatter.tags.iter().map(|s| s.as_str()).collect();
    let trigger = if parsed.frontmatter.description.is_empty() {
        None
    } else {
        Some(parsed.frontmatter.description.as_str())
    };

    store.upsert(
        &parsed.frontmatter.name,
        &parsed.frontmatter.description,
        &parsed.instructions,
        trigger,
        &tags,
    )?;

    Ok(format!(
        "installed skill '{}' v{} from {}",
        parsed.frontmatter.name, parsed.frontmatter.version, path
    ))
}

fn run_skills_hub(
    command: HubCommand,
    loaded: &LoadedConfig,
    json: bool,
) -> Result<String, CliError> {
    let data_dir = &loaded.config.storage.data_dir;
    let mut hub = genesis_core::skills_hub::SkillsHub::new(data_dir);

    // Add optional bundled skills source
    let optional_dir = data_dir.join("skills").join("optional");
    hub.add_optional_source(&optional_dir);

    // Load configured taps
    if let Err(e) = hub.load_taps() {
        eprintln!("warning: failed to load taps: {e}");
    }

    match command {
        HubCommand::Browse { page, size, source } => {
            let (manifests, total) = hub
                .browse(page, size, source.as_deref())
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "skills": manifests,
                    "total": total,
                    "page": page,
                    "page_size": size,
                }))?);
            }

            if manifests.is_empty() {
                return Ok("no skills found".to_owned());
            }

            let total_pages = total.div_ceil(size);
            let mut lines = vec![format!(
                "skills hub  (page {page}/{total_pages}, {total} total)"
            )];
            for m in &manifests {
                let tags = if m.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", m.tags.join(", "))
                };
                lines.push(format!(
                    "  {} v{}  {}{}\n    source: {}  author: {}  license: {}",
                    m.name, m.version, m.description, tags, m.source, m.author, m.license
                ));
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Search { query, source, limit } => {
            let results = hub
                .search(&query, source.as_deref(), limit)
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&results)?);
            }

            if results.is_empty() {
                return Ok(format!("no skills matching \"{query}\""));
            }

            let mut lines = vec![format!("found {} skill(s) matching \"{}\"", results.len(), query)];
            for m in &results {
                let tags = if m.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", m.tags.join(", "))
                };
                lines.push(format!(
                    "  {} v{}: {}{}  ({})",
                    m.name, m.version, m.description, tags, m.source
                ));
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Inspect { name } => {
            let (manifest, report) = hub
                .inspect(&name)
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "manifest": manifest,
                    "security": {
                        "verdict": format!("{:?}", report.verdict),
                        "findings": report.findings.len(),
                        "summary": report.summary(),
                    },
                }))?);
            }

            let mut lines = vec![
                format!("skill: {}", manifest.name),
                format!("description: {}", manifest.description),
                format!("version: {}", manifest.version),
                format!("author: {}", manifest.author),
                format!("license: {}", manifest.license),
                format!("source: {}", manifest.source),
            ];
            if !manifest.tags.is_empty() {
                lines.push(format!("tags: {}", manifest.tags.join(", ")));
            }
            lines.push(String::new());
            lines.push("security scan:".to_owned());
            lines.push(format!("  {}", report.summary()));
            for finding in &report.findings {
                let file = finding.file.as_deref().unwrap_or("(unknown)");
                let line = finding
                    .line
                    .map(|l| format!(":{l}"))
                    .unwrap_or_default();
                lines.push(format!(
                    "  [{:?}] {}{}: {} ({})",
                    finding.severity, file, line, finding.description, finding.category
                ));
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Install { name, force } => {
            let (lock, report) = hub
                .install(&name, force)
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "installed": lock,
                    "security": {
                        "verdict": format!("{:?}", report.verdict),
                        "findings": report.findings.len(),
                    },
                }))?);
            }

            let mut lines = vec![format!(
                "installed skill '{}' v{} from {}",
                lock.name, lock.version, lock.source
            )];
            lines.push(format!("  content hash: {}", lock.content_hash));
            lines.push(format!("  security: {}", report.summary()));
            Ok(lines.join("\n"))
        }

        HubCommand::Uninstall { name } => {
            hub.uninstall(&name)
                .map_err(|e| CliError::Other(e.to_string()))?;
            Ok(format!("uninstalled skill '{name}'"))
        }

        HubCommand::Audit => {
            let results = hub
                .audit()
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&results)?);
            }

            if results.is_empty() {
                return Ok("no hub-installed skills to audit".to_owned());
            }

            let mut lines = vec![format!("auditing {} installed skill(s)", results.len())];
            for r in &results {
                let integrity = if r.integrity_ok { "ok" } else { "MODIFIED" };
                lines.push(format!(
                    "  {} — verdict: {}  findings: {}  integrity: {}",
                    r.name, r.verdict, r.finding_count, integrity
                ));
                if !r.integrity_ok {
                    lines.push("    WARNING: content has been modified since installation".to_owned());
                }
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Installed => {
            let installed = hub
                .list_installed()
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&installed)?);
            }

            if installed.is_empty() {
                return Ok("no hub-installed skills".to_owned());
            }

            let mut lines = vec![format!("{} hub-installed skill(s)", installed.len())];
            for lock in &installed {
                let hash_preview = &lock.hash_value()[..12.min(lock.hash_value().len())];
                lines.push(format!(
                    "  {} v{}  {}  hash: {}...",
                    lock.name, lock.version, lock.source, hash_preview
                ));
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Tap(tap_cmd) => match tap_cmd {
            TapCommand::List => {
                let taps = hub
                    .list_taps()
                    .map_err(|e| CliError::Other(e.to_string()))?;

                if json {
                    return Ok(serde_json::to_string_pretty(&taps)?);
                }

                if taps.is_empty() {
                    return Ok("no taps configured".to_owned());
                }

                let mut lines = vec![format!("{} tap(s) configured", taps.len())];
                for tap in &taps {
                    lines.push(format!("  {tap}"));
                }
                Ok(lines.join("\n"))
            }
            TapCommand::Add { name, repo, path } => {
                let parts: Vec<&str> = repo.splitn(2, '/').collect();
                if parts.len() != 2 {
                    return Err(CliError::Other(
                        "repo must be in 'owner/repo' format (e.g. 'nooesc/genesis-skills')".to_owned(),
                    ));
                }

                let tap = genesis_core::skills_hub::Tap {
                    name: name.clone(),
                    owner: parts[0].to_owned(),
                    repo: parts[1].to_owned(),
                    path,
                };

                hub.add_tap(tap)
                    .map_err(|e| CliError::Other(e.to_string()))?;
                Ok(format!("added tap '{name}'"))
            }
            TapCommand::Remove { name } => {
                let removed = hub
                    .remove_tap(&name)
                    .map_err(|e| CliError::Other(e.to_string()))?;
                if removed {
                    Ok(format!("removed tap '{name}'"))
                } else {
                    Err(CliError::Other(format!("tap '{name}' not found")))
                }
            }
        },
    }
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
        run, run_compress, run_eval_export_chatml, run_eval_quality, run_personality, run_toolset,
        BootstrapCommand, Cli, Command, ConfigCommand, ContextCommand, McpCommand,
        MemoryCommand, ModelCommand, PairingCommand, PersonalityCommand, ScheduleCommand,
        SessionsCommand, SkillsCommand, StorageCommand, SubagentsCommand, ToolsetCommand,
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
            Command::Chat { session_id, resume, prompt, system, last, .. } => {
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
                mirror: false,
                mirror_source: None,
                created_at: "2026-03-08 12:00:00".to_owned(),
            },
            genesis_storage::StoredMessage {
                id: 2,
                session_id: "s-1".to_owned(),
                role: "assistant".to_owned(),
                content: Some("hi there".to_owned()),
                tool_call_id: None,
                tool_calls_json: None,
                mirror: false,
                mirror_source: None,
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
            mirror: false,
            mirror_source: None,
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
            mirror: false,
            mirror_source: None,
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
    fn parses_mcp_serve_command() {
        let cli = Cli::try_parse_from(["genesis", "mcp", "serve"])
            .expect("mcp serve command should parse");
        assert!(matches!(cli.command, Command::Mcp(McpCommand::Serve)));
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
    fn parses_chat_worktree_flag() {
        let cli = Cli::try_parse_from(["genesis", "chat", "--worktree"])
            .expect("chat --worktree should parse");
        match cli.command {
            Command::Chat { worktree, .. } => assert!(worktree),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_chat_worktree_defaults_to_false() {
        let cli = Cli::try_parse_from(["genesis", "chat"])
            .expect("chat should parse");
        match cli.command {
            Command::Chat { worktree, .. } => assert!(!worktree),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_chat_clipboard_flag() {
        let cli = Cli::try_parse_from(["genesis", "chat", "--clipboard"])
            .expect("chat --clipboard should parse");
        match cli.command {
            Command::Chat { clipboard, .. } => assert!(clipboard),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_chat_clipboard_defaults_to_false() {
        let cli = Cli::try_parse_from(["genesis", "chat"])
            .expect("chat should parse without --clipboard");
        match cli.command {
            Command::Chat { clipboard, .. } => assert!(!clipboard),
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
    fn parses_skills_scan_command() {
        let cli = Cli::try_parse_from(["genesis", "skills", "scan", "/tmp/skills"])
            .expect("skills scan should parse");
        match cli.command {
            Command::Skills(SkillsCommand::Scan { dir }) => {
                assert_eq!(dir, "/tmp/skills");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_skills_search_command() {
        let cli = Cli::try_parse_from(["genesis", "skills", "search", "deploy"])
            .expect("skills search should parse");
        match cli.command {
            Command::Skills(SkillsCommand::Search { query, dir }) => {
                assert_eq!(query, "deploy");
                assert!(dir.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_skills_install_local_command() {
        let cli = Cli::try_parse_from(["genesis", "skills", "install-local", "/tmp/my-skill"])
            .expect("skills install-local should parse");
        match cli.command {
            Command::Skills(SkillsCommand::InstallLocal { path }) => {
                assert_eq!(path, "/tmp/my-skill");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_skills_scan_finds_skill_files() {
        let dir = tempdir().expect("tempdir");
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: A test skill\nversion: \"1.0\"\ntags:\n  - test\n---\nDo things.",
        )
        .expect("write skill");

        let result = crate::run_skills_scan(dir.path().to_str().unwrap(), false)
            .expect("scan should succeed");
        assert!(result.contains("my-skill"));
        assert!(result.contains("A test skill"));
        assert!(result.contains("1 skill(s)"));
    }

    #[test]
    fn run_skills_install_local_stores_skill() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let skill_dir = dir.path().join("review");
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review code\nversion: \"2.0\"\ntags:\n  - dev\n  - quality\n---\nReview all code carefully.",
        )
        .expect("write skill");

        let store = genesis_storage::SkillStore::new(&db_path);
        let result = crate::run_skills_install_local(&store, skill_dir.to_str().unwrap())
            .expect("install should succeed");

        assert!(result.contains("installed skill 'review'"));
        assert!(result.contains("v2.0"));

        let stored = store.get("review").expect("db lookup").expect("skill exists");
        assert_eq!(stored.description, "Review code");
        assert!(stored.instructions.contains("Review all code carefully"));
        assert_eq!(stored.tags, vec!["dev", "quality"]);
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
                toolset,
                quality_filter,
                auto_tag,
            } => {
                assert_eq!(input, "prompts.jsonl");
                assert_eq!(output, "trajectories");
                assert!(model.is_none());
                assert!(max_turns.is_none());
                assert!(concurrency.is_none());
                assert!(toolset.is_none());
                assert!(quality_filter.is_none());
                assert!(!auto_tag);
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
                toolset,
                quality_filter,
                auto_tag,
            } => {
                assert_eq!(input, "prompts.jsonl");
                assert_eq!(output, "trajectories");
                assert_eq!(model.as_deref(), Some("claude-sonnet-4-6"));
                assert_eq!(max_turns, Some(12));
                assert_eq!(concurrency, Some(8));
                assert!(toolset.is_none());
                assert!(quality_filter.is_none());
                assert!(!auto_tag);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_batch_command_with_toolset() {
        let cli = Cli::try_parse_from([
            "genesis",
            "batch",
            "--input",
            "prompts.jsonl",
            "--output",
            "trajectories",
            "--toolset",
            "development",
        ])
        .expect("batch with toolset should parse");

        match cli.command {
            Command::Batch { toolset, .. } => {
                assert_eq!(toolset.as_deref(), Some("development"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_batch_command_with_quality_filter() {
        let cli = Cli::try_parse_from([
            "genesis",
            "batch",
            "--input",
            "prompts.jsonl",
            "--output",
            "trajectories",
            "--quality-filter",
            "0.75",
        ])
        .expect("batch with quality filter should parse");

        match cli.command {
            Command::Batch { quality_filter, .. } => {
                assert_eq!(quality_filter, Some(0.75));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_batch_command_with_auto_tag() {
        let cli = Cli::try_parse_from([
            "genesis",
            "batch",
            "--input",
            "prompts.jsonl",
            "--output",
            "trajectories",
            "--auto-tag",
        ])
        .expect("batch with auto-tag should parse");

        match cli.command {
            Command::Batch { auto_tag, .. } => {
                assert!(auto_tag);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_toolset_list() {
        let cli = Cli::try_parse_from(["genesis", "toolset", "list"]).expect("should parse");
        match cli.command {
            Command::Toolset(ToolsetCommand::List) => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_toolset_show() {
        let cli =
            Cli::try_parse_from(["genesis", "toolset", "show", "development"]).expect("should parse");
        match cli.command {
            Command::Toolset(ToolsetCommand::Show { name }) => {
                assert_eq!(name, "development");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_toolset_sample_with_seed() {
        let cli = Cli::try_parse_from(["genesis", "toolset", "sample", "random", "--seed", "42"])
            .expect("should parse");
        match cli.command {
            Command::Toolset(ToolsetCommand::Sample { name, seed }) => {
                assert_eq!(name, "random");
                assert_eq!(seed, Some(42));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_toolset_list_works() {
        let result = run_toolset(ToolsetCommand::List, false).expect("should succeed");
        assert!(result.contains("full"));
        assert!(result.contains("development"));
        assert!(result.contains("minimal"));
    }

    #[test]
    fn run_toolset_show_works() {
        let result = run_toolset(
            ToolsetCommand::Show {
                name: "minimal".to_owned(),
            },
            false,
        )
        .expect("should succeed");
        assert!(result.contains("shell_exec"));
        assert!(result.contains("read_file"));
    }

    #[test]
    fn run_toolset_show_unknown_errors() {
        let result = run_toolset(
            ToolsetCommand::Show {
                name: "nonexistent".to_owned(),
            },
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn run_toolset_sample_deterministic() {
        let result1 = run_toolset(
            ToolsetCommand::Sample {
                name: "random".to_owned(),
                seed: Some(42),
            },
            true,
        )
        .expect("should succeed");
        let result2 = run_toolset(
            ToolsetCommand::Sample {
                name: "random".to_owned(),
                seed: Some(42),
            },
            true,
        )
        .expect("should succeed");
        assert_eq!(result1, result2);
    }

    #[test]
    fn run_personality_list_works() {
        let result = run_personality(PersonalityCommand::List, false).expect("should succeed");
        assert!(result.contains("default"));
        assert!(result.contains("pirate"));
        assert!(result.contains("kawaii"));
        assert!(result.contains("hacker"));
    }

    #[test]
    fn run_personality_list_json() {
        let result = run_personality(PersonalityCommand::List, true).expect("should succeed");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result).expect("valid json");
        assert!(parsed.len() >= 13);
        assert!(parsed.iter().any(|p| p["name"] == "pirate"));
    }

    #[test]
    fn run_personality_show_works() {
        let result = run_personality(
            PersonalityCommand::Show {
                name: "pirate".to_owned(),
            },
            false,
        )
        .expect("should succeed");
        assert!(result.contains("Personality: pirate"));
        assert!(result.contains("System prompt prefix:"));
    }

    #[test]
    fn run_personality_show_unknown_errors() {
        let result = run_personality(
            PersonalityCommand::Show {
                name: "nonexistent".to_owned(),
            },
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parses_personality_list_command() {
        let cli = Cli::try_parse_from(["genesis", "personality", "list"]).expect("should parse");
        match cli.command {
            Command::Personality(PersonalityCommand::List) => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_personality_show_command() {
        let cli = Cli::try_parse_from(["genesis", "personality", "show", "pirate"])
            .expect("should parse");
        match cli.command {
            Command::Personality(PersonalityCommand::Show { name }) => {
                assert_eq!(name, "pirate");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_quality_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "quality",
            "trajectories",
            "--min-score",
            "0.5",
            "--worst-first",
        ])
        .expect("should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Quality {
                dir,
                min_score,
                worst_first,
                ..
            }) => {
                assert_eq!(dir, "trajectories");
                assert_eq!(min_score, Some(0.5));
                assert!(worst_first);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_auto_tag_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "auto-tag",
            "--dir",
            "trajectories",
            "--recursive",
            "--dry-run",
        ])
        .expect("auto-tag should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::AutoTag { dir, recursive, dry_run }) => {
                assert_eq!(dir, "trajectories");
                assert!(recursive);
                assert!(dry_run);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_tag_stats_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "tag-stats",
            "trajectories",
            "--recursive",
        ])
        .expect("tag-stats should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::TagStats { dir, recursive }) => {
                assert_eq!(dir, "trajectories");
                assert!(recursive);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_deduplicate_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "deduplicate",
            "trajectories",
            "--recursive",
            "--remove",
        ])
        .expect("deduplicate should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Deduplicate { dir, recursive, remove }) => {
                assert_eq!(dir, "trajectories");
                assert!(recursive);
                assert!(remove);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_eval_quality_scores_trajectories() {
        let dir = tempdir().expect("tempdir");
        // Write a good trajectory
        let good = serde_json::json!({
            "session_id": "test-good",
            "model": "gpt-4",
            "system_prompt_hash": "abc",
            "started_at": "2025-01-01T00:00:00Z",
            "completed_at": "2025-01-01T00:01:00Z",
            "steps": [
                {"step_index": 0, "timestamp": "2025-01-01T00:00:00Z", "action_type": "user_message", "content": "Hello"},
                {"step_index": 1, "timestamp": "2025-01-01T00:00:01Z", "action_type": "assistant_message", "content": "Hi there!"}
            ],
            "outcome": {"type": "success"},
            "tags": ["test"]
        });
        fs::write(
            dir.path().join("good.json"),
            serde_json::to_string_pretty(&good).unwrap(),
        )
        .unwrap();

        // Write a bad trajectory
        let bad = serde_json::json!({
            "session_id": "test-bad",
            "model": "",
            "system_prompt_hash": "",
            "started_at": "2025-01-01T00:00:00Z",
            "completed_at": null,
            "steps": [],
            "outcome": {"type": "failure", "reason": "broke"},
            "tags": []
        });
        fs::write(
            dir.path().join("bad.json"),
            serde_json::to_string_pretty(&bad).unwrap(),
        )
        .unwrap();

        let result = run_eval_quality(dir.path().to_str().unwrap(), false, None, false, false)
            .expect("should succeed");
        assert!(result.contains("Quality report: 2/2"));
        assert!(result.contains("good"));
        assert!(result.contains("bad"));
    }

    #[test]
    fn run_eval_quality_filters_by_min_score() {
        let dir = tempdir().expect("tempdir");
        let good = serde_json::json!({
            "session_id": "test-good",
            "model": "gpt-4",
            "system_prompt_hash": "abc",
            "started_at": "2025-01-01T00:00:00Z",
            "completed_at": "2025-01-01T00:01:00Z",
            "steps": [
                {"step_index": 0, "timestamp": "2025-01-01T00:00:00Z", "action_type": "user_message", "content": "Hello"},
                {"step_index": 1, "timestamp": "2025-01-01T00:00:01Z", "action_type": "assistant_message", "content": "Hi there!"}
            ],
            "outcome": {"type": "success"},
            "tags": ["test"]
        });
        fs::write(
            dir.path().join("good.json"),
            serde_json::to_string_pretty(&good).unwrap(),
        )
        .unwrap();

        let bad = serde_json::json!({
            "session_id": "test-bad",
            "model": "",
            "system_prompt_hash": "",
            "started_at": "2025-01-01T00:00:00Z",
            "completed_at": null,
            "steps": [],
            "outcome": {"type": "failure", "reason": "broke"},
            "tags": []
        });
        fs::write(
            dir.path().join("bad.json"),
            serde_json::to_string_pretty(&bad).unwrap(),
        )
        .unwrap();

        let result = run_eval_quality(dir.path().to_str().unwrap(), false, Some(0.5), false, false)
            .expect("should succeed");
        assert!(result.contains("1/2"));
        assert!(result.contains("good"));
        assert!(!result.contains(" bad"));
    }

    #[test]
    fn discard_low_quality_trajectory_removes_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("low.json");
        let trajectory = serde_json::json!({
            "session_id": "low",
            "model": "",
            "system_prompt_hash": "",
            "started_at": "2025-01-01T00:00:00Z",
            "completed_at": null,
            "steps": [],
            "outcome": {"type": "failure", "reason": "broke"},
            "tags": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&trajectory).unwrap()).unwrap();

        crate::discard_low_quality_trajectory(dir.path().to_str().unwrap(), "low", 0.5)
            .expect("quality discard should succeed");
        assert!(!path.exists());
    }

    #[test]
    fn run_eval_deduplicate_groups_and_removes_duplicates() {
        let dir = tempdir().expect("tempdir");

        let write_duplicate = |session_id: &str, filename: &str| {
            let trajectory = serde_json::json!({
                "session_id": session_id,
                "model": "gpt-4.1-mini",
                "system_prompt_hash": "same-hash",
                "started_at": "2026-03-08T12:00:00Z",
                "completed_at": "2026-03-08T12:01:00Z",
                "steps": [
                    {"step_index": 0, "timestamp": "2026-03-08T12:00:00Z", "action_type": "user_message", "content": "same prompt"},
                    {"step_index": 1, "timestamp": "2026-03-08T12:00:01Z", "action_type": "assistant_message", "content": "response"}
                ],
                "outcome": {"type": "success"},
                "tags": []
            });
            std::fs::write(
                dir.path().join(filename),
                serde_json::to_string_pretty(&trajectory).unwrap(),
            )
            .unwrap();
        };

        write_duplicate("s1", "a.json");
        write_duplicate("s2", "b.json");
        std::fs::write(
            dir.path().join("unique.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "session_id": "s3",
                "model": "gpt-4.1-mini",
                "system_prompt_hash": "other-hash",
                "started_at": "2026-03-08T12:00:00Z",
                "completed_at": "2026-03-08T12:01:00Z",
                "steps": [
                    {"step_index": 0, "timestamp": "2026-03-08T12:00:00Z", "action_type": "user_message", "content": "different prompt"}
                ],
                "outcome": {"type": "success"},
                "tags": []
            }))
            .unwrap(),
        )
        .unwrap();

        let output =
            crate::run_eval_deduplicate(dir.path().to_str().unwrap(), false, true, false)
                .expect("deduplicate should succeed");

        assert!(output.contains("duplicate groups: 1"));
        assert!(output.contains("removed files:    1"));
        assert!(dir.path().join("unique.json").exists());
        assert!(!(dir.path().join("a.json").exists() && dir.path().join("b.json").exists()));
    }

    #[test]
    fn run_eval_auto_tag_dry_run_and_writeback() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("trajectory.json");
        let trajectory = serde_json::json!({
            "session_id": "auto-tag",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash",
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [
                {"step_index": 0, "timestamp": "2026-03-08T12:00:00Z", "action_type": "user_message", "content": "please fix this bug"},
                {"step_index": 1, "timestamp": "2026-03-08T12:00:01Z", "action_type": "tool_call", "content": "tool_call: shell_exec", "tool_name": "shell_exec", "tool_arguments": "{\"cmd\":\"pwd\"}"},
                {"step_index": 2, "timestamp": "2026-03-08T12:00:02Z", "action_type": "tool_result", "content": "tool_result: shell_exec", "tool_name": "shell_exec", "tool_result": "/tmp"},
                {"step_index": 3, "timestamp": "2026-03-08T12:00:03Z", "action_type": "assistant_message", "content": "fixed"}
            ],
            "outcome": {"type": "success"},
            "tags": ["existing"]
        });
        std::fs::write(&path, serde_json::to_string_pretty(&trajectory).unwrap()).unwrap();

        let dry_run = crate::run_eval_auto_tag(
            dir.path().to_str().unwrap(),
            false,
            true,
            false,
        )
        .expect("dry run should succeed");
        assert!(dry_run.contains("files changed: 1"));

        let unchanged: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(unchanged["tags"], serde_json::json!(["existing"]));

        crate::run_eval_auto_tag(dir.path().to_str().unwrap(), false, false, false)
            .expect("writeback should succeed");
        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let tags = updated["tags"].as_array().unwrap();
        assert!(tags.iter().any(|tag| tag == "existing"));
        assert!(tags.iter().any(|tag| tag == "success"));
        assert!(tags.iter().any(|tag| tag == "shell"));
        assert!(tags.iter().any(|tag| tag == "debugging"));
    }

    #[test]
    fn run_eval_tag_stats_reports_frequency() {
        let dir = tempdir().expect("tempdir");
        let write = |name: &str, tags: &[&str]| {
            let path = dir.path().join(name);
            let trajectory = serde_json::json!({
                "session_id": name,
                "model": "gpt-4.1-mini",
                "system_prompt_hash": "hash",
                "started_at": "2026-03-08T12:00:00Z",
                "completed_at": "2026-03-08T12:01:00Z",
                "steps": [],
                "outcome": {"type": "success"},
                "tags": tags
            });
            std::fs::write(path, serde_json::to_string_pretty(&trajectory).unwrap()).unwrap();
        };

        write("a.json", &["shell", "success"]);
        write("b.json", &["success"]);
        write("c.json", &["shell"]);

        let output = crate::run_eval_tag_stats(dir.path().to_str().unwrap(), false, false)
            .expect("tag stats should succeed");

        assert!(output.contains("shell: 2"));
        assert!(output.contains("success: 2"));
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
                training,
            } => {
                assert_eq!(input, "trajectory.json");
                assert!(output.is_none());
                assert!(level.is_none());
                assert!(format.is_none());
                assert!(!training);
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
                training,
            } => {
                assert_eq!(input, "trajectory.json");
                assert_eq!(output.as_deref(), Some("out/sharegpt.json"));
                assert_eq!(level.as_deref(), Some("heavy"));
                assert_eq!(format.as_deref(), Some("sharegpt"));
                assert!(!training);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_compress_command_with_training_flag() {
        let cli = Cli::try_parse_from([
            "genesis",
            "compress",
            "--input",
            "trajectory.json",
            "--training",
        ])
        .expect("compress with training should parse");

        match cli.command {
            Command::Compress {
                input,
                output,
                level,
                format,
                training,
            } => {
                assert_eq!(input, "trajectory.json");
                assert!(output.is_none());
                assert!(level.is_none());
                assert!(format.is_none());
                assert!(training);
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
            tokens_per_day: vec![
                ("2026-03-07".to_owned(), 1500, 900),
                ("2026-03-08".to_owned(), 3500, 2100),
            ],
            tool_usage: vec![
                ("shell_exec".to_owned(), 15),
                ("echo".to_owned(), 5),
            ],
            avg_input_tokens: 500,
            avg_output_tokens: 300,
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
    fn chat_help_includes_personality() {
        let result = handle_chat_command("/help", "s1", &stub_session_store());
        let help = result.expect("help should return something");
        assert!(help.contains("/personality"));
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
    fn parses_eval_report_command() {
        let cli = Cli::try_parse_from(["genesis", "eval", "report", "trajectory.json"])
            .expect("eval report should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Report { file }) => {
                assert_eq!(file, "trajectory.json");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_summarize_command() {
        let cli = Cli::try_parse_from(["genesis", "eval", "summarize", "trajectories"])
            .expect("eval summarize should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Summarize {
                dir,
                recursive,
                model,
                tag,
                tool,
                failures_only,
                warnings_only,
                min_warnings,
            }) => {
                assert_eq!(dir, "trajectories");
                assert!(!recursive);
                assert!(model.is_none());
                assert!(tag.is_none());
                assert!(tool.is_none());
                assert!(!failures_only);
                assert!(!warnings_only);
                assert!(min_warnings.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_summarize_recursive_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "summarize",
            "trajectories",
            "--recursive",
        ])
        .expect("eval summarize recursive should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Summarize {
                dir,
                recursive,
                model,
                tag,
                tool,
                failures_only,
                warnings_only,
                min_warnings,
            }) => {
                assert_eq!(dir, "trajectories");
                assert!(recursive);
                assert!(model.is_none());
                assert!(tag.is_none());
                assert!(tool.is_none());
                assert!(!failures_only);
                assert!(!warnings_only);
                assert!(min_warnings.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_summarize_with_filters() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "summarize",
            "trajectories",
            "--model",
            "gpt-4.1-mini",
            "--tag",
            "offline_eval",
        ])
        .expect("eval summarize with filters should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Summarize {
                dir,
                recursive,
                model,
                tag,
                tool,
                failures_only,
                warnings_only,
                min_warnings,
            }) => {
                assert_eq!(dir, "trajectories");
                assert!(!recursive);
                assert_eq!(model.as_deref(), Some("gpt-4.1-mini"));
                assert_eq!(tag.as_deref(), Some("offline_eval"));
                assert!(tool.is_none());
                assert!(!failures_only);
                assert!(!warnings_only);
                assert!(min_warnings.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_summarize_with_failure_and_warning_filters() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "summarize",
            "trajectories",
            "--failures-only",
            "--warnings-only",
        ])
        .expect("eval summarize with failure and warning filters should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Summarize {
                dir,
                recursive,
                model,
                tag,
                tool,
                failures_only,
                warnings_only,
                min_warnings,
            }) => {
                assert_eq!(dir, "trajectories");
                assert!(!recursive);
                assert!(model.is_none());
                assert!(tag.is_none());
                assert!(tool.is_none());
                assert!(failures_only);
                assert!(warnings_only);
                assert!(min_warnings.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_compare_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "compare",
            "left.json",
            "right.json",
        ])
        .expect("eval compare should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Compare { left, right }) => {
                assert_eq!(left, "left.json");
                assert_eq!(right, "right.json");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_export_chatml_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "export-chatml",
            "trajectories",
            "--recursive",
        ])
        .expect("eval export-chatml should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::ExportChatml { dir, recursive }) => {
                assert_eq!(dir, "trajectories");
                assert!(recursive);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_export_sharegpt_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "export-sharegpt",
            "trajectories",
            "--recursive",
        ])
        .expect("eval export-sharegpt should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::ExportSharegpt { dir, recursive }) => {
                assert_eq!(dir, "trajectories");
                assert!(recursive);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_import_chatml_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "import-chatml",
            "dataset.jsonl",
            "--output",
            "out",
        ])
        .expect("eval import-chatml should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::ImportChatml { file, output }) => {
                assert_eq!(file, "dataset.jsonl");
                assert_eq!(output, "out");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_convert_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "convert",
            "--input",
            "trajectory.json",
            "--output",
            "out.jsonl",
            "--format",
            "chatml",
        ])
        .expect("eval convert should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Convert { input, output, format }) => {
                assert_eq!(input, "trajectory.json");
                assert_eq!(output, "out.jsonl");
                assert_eq!(format, "chatml");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_stats_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "stats",
            "trajectories",
            "--recursive",
            "--model",
            "gpt-4.1-mini",
            "--tag",
            "offline_eval",
            "--tool",
            "shell",
            "--failures-only",
        ])
        .expect("eval stats should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Stats {
                dir,
                recursive,
                model,
                tag,
                tool,
                failures_only,
            }) => {
                assert_eq!(dir, "trajectories");
                assert!(recursive);
                assert_eq!(model.as_deref(), Some("gpt-4.1-mini"));
                assert_eq!(tag.as_deref(), Some("offline_eval"));
                assert_eq!(tool.as_deref(), Some("shell"));
                assert!(failures_only);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn summarize_replay_reports_aggregates_directory() {
        let dir = tempdir().expect("tempdir");
        let file_one = dir.path().join("one.json");
        let file_two = dir.path().join("two.json");

        let first = serde_json::json!({
            "session_id": "s-1",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-1",
            "started_at": "2026-03-08T10:00:00Z",
            "completed_at": "2026-03-08T10:01:00Z",
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-03-08T10:00:00Z",
                    "action_type": "user_message",
                    "content": "hello"
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-03-08T10:00:01Z",
                    "action_type": "assistant_message",
                    "content": "hi"
                },
                {
                    "step_index": 2,
                    "timestamp": "2026-03-08T10:00:02Z",
                    "action_type": "tool_call",
                    "content": "tool_call: shell",
                    "tool_name": "shell",
                    "tool_arguments": "{\"cmd\":\"pwd\"}"
                },
                {
                    "step_index": 3,
                    "timestamp": "2026-03-08T10:00:03Z",
                    "action_type": "tool_result",
                    "content": "tool_result: shell",
                    "tool_name": "shell",
                    "tool_result": "/tmp"
                }
            ],
            "outcome": { "type": "success" },
            "tags": ["smoke", "offline_eval"]
        });

        let second = serde_json::json!({
            "session_id": "s-2",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-2",
            "started_at": "2026-03-08T11:00:00Z",
            "completed_at": "2026-03-08T11:01:00Z",
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-03-08T11:00:00Z",
                    "action_type": "system_message",
                    "content": "boot"
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-03-08T11:00:01Z",
                    "action_type": "user_message",
                    "content": "do work"
                }
            ],
            "outcome": { "type": "abandoned" },
            "tags": ["offline_eval"]
        });

        std::fs::write(&file_one, serde_json::to_string_pretty(&first).unwrap())
            .expect("write first trajectory");
        std::fs::write(&file_two, serde_json::to_string_pretty(&second).unwrap())
            .expect("write second trajectory");

        let summary = crate::summarize_replay_reports(
            dir.path().to_str().unwrap(),
            false,
            None,
            None,
            None,
            false,
            false,
            None,
        )
        .expect("summary should build");

        assert_eq!(summary.files_processed, 2);
        assert!(!summary.recursive);
        assert!(summary.model_filter.is_none());
        assert!(summary.tag_filter.is_none());
        assert!(!summary.failures_only);
        assert!(!summary.warnings_only);
        assert_eq!(summary.total_events, 6);
        assert_eq!(summary.event_counts.user, 2);
        assert_eq!(summary.event_counts.assistant, 1);
        assert_eq!(summary.event_counts.tool_call, 1);
        assert_eq!(summary.event_counts.tool_result, 1);
        assert_eq!(summary.event_counts.system, 1);
        assert_eq!(summary.success_count, 1);
        assert_eq!(summary.abandoned_count, 1);
        assert_eq!(summary.failure_count, 0);
        assert_eq!(summary.missing_outcome_count, 0);
        assert!(summary.top_failure_reasons.is_empty());
        assert!(summary.top_warning_messages.is_empty());
        assert_eq!(summary.models, vec![("gpt-4.1-mini".to_owned(), 2)]);
        assert!(summary
            .tags
            .contains(&("offline_eval".to_owned(), 2)));
        assert!(summary
            .tools
            .iter()
            .any(|tool| tool.name == "shell" && tool.call_count == 1 && tool.result_count == 1));
    }

    #[test]
    fn compare_replay_reports_reports_deltas() {
        let dir = tempdir().expect("tempdir");
        let left = dir.path().join("left.json");
        let right = dir.path().join("right.json");

        let left_trajectory = serde_json::json!({
            "session_id": "left-session",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-left",
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-03-08T12:00:00Z",
                    "action_type": "user_message",
                    "content": "hello"
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-03-08T12:00:01Z",
                    "action_type": "assistant_message",
                    "content": "hi"
                }
            ],
            "outcome": { "type": "success" },
            "tags": ["baseline"]
        });

        let right_trajectory = serde_json::json!({
            "session_id": "right-session",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-right",
            "started_at": "2026-03-08T12:05:00Z",
            "completed_at": "2026-03-08T12:06:00Z",
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-03-08T12:05:00Z",
                    "action_type": "user_message",
                    "content": "hello"
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-03-08T12:05:01Z",
                    "action_type": "assistant_message",
                    "content": "hi"
                },
                {
                    "step_index": 2,
                    "timestamp": "2026-03-08T12:05:02Z",
                    "action_type": "tool_call",
                    "content": "tool_call: shell",
                    "tool_name": "shell",
                    "tool_arguments": "{\"cmd\":\"pwd\"}"
                },
                {
                    "step_index": 3,
                    "timestamp": "2026-03-08T12:05:03Z",
                    "action_type": "tool_result",
                    "content": "tool_result: shell",
                    "tool_name": "shell",
                    "tool_result": "/tmp"
                }
            ],
            "outcome": { "type": "success" },
            "tags": ["baseline", "with_tools"]
        });

        std::fs::write(&left, serde_json::to_string_pretty(&left_trajectory).unwrap())
            .expect("write left");
        std::fs::write(&right, serde_json::to_string_pretty(&right_trajectory).unwrap())
            .expect("write right");

        let comparison = crate::compare_replay_reports(
            left.to_str().unwrap(),
            right.to_str().unwrap(),
        )
        .expect("comparison should build");

        assert_eq!(comparison.left_session_id, "left-session");
        assert_eq!(comparison.right_session_id, "right-session");
        assert_eq!(comparison.left_total_events, 2);
        assert_eq!(comparison.right_total_events, 4);
        assert_eq!(comparison.event_delta.user, 0);
        assert_eq!(comparison.event_delta.assistant, 0);
        assert_eq!(comparison.event_delta.tool_call, 1);
        assert_eq!(comparison.event_delta.tool_result, 1);
        assert!(comparison.left_only_tags.is_empty());
        assert_eq!(comparison.right_only_tags, vec!["with_tools".to_owned()]);
        assert!(comparison.tools.iter().any(|tool| {
            tool.name == "shell"
                && tool.left_call_count == 0
                && tool.right_call_count == 1
                && tool.left_result_count == 0
                && tool.right_result_count == 1
        }));
    }

    #[test]
    fn run_compress_training_uses_trajectory_compressor() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("trajectory.json");
        let trajectory = serde_json::json!({
            "session_id": "s-train",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-train",
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-03-08T12:00:00Z",
                    "action_type": "user_message",
                    "content": "first"
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-03-08T12:00:01Z",
                    "action_type": "assistant_message",
                    "content": "second"
                },
                {
                    "step_index": 2,
                    "timestamp": "2026-03-08T12:00:02Z",
                    "action_type": "tool_call",
                    "content": "tool_call: shell",
                    "tool_name": "shell",
                    "tool_arguments": "{\"cmd\":\"pwd\"}"
                },
                {
                    "step_index": 3,
                    "timestamp": "2026-03-08T12:00:03Z",
                    "action_type": "tool_result",
                    "content": "tool_result: shell",
                    "tool_name": "shell",
                    "tool_result": "/tmp"
                },
                {
                    "step_index": 4,
                    "timestamp": "2026-03-08T12:00:04Z",
                    "action_type": "assistant_message",
                    "content": "third"
                },
                {
                    "step_index": 5,
                    "timestamp": "2026-03-08T12:00:05Z",
                    "action_type": "user_message",
                    "content": "last user"
                },
                {
                    "step_index": 6,
                    "timestamp": "2026-03-08T12:00:06Z",
                    "action_type": "assistant_message",
                    "content": "last assistant"
                }
            ],
            "outcome": { "type": "success" },
            "tags": ["training"]
        });
        std::fs::write(&input, serde_json::to_string_pretty(&trajectory).unwrap())
            .expect("write trajectory");

        let rendered = run_compress(
            input.to_string_lossy().into_owned(),
            None,
            Some("medium".to_owned()),
            Some("json".to_owned()),
            true,
        )
        .expect("training compression should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("compressed json should parse");
        assert_eq!(parsed["turns"].as_array().unwrap().len(), 5);
        assert!(parsed["turns"][2]["content"]
            .as_str()
            .unwrap()
            .contains("Summary of"));
    }

    #[test]
    fn run_eval_export_chatml_emits_jsonl() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("trajectory.json");
        let trajectory = serde_json::json!({
            "session_id": "s-chatml",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-chatml",
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-03-08T12:00:00Z",
                    "action_type": "user_message",
                    "content": "hello"
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-03-08T12:00:01Z",
                    "action_type": "assistant_message",
                    "content": "hi"
                }
            ],
            "outcome": { "type": "success" },
            "tags": ["dataset"]
        });
        std::fs::write(&input, serde_json::to_string_pretty(&trajectory).unwrap())
            .expect("write trajectory");

        let output = run_eval_export_chatml(dir.path().to_str().unwrap(), false)
            .expect("chatml export should succeed");
        let line = output.lines().next().expect("one jsonl line");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("valid jsonl object");

        assert_eq!(parsed["session_id"], "s-chatml");
        assert_eq!(parsed["model"], "gpt-4.1-mini");
        assert_eq!(parsed["tags"][0], "dataset");
        assert!(parsed["chatml"]
            .as_str()
            .unwrap()
            .contains("<|im_start|>user"));
    }

    #[test]
    fn run_eval_export_sharegpt_emits_jsonl() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("trajectory.json");
        let trajectory = serde_json::json!({
            "session_id": "s-sharegpt",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-sharegpt",
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-03-08T12:00:00Z",
                    "action_type": "user_message",
                    "content": "hello"
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-03-08T12:00:01Z",
                    "action_type": "assistant_message",
                    "content": "hi"
                }
            ],
            "outcome": { "type": "success" },
            "tags": ["dataset"]
        });
        std::fs::write(&input, serde_json::to_string_pretty(&trajectory).unwrap())
            .expect("write trajectory");

        let output = crate::run_eval_export_sharegpt(dir.path().to_str().unwrap(), false)
            .expect("sharegpt export should succeed");
        let line = output.lines().next().expect("one jsonl line");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("valid jsonl object");

        assert_eq!(parsed["session_id"], "s-sharegpt");
        assert_eq!(parsed["model"], "gpt-4.1-mini");
        assert_eq!(parsed["tags"][0], "dataset");
        assert_eq!(parsed["sharegpt"][0]["from"], "human");
    }

    #[test]
    fn run_eval_import_chatml_creates_trajectory_files() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("dataset.jsonl");
        let output_dir = dir.path().join("out");
        let line = serde_json::json!({
            "session_id": "chatml-session",
            "model": "gpt-4.1-mini",
            "tags": ["dataset"],
            "outcome": { "type": "success" },
            "chatml": "<|im_start|>system\nYou are Eve.<|im_end|>\n<|im_start|>user\nhello<|im_end|>\n<|im_start|>assistant\nhi<|im_end|>\n"
        });
        std::fs::write(&input, format!("{}\n", serde_json::to_string(&line).unwrap()))
            .expect("write jsonl");

        let result = crate::run_eval_import_chatml(
            input.to_str().unwrap(),
            output_dir.to_str().unwrap(),
        )
        .expect("chatml import should succeed");

        assert!(result.contains("imported 1 trajectories"));
        let imported_path = output_dir.join("chatml-session.json");
        assert!(imported_path.exists());

        let raw = std::fs::read_to_string(imported_path).expect("read imported trajectory");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid trajectory");
        assert_eq!(parsed["session_id"], "chatml-session");
        assert_eq!(parsed["model"], "gpt-4.1-mini");
        assert_eq!(parsed["tags"][0], "dataset");
        assert_eq!(parsed["steps"].as_array().unwrap().len(), 3);
        assert_eq!(parsed["steps"][0]["action_type"], "system_message");
        assert_eq!(parsed["steps"][1]["action_type"], "user_message");
        assert_eq!(parsed["steps"][2]["action_type"], "assistant_message");
    }

    #[test]
    fn run_eval_import_sharegpt_creates_trajectory_files() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("dataset.jsonl");
        let output_dir = dir.path().join("out");
        let line = serde_json::json!({
            "session_id": "sharegpt-session",
            "model": "claude-sonnet-4-6",
            "tags": ["imported"],
            "outcome": { "type": "success" },
            "sharegpt": [
                {"from": "human", "value": "hello"},
                {"from": "gpt", "value": "hi there"}
            ]
        });
        std::fs::write(&input, format!("{}\n", serde_json::to_string(&line).unwrap()))
            .expect("write jsonl");

        let result = crate::run_eval_import_sharegpt(
            input.to_str().unwrap(),
            output_dir.to_str().unwrap(),
        )
        .expect("sharegpt import should succeed");

        assert!(result.contains("imported 1 trajectories"));
        let imported_path = output_dir.join("sharegpt-session.json");
        assert!(imported_path.exists());

        let raw = std::fs::read_to_string(imported_path).expect("read imported trajectory");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid trajectory");
        assert_eq!(parsed["session_id"], "sharegpt-session");
        assert_eq!(parsed["model"], "claude-sonnet-4-6");
        assert_eq!(parsed["tags"][0], "imported");
        assert_eq!(parsed["steps"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["steps"][0]["action_type"], "user_message");
        assert_eq!(parsed["steps"][1]["action_type"], "assistant_message");
    }

    #[test]
    fn run_eval_merge_combines_directories() {
        let dir = tempdir().expect("tempdir");
        let src1 = dir.path().join("src1");
        let src2 = dir.path().join("src2");
        let output = dir.path().join("merged");
        std::fs::create_dir_all(&src1).unwrap();
        std::fs::create_dir_all(&src2).unwrap();

        let traj1 = serde_json::json!({
            "session_id": "s1",
            "model": "gpt-4",
            "system_prompt_hash": "h1",
            "started_at": "2026-01-01T00:00:00Z",
            "steps": [],
            "tags": []
        });
        let traj2 = serde_json::json!({
            "session_id": "s2",
            "model": "gpt-4",
            "system_prompt_hash": "h2",
            "started_at": "2026-01-01T00:00:00Z",
            "steps": [],
            "tags": []
        });
        std::fs::write(src1.join("s1.json"), serde_json::to_string(&traj1).unwrap()).unwrap();
        std::fs::write(src2.join("s2.json"), serde_json::to_string(&traj2).unwrap()).unwrap();

        let result = crate::run_eval_merge(
            &[
                src1.to_str().unwrap().to_owned(),
                src2.to_str().unwrap().to_owned(),
            ],
            output.to_str().unwrap(),
            false,
        )
        .expect("merge should succeed");

        assert!(result.contains("merged 2"));
        assert!(output.join("s1.json").exists());
        assert!(output.join("s2.json").exists());
    }

    #[test]
    fn run_eval_merge_dedup_by_session_id() {
        let dir = tempdir().expect("tempdir");
        let src1 = dir.path().join("src1");
        let src2 = dir.path().join("src2");
        let output = dir.path().join("merged");
        std::fs::create_dir_all(&src1).unwrap();
        std::fs::create_dir_all(&src2).unwrap();

        let traj = serde_json::json!({
            "session_id": "duplicate-id",
            "model": "gpt-4",
            "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z",
            "steps": [],
            "tags": []
        });
        std::fs::write(src1.join("a.json"), serde_json::to_string(&traj).unwrap()).unwrap();
        std::fs::write(src2.join("b.json"), serde_json::to_string(&traj).unwrap()).unwrap();

        let result = crate::run_eval_merge(
            &[
                src1.to_str().unwrap().to_owned(),
                src2.to_str().unwrap().to_owned(),
            ],
            output.to_str().unwrap(),
            true,
        )
        .expect("merge with dedup should succeed");

        assert!(result.contains("merged 1"));
        assert!(result.contains("skipped 1"));
    }

    #[test]
    fn parses_eval_import_sharegpt_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "import-sharegpt", "data.jsonl", "--output", "out",
        ])
        .expect("import-sharegpt should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::ImportSharegpt { file, output }) => {
                assert_eq!(file, "data.jsonl");
                assert_eq!(output, "out");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_merge_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "merge", "dir1", "dir2", "--output", "merged", "--dedup",
        ])
        .expect("merge should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Merge {
                sources,
                output,
                dedup,
            }) => {
                assert_eq!(sources, vec!["dir1", "dir2"]);
                assert_eq!(output, "merged");
                assert!(dedup);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_eval_filter_by_model_and_success() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();

        let t1 = serde_json::json!({
            "session_id": "s1", "model": "gpt-4", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z", "steps": [],
            "outcome": {"type": "success"}, "tags": ["test"]
        });
        let t2 = serde_json::json!({
            "session_id": "s2", "model": "claude", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z", "steps": [],
            "outcome": {"type": "success"}, "tags": []
        });
        let t3 = serde_json::json!({
            "session_id": "s3", "model": "gpt-4", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z", "steps": [],
            "outcome": {"type": "failure", "reason": "oops"}, "tags": []
        });
        std::fs::write(src.join("s1.json"), serde_json::to_string(&t1).unwrap()).unwrap();
        std::fs::write(src.join("s2.json"), serde_json::to_string(&t2).unwrap()).unwrap();
        std::fs::write(src.join("s3.json"), serde_json::to_string(&t3).unwrap()).unwrap();

        let result = crate::run_eval_filter(
            src.to_str().unwrap(), out.to_str().unwrap(), false,
            Some("gpt-4"), None, None, None, true, false, None, None, None,
        )
        .expect("filter should succeed");

        assert!(result.contains("1/3"));
        assert!(out.join("s1.json").exists());
        assert!(!out.join("s2.json").exists());
        assert!(!out.join("s3.json").exists());
    }

    #[test]
    fn run_eval_filter_by_tag() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();

        let t1 = serde_json::json!({
            "session_id": "s1", "model": "m", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z", "steps": [],
            "tags": ["coding", "debug"]
        });
        let t2 = serde_json::json!({
            "session_id": "s2", "model": "m", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z", "steps": [],
            "tags": ["research"]
        });
        std::fs::write(src.join("s1.json"), serde_json::to_string(&t1).unwrap()).unwrap();
        std::fs::write(src.join("s2.json"), serde_json::to_string(&t2).unwrap()).unwrap();

        let result = crate::run_eval_filter(
            src.to_str().unwrap(), out.to_str().unwrap(), false,
            None, Some("coding"), None, None, false, false, None, None, None,
        )
        .expect("filter should succeed");

        assert!(result.contains("1/2"));
        assert!(out.join("s1.json").exists());
    }

    #[test]
    fn run_eval_split_creates_train_test() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let train = dir.path().join("train");
        let test = dir.path().join("test");
        std::fs::create_dir_all(&src).unwrap();

        for i in 0..10 {
            let t = serde_json::json!({
                "session_id": format!("s{i}"), "model": "m", "system_prompt_hash": "h",
                "started_at": "2026-01-01T00:00:00Z", "steps": [], "tags": []
            });
            std::fs::write(
                src.join(format!("s{i}.json")),
                serde_json::to_string(&t).unwrap(),
            )
            .unwrap();
        }

        let result = crate::run_eval_split(
            src.to_str().unwrap(),
            train.to_str().unwrap(),
            test.to_str().unwrap(),
            0.8,
            Some(42),
            false,
        )
        .expect("split should succeed");

        assert!(result.contains("8 train"));
        assert!(result.contains("2 test"));

        let train_count = std::fs::read_dir(&train).unwrap().count();
        let test_count = std::fs::read_dir(&test).unwrap().count();
        assert_eq!(train_count, 8);
        assert_eq!(test_count, 2);
    }

    #[test]
    fn run_eval_split_deterministic_with_seed() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        for i in 0..5 {
            let t = serde_json::json!({
                "session_id": format!("s{i}"), "model": "m", "system_prompt_hash": "h",
                "started_at": "2026-01-01T00:00:00Z", "steps": [], "tags": []
            });
            std::fs::write(
                src.join(format!("s{i}.json")),
                serde_json::to_string(&t).unwrap(),
            )
            .unwrap();
        }

        let train1 = dir.path().join("t1");
        let test1 = dir.path().join("e1");
        let train2 = dir.path().join("t2");
        let test2 = dir.path().join("e2");

        crate::run_eval_split(
            src.to_str().unwrap(), train1.to_str().unwrap(), test1.to_str().unwrap(),
            0.6, Some(99), false,
        ).unwrap();

        crate::run_eval_split(
            src.to_str().unwrap(), train2.to_str().unwrap(), test2.to_str().unwrap(),
            0.6, Some(99), false,
        ).unwrap();

        let names1: Vec<String> = std::fs::read_dir(&train1).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let names2: Vec<String> = std::fs::read_dir(&train2).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(names1, names2);
    }

    #[test]
    fn parses_eval_filter_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "filter", "src", "--output", "out",
            "--model", "gpt-4", "--success-only", "--min-quality", "0.5",
        ])
        .expect("filter should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Filter {
                dir, output, model, success_only, min_quality, ..
            }) => {
                assert_eq!(dir, "src");
                assert_eq!(output, "out");
                assert_eq!(model.as_deref(), Some("gpt-4"));
                assert!(success_only);
                assert_eq!(min_quality, Some(0.5));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_split_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "split", "src", "--train", "train", "--test", "test",
            "--ratio", "0.7", "--seed", "42",
        ])
        .expect("split should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Split {
                dir, train, test, ratio, seed, ..
            }) => {
                assert_eq!(dir, "src");
                assert_eq!(train, "train");
                assert_eq!(test, "test");
                assert!((ratio - 0.7).abs() < f64::EPSILON);
                assert_eq!(seed, Some(42));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_eval_manifest_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "manifest", "src", "--name", "my-dataset",
            "--description", "test set", "--save",
        ])
        .expect("manifest should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Manifest {
                dir, name, description, save, ..
            }) => {
                assert_eq!(dir, "src");
                assert_eq!(name.as_deref(), Some("my-dataset"));
                assert_eq!(description.as_deref(), Some("test set"));
                assert!(save);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_eval_manifest_shows_stats() {
        let dir = tempdir().expect("tempdir");
        let t = serde_json::json!({
            "session_id": "s1", "model": "gpt-4", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z",
            "steps": [{"step_index": 0, "timestamp": "t", "action_type": "user_message", "content": "hi"}],
            "outcome": {"type": "success"}, "tags": ["test"]
        });
        std::fs::write(
            dir.path().join("s1.json"),
            serde_json::to_string(&t).unwrap(),
        ).unwrap();

        let result = crate::run_eval_manifest(
            dir.path().to_str().unwrap(), "test-ds", "a test", false, false, false,
        ).expect("manifest should succeed");

        assert!(result.contains("test-ds"));
        assert!(result.contains("files: 1"));
        assert!(result.contains("gpt-4"));
    }

    #[test]
    fn parses_eval_pipeline_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "pipeline", "src", "--output", "out",
            "--validate", "--auto-tag", "--min-quality", "0.5",
            "--success-only", "--manifest",
        ])
        .expect("pipeline should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Pipeline {
                dir,
                output,
                validate,
                auto_tag,
                min_quality,
                success_only,
                manifest,
                ..
            }) => {
                assert_eq!(dir, "src");
                assert_eq!(output, "out");
                assert!(validate);
                assert!(auto_tag);
                assert_eq!(min_quality, Some(0.5));
                assert!(success_only);
                assert!(manifest);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_eval_pipeline_filters_and_outputs() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();

        let good = serde_json::json!({
            "session_id": "good", "model": "gpt-4", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z",
            "steps": [{"step_index": 0, "timestamp": "t", "action_type": "user_message", "content": "hi"},
                      {"step_index": 1, "timestamp": "t", "action_type": "assistant_message", "content": "hello"}],
            "outcome": {"type": "success"}, "tags": []
        });
        let bad = serde_json::json!({
            "session_id": "bad", "model": "gpt-4", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z",
            "steps": [{"step_index": 0, "timestamp": "t", "action_type": "user_message", "content": "hi"}],
            "outcome": {"type": "failure", "reason": "oops"}, "tags": []
        });
        std::fs::write(src.join("good.json"), serde_json::to_string(&good).unwrap()).unwrap();
        std::fs::write(src.join("bad.json"), serde_json::to_string(&bad).unwrap()).unwrap();

        let result = crate::run_eval_pipeline(
            src.to_str().unwrap(), out.to_str().unwrap(),
            false, true, true, None, true, None, None, None, false, None, None,
        )
        .expect("pipeline should succeed");

        assert!(result.contains("1 JSON files"));
        assert!(out.join("good.json").exists());
        assert!(!out.join("bad.json").exists());

        // Check auto-tagging was applied
        let output_raw = std::fs::read_to_string(out.join("good.json")).unwrap();
        let output_traj: serde_json::Value = serde_json::from_str(&output_raw).unwrap();
        let tags = output_traj["tags"].as_array().unwrap();
        assert!(!tags.is_empty()); // auto-tagger should have added tags
    }

    #[test]
    fn run_eval_pipeline_with_limit() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();

        for i in 0..10 {
            let t = serde_json::json!({
                "session_id": format!("s{i}"), "model": "m", "system_prompt_hash": "h",
                "started_at": "2026-01-01T00:00:00Z",
                "steps": [{"step_index": 0, "timestamp": "t", "action_type": "user_message", "content": "hi"}],
                "outcome": {"type": "success"}, "tags": []
            });
            std::fs::write(src.join(format!("s{i}.json")), serde_json::to_string(&t).unwrap()).unwrap();
        }

        let result = crate::run_eval_pipeline(
            src.to_str().unwrap(), out.to_str().unwrap(),
            false, false, false, None, false, None, None, None, false, Some(3), Some(42),
        )
        .expect("pipeline should succeed");

        assert!(result.contains("limited to 3"));
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3);
    }

    #[test]
    fn parses_eval_validate_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "validate", "src", "--remove",
        ])
        .expect("validate should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Validate { dir, remove, .. }) => {
                assert_eq!(dir, "src");
                assert!(remove);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_eval_validate_detects_valid_and_invalid() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        // Valid trajectory
        let t1 = serde_json::json!({
            "session_id": "s1", "model": "gpt-4", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z",
            "steps": [{"step_index": 0, "timestamp": "t", "action_type": "user_message", "content": "hi"}],
            "tags": []
        });
        std::fs::write(src.join("valid.json"), serde_json::to_string(&t1).unwrap()).unwrap();

        // Invalid JSON
        std::fs::write(src.join("broken.json"), "not json at all").unwrap();

        // Valid JSON but empty steps
        let t3 = serde_json::json!({
            "session_id": "s3", "model": "m", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z", "steps": [], "tags": []
        });
        std::fs::write(src.join("empty.json"), serde_json::to_string(&t3).unwrap()).unwrap();

        let result = crate::run_eval_validate(
            src.to_str().unwrap(), false, false,
        ).expect("validate should succeed");

        assert!(result.contains("1 valid"));
        assert!(result.contains("2 invalid"));
    }

    #[test]
    fn run_eval_validate_remove_deletes_invalid() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        std::fs::write(src.join("bad.json"), "invalid").unwrap();
        let t = serde_json::json!({
            "session_id": "ok", "model": "m", "system_prompt_hash": "h",
            "started_at": "t",
            "steps": [{"step_index": 0, "timestamp": "t", "action_type": "user_message", "content": "hi"}],
            "tags": []
        });
        std::fs::write(src.join("good.json"), serde_json::to_string(&t).unwrap()).unwrap();

        let result = crate::run_eval_validate(
            src.to_str().unwrap(), false, true,
        ).expect("validate with remove should succeed");

        assert!(result.contains("removed 1 invalid"));
        assert!(!src.join("bad.json").exists());
        assert!(src.join("good.json").exists());
    }

    #[test]
    fn parses_eval_sample_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "sample", "src", "--output", "out",
            "--count", "100", "--seed", "42",
        ])
        .expect("sample should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Sample {
                dir, output, count, seed, ..
            }) => {
                assert_eq!(dir, "src");
                assert_eq!(output, "out");
                assert_eq!(count, 100);
                assert_eq!(seed, Some(42));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_eval_sample_selects_subset() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();

        for i in 0..10 {
            let t = serde_json::json!({
                "session_id": format!("s{i}"), "model": "m", "system_prompt_hash": "h",
                "started_at": "2026-01-01T00:00:00Z", "steps": [], "tags": []
            });
            std::fs::write(
                src.join(format!("s{i}.json")),
                serde_json::to_string(&t).unwrap(),
            ).unwrap();
        }

        let result = crate::run_eval_sample(
            src.to_str().unwrap(), out.to_str().unwrap(), 3, Some(42), false,
        ).expect("sample should succeed");

        assert!(result.contains("sampled 3/10"));
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3);
    }

    #[test]
    fn run_eval_sample_caps_at_available() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();

        let t = serde_json::json!({
            "session_id": "s1", "model": "m", "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z", "steps": [], "tags": []
        });
        std::fs::write(src.join("s1.json"), serde_json::to_string(&t).unwrap()).unwrap();

        let result = crate::run_eval_sample(
            src.to_str().unwrap(), out.to_str().unwrap(), 100, Some(1), false,
        ).expect("sample should succeed");

        assert!(result.contains("sampled 1/1"));
    }

    #[test]
    fn run_eval_convert_trajectory_to_chatml() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("trajectory.json");
        let output = dir.path().join("out.jsonl");
        let trajectory = serde_json::json!({
            "session_id": "convert-chatml",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-chatml",
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [
                {"step_index": 0, "timestamp": "2026-03-08T12:00:00Z", "action_type": "user_message", "content": "hello"},
                {"step_index": 1, "timestamp": "2026-03-08T12:00:01Z", "action_type": "assistant_message", "content": "hi"}
            ],
            "outcome": { "type": "success" },
            "tags": ["dataset"]
        });
        std::fs::write(&input, serde_json::to_string_pretty(&trajectory).unwrap()).unwrap();

        crate::run_eval_convert(
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "chatml",
        )
        .expect("conversion should succeed");

        let raw = std::fs::read_to_string(output).expect("read output");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json line");
        assert_eq!(parsed["session_id"], "convert-chatml");
        assert!(parsed["chatml"].as_str().unwrap().contains("<|im_start|>user"));
    }

    #[test]
    fn run_eval_convert_trajectory_to_sharegpt() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("trajectory.json");
        let output = dir.path().join("out.jsonl");
        let trajectory = serde_json::json!({
            "session_id": "convert-sharegpt",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-sharegpt",
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [
                {"step_index": 0, "timestamp": "2026-03-08T12:00:00Z", "action_type": "user_message", "content": "hello"},
                {"step_index": 1, "timestamp": "2026-03-08T12:00:01Z", "action_type": "assistant_message", "content": "hi"}
            ],
            "outcome": { "type": "success" },
            "tags": ["dataset"]
        });
        std::fs::write(&input, serde_json::to_string_pretty(&trajectory).unwrap()).unwrap();

        crate::run_eval_convert(
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "sharegpt",
        )
        .expect("conversion should succeed");

        let raw = std::fs::read_to_string(output).expect("read output");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json line");
        assert_eq!(parsed["session_id"], "convert-sharegpt");
        assert_eq!(parsed["sharegpt"][0]["from"], "human");
    }

    #[test]
    fn run_eval_convert_chatml_to_json() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("out.json");
        let line = serde_json::json!({
            "session_id": "chatml-session",
            "model": "gpt-4.1-mini",
            "tags": ["dataset"],
            "outcome": { "type": "success" },
            "chatml": "<|im_start|>system\nYou are Eve.<|im_end|>\n<|im_start|>user\nhello<|im_end|>\n<|im_start|>assistant\nhi<|im_end|>\n"
        });
        std::fs::write(&input, format!("{}\n", serde_json::to_string(&line).unwrap())).unwrap();

        crate::run_eval_convert(
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "json",
        )
        .expect("conversion should succeed");

        let raw = std::fs::read_to_string(output).expect("read output");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(parsed["session_id"], "chatml-session");
        assert_eq!(parsed["steps"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn run_eval_convert_sharegpt_to_json() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("out.json");
        let line = serde_json::json!({
            "session_id": "sharegpt-session",
            "model": "gpt-4.1-mini",
            "tags": ["dataset"],
            "outcome": { "type": "success" },
            "sharegpt": [
                { "from": "human", "value": "hello" },
                { "from": "gpt", "value": "hi" }
            ]
        });
        std::fs::write(&input, format!("{}\n", serde_json::to_string(&line).unwrap())).unwrap();

        crate::run_eval_convert(
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "json",
        )
        .expect("conversion should succeed");

        let raw = std::fs::read_to_string(output).expect("read output");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(parsed["session_id"], "sharegpt-session");
        assert_eq!(parsed["steps"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["steps"][0]["action_type"], "user_message");
        assert_eq!(parsed["steps"][1]["action_type"], "assistant_message");
    }

    #[test]
    fn compute_eval_stats_reports_dataset_metrics() {
        let dir = tempdir().expect("tempdir");
        let write = |name: &str, value: serde_json::Value| {
            let path = dir.path().join(name);
            std::fs::write(path, serde_json::to_string_pretty(&value).unwrap())
                .expect("write trajectory");
        };

        write("a.json", serde_json::json!({
            "session_id": "a",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-a",
            "started_at": "2026-03-08T10:00:00Z",
            "completed_at": "2026-03-08T10:01:00Z",
            "steps": [
                {"step_index": 0, "timestamp": "2026-03-08T10:00:00Z", "action_type": "user_message", "content": "u1"},
                {"step_index": 1, "timestamp": "2026-03-08T10:00:01Z", "action_type": "assistant_message", "content": "a1"},
                {"step_index": 2, "timestamp": "2026-03-08T10:00:02Z", "action_type": "tool_call", "content": "tool_call: shell", "tool_name": "shell", "tool_arguments": "{\"cmd\":\"pwd\"}"},
                {"step_index": 3, "timestamp": "2026-03-08T10:00:03Z", "action_type": "tool_result", "content": "tool_result: shell", "tool_name": "shell", "tool_result": "/tmp"}
            ],
            "outcome": { "type": "success" },
            "tags": ["offline_eval", "baseline"]
        }));

        write("b.json", serde_json::json!({
            "session_id": "b",
            "model": "gpt-4.1-mini",
            "system_prompt_hash": "hash-b",
            "started_at": "2026-03-08T11:00:00Z",
            "completed_at": "2026-03-08T11:01:00Z",
            "steps": [
                {"step_index": 0, "timestamp": "2026-03-08T11:00:00Z", "action_type": "user_message", "content": "u2"},
                {"step_index": 1, "timestamp": "2026-03-08T11:00:01Z", "action_type": "assistant_message", "content": "a2"}
            ],
            "outcome": { "type": "failure", "reason": "tool broke" },
            "tags": ["offline_eval"]
        }));

        write("c.json", serde_json::json!({
            "session_id": "c",
            "model": "claude-sonnet-4-6",
            "system_prompt_hash": "hash-c",
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [
                {"step_index": 0, "timestamp": "2026-03-08T12:00:00Z", "action_type": "user_message", "content": "u3"},
                {"step_index": 1, "timestamp": "2026-03-08T12:00:01Z", "action_type": "assistant_message", "content": "a3"},
                {"step_index": 2, "timestamp": "2026-03-08T12:00:02Z", "action_type": "assistant_message", "content": "a4"}
            ],
            "outcome": { "type": "abandoned" },
            "tags": ["other"]
        }));

        let stats = crate::compute_eval_stats(
            dir.path().to_str().unwrap(),
            false,
            None,
            None,
            None,
            false,
        )
        .expect("stats should compute");

        assert_eq!(stats.total_trajectories, 3);
        assert_eq!(stats.total_turns, 9);
        assert!((stats.average_turns_per_trajectory - 3.0).abs() < f64::EPSILON);
        assert_eq!(stats.min_turns, 2);
        assert_eq!(stats.max_turns, 4);
        assert_eq!(stats.p50_turns, 3);
        assert_eq!(stats.p90_turns, 4);
        assert_eq!(stats.p99_turns, 4);
        assert!((stats.average_tool_calls_per_trajectory - (1.0 / 3.0)).abs() < 0.0001);
        assert_eq!(stats.model_distribution.len(), 2);
        assert!(stats
            .tag_distribution
            .contains(&("offline_eval".to_owned(), 2)));
        assert!(stats
            .outcome_distribution
            .contains(&("success".to_owned(), 1)));
        assert!(stats
            .outcome_distribution
            .contains(&("failure".to_owned(), 1)));
        assert!(stats
            .outcome_distribution
            .contains(&("abandoned".to_owned(), 1)));
        assert_eq!(stats.tool_usage[0].name, "shell");
        assert_eq!(stats.tool_usage[0].call_count, 1);
    }

    #[test]
    fn summarize_replay_reports_recursive_walks_nested_directories() {
        let dir = tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).expect("nested dir");

        let root_file = dir.path().join("root.json");
        let nested_file = nested.join("child.json");

        let trajectory = |session_id: &str| serde_json::json!({
            "session_id": session_id,
            "model": "gpt-4.1-mini",
            "system_prompt_hash": format!("hash-{session_id}"),
            "started_at": "2026-03-08T12:00:00Z",
            "completed_at": "2026-03-08T12:01:00Z",
            "steps": [{
                "step_index": 0,
                "timestamp": "2026-03-08T12:00:00Z",
                "action_type": "user_message",
                "content": "hello"
            }],
            "outcome": { "type": "success" },
            "tags": []
        });

        std::fs::write(&root_file, serde_json::to_string_pretty(&trajectory("root")).unwrap())
            .expect("write root");
        std::fs::write(
            &nested_file,
            serde_json::to_string_pretty(&trajectory("nested")).unwrap(),
        )
        .expect("write nested");

        let non_recursive = crate::summarize_replay_reports(
            dir.path().to_str().unwrap(),
            false,
            None,
            None,
            None,
            false,
            false,
            None,
        )
        .expect("non-recursive summary");
        let recursive = crate::summarize_replay_reports(
            dir.path().to_str().unwrap(),
            true,
            None,
            None,
            None,
            false,
            false,
            None,
        )
        .expect("recursive summary");

        assert_eq!(non_recursive.files_processed, 1);
        assert_eq!(recursive.files_processed, 2);
        assert!(recursive.recursive);
    }

    #[test]
    fn summarize_replay_reports_filters_by_model_and_tag() {
        let dir = tempdir().expect("tempdir");
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        let third = dir.path().join("third.json");

        let write = |path: &std::path::Path, session_id: &str, model: &str, tags: &[&str]| {
            let trajectory = serde_json::json!({
                "session_id": session_id,
                "model": model,
                "system_prompt_hash": format!("hash-{session_id}"),
                "started_at": "2026-03-08T12:00:00Z",
                "completed_at": "2026-03-08T12:01:00Z",
                "steps": [{
                    "step_index": 0,
                    "timestamp": "2026-03-08T12:00:00Z",
                    "action_type": "user_message",
                    "content": "hello"
                }],
                "outcome": { "type": "success" },
                "tags": tags
            });
            std::fs::write(path, serde_json::to_string_pretty(&trajectory).unwrap())
                .expect("write trajectory");
        };

        write(&first, "s-1", "gpt-4.1-mini", &["offline_eval", "smoke"]);
        write(&second, "s-2", "gpt-4.1-mini", &["other"]);
        write(&third, "s-3", "claude-sonnet-4-6", &["offline_eval"]);

        let summary = crate::summarize_replay_reports(
            dir.path().to_str().unwrap(),
            false,
            Some("gpt-4.1-mini"),
            Some("offline_eval"),
            None,
            false,
            false,
            None,
        )
        .expect("filtered summary should build");

        assert_eq!(summary.files_processed, 1);
        assert_eq!(summary.total_events, 1);
        assert_eq!(summary.model_filter.as_deref(), Some("gpt-4.1-mini"));
        assert_eq!(summary.tag_filter.as_deref(), Some("offline_eval"));
        assert_eq!(summary.models, vec![("gpt-4.1-mini".to_owned(), 1)]);
        assert_eq!(summary.tags, vec![
            ("offline_eval".to_owned(), 1),
            ("smoke".to_owned(), 1),
        ]);
    }

    #[test]
    fn summarize_replay_reports_filters_failures_and_warnings() {
        let dir = tempdir().expect("tempdir");
        let success_clean = dir.path().join("success_clean.json");
        let failure_warn = dir.path().join("failure_warn.json");
        let failure_clean = dir.path().join("failure_clean.json");

        let write = |path: &std::path::Path,
                     session_id: &str,
                     outcome: serde_json::Value,
                     with_warning: bool| {
            let tool_step = if with_warning {
                vec![serde_json::json!({
                    "step_index": 0,
                    "timestamp": "2026-03-08T12:00:00Z",
                    "action_type": "tool_call",
                    "content": "tool_call... (truncated)",
                    "tool_name": "shell",
                    "tool_arguments": "{}"
                })]
            } else {
                vec![serde_json::json!({
                    "step_index": 0,
                    "timestamp": "2026-03-08T12:00:00Z",
                    "action_type": "user_message",
                    "content": "hello"
                })]
            };

            let trajectory = serde_json::json!({
                "session_id": session_id,
                "model": "gpt-4.1-mini",
                "system_prompt_hash": format!("hash-{session_id}"),
                "started_at": "2026-03-08T12:00:00Z",
                "completed_at": "2026-03-08T12:01:00Z",
                "steps": tool_step,
                "outcome": outcome,
                "tags": []
            });
            std::fs::write(path, serde_json::to_string_pretty(&trajectory).unwrap())
                .expect("write trajectory");
        };

        write(&success_clean, "s-1", serde_json::json!({ "type": "success" }), false);
        write(
            &failure_warn,
            "s-2",
            serde_json::json!({ "type": "failure", "reason": "tool broke" }),
            true,
        );
        write(
            &failure_clean,
            "s-3",
            serde_json::json!({ "type": "failure", "reason": "tool broke" }),
            false,
        );

        let summary = crate::summarize_replay_reports(
            dir.path().to_str().unwrap(),
            false,
            None,
            None,
            None,
            true,
            true,
            None,
        )
        .expect("filtered summary should build");

        assert_eq!(summary.files_processed, 1);
        assert!(summary.failures_only);
        assert!(summary.warnings_only);
        assert_eq!(summary.failure_count, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(
            summary.top_failure_reasons,
            vec![("tool broke".to_owned(), 1)]
        );
        assert_eq!(summary.top_warning_messages.len(), 1);
        assert!(summary.top_warning_messages[0]
            .0
            .contains("appears truncated"));
    }

    #[test]
    fn eval_comparison_json_includes_deltas_and_tags() {
        let comparison = crate::EvalComparison {
            left_path: "left.json".to_owned(),
            right_path: "right.json".to_owned(),
            left_session_id: "left-session".to_owned(),
            right_session_id: "right-session".to_owned(),
            left_model: "gpt-4.1-mini".to_owned(),
            right_model: "claude-sonnet-4-6".to_owned(),
            left_total_events: 4,
            right_total_events: 6,
            left_warning_count: 1,
            right_warning_count: 3,
            event_delta: crate::ReplayEventDelta {
                user: 0,
                assistant: 1,
                tool_call: 1,
                tool_result: 1,
                system: -1,
            },
            tools: vec![crate::ToolUsageDelta {
                name: "shell".to_owned(),
                left_call_count: 0,
                right_call_count: 1,
                left_result_count: 0,
                right_result_count: 1,
            }],
            left_only_tags: vec!["baseline".to_owned()],
            right_only_tags: vec!["with_tools".to_owned()],
        };

        let json = crate::eval_comparison_to_json(&comparison);

        assert_eq!(json["left_path"], "left.json");
        assert_eq!(json["right_path"], "right.json");
        assert_eq!(json["event_delta"]["tool_call"], 1);
        assert_eq!(json["event_delta"]["system"], -1);
        assert_eq!(json["tools"][0]["name"], "shell");
        assert_eq!(json["tools"][0]["right_call_count"], 1);
        assert_eq!(json["left_only_tags"][0], "baseline");
        assert_eq!(json["right_only_tags"][0], "with_tools");
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

    // --- Pairing command tests ---

    #[test]
    fn parses_pairing_list() {
        let cli = Cli::try_parse_from(["genesis", "pairing", "list"])
            .expect("pairing list should parse");
        match cli.command {
            Command::Pairing(PairingCommand::List { platform }) => {
                assert!(platform.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pairing_list_with_platform() {
        let cli = Cli::try_parse_from([
            "genesis", "pairing", "list", "--platform", "telegram",
        ])
        .expect("pairing list --platform should parse");
        match cli.command {
            Command::Pairing(PairingCommand::List { platform }) => {
                assert_eq!(platform.as_deref(), Some("telegram"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pairing_pending() {
        let cli = Cli::try_parse_from(["genesis", "pairing", "pending"])
            .expect("pairing pending should parse");
        match cli.command {
            Command::Pairing(PairingCommand::Pending { platform }) => {
                assert!(platform.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pairing_approve() {
        let cli = Cli::try_parse_from([
            "genesis", "pairing", "approve", "telegram", "ABC12345",
        ])
        .expect("pairing approve should parse");
        match cli.command {
            Command::Pairing(PairingCommand::Approve { platform, code }) => {
                assert_eq!(platform, "telegram");
                assert_eq!(code, "ABC12345");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pairing_revoke() {
        let cli = Cli::try_parse_from([
            "genesis", "pairing", "revoke", "discord", "user-42",
        ])
        .expect("pairing revoke should parse");
        match cli.command {
            Command::Pairing(PairingCommand::Revoke { platform, user_id }) => {
                assert_eq!(platform, "discord");
                assert_eq!(user_id, "user-42");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pairing_clear_pending() {
        let cli = Cli::try_parse_from(["genesis", "pairing", "clear-pending"])
            .expect("pairing clear-pending should parse");
        match cli.command {
            Command::Pairing(PairingCommand::ClearPending { platform }) => {
                assert!(platform.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pairing_clear_pending_with_platform() {
        let cli = Cli::try_parse_from([
            "genesis", "pairing", "clear-pending", "--platform", "slack",
        ])
        .expect("pairing clear-pending --platform should parse");
        match cli.command {
            Command::Pairing(PairingCommand::ClearPending { platform }) => {
                assert_eq!(platform.as_deref(), Some("slack"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_uninstall_command_defaults() {
        let cli = Cli::try_parse_from(["genesis", "uninstall"])
            .expect("uninstall command should parse");
        match cli.command {
            Command::Uninstall { remove_data, remove_config, force } => {
                assert!(!remove_data);
                assert!(!remove_config);
                assert!(!force);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_uninstall_command_with_all_flags() {
        let cli = Cli::try_parse_from([
            "genesis", "uninstall", "--remove-data", "--remove-config", "--force",
        ])
        .expect("uninstall command with flags should parse");
        match cli.command {
            Command::Uninstall { remove_data, remove_config, force } => {
                assert!(remove_data);
                assert!(remove_config);
                assert!(force);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_uninstall_command_with_remove_data_only() {
        let cli = Cli::try_parse_from(["genesis", "uninstall", "--remove-data"])
            .expect("uninstall --remove-data should parse");
        match cli.command {
            Command::Uninstall { remove_data, remove_config, force } => {
                assert!(remove_data);
                assert!(!remove_config);
                assert!(!force);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_uninstall_command_with_remove_config_only() {
        let cli = Cli::try_parse_from(["genesis", "uninstall", "--remove-config"])
            .expect("uninstall --remove-config should parse");
        match cli.command {
            Command::Uninstall { remove_data, remove_config, force } => {
                assert!(!remove_data);
                assert!(remove_config);
                assert!(!force);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
