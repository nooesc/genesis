use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const APP_DIR_NAME: &str = "genesis";
const DEFAULT_PROFILE: &str = "default";
const DEFAULT_PROVIDER_BACKEND: &str = "openai";
const DEFAULT_MODEL: &str = "gpt-4.1-mini";
const DEFAULT_DATABASE_FILE: &str = "genesis.db";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenesisConfig {
    pub schema_version: u32,
    pub profile: String,
    pub provider: ProviderConfig,
    /// Optional secondary provider for tool-calling turns. When set, the agent
    /// uses this cheaper/faster model for turns that follow tool results and
    /// reserves the primary provider for reasoning turns (after user messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_provider: Option<ProviderConfig>,
    /// Fallback providers tried in order when the primary provider fails.
    /// After the primary provider exhausts its retries, each fallback is
    /// attempted in sequence until one succeeds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_providers: Vec<ProviderConfig>,
    /// MCP (Model Context Protocol) server definitions. Each entry maps a
    /// server name to its connection config (stdio or HTTP transport).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    pub storage: StorageConfig,
    pub runtime: RuntimeConfig,
    /// Gateway-specific settings (session policies, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayConfig>,
    /// Custom toolset distributions for batch training. Each entry maps a
    /// distribution name to a map of tool name -> inclusion probability (0.0-1.0).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub toolsets: HashMap<String, HashMap<String, f64>>,
    /// Agent personality name (e.g. "pirate", "zen", "hacker").
    /// Adjusts the agent's conversational tone without changing capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    /// Optional embedding provider for vector/semantic memory search.
    /// When configured, memories are embedded on storage and searched
    /// via cosine similarity in addition to FTS5 keyword matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingConfig>,
    /// Display and UI settings for the CLI.
    #[serde(default)]
    pub display: DisplayConfig,
}

/// Display and UI settings for the CLI.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplayConfig {
    /// Tool call progress display mode.
    #[serde(default)]
    pub tool_progress: ToolDisplayMode,
}

/// Controls how tool call progress is displayed in the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolDisplayMode {
    /// Do not show tool call progress.
    Off,
    /// Show a brief one-line summary per tool call.
    Summary,
    /// Group tool calls visually (default).
    #[default]
    Grouped,
    /// Show full tool call details.
    Verbose,
}

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    /// Command to spawn for stdio transport.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments for the stdio command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Environment variables passed to the subprocess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// URL for HTTP transport.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// HTTP headers for URL transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    /// Tool call timeout in seconds (default: 120).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Connection/initialization timeout in seconds (default: 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub backend: String,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    /// Extra body fields merged into every request. Useful for OpenRouter
    /// provider preferences (e.g. `{"provider": {"sort": "price"}}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
    /// Tool call parser for models that embed tool calls in text content
    /// rather than using native tool_calls. Auto-detected from model name
    /// when not set. Examples: "hermes", "llama", "mistral", "deepseek_v3".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_parser: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageConfig {
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeConfig {
    pub max_concurrency: usize,
    pub allow_destructive_tools: bool,
    /// Maximum agent loop iterations per user turn (default: 20).
    pub max_turns: usize,
    /// Max conversation messages kept in context. Oldest messages are pruned
    /// with a summary when exceeded. `None` means unlimited.
    pub max_context_messages: Option<usize>,
    /// Optional per-session budget limit in USD. When exceeded, the agent
    /// loop stops early. `None` means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_limit: Option<f64>,
    /// Terminal backend for shell_exec. Defaults to local shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalConfig>,
    /// Extended thinking budget in tokens. When set, providers that support
    /// reasoning (Claude, o1/o3) will use extended thinking with this budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// Maximum input tokens before context compression triggers. When the last
    /// API response reports prompt_tokens above this threshold, the middle
    /// portion of the conversation is summarized and replaced. `None` disables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    /// Maximum number of LLM iterations across the agent's lifetime.
    /// Unlike `max_turns` which resets each user message, this is a hard cap
    /// on total LLM round-trips. Useful for autonomous/batch agents. `None`
    /// means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<usize>,
    /// Context file security scanning policy.
    #[serde(default)]
    pub context_security: ContextSecurityPolicy,
    /// Reasoning effort level for providers that support it.
    /// Affects how much compute the model spends on reasoning.
    /// Supported on OpenRouter, Anthropic, and some custom providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Response cache configuration. When enabled, identical LLM requests
    /// are served from a local SQLite cache instead of calling the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheConfig>,
    /// Tool filter for controlling which tools the agent can use.
    /// When set, only tools matching the filter criteria are available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_filter: Option<ToolFilterConfig>,
    /// Guardrails configuration. When set, input and output are validated
    /// against the specified rules before processing / returning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrails: Option<GuardrailsConfig>,
}

/// Configuration for filtering which tools are available to the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolFilterConfig {
    /// If non-empty, only these tools are allowed (allowlist takes priority).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Tools to block even if they appear in the allowlist or default set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
}

/// Response cache configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheConfig {
    /// Whether caching is enabled (default: true when cache section present).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Time-to-live for cached entries in seconds (default: 3600 = 1 hour).
    #[serde(default = "default_cache_ttl")]
    pub ttl_seconds: u32,
    /// Maximum number of recent messages to include in the cache key.
    /// Fewer messages = more cache hits but less context sensitivity.
    /// Default: 4.
    #[serde(default = "default_cache_context_messages")]
    pub max_context_messages: usize,
}

fn default_true() -> bool { true }
fn default_cpu() -> f32 { 1.0 }
fn default_memory() -> u32 { 5120 }
fn default_disk() -> u32 { 51200 }
fn default_daytona_disk() -> u32 { 10240 }
fn default_cache_ttl() -> u32 { 3600 }
fn default_cache_context_messages() -> usize { 4 }

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            ttl_seconds: default_cache_ttl(),
            max_context_messages: default_cache_context_messages(),
        }
    }
}

/// Guardrails configuration for input/output validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuardrailsConfig {
    /// Enable PII detection (phone, email, SSN, credit card).
    #[serde(default)]
    pub detect_pii: bool,
    /// Action when PII is detected: block, warn, or redact.
    #[serde(default = "default_pii_action")]
    pub pii_action: String,
    /// Maximum response length in characters (0 = unlimited).
    #[serde(default)]
    pub max_response_length: usize,
    /// Forbidden input patterns (regex). Inputs matching these are blocked.
    #[serde(default)]
    pub forbidden_input_patterns: Vec<String>,
    /// Forbidden output patterns (regex). Outputs matching these are blocked.
    #[serde(default)]
    pub forbidden_output_patterns: Vec<String>,
    /// Require output to be valid JSON.
    #[serde(default)]
    pub require_json_output: bool,
    /// Maximum token budget per turn (0 = unlimited).
    #[serde(default)]
    pub max_tokens_per_turn: u32,
    /// Custom rules with pattern and action.
    #[serde(default)]
    pub custom_rules: Vec<GuardrailCustomRule>,
}

/// A custom guardrail rule in config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuardrailCustomRule {
    pub name: String,
    pub pattern: String,
    #[serde(default = "default_applies_to")]
    pub applies_to: String,
    pub action: String,
    #[serde(default)]
    pub message: String,
}

fn default_pii_action() -> String { "redact".to_owned() }
fn default_applies_to() -> String { "both".to_owned() }

/// Reasoning effort level controlling how much compute the model spends.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// Maximum reasoning depth.
    High,
    /// Balanced reasoning (default for most providers).
    Medium,
    /// Minimal reasoning for fast, cheap responses.
    Low,
}

/// Policy for handling detected threats in context files (AGENTS.md, SOUL.md, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextSecurityPolicy {
    /// Warn in the prompt but still include the file (default).
    #[default]
    Warn,
    /// Block files with any high-severity threats entirely.
    BlockHigh,
    /// Block files with any threats (regardless of severity).
    BlockAll,
    /// Disable context security scanning.
    Disabled,
}

/// Terminal backend configuration for shell command execution.
/// When configured, `shell_exec` routes commands through the specified backend
/// instead of the local shell.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "backend")]
pub enum TerminalConfig {
    /// Execute in a Docker container.
    #[serde(rename = "docker")]
    Docker {
        container: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    /// Execute on a remote host via SSH.
    #[serde(rename = "ssh")]
    Ssh {
        host: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        identity_file: Option<String>,
    },
    /// Execute inside a Singularity/Apptainer container (HPC environments).
    #[serde(rename = "singularity")]
    Singularity {
        image: String,
        #[serde(default = "default_cpu")]
        cpu: f32,
        #[serde(default = "default_memory")]
        memory_mb: u32,
        #[serde(default = "default_true")]
        persistent: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    /// Execute via Modal cloud sandbox.
    #[serde(rename = "modal")]
    Modal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        #[serde(default = "default_cpu")]
        cpu: f32,
        #[serde(default = "default_memory")]
        memory_mb: u32,
        #[serde(default = "default_disk")]
        disk_mb: u32,
        #[serde(default = "default_true")]
        persistent: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gpu: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    /// Execute in a Daytona workspace.
    #[serde(rename = "daytona")]
    Daytona {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        #[serde(default = "default_cpu")]
        cpu: f32,
        #[serde(default = "default_memory")]
        memory_mb: u32,
        #[serde(default = "default_daytona_disk")]
        disk_mb: u32,
        #[serde(default = "default_true")]
        persistent: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
}

/// Embedding provider configuration for vector/semantic memory search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingConfig {
    /// Provider backend for embeddings (e.g. "openai", "openrouter").
    #[serde(default = "default_embedding_backend")]
    pub backend: String,
    /// Embedding model name (e.g. "text-embedding-3-small").
    #[serde(default = "default_embedding_model")]
    pub model: String,
    /// Base URL override for the embedding API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Env var name holding the API key. Falls back to standard provider env vars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Number of dimensions in the embedding vector.
    /// Only sent to embedding APIs that support it (e.g. OpenAI).
    /// When `None`, the model's default dimensions are used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
}

fn default_embedding_backend() -> String { "openai".to_owned() }
fn default_embedding_model() -> String { "text-embedding-3-small".to_owned() }

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            backend: default_embedding_backend(),
            model: default_embedding_model(),
            base_url: None,
            api_key_env: None,
            dimensions: None,
        }
    }
}

/// Gateway-specific settings for session lifecycle policies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GatewayConfig {
    /// Auto-reset sessions that have been idle for this many minutes.
    /// When a new message arrives and the session's `updated_at` is older
    /// than this threshold, the session is cleared before processing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_minutes: Option<u64>,
    /// Auto-reset sessions daily at this hour (0-23, local time).
    /// If the session's `updated_at` is before today's reset hour, it
    /// is cleared on the next incoming message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_reset_hour: Option<u8>,
    /// Maximum requests per minute per IP. Overridden by GENESIS_RATE_LIMIT_RPM
    /// env var when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_rpm: Option<u32>,
    /// Webhook URLs to POST event notifications to. Events include:
    /// `message_received`, `tool_called`, `response_sent`, `error`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub webhooks: Vec<WebhookConfig>,
    /// Allowed CORS origins. When empty/unset, only localhost origins are
    /// permitted. Set to `["*"]` to allow all origins (not recommended
    /// for production).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cors_origins: Vec<String>,
}

/// Configuration for a single webhook endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookConfig {
    /// URL to POST event payloads to.
    pub url: String,
    /// Optional shared secret for HMAC-SHA256 signature verification.
    /// When set, a `X-Genesis-Signature` header is included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Event types to send. If empty, all events are sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    /// Maximum number of retry attempts on failure (default: 3).
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Initial backoff delay in milliseconds (default: 1000). Doubles each retry.
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: u64,
}

fn default_max_retries() -> u32 { 3 }
fn default_retry_backoff_ms() -> u64 { 1000 }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: GenesisConfig,
    pub paths: AppPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<FileProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_provider: Option<FileProviderConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fallback_providers: Option<Vec<FileProviderConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mcp_servers: Option<HashMap<String, McpServerConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage: Option<FileStorageConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<FileRuntimeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway: Option<GatewayConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    toolsets: Option<HashMap<String, HashMap<String, f64>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    personality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embedding: Option<EmbeddingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display: Option<DisplayConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FileProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extra_body: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_parser: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FileStorageConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    data_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    database_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FileRuntimeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_concurrency: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_destructive_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_turns: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_context_messages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal: Option<TerminalConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budget: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_context_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_iterations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_security: Option<ContextSecurityPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache: Option<CacheConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_filter: Option<ToolFilterConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guardrails: Option<GuardrailsConfig>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine a configuration directory for genesis")]
    MissingConfigDirectory,
    #[error("could not determine a data directory for genesis")]
    MissingDataDirectory,
    #[error("failed to read config file at {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unsupported config file extension at {0}; use .yaml, .yml, or .toml")]
    UnsupportedExtension(PathBuf),
    #[error("failed to parse yaml config at {path}: {source}")]
    ParseYaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("failed to parse toml config at {path}: {source}")]
    ParseToml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to write config file at {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize config: {source}")]
    SerializeYaml {
        #[source]
        source: serde_yaml::Error,
    },
    #[error("invalid value for {name}: {value}")]
    InvalidEnvValue { name: &'static str, value: String },
}

pub fn load(config_path_override: Option<&Path>) -> Result<LoadedConfig, ConfigError> {
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    load_from_map(config_path_override, &env)
}

pub fn example_config(config_path_override: Option<&Path>) -> Result<GenesisConfig, ConfigError> {
    let paths = AppPaths::resolve(config_path_override)?;
    Ok(GenesisConfig {
        schema_version: 1,
        profile: DEFAULT_PROFILE.to_owned(),
        provider: ProviderConfig {
            backend: DEFAULT_PROVIDER_BACKEND.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            base_url: None,
            api_key_env: Some("OPENAI_API_KEY".to_owned()),
            extra_body: None,
            tool_call_parser: None,
        },
        tool_provider: None,
        fallback_providers: Vec::new(),
        mcp_servers: HashMap::new(),
        storage: StorageConfig {
            data_dir: paths.data_dir.clone(),
            database_path: paths.database_path,
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
            context_security: ContextSecurityPolicy::default(),
            reasoning_effort: None,
            cache: None,
            tool_filter: None,
            guardrails: None,
        },
        gateway: None,
        toolsets: HashMap::new(),
        personality: None,
        embedding: None,
        display: DisplayConfig::default(),
    })
}

pub fn render_example_yaml(config_path_override: Option<&Path>) -> Result<String, ConfigError> {
    let example = example_config(config_path_override)?;
    Ok(serde_yaml::to_string(&example).expect("example config should always serialize"))
}

pub fn load_from_map(
    config_path_override: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> Result<LoadedConfig, ConfigError> {
    let paths = AppPaths::resolve(config_path_override)?;
    let file_config = read_config_file(&paths.config_path)?;

    let data_dir = env
        .get("GENESIS_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| file_config.storage.as_ref().and_then(|storage| storage.data_dir.clone()))
        .unwrap_or_else(|| paths.data_dir.clone());

    let database_path = env
        .get("GENESIS_DATABASE_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            file_config
                .storage
                .as_ref()
                .and_then(|storage| storage.database_path.clone())
        })
        .unwrap_or_else(|| data_dir.join(DEFAULT_DATABASE_FILE));

    let profile = env
        .get("GENESIS_PROFILE")
        .cloned()
        .or_else(|| file_config.profile.clone())
        .unwrap_or_else(|| DEFAULT_PROFILE.to_owned());

    let prov = file_config.provider.as_ref();

    let provider = ProviderConfig {
        backend: env
            .get("GENESIS_PROVIDER_BACKEND")
            .cloned()
            .or_else(|| prov.and_then(|p| p.backend.clone()))
            .unwrap_or_else(|| DEFAULT_PROVIDER_BACKEND.to_owned()),
        model: env
            .get("GENESIS_MODEL")
            .cloned()
            .or_else(|| prov.and_then(|p| p.model.clone()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        base_url: env
            .get("GENESIS_PROVIDER_BASE_URL")
            .cloned()
            .or_else(|| prov.and_then(|p| p.base_url.clone())),
        api_key_env: env
            .get("GENESIS_PROVIDER_API_KEY_ENV")
            .cloned()
            .or_else(|| prov.and_then(|p| p.api_key_env.clone())),
        extra_body: prov.and_then(|p| p.extra_body.clone()),
        tool_call_parser: prov.and_then(|p| p.tool_call_parser.clone()),
    };

    // Optional tool provider — inherits primary provider defaults when partially specified.
    let tool_provider = file_config.tool_provider.as_ref().map(|tp| {
        ProviderConfig {
            backend: env
                .get("GENESIS_TOOL_PROVIDER_BACKEND")
                .cloned()
                .or_else(|| tp.backend.clone())
                .unwrap_or_else(|| provider.backend.clone()),
            model: env
                .get("GENESIS_TOOL_MODEL")
                .cloned()
                .or_else(|| tp.model.clone())
                .unwrap_or_else(|| provider.model.clone()),
            base_url: env
                .get("GENESIS_TOOL_PROVIDER_BASE_URL")
                .cloned()
                .or_else(|| tp.base_url.clone())
                .or_else(|| provider.base_url.clone()),
            api_key_env: env
                .get("GENESIS_TOOL_PROVIDER_API_KEY_ENV")
                .cloned()
                .or_else(|| tp.api_key_env.clone())
                .or_else(|| provider.api_key_env.clone()),
            extra_body: tp.extra_body.clone(),
            tool_call_parser: tp.tool_call_parser.clone(),
        }
    });

    // Fallback providers — each inherits primary provider defaults when partially specified.
    let fallback_providers = file_config
        .fallback_providers
        .unwrap_or_default()
        .iter()
        .map(|fp| ProviderConfig {
            backend: fp
                .backend
                .clone()
                .unwrap_or_else(|| provider.backend.clone()),
            model: fp
                .model
                .clone()
                .unwrap_or_else(|| provider.model.clone()),
            base_url: fp
                .base_url
                .clone()
                .or_else(|| provider.base_url.clone()),
            api_key_env: fp
                .api_key_env
                .clone()
                .or_else(|| provider.api_key_env.clone()),
            extra_body: fp.extra_body.clone(),
            tool_call_parser: fp.tool_call_parser.clone(),
        })
        .collect::<Vec<_>>();

    let rt = file_config.runtime.as_ref();

    let runtime = RuntimeConfig {
        max_concurrency: parse_env(
            env,
            "GENESIS_MAX_CONCURRENCY",
            rt.and_then(|r| r.max_concurrency).unwrap_or(4),
        )?,
        allow_destructive_tools: parse_env(
            env,
            "GENESIS_ALLOW_DESTRUCTIVE_TOOLS",
            rt.and_then(|r| r.allow_destructive_tools).unwrap_or(false),
        )?,
        max_turns: parse_env(
            env,
            "GENESIS_MAX_TURNS",
            rt.and_then(|r| r.max_turns).unwrap_or(20),
        )?,
        max_context_messages: rt.and_then(|r| r.max_context_messages),
        budget_limit: rt.and_then(|r| r.budget_limit),
        terminal: rt.and_then(|r| r.terminal.clone()),
        thinking_budget: rt.and_then(|r| r.thinking_budget),
        max_context_tokens: rt.and_then(|r| r.max_context_tokens),
        max_iterations: rt.and_then(|r| r.max_iterations),
        context_security: rt
            .and_then(|r| r.context_security.clone())
            .unwrap_or_default(),
        reasoning_effort: rt.and_then(|r| r.reasoning_effort),
        cache: rt.and_then(|r| r.cache.clone()),
        tool_filter: rt.and_then(|r| r.tool_filter.clone()),
        guardrails: rt.and_then(|r| r.guardrails.clone()),
    };

    let mcp_servers = file_config.mcp_servers.unwrap_or_default();

    Ok(LoadedConfig {
        config: GenesisConfig {
            schema_version: file_config.schema_version.unwrap_or(1),
            profile,
            provider,
            tool_provider,
            fallback_providers,
            mcp_servers,
            storage: StorageConfig {
                data_dir: data_dir.clone(),
                database_path: database_path.clone(),
            },
            runtime,
            gateway: file_config.gateway,
            toolsets: file_config.toolsets.unwrap_or_default(),
            personality: file_config.personality,
            embedding: file_config.embedding,
            display: file_config.display.unwrap_or_default(),
        },
        paths: AppPaths {
            config_path: paths.config_path,
            data_dir,
            database_path,
        },
    })
}

impl AppPaths {
    pub fn resolve(config_path_override: Option<&Path>) -> Result<Self, ConfigError> {
        let config_path = config_path_override
            .map(Path::to_path_buf)
            .unwrap_or(default_config_path()?);
        let data_dir = default_data_dir()?;

        Ok(Self {
            config_path,
            database_path: data_dir.join(DEFAULT_DATABASE_FILE),
            data_dir,
        })
    }
}

fn default_config_path() -> Result<PathBuf, ConfigError> {
    let base = dirs::config_dir().ok_or(ConfigError::MissingConfigDirectory)?;
    Ok(base.join(APP_DIR_NAME).join("config.yaml"))
}

fn default_data_dir() -> Result<PathBuf, ConfigError> {
    let base = dirs::data_dir().ok_or(ConfigError::MissingDataDirectory)?;
    Ok(base.join(APP_DIR_NAME))
}

fn read_config_file(path: &Path) -> Result<FileConfig, ConfigError> {
    let raw = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileConfig::default());
        }
        Err(source) => {
            return Err(ConfigError::ReadFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    match path.extension().and_then(|extension| extension.to_str()) {
        Some("yaml") | Some("yml") => serde_yaml::from_str(&raw).map_err(|source| {
            ConfigError::ParseYaml {
                path: path.to_path_buf(),
                source,
            }
        }),
        Some("toml") => toml::from_str(&raw).map_err(|source| ConfigError::ParseToml {
            path: path.to_path_buf(),
            source,
        }),
        _ => Err(ConfigError::UnsupportedExtension(path.to_path_buf())),
    }
}

fn parse_env<T>(
    env: &BTreeMap<String, String>,
    name: &'static str,
    default: T,
) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    match env.get(name) {
        Some(value) => value.parse::<T>().map_err(|_| ConfigError::InvalidEnvValue {
            name,
            value: value.clone(),
        }),
        None => Ok(default),
    }
}

/// Update provider fields in the config file.  Creates the file (and parent
/// directories) when it does not exist yet.  Only the supplied `Some` fields
/// are written; `None` fields are left untouched.
pub fn update_provider_in_file(
    config_path: &Path,
    backend: Option<&str>,
    model: Option<&str>,
    base_url: Option<Option<&str>>,
    api_key_env: Option<Option<&str>>,
) -> Result<(), ConfigError> {
    // Read existing partial config (or start fresh).
    let mut file_config = read_config_file(config_path)?;

    let provider = file_config.provider.get_or_insert_with(FileProviderConfig::default);

    if let Some(b) = backend {
        provider.backend = Some(b.to_owned());
    }
    if let Some(m) = model {
        provider.model = Some(m.to_owned());
    }
    if let Some(url) = base_url {
        provider.base_url = url.map(str::to_owned);
    }
    if let Some(key) = api_key_env {
        provider.api_key_env = key.map(str::to_owned);
    }

    write_file_config(config_path, &file_config)
}

/// Parse `value` into `$ty` and assign `Some(v)` to `$target`, or return a
/// `ConfigError::InvalidEnvValue` on parse failure.
macro_rules! parse_and_set {
    ($value:expr, $key:expr, $ty:ty, $target:expr) => {{
        let val = $value;
        let v: $ty = val.parse().map_err(|_| ConfigError::InvalidEnvValue {
            name: $key,
            value: val.to_owned(),
        })?;
        $target = Some(v);
    }};
}

/// Set a configuration value using dot-notation keys.
///
/// Supported keys:
///   profile, provider.backend, provider.model, provider.base_url,
///   provider.api_key_env, provider.tool_call_parser,
///   runtime.max_turns, runtime.max_concurrency,
///   runtime.allow_destructive_tools, runtime.max_context_messages,
///   runtime.thinking_budget, runtime.max_context_tokens, runtime.max_iterations,
///   runtime.reasoning_effort,
///   gateway.idle_timeout_minutes, gateway.daily_reset_hour, gateway.rate_limit_rpm
pub fn set_value_in_file(config_path: &Path, key: &str, value: &str) -> Result<(), ConfigError> {
    let mut file_config = read_config_file(config_path)?;

    match key {
        "profile" => {
            file_config.profile = Some(value.to_owned());
        }
        "provider.backend" => {
            file_config
                .provider
                .get_or_insert_with(FileProviderConfig::default)
                .backend = Some(value.to_owned());
        }
        "provider.model" => {
            file_config
                .provider
                .get_or_insert_with(FileProviderConfig::default)
                .model = Some(value.to_owned());
        }
        "provider.base_url" => {
            file_config
                .provider
                .get_or_insert_with(FileProviderConfig::default)
                .base_url = Some(value.to_owned());
        }
        "provider.api_key_env" => {
            file_config
                .provider
                .get_or_insert_with(FileProviderConfig::default)
                .api_key_env = Some(value.to_owned());
        }
        "provider.tool_call_parser" => {
            file_config
                .provider
                .get_or_insert_with(FileProviderConfig::default)
                .tool_call_parser = Some(value.to_owned());
        }
        "runtime.max_turns" => parse_and_set!(
            value, "runtime.max_turns", usize,
            file_config.runtime.get_or_insert_with(FileRuntimeConfig::default).max_turns
        ),
        "runtime.max_concurrency" => parse_and_set!(
            value, "runtime.max_concurrency", usize,
            file_config.runtime.get_or_insert_with(FileRuntimeConfig::default).max_concurrency
        ),
        "runtime.allow_destructive_tools" => parse_and_set!(
            value, "runtime.allow_destructive_tools", bool,
            file_config.runtime.get_or_insert_with(FileRuntimeConfig::default).allow_destructive_tools
        ),
        "runtime.max_context_messages" => parse_and_set!(
            value, "runtime.max_context_messages", usize,
            file_config.runtime.get_or_insert_with(FileRuntimeConfig::default).max_context_messages
        ),
        "runtime.thinking_budget" => parse_and_set!(
            value, "runtime.thinking_budget", u32,
            file_config.runtime.get_or_insert_with(FileRuntimeConfig::default).thinking_budget
        ),
        "runtime.max_context_tokens" => parse_and_set!(
            value, "runtime.max_context_tokens", u32,
            file_config.runtime.get_or_insert_with(FileRuntimeConfig::default).max_context_tokens
        ),
        "runtime.max_iterations" => parse_and_set!(
            value, "runtime.max_iterations", usize,
            file_config.runtime.get_or_insert_with(FileRuntimeConfig::default).max_iterations
        ),
        "runtime.reasoning_effort" => {
            let effort: ReasoningEffort = match value.to_ascii_lowercase().as_str() {
                "high" => ReasoningEffort::High,
                "medium" => ReasoningEffort::Medium,
                "low" => ReasoningEffort::Low,
                _ => {
                    return Err(ConfigError::InvalidEnvValue {
                        name: "runtime.reasoning_effort",
                        value: value.to_owned(),
                    })
                }
            };
            file_config
                .runtime
                .get_or_insert_with(FileRuntimeConfig::default)
                .reasoning_effort = Some(effort);
        }
        "gateway.idle_timeout_minutes" => parse_and_set!(
            value, "gateway.idle_timeout_minutes", u64,
            file_config.gateway.get_or_insert_with(GatewayConfig::default).idle_timeout_minutes
        ),
        "gateway.daily_reset_hour" => {
            parse_and_set!(
                value, "gateway.daily_reset_hour", u8,
                file_config.gateway.get_or_insert_with(GatewayConfig::default).daily_reset_hour
            );
            // Extra validation: hour must be 0..=23.
            if file_config.gateway.as_ref().expect("just set above").daily_reset_hour.expect("just set above") >= 24 {
                return Err(ConfigError::InvalidEnvValue {
                    name: "gateway.daily_reset_hour",
                    value: value.to_owned(),
                });
            }
        }
        "gateway.rate_limit_rpm" => parse_and_set!(
            value, "gateway.rate_limit_rpm", u32,
            file_config.gateway.get_or_insert_with(GatewayConfig::default).rate_limit_rpm
        ),
        _ => {
            return Err(ConfigError::InvalidEnvValue {
                name: "key",
                value: format!(
                    "unknown key `{key}`. Supported: profile, provider.backend, provider.model, \
                     provider.base_url, provider.api_key_env, provider.tool_call_parser, \
                     runtime.max_turns, runtime.max_concurrency, \
                     runtime.allow_destructive_tools, runtime.max_context_messages, \
                     runtime.thinking_budget, runtime.max_context_tokens, \
                     runtime.max_iterations, runtime.reasoning_effort, \
                     gateway.idle_timeout_minutes, gateway.daily_reset_hour, \
                     gateway.rate_limit_rpm"
                ),
            });
        }
    }

    write_file_config(config_path, &file_config)
}

fn write_file_config(path: &Path, file_config: &FileConfig) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::WriteFile {
            path: path.to_path_buf(),
            source,
        })?;
    }

    let yaml =
        serde_yaml::to_string(file_config).map_err(|source| ConfigError::SerializeYaml { source })?;
    fs::write(path, yaml).map_err(|source| ConfigError::WriteFile {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{load, load_from_map};
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn defaults_to_rust_native_paths_when_no_file_exists() {
        let config = load_from_map(None, &BTreeMap::new()).expect("config should load");

        assert_eq!(config.config.profile, "default");
        assert_eq!(config.config.provider.backend, "openai");
        assert_eq!(config.config.provider.model, "gpt-4.1-mini");
        assert!(config.paths.config_path.ends_with("genesis/config.yaml"));
        assert!(config.paths.database_path.ends_with("genesis/genesis.db"));
    }

    #[test]
    fn merges_yaml_config_with_env_overrides() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
schema_version: 3
profile: operator
provider:
  backend: openrouter
  model: moonshotai/kimi-k2
runtime:
  max_concurrency: 9
"#,
        )
        .expect("config file should be written");

        let env = BTreeMap::from([
            ("GENESIS_MODEL".to_owned(), "gpt-5".to_owned()),
            (
                "GENESIS_DATABASE_PATH".to_owned(),
                dir.path().join("custom.db").display().to_string(),
            ),
        ]);

        let loaded =
            load_from_map(Some(&config_path), &env).expect("config should merge file and env");

        assert_eq!(loaded.config.schema_version, 3);
        assert_eq!(loaded.config.profile, "operator");
        assert_eq!(loaded.config.provider.backend, "openrouter");
        assert_eq!(loaded.config.provider.model, "gpt-5");
        assert!(loaded.config.storage.database_path.ends_with("custom.db"));
        assert_eq!(loaded.config.runtime.max_concurrency, 9);
    }

    #[test]
    fn renders_example_yaml_with_expected_defaults() {
        let rendered = super::render_example_yaml(None).expect("yaml should render");

        assert!(rendered.contains("schema_version: 1"));
        assert!(rendered.contains("profile: default"));
        assert!(rendered.contains("backend: openai"));
        assert!(rendered.contains("model: gpt-4.1-mini"));
    }

    #[test]
    fn update_provider_creates_config_file_when_missing() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("subdir").join("config.yaml");

        super::update_provider_in_file(
            &config_path,
            Some("anthropic"),
            Some("claude-sonnet-4-6"),
            None,
            None,
        )
        .expect("update should succeed");

        let contents = fs::read_to_string(&config_path).expect("file should exist");
        assert!(contents.contains("backend: anthropic"));
        assert!(contents.contains("model: claude-sonnet-4-6"));
    }

    #[test]
    fn update_provider_preserves_existing_fields() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            "profile: operator\nprovider:\n  backend: openai\n  model: gpt-4.1-mini\n",
        )
        .expect("initial write");

        super::update_provider_in_file(
            &config_path,
            None,
            Some("gpt-5"),
            None,
            None,
        )
        .expect("update should succeed");

        let loaded = load_from_map(Some(&config_path), &std::collections::BTreeMap::new())
            .expect("reload should work");
        assert_eq!(loaded.config.profile, "operator");
        assert_eq!(loaded.config.provider.backend, "openai");
        assert_eq!(loaded.config.provider.model, "gpt-5");
    }

    #[test]
    fn update_provider_changes_both_backend_and_model() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "provider:\n  backend: openai\n  model: gpt-4.1-mini\n")
            .expect("initial write");

        super::update_provider_in_file(
            &config_path,
            Some("openrouter"),
            Some("nous/hermes-3"),
            Some(Some("https://openrouter.ai/api/v1")),
            None,
        )
        .expect("update should succeed");

        let loaded = load_from_map(Some(&config_path), &std::collections::BTreeMap::new())
            .expect("reload should work");
        assert_eq!(loaded.config.provider.backend, "openrouter");
        assert_eq!(loaded.config.provider.model, "nous/hermes-3");
        assert_eq!(
            loaded.config.provider.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    fn tool_provider_parsed_from_config_file() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
provider:
  backend: openrouter
  model: anthropic/claude-sonnet-4-6
  api_key_env: OPENROUTER_API_KEY
tool_provider:
  model: openai/gpt-4.1-mini
"#,
        )
        .expect("config file should be written");

        let loaded =
            load_from_map(Some(&config_path), &BTreeMap::new()).expect("config should load");

        let tp = loaded.config.tool_provider.expect("tool_provider should be set");
        assert_eq!(tp.model, "openai/gpt-4.1-mini");
        // Should inherit backend from primary provider
        assert_eq!(tp.backend, "openrouter");
        // Should inherit api_key_env from primary provider
        assert_eq!(tp.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
    }

    #[test]
    fn tool_provider_absent_when_not_configured() {
        let config = load_from_map(None, &BTreeMap::new()).expect("config should load");
        assert!(config.config.tool_provider.is_none());
    }

    #[test]
    fn fallback_providers_parsed_from_config_file() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
provider:
  backend: openrouter
  model: anthropic/claude-sonnet-4-6
  api_key_env: OPENROUTER_API_KEY
fallback_providers:
  - backend: openai
    model: gpt-4.1
    api_key_env: OPENAI_API_KEY
  - model: anthropic/claude-haiku-4-5
"#,
        )
        .expect("write config");

        let loaded =
            load_from_map(Some(&config_path), &BTreeMap::new()).expect("config should load");

        assert_eq!(loaded.config.fallback_providers.len(), 2);
        let first = &loaded.config.fallback_providers[0];
        assert_eq!(first.backend, "openai");
        assert_eq!(first.model, "gpt-4.1");
        assert_eq!(first.api_key_env.as_deref(), Some("OPENAI_API_KEY"));

        // Second inherits backend from primary provider
        let second = &loaded.config.fallback_providers[1];
        assert_eq!(second.backend, "openrouter");
        assert_eq!(second.model, "anthropic/claude-haiku-4-5");
    }

    #[test]
    fn fallback_providers_empty_when_not_configured() {
        let config = load_from_map(None, &BTreeMap::new()).expect("config should load");
        assert!(config.config.fallback_providers.is_empty());
    }

    #[test]
    fn mcp_servers_parsed_from_config_file() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
provider:
  backend: openai
  model: gpt-4.1-mini
mcp_servers:
  filesystem:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
  github:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_TOKEN: ghp_xxx
  remote_db:
    url: https://mcp.example.com/db
    headers:
      Authorization: Bearer sk-xxx
    timeout: 180
"#,
        )
        .expect("config file should be written");

        let loaded =
            load_from_map(Some(&config_path), &BTreeMap::new()).expect("config should load");

        assert_eq!(loaded.config.mcp_servers.len(), 3);

        let fs_server = &loaded.config.mcp_servers["filesystem"];
        assert_eq!(fs_server.command.as_deref(), Some("npx"));
        assert_eq!(fs_server.args.as_ref().unwrap().len(), 3);

        let gh_server = &loaded.config.mcp_servers["github"];
        assert_eq!(
            gh_server.env.as_ref().unwrap().get("GITHUB_TOKEN").unwrap(),
            "ghp_xxx"
        );

        let db_server = &loaded.config.mcp_servers["remote_db"];
        assert_eq!(db_server.url.as_deref(), Some("https://mcp.example.com/db"));
        assert_eq!(db_server.timeout, Some(180));
        assert!(db_server.command.is_none());
    }

    #[test]
    fn mcp_servers_empty_when_not_configured() {
        let config = load_from_map(None, &BTreeMap::new()).expect("config should load");
        assert!(config.config.mcp_servers.is_empty());
    }

    #[test]
    fn set_value_in_file_sets_provider_model() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "provider:\n  backend: openai\n  model: gpt-4.1-mini\n")
            .expect("initial write");

        super::set_value_in_file(&config_path, "provider.model", "gpt-5")
            .expect("set should succeed");

        let loaded = load_from_map(Some(&config_path), &BTreeMap::new())
            .expect("reload should work");
        assert_eq!(loaded.config.provider.model, "gpt-5");
        assert_eq!(loaded.config.provider.backend, "openai");
    }

    #[test]
    fn set_value_in_file_sets_runtime_max_turns() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "").expect("initial write");

        super::set_value_in_file(&config_path, "runtime.max_turns", "50")
            .expect("set should succeed");

        let loaded = load_from_map(Some(&config_path), &BTreeMap::new())
            .expect("reload should work");
        assert_eq!(loaded.config.runtime.max_turns, 50);
    }

    #[test]
    fn set_value_in_file_sets_thinking_budget() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "").expect("initial write");

        super::set_value_in_file(&config_path, "runtime.thinking_budget", "4096")
            .expect("set should succeed");

        let loaded = load_from_map(Some(&config_path), &BTreeMap::new())
            .expect("reload should work");
        assert_eq!(loaded.config.runtime.thinking_budget, Some(4096));
    }

    #[test]
    fn set_value_in_file_sets_gateway_idle_timeout() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "").expect("initial write");

        super::set_value_in_file(&config_path, "gateway.idle_timeout_minutes", "120")
            .expect("set should succeed");

        let loaded = load_from_map(Some(&config_path), &BTreeMap::new())
            .expect("reload should work");
        let gw = loaded.config.gateway.expect("gateway should be set");
        assert_eq!(gw.idle_timeout_minutes, Some(120));
        assert_eq!(gw.daily_reset_hour, None);
    }

    #[test]
    fn set_value_in_file_rejects_invalid_reset_hour() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "").expect("initial write");

        let result = super::set_value_in_file(&config_path, "gateway.daily_reset_hour", "25");
        assert!(result.is_err());
    }

    #[test]
    fn set_value_in_file_rejects_unknown_key() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "").expect("initial write");

        let result = super::set_value_in_file(&config_path, "nonexistent.key", "value");
        assert!(result.is_err());
    }

    #[test]
    fn set_value_in_file_rejects_invalid_number() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "").expect("initial write");

        let result = super::set_value_in_file(&config_path, "runtime.max_turns", "not_a_number");
        assert!(result.is_err());
    }

    #[test]
    fn gateway_config_parsed_from_file() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
provider:
  backend: openai
  model: gpt-4.1-mini
gateway:
  idle_timeout_minutes: 120
  daily_reset_hour: 6
"#,
        )
        .expect("config file should be written");

        let loaded =
            load_from_map(Some(&config_path), &BTreeMap::new()).expect("config should load");

        let gw = loaded.config.gateway.expect("gateway should be set");
        assert_eq!(gw.idle_timeout_minutes, Some(120));
        assert_eq!(gw.daily_reset_hour, Some(6));
    }

    #[test]
    fn gateway_config_absent_when_not_configured() {
        let config = load_from_map(None, &BTreeMap::new()).expect("config should load");
        assert!(config.config.gateway.is_none());
    }

    #[test]
    fn toolsets_parsed_from_yaml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("genesis.yaml");
        std::fs::write(
            &config_path,
            r#"
profile: default
provider:
  backend: openai
  model: gpt-4
  api_key_env: OPENAI_API_KEY
storage:
  data_dir: /tmp/genesis-data
  database_path: /tmp/genesis-data/genesis.db
runtime:
  max_concurrency: 4
  allow_destructive_tools: false
  max_turns: 20
toolsets:
  my-custom:
    shell_exec: 1.0
    read_file: 0.8
    write_file: 0.5
"#,
        )
        .unwrap();

        let loaded = load(Some(&config_path)).expect("should load");
        assert_eq!(loaded.config.toolsets.len(), 1);
        let custom = loaded.config.toolsets.get("my-custom").unwrap();
        assert_eq!(custom.get("shell_exec"), Some(&1.0));
        assert_eq!(custom.get("read_file"), Some(&0.8));
        assert_eq!(custom.get("write_file"), Some(&0.5));
    }

    #[test]
    fn toolsets_default_empty() {
        let config = load_from_map(None, &BTreeMap::new()).expect("config should load");
        assert!(config.config.toolsets.is_empty());
    }

    #[test]
    fn display_config_defaults_to_grouped() {
        use super::{DisplayConfig, ToolDisplayMode};
        let config: DisplayConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(config.tool_progress, ToolDisplayMode::Grouped);
    }

    #[test]
    fn display_config_parses_modes() {
        use super::{DisplayConfig, ToolDisplayMode};
        let config: DisplayConfig = serde_yaml::from_str("tool_progress: verbose").unwrap();
        assert_eq!(config.tool_progress, ToolDisplayMode::Verbose);
    }

    #[test]
    fn terminal_config_singularity_round_trips() {
        let json = r#"{"backend":"singularity","image":"docker://ubuntu:22.04","cpu":2.0,"memory_mb":8192,"persistent":true}"#;
        let config: super::TerminalConfig = serde_json::from_str(json).unwrap();
        match &config {
            super::TerminalConfig::Singularity { image, cpu, memory_mb, persistent, .. } => {
                assert_eq!(image, "docker://ubuntu:22.04");
                assert_eq!(*cpu, 2.0);
                assert_eq!(*memory_mb, 8192);
                assert!(*persistent);
            }
            _ => panic!("expected Singularity"),
        }
    }

    #[test]
    fn terminal_config_modal_defaults() {
        let json = r#"{"backend":"modal"}"#;
        let config: super::TerminalConfig = serde_json::from_str(json).unwrap();
        match &config {
            super::TerminalConfig::Modal { cpu, memory_mb, disk_mb, persistent, .. } => {
                assert_eq!(*cpu, 1.0);
                assert_eq!(*memory_mb, 5120);
                assert_eq!(*disk_mb, 51200);
                assert!(*persistent);
            }
            _ => panic!("expected Modal"),
        }
    }

    #[test]
    fn terminal_config_daytona_round_trips() {
        let json = r#"{"backend":"daytona","image":"ubuntu:22.04","disk_mb":10240}"#;
        let config: super::TerminalConfig = serde_json::from_str(json).unwrap();
        match &config {
            super::TerminalConfig::Daytona { image, disk_mb, .. } => {
                assert_eq!(image.as_deref(), Some("ubuntu:22.04"));
                assert_eq!(*disk_mb, 10240);
            }
            _ => panic!("expected Daytona"),
        }
    }
}
