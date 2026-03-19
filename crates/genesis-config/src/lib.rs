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
    /// TUI (terminal user interface) settings.
    #[serde(default)]
    pub tui: TuiConfig,
    /// OpenTelemetry telemetry export configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryConfig>,
    /// Adaptive model routing configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<RoutingConfig>,
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

/// Granular toggles for individual visual effects.
/// When `TuiConfig::animations` is false, all effects are suppressed regardless
/// of these settings. These toggles allow fine-grained control for accessibility
/// (e.g. respecting REDUCE_MOTION at the effect level).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectsConfig {
    /// Show the boot sequence animation on startup.
    #[serde(default = "default_true")]
    pub boot_sequence: bool,
    /// Enable transition animations between UI states.
    #[serde(default = "default_true")]
    pub transitions: bool,
    /// Enable pulsing animation on status indicators.
    #[serde(default = "default_true")]
    pub status_pulse: bool,
    /// Enable idle glow effect on the input border.
    #[serde(default = "default_true")]
    pub idle_glow: bool,
    /// Enable breathing (fade in/out) animation while idle.
    #[serde(default = "default_true")]
    pub idle_breathing: bool,
    /// Enable the braille-dot particle canvas.
    #[serde(default = "default_true")]
    pub braille_canvas: bool,
}

impl Default for EffectsConfig {
    fn default() -> Self {
        Self {
            boot_sequence: true,
            transitions: true,
            status_pulse: true,
            idle_glow: true,
            idle_breathing: true,
            braille_canvas: true,
        }
    }
}

/// TUI (terminal user interface) configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TuiConfig {
    /// Whether TUI mode is enabled (default: true, --no-tui overrides).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Enable animations (Eve sway, status bar sprites).
    #[serde(default = "default_true")]
    pub animations: bool,
    /// Alternate screen mode for overlays.
    #[serde(default)]
    pub alt_screen: AltScreenMode,
    /// Show Eve welcome screen on launch.
    #[serde(default = "default_true")]
    pub welcome_screen: bool,
    /// Display settings.
    #[serde(default)]
    pub display: TuiDisplayConfig,
    /// Granular per-effect toggles (all default to true).
    #[serde(default)]
    pub effects: EffectsConfig,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            animations: true,
            alt_screen: AltScreenMode::default(),
            welcome_screen: true,
            display: TuiDisplayConfig::default(),
            effects: EffectsConfig::default(),
        }
    }
}

/// TUI display settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TuiDisplayConfig {
    /// How tool calls are displayed.
    #[serde(default)]
    pub tool_mode: ToolDisplayMode,
    /// How file diffs are displayed.
    #[serde(default)]
    pub diff_mode: DiffMode,
}

/// How file diffs are rendered in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiffMode {
    /// Auto-detect based on terminal width (side-by-side >= 120, unified otherwise).
    #[default]
    Auto,
    /// Unified diff format.
    Unified,
    /// Side-by-side diff format.
    SideBySide,
}

/// Alternate screen mode controlling whether the TUI uses the terminal's
/// alternate screen buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AltScreenMode {
    /// Auto-detect (disabled in Zellij, enabled elsewhere).
    #[default]
    Auto,
    /// Always use alternate screen.
    Always,
    /// Never use alternate screen.
    Never,
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
    /// Circuit breaker configuration for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_breaker: Option<CircuitBreakerConfig>,
}

/// Circuit breaker configuration for a provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before opening the circuit (default: 5).
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    /// Seconds to wait in Open state before probing (default: 30).
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
}

fn default_failure_threshold() -> u32 {
    5
}

fn default_cooldown_secs() -> u64 {
    30
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: default_failure_threshold(),
            cooldown_secs: default_cooldown_secs(),
        }
    }
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
    /// Core tool set sent to the LLM per request. When set, only these tools
    /// (plus any discovered via `find_tools`) are included in the request.
    /// Other tools remain available and can be discovered at runtime.
    /// Reduces input tokens by 85-96% for large tool registries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_tools: Option<Vec<String>>,
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

fn default_true() -> bool {
    true
}
fn default_cpu() -> f32 {
    1.0
}
fn default_memory() -> u32 {
    5120
}
fn default_disk() -> u32 {
    51200
}
fn default_daytona_disk() -> u32 {
    10240
}
fn default_cache_ttl() -> u32 {
    3600
}
fn default_cache_context_messages() -> usize {
    4
}

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

fn default_pii_action() -> String {
    "redact".to_owned()
}
fn default_applies_to() -> String {
    "both".to_owned()
}

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

fn default_embedding_backend() -> String {
    "openai".to_owned()
}
fn default_embedding_model() -> String {
    "text-embedding-3-small".to_owned()
}

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

fn default_max_retries() -> u32 {
    3
}
fn default_retry_backoff_ms() -> u64 {
    1000
}

/// OpenTelemetry telemetry export configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelemetryConfig {
    /// Whether telemetry export is enabled (default: true when section present).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// OTLP endpoint for trace export (default: http://localhost:4317).
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,
    /// Service name reported in traces (default: "genesis").
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4317".to_owned()
}

fn default_service_name() -> String {
    "genesis".to_owned()
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            otlp_endpoint: default_otlp_endpoint(),
            service_name: default_service_name(),
        }
    }
}

/// Adaptive model routing configuration.
///
/// Routes tasks to different model tiers based on complexity analysis.
/// When disabled (the default), the primary provider is always used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingConfig {
    /// Whether adaptive routing is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Model for simple/cheap tasks (short messages, basic Q&A).
    /// Falls back to primary provider model if not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap_model: Option<String>,
    /// Model for moderate tasks (code editing, tool use).
    /// Falls back to primary provider model if not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid_model: Option<String>,
    /// Model for complex tasks (multi-step reasoning, architecture).
    /// Falls back to primary provider model if not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_model: Option<String>,
    /// Default tier when classification is ambiguous.
    #[serde(default = "default_routing_tier")]
    pub default_tier: String,
}

fn default_routing_tier() -> String {
    "mid".to_owned()
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cheap_model: None,
            mid_model: None,
            top_model: None,
            default_tier: default_routing_tier(),
        }
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tui: Option<TuiConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    telemetry: Option<TelemetryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routing: Option<RoutingConfig>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    circuit_breaker: Option<CircuitBreakerConfig>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    core_tools: Option<Vec<String>>,
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
            circuit_breaker: None,
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
            core_tools: None,
        },
        gateway: None,
        toolsets: HashMap::new(),
        personality: None,
        embedding: None,
        display: DisplayConfig::default(),
        tui: TuiConfig::default(),
        telemetry: None,
        routing: None,
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
        .or_else(|| {
            file_config
                .storage
                .as_ref()
                .and_then(|storage| storage.data_dir.clone())
        })
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
        circuit_breaker: prov.and_then(|p| p.circuit_breaker.clone()),
    };

    // Optional tool provider — inherits primary provider defaults when partially specified.
    let tool_provider = file_config.tool_provider.as_ref().map(|tp| ProviderConfig {
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
        circuit_breaker: tp.circuit_breaker.clone(),
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
            model: fp.model.clone().unwrap_or_else(|| provider.model.clone()),
            base_url: fp.base_url.clone().or_else(|| provider.base_url.clone()),
            api_key_env: fp
                .api_key_env
                .clone()
                .or_else(|| provider.api_key_env.clone()),
            extra_body: fp.extra_body.clone(),
            tool_call_parser: fp.tool_call_parser.clone(),
            circuit_breaker: fp
                .circuit_breaker
                .clone()
                .or_else(|| provider.circuit_breaker.clone()),
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
        core_tools: rt.and_then(|r| r.core_tools.clone()),
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
            tui: file_config.tui.unwrap_or_default(),
            telemetry: file_config.telemetry,
            routing: file_config.routing,
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
        Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&raw).map_err(|source| ConfigError::ParseYaml {
                path: path.to_path_buf(),
                source,
            })
        }
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
        Some(value) => value
            .parse::<T>()
            .map_err(|_| ConfigError::InvalidEnvValue {
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

    let provider = file_config
        .provider
        .get_or_insert_with(FileProviderConfig::default);

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
            value,
            "runtime.max_turns",
            usize,
            file_config
                .runtime
                .get_or_insert_with(FileRuntimeConfig::default)
                .max_turns
        ),
        "runtime.max_concurrency" => parse_and_set!(
            value,
            "runtime.max_concurrency",
            usize,
            file_config
                .runtime
                .get_or_insert_with(FileRuntimeConfig::default)
                .max_concurrency
        ),
        "runtime.allow_destructive_tools" => parse_and_set!(
            value,
            "runtime.allow_destructive_tools",
            bool,
            file_config
                .runtime
                .get_or_insert_with(FileRuntimeConfig::default)
                .allow_destructive_tools
        ),
        "runtime.max_context_messages" => parse_and_set!(
            value,
            "runtime.max_context_messages",
            usize,
            file_config
                .runtime
                .get_or_insert_with(FileRuntimeConfig::default)
                .max_context_messages
        ),
        "runtime.thinking_budget" => parse_and_set!(
            value,
            "runtime.thinking_budget",
            u32,
            file_config
                .runtime
                .get_or_insert_with(FileRuntimeConfig::default)
                .thinking_budget
        ),
        "runtime.max_context_tokens" => parse_and_set!(
            value,
            "runtime.max_context_tokens",
            u32,
            file_config
                .runtime
                .get_or_insert_with(FileRuntimeConfig::default)
                .max_context_tokens
        ),
        "runtime.max_iterations" => parse_and_set!(
            value,
            "runtime.max_iterations",
            usize,
            file_config
                .runtime
                .get_or_insert_with(FileRuntimeConfig::default)
                .max_iterations
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
            value,
            "gateway.idle_timeout_minutes",
            u64,
            file_config
                .gateway
                .get_or_insert_with(GatewayConfig::default)
                .idle_timeout_minutes
        ),
        "gateway.daily_reset_hour" => {
            parse_and_set!(
                value,
                "gateway.daily_reset_hour",
                u8,
                file_config
                    .gateway
                    .get_or_insert_with(GatewayConfig::default)
                    .daily_reset_hour
            );
            // Extra validation: hour must be 0..=23.
            if let Some(gw) = file_config.gateway.as_ref() {
                if gw.daily_reset_hour.is_some_and(|h| h >= 24) {
                    return Err(ConfigError::InvalidEnvValue {
                        name: "gateway.daily_reset_hour",
                        value: value.to_owned(),
                    });
                }
            }
        }
        "gateway.rate_limit_rpm" => parse_and_set!(
            value,
            "gateway.rate_limit_rpm",
            u32,
            file_config
                .gateway
                .get_or_insert_with(GatewayConfig::default)
                .rate_limit_rpm
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

    let yaml = serde_yaml::to_string(file_config)
        .map_err(|source| ConfigError::SerializeYaml { source })?;
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

        super::update_provider_in_file(&config_path, None, Some("gpt-5"), None, None)
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
        fs::write(
            &config_path,
            "provider:\n  backend: openai\n  model: gpt-4.1-mini\n",
        )
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

        let tp = loaded
            .config
            .tool_provider
            .expect("tool_provider should be set");
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
        fs::write(
            &config_path,
            "provider:\n  backend: openai\n  model: gpt-4.1-mini\n",
        )
        .expect("initial write");

        super::set_value_in_file(&config_path, "provider.model", "gpt-5")
            .expect("set should succeed");

        let loaded =
            load_from_map(Some(&config_path), &BTreeMap::new()).expect("reload should work");
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

        let loaded =
            load_from_map(Some(&config_path), &BTreeMap::new()).expect("reload should work");
        assert_eq!(loaded.config.runtime.max_turns, 50);
    }

    #[test]
    fn set_value_in_file_sets_thinking_budget() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "").expect("initial write");

        super::set_value_in_file(&config_path, "runtime.thinking_budget", "4096")
            .expect("set should succeed");

        let loaded =
            load_from_map(Some(&config_path), &BTreeMap::new()).expect("reload should work");
        assert_eq!(loaded.config.runtime.thinking_budget, Some(4096));
    }

    #[test]
    fn set_value_in_file_sets_gateway_idle_timeout() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(&config_path, "").expect("initial write");

        super::set_value_in_file(&config_path, "gateway.idle_timeout_minutes", "120")
            .expect("set should succeed");

        let loaded =
            load_from_map(Some(&config_path), &BTreeMap::new()).expect("reload should work");
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
            super::TerminalConfig::Singularity {
                image,
                cpu,
                memory_mb,
                persistent,
                ..
            } => {
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
            super::TerminalConfig::Modal {
                cpu,
                memory_mb,
                disk_mb,
                persistent,
                ..
            } => {
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

    #[test]
    fn tui_config_defaults() {
        let config: super::TuiConfig = serde_json::from_str("{}").unwrap();
        assert!(config.enabled);
        assert!(config.animations);
        assert!(config.welcome_screen);
        assert!(matches!(config.display.tool_mode, super::ToolDisplayMode::Grouped));
        assert!(matches!(config.alt_screen, super::AltScreenMode::Auto));
        assert!(matches!(config.display.diff_mode, super::DiffMode::Auto));
    }

    #[test]
    fn effects_config_defaults_all_true() {
        let config: super::EffectsConfig = serde_yaml::from_str("{}").unwrap();
        assert!(config.boot_sequence);
        assert!(config.transitions);
        assert!(config.status_pulse);
        assert!(config.idle_glow);
        assert!(config.idle_breathing);
        assert!(config.braille_canvas);
    }

    #[test]
    fn tui_config_deserializes_from_toml() {
        let toml = r#"
[tui]
enabled = false
animations = false
[tui.display]
tool_mode = "verbose"
"#;
        #[derive(serde::Deserialize)]
        struct Wrapper {
            tui: super::TuiConfig,
        }
        let wrapper: Wrapper = toml::from_str(toml).unwrap();
        let config = wrapper.tui;
        assert!(!config.enabled);
        assert!(!config.animations);
        assert!(matches!(config.display.tool_mode, super::ToolDisplayMode::Verbose));
    }

    #[test]
    fn tool_display_mode_round_trips() {
        use super::ToolDisplayMode;
        let variants = [
            (ToolDisplayMode::Off, "\"off\""),
            (ToolDisplayMode::Summary, "\"summary\""),
            (ToolDisplayMode::Grouped, "\"grouped\""),
            (ToolDisplayMode::Verbose, "\"verbose\""),
        ];
        for (variant, expected_json) in variants {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, expected_json);
            let deserialized: ToolDisplayMode = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn diff_mode_round_trips() {
        use super::DiffMode;
        let variants = [
            (DiffMode::Auto, "\"auto\""),
            (DiffMode::Unified, "\"unified\""),
            (DiffMode::SideBySide, "\"side_by_side\""),
        ];
        for (variant, expected_json) in variants {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, expected_json);
            let deserialized: DiffMode = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn alt_screen_mode_round_trips() {
        use super::AltScreenMode;
        let variants = [
            (AltScreenMode::Auto, "\"auto\""),
            (AltScreenMode::Always, "\"always\""),
            (AltScreenMode::Never, "\"never\""),
        ];
        for (variant, expected_json) in variants {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, expected_json);
            let deserialized: AltScreenMode = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn routing_config_defaults() {
        let cfg = super::RoutingConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.cheap_model.is_none());
        assert!(cfg.mid_model.is_none());
        assert!(cfg.top_model.is_none());
        assert_eq!(cfg.default_tier, "mid");
    }

    #[test]
    fn routing_config_from_yaml() {
        let yaml = r#"
routing:
  enabled: true
  cheap_model: "haiku-4.5"
  mid_model: "sonnet-4.6"
  top_model: "opus-4.6"
  default_tier: "cheap"
"#;
        let val: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let routing: super::RoutingConfig =
            serde_yaml::from_value(val["routing"].clone()).unwrap();
        assert!(routing.enabled);
        assert_eq!(routing.cheap_model.as_deref(), Some("haiku-4.5"));
        assert_eq!(routing.top_model.as_deref(), Some("opus-4.6"));
        assert_eq!(routing.default_tier, "cheap");
    }
}
