mod chat;
mod clipboard;
mod commands;
mod format;
mod slash;

use std::fs;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{CommandFactory, Parser, Subcommand};
use genesis_config::{load, LoadedConfig};
use genesis_core::agent_loop::AgentError;
use genesis_core::execution::SessionExecutionError;
use genesis_core::replay::load_and_report;
use genesis_core::run_doctor;
use genesis_provider::ProviderError;
use genesis_storage::{
    bootstrap, MemoryStore, ScheduleStore, SessionStore, SkillStore, StorageError, SubagentStore,
};
use genesis_ui::terminal::ColorMode;
use genesis_ui::UiContext;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(name = "genesis", version, about = "Rust-native Genesis bootstrap CLI")]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true, help = "Render machine-readable JSON output")]
    pub json: bool,
    #[arg(
        long,
        global = true,
        help = "Disable Lua plugin loading for this process"
    )]
    pub no_plugins: bool,
    #[arg(
        long,
        global = true,
        help = "Log plugin execution timing and lifecycle events"
    )]
    pub plugin_verbose: bool,
    /// Color output mode: auto, always, never.
    #[arg(long, global = true, default_value = "auto")]
    pub color: String,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Start an interactive Eve chat session")]
    Chat {
        #[arg(long, help = "Override the generated session id")]
        session_id: Option<String>,
        #[arg(
            long,
            help = "Resume an existing session instead of creating a new one"
        )]
        resume: Option<String>,
        #[arg(
            short,
            long,
            help = "Send an initial prompt before entering interactive mode"
        )]
        prompt: Option<String>,
        #[arg(long, help = "Override the system prompt / agent identity")]
        system: Option<String>,
        #[arg(long, help = "Resume the most recent session")]
        last: bool,
        #[arg(long, help = "Run in an isolated git worktree (requires git repo)")]
        worktree: bool,
        #[arg(long, help = "Attach the clipboard image to the first message")]
        clipboard: bool,
        #[arg(long, help = "Disable the TUI, use legacy readline mode")]
        no_tui: bool,
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
        #[arg(long, default_value = "127.0.0.1", help = "Host to bind")]
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
        #[arg(
            long,
            default_value = "30",
            help = "Number of days to analyze (default: 30)"
        )]
        days: u32,
    },
    #[command(
        alias = "setup",
        about = "Initialize Genesis — interactive setup wizard (or pass flags for non-interactive)"
    )]
    Init {
        #[arg(
            long,
            help = "LLM provider backend (e.g. openai, openrouter, anthropic)"
        )]
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
        #[arg(
            long,
            help = "Stream output as it arrives (default: wait for full response)"
        )]
        stream: bool,
        #[arg(
            short = 'i',
            long = "image",
            help = "Attach an image file or URL to the prompt (can be repeated)"
        )]
        images: Vec<String>,
    },
    #[command(about = "Show status dashboard of all Genesis components")]
    Status,
    #[command(about = "Generate agent training trajectories from a JSONL prompt file")]
    Batch {
        #[arg(
            long,
            help = "Input JSONL file where each line is {\"prompt\": ..., \"tags\": [...]}"
        )]
        input: String,
        #[arg(long, help = "Output directory for saved trajectory files")]
        output: String,
        #[arg(long, help = "Override the model used for generation")]
        model: Option<String>,
        #[arg(long, help = "Override max turns per prompt")]
        max_turns: Option<usize>,
        #[arg(long, help = "Maximum number of prompts to run concurrently")]
        concurrency: Option<usize>,
        #[arg(
            long,
            help = "Toolset distribution name (e.g. full, development, research, safe, minimal, creative, ops, home-assistant, coding-agent, random)"
        )]
        toolset: Option<String>,
        #[arg(
            long,
            help = "Discard generated trajectories whose quality score is below this threshold (0.0-1.0)"
        )]
        quality_filter: Option<f64>,
        #[arg(
            long,
            help = "Automatically tag generated trajectories based on content analysis"
        )]
        auto_tag: bool,
    },
    #[command(about = "Compress a trajectory JSON file for training/export")]
    Compress {
        #[arg(long, help = "Input trajectory JSON file")]
        input: String,
        #[arg(
            long,
            help = "Optional output file path; writes to stdout when omitted"
        )]
        output: Option<String>,
        #[arg(long, help = "Compression level: light, medium, or heavy")]
        level: Option<String>,
        #[arg(long, help = "Output format: json, sharegpt, or chatml")]
        format: Option<String>,
        #[arg(
            long,
            help = "Use the training compressor that protects first/last turns and summarizes the middle"
        )]
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
    #[command(
        subcommand,
        about = "Manage DM pairing authorization for messaging platforms"
    )]
    Pairing(PairingCommand),
    #[command(
        subcommand,
        about = "List and inspect toolset distributions for batch training"
    )]
    Toolset(ToolsetCommand),
    #[command(subcommand, about = "Inspect and manage Lua plugins")]
    Plugins(PluginsCommand),
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
        #[arg(
            long,
            help = "Also remove the data directory (database, trajectories, etc.)"
        )]
        remove_data: bool,
        #[arg(
            long,
            help = "Also remove the config directory (config.yaml, auth, etc.)"
        )]
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
pub enum PluginsCommand {
    #[command(about = "List discovered Lua plugins")]
    List,
    #[command(about = "Show details for a specific Lua plugin")]
    Info {
        #[arg(help = "Plugin name")]
        name: String,
    },
    #[command(about = "Disable a Lua plugin in config")]
    Disable {
        #[arg(help = "Plugin name")]
        name: String,
    },
    #[command(about = "Enable a Lua plugin in config")]
    Enable {
        #[arg(help = "Plugin name")]
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
        #[arg(
            long,
            help = "Recursively scan nested directories for trajectory JSON files"
        )]
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
        #[arg(
            long,
            help = "Only include trajectories with at least this many replay warnings"
        )]
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
        #[arg(
            long,
            help = "Recursively scan nested directories for trajectory JSON files"
        )]
        recursive: bool,
    },
    #[command(about = "Export a directory of trajectories as ShareGPT JSONL")]
    ExportSharegpt {
        #[arg(help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(
            long,
            help = "Recursively scan nested directories for trajectory JSON files"
        )]
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
        #[arg(
            long,
            help = "Recursively scan nested directories for trajectory JSON files"
        )]
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
        #[arg(
            long,
            help = "Minimum quality score to pass (0.0-1.0, default: show all)"
        )]
        min_score: Option<f64>,
        #[arg(
            long,
            help = "Sort by score ascending (worst first) instead of descending"
        )]
        worst_first: bool,
    },
    #[command(about = "Automatically tag trajectory files using genesis_core::tagger::auto_tag")]
    AutoTag {
        #[arg(long, help = "Directory containing trajectory JSON files")]
        dir: String,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
        #[arg(
            long,
            help = "Only print the tags that would be added without writing files"
        )]
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
        #[arg(
            long,
            help = "Delete duplicate files, keeping the first file in each group"
        )]
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
        #[arg(
            long,
            default_value = "0.8",
            help = "Fraction of data for training (0.0-1.0)"
        )]
        ratio: f64,
        #[arg(long, help = "Random seed for reproducibility")]
        seed: Option<u64>,
        #[arg(long, help = "Recursively scan nested directories")]
        recursive: bool,
    },
    #[command(
        about = "Build or show a dataset manifest (dataset.json) for a trajectory directory"
    )]
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
        #[arg(
            long,
            default_value = "50",
            help = "Maximum number of memories to show"
        )]
        limit: usize,
    },
    #[command(about = "Search memories")]
    Search {
        /// Search query
        query: String,
        #[arg(long, default_value = "10", help = "Maximum results to return")]
        limit: usize,
        #[arg(
            long,
            default_value = "keyword",
            value_parser = ["keyword", "vector", "graph", "hybrid"],
            help = "Search mode"
        )]
        mode: String,
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
    #[command(
        about = "Set a config value (dot-notation: provider.model, runtime.max_turns, etc.)"
    )]
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
        #[arg(
            long,
            help = "Filter by provider backend (e.g. openai, anthropic, google)"
        )]
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
    /// Browse available models from OpenRouter with search and filters.
    Browse {
        /// Filter by search query (matches ID and name).
        #[arg(short, long)]
        query: Option<String>,
        /// Filter to models supporting tools.
        #[arg(long)]
        tools: bool,
        /// Filter to models supporting vision.
        #[arg(long)]
        vision: bool,
        /// Filter to models supporting reasoning.
        #[arg(long)]
        reasoning: bool,
        /// Sort by: newest, cheapest, context (default: newest).
        #[arg(short, long, default_value = "newest")]
        sort: String,
        /// Maximum number of models to display.
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
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
        #[arg(
            long,
            help = "Import format: 'sharegpt' or 'jsonl' (auto-detected from extension if omitted)"
        )]
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
    #[command(
        subcommand,
        about = "Browse, install, and manage skills from registries"
    )]
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
        #[arg(
            long,
            default_value = "skills",
            help = "Path within the repo where skills live"
        )]
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
        #[arg(
            long,
            help = "IANA timezone name (e.g. America/New_York). Defaults to UTC"
        )]
        timezone: Option<String>,
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
    #[error(transparent)]
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
    #[error("TUI error: {0}")]
    Tui(String),
    #[error("{0}")]
    Other(String),
}

pub async fn run(cli: Cli) -> Result<String, CliError> {
    let runtime_overrides = runtime_overrides_from_cli(&cli);
    // --json implies --color=never (machine-readable output must be plain).
    let color_mode = if cli.json {
        ColorMode::Never
    } else {
        match cli.color.as_str() {
            "always" => ColorMode::Always,
            "never" => ColorMode::Never,
            _ => ColorMode::Auto,
        }
    };
    let ui = UiContext::new(color_mode);

    match cli.command {
        Command::Chat {
            session_id,
            resume,
            prompt,
            system,
            last,
            worktree,
            clipboard,
            no_tui,
        } => {
            if no_tui || !std::io::stdout().is_terminal() {
                // Legacy rustyline path
                chat::run_chat(
                    cli.config,
                    session_id,
                    resume,
                    prompt,
                    system,
                    last,
                    worktree,
                    clipboard,
                    runtime_overrides,
                    &ui,
                )
                .await
            } else {
                // Ratatui TUI path
                chat::run_chat_tui(
                    cli.config,
                    session_id,
                    resume,
                    prompt,
                    system,
                    last,
                    worktree,
                    runtime_overrides,
                )
                .await
            }
        }
        Command::Doctor {
            bootstrap_storage,
            verify,
        } => {
            let report = run_doctor(cli.config.as_deref(), bootstrap_storage)?;
            let mut output = if cli.json {
                serde_json::to_string_pretty(&report)?
            } else {
                format::format_doctor_report(&report, &ui)
            };

            // Optional API connectivity verification
            if verify {
                output.push_str("\n\nAPI connectivity:\n");
                let loaded = load(cli.config.as_deref())?;
                match commands::misc::verify_api_connectivity(&loaded).await {
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
                            loaded.config.provider.backend, loaded.config.provider.model, e
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
                Err(CliError::Other(format!(
                    "{editor} exited with status {status}"
                )))
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
            let valid_backends = [
                "openai",
                "anthropic",
                "google",
                "openrouter",
                "custom",
                "openai-codex",
                "gemini",
                "vllm",
                "ollama",
            ];
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
            let api_key_env =
                loaded.config.provider.api_key_env.as_deref().unwrap_or(
                    match loaded.config.provider.backend.as_str() {
                        "anthropic" => "ANTHROPIC_API_KEY",
                        "google" | "gemini" => "GOOGLE_API_KEY",
                        "openrouter" => "OPENROUTER_API_KEY",
                        _ => "OPENAI_API_KEY",
                    },
                );
            if std::env::var(api_key_env).is_err() {
                warnings.push(format!("API key env var '{}' is not set.", api_key_env));
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
                        "MCP server '{}' has no command or URL configured.",
                        name
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
                Ok(format::format_bootstrap_report(&report, &ui))
            }
        }
        Command::Eval(eval_command) => match eval_command {
            EvalCommand::Report { file } => {
                let report = load_and_report(&file).map_err(|e| CliError::Replay(e.to_string()))?;
                if cli.json {
                    Ok(serde_json::to_string_pretty(&report)?)
                } else {
                    Ok(commands::eval::format_replay_report(&report))
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
                let summary = commands::eval::summarize_replay_reports(
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
                    Ok(serde_json::to_string_pretty(
                        &commands::eval::eval_summary_to_json(&summary),
                    )?)
                } else {
                    Ok(commands::eval::format_eval_summary(&summary))
                }
            }
            EvalCommand::Compare { left, right } => {
                let comparison = commands::eval::compare_replay_reports(&left, &right)?;
                if cli.json {
                    Ok(serde_json::to_string_pretty(
                        &commands::eval::eval_comparison_to_json(&comparison),
                    )?)
                } else {
                    Ok(commands::eval::format_eval_comparison(&comparison))
                }
            }
            EvalCommand::ExportChatml { dir, recursive } => {
                commands::eval::run_eval_export_chatml(&dir, recursive)
            }
            EvalCommand::ExportSharegpt { dir, recursive } => {
                commands::eval::run_eval_export_sharegpt(&dir, recursive)
            }
            EvalCommand::ImportChatml { file, output } => {
                commands::eval::run_eval_import_chatml(&file, &output)
            }
            EvalCommand::ImportSharegpt { file, output } => {
                commands::eval::run_eval_import_sharegpt(&file, &output)
            }
            EvalCommand::Merge {
                sources,
                output,
                dedup,
            } => commands::eval::run_eval_merge(&sources, &output, dedup),
            EvalCommand::Convert {
                input,
                output,
                format,
            } => commands::eval::run_eval_convert(&input, &output, &format),
            EvalCommand::Stats {
                dir,
                recursive,
                model,
                tag,
                tool,
                failures_only,
            } => {
                let stats = commands::eval::compute_eval_stats(
                    &dir,
                    recursive,
                    model.as_deref(),
                    tag.as_deref(),
                    tool.as_deref(),
                    failures_only,
                )?;
                if cli.json {
                    Ok(serde_json::to_string_pretty(
                        &commands::eval::eval_stats_to_json(&stats),
                    )?)
                } else {
                    Ok(commands::eval::format_eval_stats(&stats))
                }
            }
            EvalCommand::Quality {
                dir,
                recursive,
                min_score,
                worst_first,
            } => {
                commands::eval::run_eval_quality(&dir, recursive, min_score, worst_first, cli.json)
            }
            EvalCommand::AutoTag {
                dir,
                recursive,
                dry_run,
            } => commands::eval::run_eval_auto_tag(&dir, recursive, dry_run, cli.json),
            EvalCommand::TagStats { dir, recursive } => {
                commands::eval::run_eval_tag_stats(&dir, recursive, cli.json)
            }
            EvalCommand::Deduplicate {
                dir,
                recursive,
                remove,
            } => commands::eval::run_eval_deduplicate(&dir, recursive, remove, cli.json),
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
            } => commands::eval::run_eval_filter(
                &dir,
                &output,
                recursive,
                model.as_deref(),
                tag.as_deref(),
                min_quality,
                max_quality,
                success_only,
                failure_only,
                min_steps,
                max_steps,
                tool.as_deref(),
            ),
            EvalCommand::Split {
                dir,
                train,
                test,
                ratio,
                seed,
                recursive,
            } => commands::eval::run_eval_split(&dir, &train, &test, ratio, seed, recursive),
            EvalCommand::Manifest {
                dir,
                name,
                description,
                save,
                recursive,
            } => commands::eval::run_eval_manifest(
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
            } => commands::eval::run_eval_pipeline(
                &dir,
                &output,
                recursive,
                validate,
                auto_tag,
                min_quality,
                success_only,
                tag.as_deref(),
                model.as_deref(),
                format.as_deref(),
                manifest,
                limit,
                seed,
            ),
            EvalCommand::Validate {
                dir,
                recursive,
                remove,
            } => commands::eval::run_eval_validate(&dir, recursive, remove),
            EvalCommand::Sample {
                dir,
                output,
                count,
                seed,
                recursive,
            } => commands::eval::run_eval_sample(&dir, &output, count, seed, recursive),
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
                        Ok(format::format_session_list(&sessions, &ui))
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
                        Ok(format::format_session_messages(
                            &session.id,
                            display_messages,
                        ))
                    }
                }
                SessionsCommand::Export { id, format: fmt } => {
                    let _session = store
                        .get_session(&id)?
                        .ok_or_else(|| CliError::SessionNotFound(id.clone()))?;
                    let messages = store.load_messages(&id)?;

                    match fmt.as_str() {
                        "json" => Ok(serde_json::to_string_pretty(&messages)?),
                        "md" | "markdown" => Ok(format::export_session_markdown(&id, &messages)),
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
                        Ok(format::format_session_list(&results, &ui))
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
                        Ok(format::format_usage_stats(&stats))
                    }
                }
                SessionsCommand::Purge { older_than } => {
                    let deleted = store.purge_older_than(older_than)?;
                    Ok(format!(
                        "Purged {deleted} session(s) older than {older_than} days"
                    ))
                }
                SessionsCommand::Rename { id, title } => {
                    if store.set_title(&id, &title)? {
                        Ok(format!("Renamed session {id} to \"{title}\""))
                    } else {
                        Err(CliError::SessionNotFound(id))
                    }
                }
                SessionsCommand::Import {
                    file,
                    format: fmt,
                    title,
                } => format::run_session_import(&store, &file, fmt.as_deref(), title.as_deref()),
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
                        Ok(format::format_skill_list(&skills))
                    }
                }
                SkillsCommand::Show { name } => {
                    let skill = store
                        .get(&name)?
                        .ok_or_else(|| CliError::SkillNotFound(name.clone()))?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&skill)?)
                    } else {
                        Ok(format::format_skill(&skill))
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
                    let contents = std::fs::read_to_string(&file).map_err(|e| {
                        CliError::Other(format!("failed to read {}: {e}", file.display()))
                    })?;
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

                    Ok(format!(
                        "imported {imported} skill(s) from {}",
                        file.display()
                    ))
                }
                SkillsCommand::Scan { dir } => format::run_skills_scan(&dir, cli.json),
                SkillsCommand::Search { query, dir } => {
                    format::run_skills_search(&store, &query, dir.as_deref(), cli.json)
                }
                SkillsCommand::InstallLocal { path } => {
                    format::run_skills_install_local(&store, &path)
                }
                SkillsCommand::Hub(hub_command) => {
                    format::run_skills_hub(hub_command, &loaded, cli.json)
                }
            }
        }
        Command::Context(context_command) => commands::misc::run_context(context_command),
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
                        Ok(format::format_subagent_list(&subs))
                    }
                }
                SubagentsCommand::Show { id } => {
                    let sub = store
                        .get(&id)?
                        .ok_or_else(|| CliError::SubagentNotFound(id.clone()))?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&sub)?)
                    } else {
                        Ok(format::format_subagent(&sub))
                    }
                }
            }
        }
        Command::Tools => commands::misc::run_tools(cli.config, cli.json),
        Command::Info => commands::misc::run_info(cli.config, cli.json),
        Command::Schedule(schedule_command) => {
            let loaded = load(cli.config.as_deref())?;
            bootstrap(&loaded.config.storage.database_path)?;
            let store = ScheduleStore::new(&loaded.config.storage.database_path);

            match schedule_command {
                ScheduleCommand::Create {
                    cron,
                    destination,
                    prompt,
                    timezone,
                } => {
                    // Validate cron expression at creation time
                    genesis_core::scheduler::validate_cron(&cron)
                        .map_err(|e| CliError::Other(format!("invalid cron expression: {e}")))?;

                    // Validate timezone if provided
                    if let Some(ref tz) = timezone {
                        genesis_core::scheduler::resolve_timezone(Some(tz))
                            .map_err(CliError::Other)?;
                    }

                    let schedule = store.create_with_timezone(
                        &commands::serve::default_schedule_id(),
                        &cron,
                        &destination,
                        &prompt,
                        timezone.as_deref(),
                    )?;

                    if cli.json {
                        Ok(serde_json::to_string_pretty(&schedule)?)
                    } else {
                        Ok(format::format_created_schedule(&schedule))
                    }
                }
                ScheduleCommand::List => {
                    let schedules = store.list_all()?;
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&schedules)?)
                    } else {
                        Ok(format::format_schedule_list(&schedules))
                    }
                }
                ScheduleCommand::Run => {
                    commands::serve::run_schedule_daemon(&loaded, runtime_overrides).await
                }
                ScheduleCommand::Delete { id } => {
                    if !store.delete(&id)? {
                        return Err(CliError::ScheduleNotFound(id));
                    }

                    Ok(format!("deleted schedule {id}"))
                }
            }
        }
        Command::Model(model_command) => {
            commands::misc::run_model(cli.config, model_command, cli.json).await
        }
        Command::Serve { host, port } => {
            commands::serve::run_serve(cli.config, &host, port, runtime_overrides).await
        }
        Command::Nudge => commands::serve::run_nudge(cli.config, runtime_overrides).await,
        Command::Insights { days } => {
            let loaded = load(cli.config.as_deref())?;
            bootstrap(&loaded.config.storage.database_path)?;
            let store = SessionStore::new(&loaded.config.storage.database_path);
            let insights = store.insights(days)?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&insights)?)
            } else {
                Ok(format::format_insights(
                    &insights,
                    &loaded.config.provider.model,
                ))
            }
        }
        Command::Init {
            backend,
            model,
            base_url,
            api_key_env,
        } => commands::init::run_init(cli.config, backend, model, base_url, api_key_env).await,
        Command::Bootstrap(BootstrapCommand::Config) => {
            let loaded = load(cli.config.as_deref())?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&loaded.config)?)
            } else {
                Ok(serde_yaml::to_string(&loaded.config)?)
            }
        }
        Command::Run {
            prompt,
            session_id,
            raw,
            system,
            stream,
            images,
        } => {
            chat::run_oneshot(
                cli.config,
                &prompt,
                session_id,
                raw,
                cli.json,
                system,
                stream,
                &images,
                runtime_overrides,
                &ui,
            )
            .await
        }
        Command::Status => {
            let loaded = load(cli.config.as_deref())?;
            if cli.json {
                Ok(serde_json::to_string_pretty(&format::build_status_json(
                    &loaded,
                ))?)
            } else {
                Ok(format::build_status_text(&loaded, &ui))
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
            commands::batch::run_batch(
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
        } => commands::misc::run_compress(input, output, level, format, training),
        Command::Update => commands::init::run_update().await,
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
                        Ok(format::format_memory_list(&memories))
                    }
                }
                MemoryCommand::Search { query, limit, mode } => {
                    let mode_name = mode.clone();
                    let mode =
                        genesis_core::embedding::SearchMode::from_str_opt(Some(mode.as_str()));
                    let memories = match mode {
                        genesis_core::embedding::SearchMode::Keyword => {
                            store.search(&query, limit)?
                        }
                        genesis_core::embedding::SearchMode::Graph => store
                            .graph_search(&query, limit)?
                            .into_iter()
                            .map(|item| item.memory)
                            .collect(),
                        genesis_core::embedding::SearchMode::Vector
                        | genesis_core::embedding::SearchMode::Hybrid => {
                            let config = loaded.config.embedding.as_ref().ok_or_else(|| {
                                CliError::Other(
                                    format!(
                                        "memory search mode '{mode_name}' requires an [embedding] configuration"
                                    ),
                                )
                            })?;
                            let provider =
                                genesis_core::embedding::EmbeddingProvider::from_config(config)
                                    .map_err(|error| {
                                        CliError::Other(format!(
                                            "embedding provider error: {error}"
                                        ))
                                    })?;
                            genesis_core::embedding::hybrid_search(
                                &query,
                                limit,
                                mode,
                                &store,
                                Some(&provider),
                            )
                            .await
                            .map_err(|error| {
                                CliError::Other(format!("memory search failed: {error}"))
                            })?
                            .into_iter()
                            .map(|item| item.memory)
                            .collect()
                        }
                    };
                    if cli.json {
                        Ok(serde_json::to_string_pretty(&memories)?)
                    } else if memories.is_empty() {
                        Ok(format!("no memories matching \"{query}\""))
                    } else {
                        Ok(format::format_memory_list(&memories))
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
        Command::Mcp(mcp_command) => {
            commands::misc::run_mcp(cli.config, mcp_command, cli.json).await
        }
        Command::Benchmark {
            runs,
            tool_provider,
        } => commands::misc::run_benchmark(cli.config, runs, tool_provider, cli.json).await,
        Command::Pairing(pairing_command) => {
            commands::misc::run_pairing(cli.config, pairing_command, cli.json).await
        }
        Command::Toolset(toolset_command) => commands::misc::run_toolset(toolset_command, cli.json),
        Command::Plugins(plugins_command) => {
            commands::misc::run_plugins(cli.config, plugins_command, cli.json)
        }
        Command::Personality(personality_command) => {
            commands::misc::run_personality(personality_command, cli.json)
        }
        Command::Workflow(WorkflowCommand::Validate { file }) => {
            let yaml = fs::read_to_string(&file)
                .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;
            let workflow = genesis_core::workflow::parse_workflow(&yaml)
                .map_err(|e| CliError::Other(format!("invalid workflow YAML: {e}")))?;
            let issues = genesis_core::workflow::validate_workflow(&workflow);
            if issues.is_empty() {
                Ok(format!(
                    "Workflow '{}' is valid ({} steps)",
                    workflow.name,
                    workflow.steps.len()
                ))
            } else {
                Err(CliError::Other(format!(
                    "Validation errors:\n{}",
                    issues.join("\n")
                )))
            }
        }
        Command::Workflow(WorkflowCommand::Run {
            file,
            input,
            session_id,
        }) => {
            let yaml = fs::read_to_string(&file)
                .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;
            let workflow = genesis_core::workflow::parse_workflow(&yaml)
                .map_err(|e| CliError::Other(format!("invalid workflow YAML: {e}")))?;

            let loaded = load(cli.config.as_deref())?;
            let session_id = session_id.unwrap_or_else(|| {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                format!("workflow-{}-{ts}", workflow.name)
            });

            let mut svc = genesis_core::execution::SessionExecutionService::new(&loaded);
            svc.set_plugin_runtime_overrides(runtime_overrides);
            let result = svc
                .run_workflow(&workflow, &input, &session_id)
                .await
                .map_err(|e| CliError::Other(format!("workflow failed: {e}")))?;

            if cli.json {
                Ok(serde_json::to_string_pretty(&result)?)
            } else {
                let mut output = format!(
                    "Workflow '{}' completed ({} steps)\n",
                    result.workflow_name,
                    result.steps_completed()
                );
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
        Command::Login => commands::init::run_login(cli.config).await,
        Command::Logout => commands::init::run_logout(),
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "genesis", &mut io::stdout());
            Ok(String::new())
        }
        Command::Uninstall {
            remove_data,
            remove_config,
            force,
        } => {
            commands::init::run_uninstall(cli.config.as_deref(), remove_data, remove_config, force)
        }
    }
}

fn runtime_overrides_from_cli(cli: &Cli) -> genesis_core::execution::PluginRuntimeOverrides {
    genesis_core::execution::PluginRuntimeOverrides {
        plugins_enabled: cli.no_plugins.then_some(false),
        plugin_verbose: cli.plugin_verbose.then_some(true),
    }
}

fn percentile(values: &[usize], pct: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    let rank = ((pct / 100.0) * (values.len().saturating_sub(1) as f64)).ceil() as usize;
    values[rank.min(values.len() - 1)]
}

fn is_production_profile(profile: &str) -> bool {
    matches!(profile.to_ascii_lowercase().as_str(), "prod" | "production")
}

fn parse_bool_env(name: &str) -> Option<Result<bool, CliError>> {
    std::env::var(name).ok().map(|value| {
        value.parse::<bool>().map_err(|_| {
            CliError::Other(format!(
                "invalid value for {name}: {value} (expected true or false)"
            ))
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

fn is_exit_command(input: &str) -> bool {
    matches!(input, "exit" | "quit" | "/exit" | "/quit")
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(input.as_bytes());
    format!("{hash:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use tempfile::tempdir;

    use crate::chat::default_session_id;
    use crate::commands::batch::{batch_output_path, parse_batch_input_line, sha256_hex};
    use crate::commands::eval::run_eval_export_chatml;
    use crate::commands::eval::run_eval_quality;
    use crate::commands::misc::{
        known_models, parse_compression_format, parse_compression_level, run_compress,
        run_personality, run_plugins, run_toolset,
    };
    use crate::commands::serve::{
        cron_time_from_datetime, default_schedule_id, default_schedule_session_id,
    };
    use crate::format::{
        context_template, export_session_markdown, format_insights, format_memory_list,
        format_schedule_list, format_session_list, format_session_messages, format_skill,
        format_skill_list, format_subagent, format_subagent_list, format_usage_stats,
    };
    use crate::slash::handle_chat_command;
    use chrono::{LocalResult, TimeZone};
    use genesis_core::execution::delivery_platform_from_str;
    use genesis_storage::{InsightsData, SessionSummary, StoredSchedule, StoredSkill, UsageStats};

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
            Command::Chat {
                session_id,
                resume,
                prompt,
                system,
                last,
                ..
            } => {
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
        let ui = UiContext::new(ColorMode::Never);
        let output = format_session_list(
            &[SessionSummary {
                id: "session-1".to_owned(),
                title: None,
                platform: "cli".to_owned(),
                total_input_tokens: 0,
                total_output_tokens: 0,
                parent_session_id: None,
                created_at: "2026-03-08 12:00:00".to_owned(),
                updated_at: "2026-03-08 12:05:00".to_owned(),
            }],
            &ui,
        );

        assert!(output.contains("genesis sessions"));
        assert!(output.contains("session-1"));
        assert!(output.contains("cli"));
        assert!(output.contains("2026-03-08 12:00:00"));
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
            timezone: None,
        }]);

        assert!(output.contains("genesis schedules"));
        assert!(output.contains("sched-123\tcli\t*/5 * * * *\tUTC\t2026-03-08 12:00:00"));
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
                timezone,
            }) => {
                assert_eq!(cron, "*/5 * * * *");
                assert_eq!(destination, "cli");
                assert_eq!(prompt, "check status");
                assert_eq!(timezone, None);
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
        let cli = Cli::try_parse_from(["genesis", "serve"]).expect("serve command should parse");

        match cli.command {
            Command::Serve { host, port } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, 3000);
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_serve_command_with_custom_host_port() {
        let cli =
            Cli::try_parse_from(["genesis", "serve", "--host", "127.0.0.1", "--port", "8080"])
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
    fn parses_global_plugin_flags() {
        let cli = Cli::try_parse_from([
            "genesis",
            "--no-plugins",
            "--plugin-verbose",
            "run",
            "hello",
        ])
        .expect("global plugin flags should parse");

        assert!(cli.no_plugins);
        assert!(cli.plugin_verbose);
        assert!(matches!(cli.command, Command::Run { .. }));
    }

    #[test]
    fn cli_runtime_overrides_map_global_plugin_flags() {
        let overrides = runtime_overrides_from_cli(&Cli {
            config: None,
            json: false,
            no_plugins: true,
            plugin_verbose: true,
            color: "auto".to_owned(),
            command: Command::Status,
        });

        assert_eq!(overrides.plugins_enabled, Some(false));
        assert_eq!(overrides.plugin_verbose, Some(true));
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
            "genesis",
            "model",
            "set",
            "claude-sonnet-4-6",
            "--backend",
            "anthropic",
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
        let cli = Cli::try_parse_from(["genesis", "tools"]).expect("tools command should parse");
        assert!(matches!(cli.command, Command::Tools));
    }

    #[tokio::test]
    async fn tools_command_lists_registered_tools() {
        let output = run(Cli {
            config: None,
            json: false,
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
                provider_metadata: None,
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
                provider_metadata: None,
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
        let cli = Cli::try_parse_from(["genesis", "sessions", "search", "hello world"])
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
        store
            .create_session("s-1", "cli", None)
            .expect("create session");

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
        store
            .create_session("s-tok", "cli", None)
            .expect("create session");
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
            provider_metadata: None,
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
            provider_metadata: None,
            created_at: "2026-03-08 12:00:00".to_owned(),
        }];

        let output = format_session_messages("s-1", &messages);
        assert!(output.contains("..."));
        assert!(output.len() < 400);
    }

    #[test]
    fn parses_info_command() {
        let cli = Cli::try_parse_from(["genesis", "info"]).expect("info command should parse");
        assert!(matches!(cli.command, Command::Info));
    }

    #[test]
    fn parses_nudge_command() {
        let cli = Cli::try_parse_from(["genesis", "nudge"]).expect("nudge command should parse");
        assert!(matches!(cli.command, Command::Nudge));
    }

    #[test]
    fn parses_update_command() {
        let cli = Cli::try_parse_from(["genesis", "update"]).expect("update command should parse");
        assert!(matches!(cli.command, Command::Update));
    }

    #[test]
    fn parses_mcp_list_command() {
        let cli =
            Cli::try_parse_from(["genesis", "mcp", "list"]).expect("mcp list command should parse");
        assert!(matches!(cli.command, Command::Mcp(McpCommand::List)));
    }

    #[test]
    fn parses_mcp_test_command() {
        let cli =
            Cli::try_parse_from(["genesis", "mcp", "test"]).expect("mcp test command should parse");
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
            Command::Run {
                prompt,
                session_id,
                raw,
                system,
                stream,
                ..
            } => {
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
            "genesis",
            "run",
            "--raw",
            "--session-id",
            "my-session",
            "what is 2+2",
        ])
        .expect("run command with flags should parse");
        match cli.command {
            Command::Run {
                prompt,
                session_id,
                raw,
                system,
                stream,
                ..
            } => {
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
        let cli = Cli::try_parse_from(["genesis", "chat", "--system", "You are a pirate."])
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
            "genesis",
            "run",
            "--system",
            "You are a calculator.",
            "what is 2+2",
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
        let cli = Cli::try_parse_from(["genesis", "run", "--stream", "tell me a story"])
            .expect("run with --stream should parse");
        match cli.command {
            Command::Run {
                prompt,
                stream,
                raw,
                ..
            } => {
                assert_eq!(prompt, "tell me a story");
                assert!(stream);
                assert!(!raw);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_init_command() {
        let cli = Cli::try_parse_from(["genesis", "init"]).expect("init command should parse");
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
            no_plugins: false,
            plugin_verbose: false,
            color: "auto".to_owned(),
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
        let cli =
            Cli::try_parse_from(["genesis", "config", "edit"]).expect("config edit should parse");
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
        let cli =
            Cli::try_parse_from(["genesis", "sessions", "rename", "session-42", "My Project"])
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
        let cli =
            Cli::try_parse_from(["genesis", "chat", "--last"]).expect("chat --last should parse");
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
        let cli = Cli::try_parse_from(["genesis", "chat"]).expect("chat should parse");
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
        let cli = Cli::try_parse_from(["genesis", "sessions", "purge", "--older-than", "30"])
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
        let cli =
            Cli::try_parse_from(["genesis", "context", "edit"]).expect("context edit should parse");
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

        let result = crate::format::run_skills_scan(dir.path().to_str().unwrap(), false)
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
        let result = crate::format::run_skills_install_local(&store, skill_dir.to_str().unwrap())
            .expect("install should succeed");

        assert!(result.contains("installed skill 'review'"));
        assert!(result.contains("v2.0"));

        let stored = store
            .get("review")
            .expect("db lookup")
            .expect("skill exists");
        assert_eq!(stored.description, "Review code");
        assert!(stored.instructions.contains("Review all code carefully"));
        assert_eq!(stored.tags, vec!["dev", "quality"]);
    }

    #[test]
    fn parses_config_set_command() {
        let cli = Cli::try_parse_from(["genesis", "config", "set", "provider.model", "gpt-5"])
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
        let cli = Cli::try_parse_from(["genesis", "insights"]).expect("insights should parse");
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
        let cli = Cli::try_parse_from(["genesis", "toolset", "show", "development"])
            .expect("should parse");
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
        assert!(result.contains("bundled"));
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
        assert!(result.contains("Source: bundled"));
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
    fn run_plugins_list_reports_enabled_and_disabled_plugins() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = write_plugin_test_config(dir.path(), None);
        let plugin_dir = dir.path().join("data").join("plugins");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir should exist");
        std::fs::write(plugin_dir.join("enabled.lua"), "genesis.log('enabled')")
            .expect("enabled plugin should write");
        std::fs::write(plugin_dir.join("disabled.lua"), "genesis.log('disabled')")
            .expect("disabled plugin should write");
        genesis_config::set_plugin_disabled_in_file(&config_path, "disabled", true)
            .expect("disable should persist");

        let result = run_plugins(Some(config_path), PluginsCommand::List, false)
            .expect("plugin list should succeed");

        assert!(result.contains("enabled"));
        assert!(result.contains("disabled"));
        assert!(result.contains("default"));
        assert!(result.contains("bundled"));
        assert!(result.contains("single_file"));
    }

    #[test]
    fn run_plugins_info_reports_manifest_details() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = write_plugin_test_config(dir.path(), None);
        let plugin_dir = dir.path().join("data").join("plugins");
        let package_dir = plugin_dir.join("weather");
        std::fs::create_dir_all(&package_dir).expect("package dir should exist");
        std::fs::write(package_dir.join("init.lua"), "return true").expect("init should write");
        std::fs::write(
            package_dir.join("plugin.toml"),
            r#"
[plugin]
name = "weather"
version = "0.1.0"
description = "Weather plugin"
author = "tester"

[permissions]
tools = ["read_file"]
hooks = ["PreTurn"]
trusted = true
"#,
        )
        .expect("manifest should write");

        let result = run_plugins(
            Some(config_path),
            PluginsCommand::Info {
                name: "weather".to_owned(),
            },
            false,
        )
        .expect("plugin info should succeed");

        assert!(result.contains("Plugin: weather"));
        assert!(result.contains("Source:"));
        assert!(result.contains("Version: 0.1.0"));
        assert!(result.contains("Author: tester"));
        assert!(result.contains("Allowed tools: read_file"));
        assert!(result.contains("Allowed hooks: PreTurn"));
    }

    #[test]
    fn run_plugins_info_reports_shadowed_bundled_plugin() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = write_plugin_test_config(dir.path(), None);
        let plugin_dir = dir.path().join("data").join("plugins");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir should exist");
        std::fs::write(plugin_dir.join("pirate.lua"), "genesis.log('override')")
            .expect("plugin should write");

        let result = run_plugins(
            Some(config_path),
            PluginsCommand::Info {
                name: "pirate".to_owned(),
            },
            false,
        )
        .expect("plugin info should succeed");

        assert!(result.contains("Plugin: pirate"));
        assert!(result.contains("Kind: single_file"));
        assert!(result.contains("Shadowed entries:"));
        assert!(result.contains("bundled"));
        assert!(result.contains("Source: built-in"));
    }

    #[test]
    fn run_plugins_info_reports_disabled_local_plugin_as_other_entry() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = write_plugin_test_config(dir.path(), None);
        let plugin_dir = dir.path().join("data").join("plugins");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir should exist");
        std::fs::write(plugin_dir.join("pirate.lua"), "genesis.log('override')")
            .expect("plugin should write");
        genesis_config::set_plugin_disabled_in_file(&config_path, "pirate", true)
            .expect("disable should persist");

        let result = run_plugins(
            Some(config_path),
            PluginsCommand::Info {
                name: "pirate".to_owned(),
            },
            false,
        )
        .expect("plugin info should succeed");

        assert!(result.contains("Plugin: pirate"));
        assert!(result.contains("Status: disabled"));
        assert!(result.contains("Other entries:"));
        assert!(!result.contains("Shadowed entries:"));
        assert!(result.contains("single_file"));
        assert!(result.contains("Source: built-in"));
    }

    #[test]
    fn run_plugins_disable_updates_config_file() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = write_plugin_test_config(dir.path(), None);
        let plugin_dir = dir.path().join("data").join("plugins");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir should exist");
        std::fs::write(plugin_dir.join("pirate.lua"), "return true").expect("plugin should write");

        let result = run_plugins(
            Some(config_path.clone()),
            PluginsCommand::Disable {
                name: "pirate".to_owned(),
            },
            false,
        )
        .expect("disable should succeed");

        assert!(result.contains("disabled plugin `pirate`"));
        let reloaded = genesis_config::load(Some(&config_path)).expect("reload");
        assert_eq!(reloaded.config.plugins.disabled, vec!["pirate".to_owned()]);
    }

    #[test]
    fn run_plugins_enable_removes_stale_disabled_entry() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = write_plugin_test_config(dir.path(), None);
        genesis_config::set_plugin_disabled_in_file(&config_path, "ghost", true)
            .expect("disable should persist");

        let result = run_plugins(
            Some(config_path.clone()),
            PluginsCommand::Enable {
                name: "ghost".to_owned(),
            },
            false,
        )
        .expect("enable should succeed");

        assert!(result.contains("enabled plugin `ghost`"));
        let reloaded = genesis_config::load(Some(&config_path)).expect("reload");
        assert!(reloaded.config.plugins.disabled.is_empty());
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
    fn parses_plugins_list_command() {
        let cli = Cli::try_parse_from(["genesis", "plugins", "list"]).expect("should parse");
        match cli.command {
            Command::Plugins(PluginsCommand::List) => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_plugins_info_command() {
        let cli =
            Cli::try_parse_from(["genesis", "plugins", "info", "pirate"]).expect("should parse");
        match cli.command {
            Command::Plugins(PluginsCommand::Info { name }) => assert_eq!(name, "pirate"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_plugins_disable_command() {
        let cli =
            Cli::try_parse_from(["genesis", "plugins", "disable", "pirate"]).expect("should parse");
        match cli.command {
            Command::Plugins(PluginsCommand::Disable { name }) => assert_eq!(name, "pirate"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_plugins_enable_command() {
        let cli =
            Cli::try_parse_from(["genesis", "plugins", "enable", "pirate"]).expect("should parse");
        match cli.command {
            Command::Plugins(PluginsCommand::Enable { name }) => assert_eq!(name, "pirate"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    fn write_plugin_test_config(dir: &std::path::Path, extra: Option<&str>) -> std::path::PathBuf {
        let config_path = dir.join("config.yaml");
        let mut body = format!(
            "provider:\n  backend: openai\n  model: gpt-4.1-mini\nstorage:\n  data_dir: {}\n",
            dir.join("data").display()
        );
        if let Some(extra) = extra {
            body.push_str(extra);
        }
        std::fs::write(&config_path, body).expect("config should write");
        config_path
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
            Command::Eval(crate::EvalCommand::AutoTag {
                dir,
                recursive,
                dry_run,
            }) => {
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
            Command::Eval(crate::EvalCommand::Deduplicate {
                dir,
                recursive,
                remove,
            }) => {
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

        crate::commands::batch::discard_low_quality_trajectory(
            dir.path().to_str().unwrap(),
            "low",
            0.5,
        )
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

        let output = crate::commands::eval::run_eval_deduplicate(
            dir.path().to_str().unwrap(),
            false,
            true,
            false,
        )
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

        let dry_run = crate::commands::eval::run_eval_auto_tag(
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

        crate::commands::eval::run_eval_auto_tag(dir.path().to_str().unwrap(), false, false, false)
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

        let output =
            crate::commands::eval::run_eval_tag_stats(dir.path().to_str().unwrap(), false, false)
                .expect("tag stats should succeed");

        assert!(output.contains("shell: 2"));
        assert!(output.contains("success: 2"));
    }

    #[test]
    fn batch_input_line_defaults_tags() {
        let parsed = parse_batch_input_line(r#"{"prompt":"hello"}"#).expect("json should parse");
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
        let cli = Cli::try_parse_from(["genesis", "compress", "--input", "trajectory.json"])
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
            crate::commands::misc::CompressionFormat::Json
        ));
    }

    #[test]
    fn format_insights_displays_summary() {
        let data = InsightsData {
            period_days: 30,
            sessions_count: 10,
            total_input_tokens: 5000,
            total_output_tokens: 3000,
            sessions_per_day: vec![("2026-03-07".to_owned(), 3), ("2026-03-08".to_owned(), 7)],
            platform_breakdown: vec![("cli".to_owned(), 8), ("api".to_owned(), 2)],
            tokens_per_day: vec![
                ("2026-03-07".to_owned(), 1500, 900),
                ("2026-03-08".to_owned(), 3500, 2100),
            ],
            tool_usage: vec![("shell_exec".to_owned(), 15), ("echo".to_owned(), 5)],
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
        store
            .append_message("s-undo", "system", Some("You are Eve."), None, None, None)
            .unwrap();
        store
            .append_message("s-undo", "user", Some("Hello"), None, None, None)
            .unwrap();
        store
            .append_message("s-undo", "assistant", Some("Hi!"), None, None, None)
            .unwrap();
        store
            .append_message("s-undo", "user", Some("How are you?"), None, None, None)
            .unwrap();
        store
            .append_message("s-undo", "assistant", Some("Great!"), None, None, None)
            .unwrap();

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
        store
            .create_session("s-undo2", "cli", None)
            .expect("create");
        store
            .append_message("s-undo2", "user", Some("search for X"), None, None, None)
            .unwrap();
        // assistant with tool call, tool result, then final assistant response
        store.append_message("s-undo2", "assistant", None, Some(r#"[{"id":"t1","type":"function","function":{"name":"web_search","arguments":"{}"}}]"#), None, None).unwrap();
        store
            .append_message("s-undo2", "tool", Some("result"), None, None, None)
            .unwrap();
        store
            .append_message(
                "s-undo2",
                "assistant",
                Some("Here's what I found"),
                None,
                None,
                None,
            )
            .unwrap();

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
        store
            .create_session("s-empty", "cli", None)
            .expect("create");

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
        let cli =
            Cli::try_parse_from(["genesis", "memory", "list"]).expect("memory list should parse");
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
            Command::Memory(MemoryCommand::Search { query, limit, mode }) => {
                assert_eq!(query, "rust programming");
                assert_eq!(limit, 10);
                assert_eq!(mode, "keyword");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_memory_search_command_with_graph_mode() {
        let cli = Cli::try_parse_from([
            "genesis",
            "memory",
            "search",
            "rust programming",
            "--mode",
            "graph",
            "--limit",
            "3",
        ])
        .expect("memory search should parse");
        match cli.command {
            Command::Memory(MemoryCommand::Search { query, limit, mode }) => {
                assert_eq!(query, "rust programming");
                assert_eq!(limit, 3);
                assert_eq!(mode, "graph");
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
        let cli = Cli::try_parse_from(["genesis", "eval", "compare", "left.json", "right.json"])
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
            Command::Eval(crate::EvalCommand::Convert {
                input,
                output,
                format,
            }) => {
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

        let summary = crate::commands::eval::summarize_replay_reports(
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
        assert!(summary.tags.contains(&("offline_eval".to_owned(), 2)));
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

        std::fs::write(
            &left,
            serde_json::to_string_pretty(&left_trajectory).unwrap(),
        )
        .expect("write left");
        std::fs::write(
            &right,
            serde_json::to_string_pretty(&right_trajectory).unwrap(),
        )
        .expect("write right");

        let _comparison = crate::commands::eval::compare_replay_reports(
            left.to_str().unwrap(),
            right.to_str().unwrap(),
        )
        .expect("write left");
        std::fs::write(
            &right,
            serde_json::to_string_pretty(&right_trajectory).unwrap(),
        )
        .expect("write right");

        let comparison = crate::commands::eval::compare_replay_reports(
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

        let output =
            crate::commands::eval::run_eval_export_sharegpt(dir.path().to_str().unwrap(), false)
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
        std::fs::write(
            &input,
            format!("{}\n", serde_json::to_string(&line).unwrap()),
        )
        .expect("write jsonl");

        let result = crate::commands::eval::run_eval_import_chatml(
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
        std::fs::write(
            &input,
            format!("{}\n", serde_json::to_string(&line).unwrap()),
        )
        .expect("write jsonl");

        let result = crate::commands::eval::run_eval_import_sharegpt(
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

        let result = crate::commands::eval::run_eval_merge(
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

        let result = crate::commands::eval::run_eval_merge(
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
            "genesis",
            "eval",
            "import-sharegpt",
            "data.jsonl",
            "--output",
            "out",
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

        let result = crate::commands::eval::run_eval_filter(
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            false,
            Some("gpt-4"),
            None,
            None,
            None,
            true,
            false,
            None,
            None,
            None,
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

        let result = crate::commands::eval::run_eval_filter(
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            false,
            None,
            Some("coding"),
            None,
            None,
            false,
            false,
            None,
            None,
            None,
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

        let result = crate::commands::eval::run_eval_split(
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

        crate::commands::eval::run_eval_split(
            src.to_str().unwrap(),
            train1.to_str().unwrap(),
            test1.to_str().unwrap(),
            0.6,
            Some(99),
            false,
        )
        .unwrap();

        crate::commands::eval::run_eval_split(
            src.to_str().unwrap(),
            train2.to_str().unwrap(),
            test2.to_str().unwrap(),
            0.6,
            Some(99),
            false,
        )
        .unwrap();

        let names1: Vec<String> = std::fs::read_dir(&train1)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let names2: Vec<String> = std::fs::read_dir(&train2)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(names1, names2);
    }

    #[test]
    fn parses_eval_filter_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "filter",
            "src",
            "--output",
            "out",
            "--model",
            "gpt-4",
            "--success-only",
            "--min-quality",
            "0.5",
        ])
        .expect("filter should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Filter {
                dir,
                output,
                model,
                success_only,
                min_quality,
                ..
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
            "genesis", "eval", "split", "src", "--train", "train", "--test", "test", "--ratio",
            "0.7", "--seed", "42",
        ])
        .expect("split should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Split {
                dir,
                train,
                test,
                ratio,
                seed,
                ..
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
            "genesis",
            "eval",
            "manifest",
            "src",
            "--name",
            "my-dataset",
            "--description",
            "test set",
            "--save",
        ])
        .expect("manifest should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Manifest {
                dir,
                name,
                description,
                save,
                ..
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
        )
        .unwrap();

        let result = crate::commands::eval::run_eval_manifest(
            dir.path().to_str().unwrap(),
            "test-ds",
            "a test",
            false,
            false,
            false,
        )
        .expect("manifest should succeed");

        assert!(result.contains("test-ds"));
        assert!(result.contains("files: 1"));
        assert!(result.contains("gpt-4"));
    }

    #[test]
    fn parses_eval_pipeline_command() {
        let cli = Cli::try_parse_from([
            "genesis",
            "eval",
            "pipeline",
            "src",
            "--output",
            "out",
            "--validate",
            "--auto-tag",
            "--min-quality",
            "0.5",
            "--success-only",
            "--manifest",
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

        let result = crate::commands::eval::run_eval_pipeline(
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            false,
            true,
            true,
            None,
            true,
            None,
            None,
            None,
            false,
            None,
            None,
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
            std::fs::write(
                src.join(format!("s{i}.json")),
                serde_json::to_string(&t).unwrap(),
            )
            .unwrap();
        }

        let result = crate::commands::eval::run_eval_pipeline(
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            false,
            false,
            false,
            None,
            false,
            None,
            None,
            None,
            false,
            Some(3),
            Some(42),
        )
        .expect("pipeline should succeed");

        assert!(result.contains("limited to 3"));
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3);
    }

    #[test]
    fn parses_eval_validate_command() {
        let cli = Cli::try_parse_from(["genesis", "eval", "validate", "src", "--remove"])
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

        let result = crate::commands::eval::run_eval_validate(src.to_str().unwrap(), false, false)
            .expect("validate should succeed");

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

        let result = crate::commands::eval::run_eval_validate(src.to_str().unwrap(), false, true)
            .expect("validate with remove should succeed");

        assert!(result.contains("removed 1 invalid"));
        assert!(!src.join("bad.json").exists());
        assert!(src.join("good.json").exists());
    }

    #[test]
    fn parses_eval_sample_command() {
        let cli = Cli::try_parse_from([
            "genesis", "eval", "sample", "src", "--output", "out", "--count", "100", "--seed", "42",
        ])
        .expect("sample should parse");
        match cli.command {
            Command::Eval(crate::EvalCommand::Sample {
                dir,
                output,
                count,
                seed,
                ..
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
            )
            .unwrap();
        }

        let result = crate::commands::eval::run_eval_sample(
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            3,
            Some(42),
            false,
        )
        .expect("sample should succeed");

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

        let result = crate::commands::eval::run_eval_sample(
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            100,
            Some(1),
            false,
        )
        .expect("sample should succeed");

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

        crate::commands::eval::run_eval_convert(
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "chatml",
        )
        .expect("conversion should succeed");

        let raw = std::fs::read_to_string(output).expect("read output");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json line");
        assert_eq!(parsed["session_id"], "convert-chatml");
        assert!(parsed["chatml"]
            .as_str()
            .unwrap()
            .contains("<|im_start|>user"));
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

        crate::commands::eval::run_eval_convert(
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
        std::fs::write(
            &input,
            format!("{}\n", serde_json::to_string(&line).unwrap()),
        )
        .unwrap();

        crate::commands::eval::run_eval_convert(
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
        std::fs::write(
            &input,
            format!("{}\n", serde_json::to_string(&line).unwrap()),
        )
        .unwrap();

        crate::commands::eval::run_eval_convert(
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

        write(
            "a.json",
            serde_json::json!({
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
            }),
        );

        write(
            "b.json",
            serde_json::json!({
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
            }),
        );

        write(
            "c.json",
            serde_json::json!({
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
            }),
        );

        let stats = crate::commands::eval::compute_eval_stats(
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

        let trajectory = |session_id: &str| {
            serde_json::json!({
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
            })
        };

        std::fs::write(
            &root_file,
            serde_json::to_string_pretty(&trajectory("root")).unwrap(),
        )
        .expect("write root");
        std::fs::write(
            &nested_file,
            serde_json::to_string_pretty(&trajectory("nested")).unwrap(),
        )
        .expect("write nested");

        let non_recursive = crate::commands::eval::summarize_replay_reports(
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
        let recursive = crate::commands::eval::summarize_replay_reports(
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

        let summary = crate::commands::eval::summarize_replay_reports(
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
        assert_eq!(
            summary.tags,
            vec![("offline_eval".to_owned(), 1), ("smoke".to_owned(), 1),]
        );
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

        write(
            &success_clean,
            "s-1",
            serde_json::json!({ "type": "success" }),
            false,
        );
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

        let summary = crate::commands::eval::summarize_replay_reports(
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
        let comparison = crate::commands::eval::EvalComparison {
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
            event_delta: crate::commands::eval::ReplayEventDelta {
                user: 0,
                assistant: 1,
                tool_call: 1,
                tool_result: 1,
                system: -1,
            },
            tools: vec![crate::commands::eval::ToolUsageDelta {
                name: "shell".to_owned(),
                left_call_count: 0,
                right_call_count: 1,
                left_result_count: 0,
                right_result_count: 1,
            }],
            left_only_tags: vec!["baseline".to_owned()],
            right_only_tags: vec!["with_tools".to_owned()],
        };

        let json = crate::commands::eval::eval_comparison_to_json(&comparison);

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
        let cli = Cli::try_parse_from(["genesis", "status"]).expect("status should parse");
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn parses_sessions_import_command() {
        let cli = Cli::try_parse_from(["genesis", "sessions", "import", "chat.json"])
            .expect("sessions import should parse");
        match cli.command {
            Command::Sessions(SessionsCommand::Import {
                file,
                format,
                title,
            }) => {
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
            "genesis",
            "sessions",
            "import",
            "data.jsonl",
            "--format",
            "jsonl",
            "--title",
            "My Chat",
        ])
        .expect("sessions import with options should parse");
        match cli.command {
            Command::Sessions(SessionsCommand::Import {
                file,
                format,
                title,
            }) => {
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
            "genesis",
            "sessions",
            "import",
            "conversation.json",
            "--format",
            "sharegpt",
            "--title",
            "Imported Conversation",
        ])
        .expect("sessions import sharegpt should parse");
        match cli.command {
            Command::Sessions(SessionsCommand::Import {
                file,
                format,
                title,
            }) => {
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
        let cli =
            Cli::try_parse_from(["genesis", "pairing", "list"]).expect("pairing list should parse");
        match cli.command {
            Command::Pairing(PairingCommand::List { platform }) => {
                assert!(platform.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pairing_list_with_platform() {
        let cli = Cli::try_parse_from(["genesis", "pairing", "list", "--platform", "telegram"])
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
        let cli = Cli::try_parse_from(["genesis", "pairing", "approve", "telegram", "ABC12345"])
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
        let cli = Cli::try_parse_from(["genesis", "pairing", "revoke", "discord", "user-42"])
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
        let cli =
            Cli::try_parse_from(["genesis", "pairing", "clear-pending", "--platform", "slack"])
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
        let cli =
            Cli::try_parse_from(["genesis", "uninstall"]).expect("uninstall command should parse");
        match cli.command {
            Command::Uninstall {
                remove_data,
                remove_config,
                force,
            } => {
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
            "genesis",
            "uninstall",
            "--remove-data",
            "--remove-config",
            "--force",
        ])
        .expect("uninstall command with flags should parse");
        match cli.command {
            Command::Uninstall {
                remove_data,
                remove_config,
                force,
            } => {
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
            Command::Uninstall {
                remove_data,
                remove_config,
                force,
            } => {
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
            Command::Uninstall {
                remove_data,
                remove_config,
                force,
            } => {
                assert!(!remove_data);
                assert!(remove_config);
                assert!(!force);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn model_browse_parses() {
        let cli = Cli::try_parse_from([
            "genesis", "model", "browse", "--tools", "--sort", "cheapest", "-n", "5",
        ])
        .expect("model browse should parse");

        match cli.command {
            Command::Model(ModelCommand::Browse {
                query,
                tools,
                vision,
                reasoning,
                sort,
                limit,
                json,
            }) => {
                assert!(query.is_none());
                assert!(tools);
                assert!(!vision);
                assert!(!reasoning);
                assert_eq!(sort, "cheapest");
                assert_eq!(limit, 5);
                assert!(!json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn model_browse_parses_with_query() {
        let cli = Cli::try_parse_from([
            "genesis",
            "model",
            "browse",
            "--query",
            "gpt",
            "--vision",
            "--reasoning",
            "--json",
        ])
        .expect("model browse with query should parse");

        match cli.command {
            Command::Model(ModelCommand::Browse {
                query,
                tools,
                vision,
                reasoning,
                sort,
                limit,
                json,
            }) => {
                assert_eq!(query.as_deref(), Some("gpt"));
                assert!(!tools);
                assert!(vision);
                assert!(reasoning);
                assert_eq!(sort, "newest");
                assert_eq!(limit, 20);
                assert!(json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
