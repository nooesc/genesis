pub mod agent_bus;
pub mod agent_loop;
pub mod audit;
pub mod compress;
pub mod context_security;
pub mod dataset;
pub mod cost;
pub mod delivery;
pub mod eval;
pub mod execution;
pub mod guardrails;
pub mod hooks;
pub mod moa;
pub mod nudge;
pub mod prompt;
pub mod replay;
pub mod sanitize;
pub mod scheduler;
pub mod skill_sync;
pub mod skills;
pub mod skills_hub;
pub mod quality;
pub mod redact;
pub mod skill_manifest;
pub mod personality;
pub mod tagger;
pub mod templates;
pub mod toolset;
pub mod workflow;
pub mod trajectory;

use std::path::Path;
use std::sync::Arc;

use genesis_config::{load, GenesisConfig, LoadedConfig};
use genesis_provider::resolve;
use genesis_storage::{bootstrap, inspect, SessionStore, StorageHealth};
use genesis_mcp::McpManager;
use genesis_tools::{
    default_registry, ApprovalPolicy, ToolCall, ToolContext, ToolError, ToolHandler,
    ToolOutput, ToolRegistry,
};
use genesis_types::{DeliveryPlatform, ModelProviderKind, ModelSelection, RuntimeEvent, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorReport {
    pub profile: String,
    pub config_path: String,
    pub data_dir: String,
    pub database_path: String,
    pub provider_backend: String,
    pub model: String,
    pub storage: StorageHealth,
    pub checks: Vec<DoctorCheck>,
    pub next_event_preview: RuntimeEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPlan {
    pub session_id: String,
    pub profile: String,
    pub platform: DeliveryPlatform,
    pub model: ModelSelection,
    pub initial_events: Vec<RuntimeEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionContext {
    pub plan: SessionPlan,
    pub data_dir: String,
    pub database_path: String,
    pub max_concurrency: usize,
    pub allow_destructive_tools: bool,
}

#[derive(Clone)]
pub struct ToolRuntime {
    registry: ToolRegistry,
    context: ToolContext,
    mcp: Option<Arc<McpManager>>,
}

#[derive(Debug, Error)]
pub enum DoctorError {
    #[error(transparent)]
    Config(#[from] genesis_config::ConfigError),
    #[error(transparent)]
    Storage(#[from] genesis_storage::StorageError),
}

#[derive(Debug, Error)]
pub enum RuntimeContextError {
    #[error(transparent)]
    Config(#[from] genesis_config::ConfigError),
}

pub fn run_doctor(
    config_path_override: Option<&Path>,
    bootstrap_storage: bool,
) -> Result<DoctorReport, DoctorError> {
    let loaded = load(config_path_override)?;
    build_doctor_report(loaded, bootstrap_storage)
}

pub fn build_execution_context(
    config_path_override: Option<&Path>,
    session_id: impl Into<String>,
    platform: DeliveryPlatform,
) -> Result<ExecutionContext, RuntimeContextError> {
    let loaded = load(config_path_override)?;
    Ok(build_execution_context_from_loaded(
        &loaded,
        session_id.into(),
        platform,
    ))
}

pub fn build_execution_context_from_loaded(
    loaded: &LoadedConfig,
    session_id: String,
    platform: DeliveryPlatform,
) -> ExecutionContext {
    let plan = SessionPlan::from_config(&loaded.config, session_id, platform.clone());

    ExecutionContext {
        plan,
        data_dir: loaded.config.storage.data_dir.display().to_string(),
        database_path: loaded.config.storage.database_path.display().to_string(),
        max_concurrency: loaded.config.runtime.max_concurrency,
        allow_destructive_tools: loaded.config.runtime.allow_destructive_tools,
    }
}

pub fn build_default_tool_runtime(execution_context: &ExecutionContext) -> ToolRuntime {
    let mut registry = default_registry();

    // Register MoA tool — handled async in ToolRuntime::execute_async
    registry.register(
        ToolDefinition {
            name: "moa_consult".to_owned(),
            description: "Consult multiple AI models for diverse perspectives on a question. \
                Runs proposer models in parallel, then synthesizes their responses into a \
                single high-quality answer. Use for complex decisions, nuanced analysis, or \
                when you want diverse viewpoints before answering."
                .to_owned(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The question or task to consult multiple models about"
                    },
                    "system_prompt": {
                        "type": "string",
                        "description": "Optional system instructions for all models"
                    },
                    "models": {
                        "type": "string",
                        "description": "Comma-separated list of model identifiers (e.g., 'gpt-4.1,claude-sonnet-4-20250514,gemini-2.5-pro'). Uses default config if omitted."
                    },
                    "layers": {
                        "type": "integer",
                        "description": "Number of refinement layers (default: 1). More layers = higher quality but slower."
                    }
                },
                "required": ["prompt"]
            })),
        },
        ApprovalPolicy::Never,
        MoaToolPlaceholder,
    );

    ToolRuntime {
        registry,
        context: ToolContext {
            session_id: execution_context.plan.session_id.clone(),
            profile: execution_context.plan.profile.clone(),
            data_dir: execution_context.data_dir.clone(),
            allow_destructive_tools: execution_context.allow_destructive_tools,
            terminal_backend: None,
            default_working_dir: None,
        },
        mcp: None,
    }
}

/// Placeholder handler for the MoA tool — actual execution is async via ToolRuntime.
struct MoaToolPlaceholder;

impl ToolHandler for MoaToolPlaceholder {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: "moa_consult requires async execution".to_owned(),
        })
    }
}

fn build_doctor_report(
    loaded: LoadedConfig,
    bootstrap_storage_enabled: bool,
) -> Result<DoctorReport, DoctorError> {
    if bootstrap_storage_enabled {
        bootstrap(&loaded.config.storage.database_path)?;
    }

    let storage = inspect(&loaded.config.storage.database_path)?;
    let mut checks = vec![
        DoctorCheck {
            name: "config_path".to_owned(),
            status: CheckStatus::Pass,
            detail: loaded.paths.config_path.display().to_string(),
        },
        DoctorCheck {
            name: "provider".to_owned(),
            status: CheckStatus::Pass,
            detail: format!(
                "{} / {}",
                loaded.config.provider.backend, loaded.config.provider.model
            ),
        },
    ];

    // Check API key resolution
    checks.push(check_api_key(&loaded));

    // Check tool registry
    checks.push(check_tool_registry());

    // Storage check
    checks.push(match storage.database_exists {
        true => DoctorCheck {
            name: "storage".to_owned(),
            status: CheckStatus::Pass,
            detail: format!(
                "sqlite schema version {}",
                storage.schema_version.unwrap_or_default()
            ),
        },
        false => DoctorCheck {
            name: "storage".to_owned(),
            status: CheckStatus::Warn,
            detail: "database not bootstrapped yet; rerun with --bootstrap-storage".to_owned(),
        },
    });

    // Storage stats (sessions, skills, schedules) when DB exists
    if storage.database_exists {
        checks.push(check_storage_stats(&loaded.config.storage.database_path));
        checks.push(check_database_integrity(&loaded.config.storage.database_path));
    }

    // MCP servers
    checks.push(check_mcp_servers(&loaded));

    Ok(DoctorReport {
        profile: loaded.config.profile,
        config_path: loaded.paths.config_path.display().to_string(),
        data_dir: loaded.config.storage.data_dir.display().to_string(),
        database_path: loaded
            .config
            .storage
            .database_path
            .display()
            .to_string(),
        provider_backend: loaded.config.provider.backend,
        model: loaded.config.provider.model,
        storage,
        checks,
        next_event_preview: RuntimeEvent::SessionStarted {
            session_id: "eve-bootstrap-preview".to_owned(),
        },
    })
}

fn mask_api_key(key: &str) -> String {
    if key.len() > 8 {
        format!("{}...", &key[..8])
    } else {
        "****".to_owned()
    }
}

/// Check whether the configured API key resolves to a non-empty value.
fn check_api_key(loaded: &LoadedConfig) -> DoctorCheck {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let resolved = resolve(
        &loaded.config.provider.backend,
        &loaded.config.provider.model,
        loaded.config.provider.base_url.as_deref(),
        loaded.config.provider.api_key_env.as_deref(),
        &env,
    );

    // Check OAuth auth store for openai-codex backend
    if resolved.api_key.is_empty()
        && loaded.config.provider.backend.eq_ignore_ascii_case("openai-codex")
    {
        if let Ok(auth_path) = genesis_auth::default_auth_path() {
            if let Ok(store) = genesis_auth::store::read_store(&auth_path) {
                if let Some(codex) = genesis_auth::store::get_codex_state(&store) {
                    let masked = mask_api_key(&codex.tokens.access_token);

                    let expiry_info =
                        if genesis_auth::jwt::is_expiring(&codex.tokens.access_token, 0) {
                            " (token expired — will refresh on next call)"
                        } else {
                            ""
                        };

                    return DoctorCheck {
                        name: "api_key".to_owned(),
                        status: CheckStatus::Pass,
                        detail: format!("openai-codex: OAuth ({masked}){expiry_info}"),
                    };
                }
            }
        }

        return DoctorCheck {
            name: "api_key".to_owned(),
            status: CheckStatus::Warn,
            detail: "openai-codex configured but not logged in — run `genesis login`".to_owned(),
        };
    }

    if resolved.api_key.is_empty() {
        DoctorCheck {
            name: "api_key".to_owned(),
            status: CheckStatus::Warn,
            detail: format!(
                "no API key found for backend '{}'; set the appropriate env var",
                loaded.config.provider.backend
            ),
        }
    } else {
        let masked = mask_api_key(&resolved.api_key);
        DoctorCheck {
            name: "api_key".to_owned(),
            status: CheckStatus::Pass,
            detail: format!("resolved ({})", masked),
        }
    }
}

/// Verify the tool registry loads successfully and report tool count.
fn check_tool_registry() -> DoctorCheck {
    let registry = default_registry();
    let count = registry.definitions().len();
    DoctorCheck {
        name: "tools".to_owned(),
        status: CheckStatus::Pass,
        detail: format!("{count} builtin tools registered"),
    }
}

/// Gather storage statistics: session, skill, and schedule counts.
fn check_storage_stats(db_path: &Path) -> DoctorCheck {
    let sessions = SessionStore::new(db_path).count_sessions().unwrap_or(0);

    // Use direct COUNT(*) queries to avoid loading full rows
    let (skills, schedules) = match rusqlite::Connection::open(db_path) {
        Ok(conn) => {
            let skills: i64 = conn
                .query_row("SELECT COUNT(*) FROM skills", [], |r| r.get(0))
                .unwrap_or(0);
            let schedules: i64 = conn
                .query_row("SELECT COUNT(*) FROM schedules", [], |r| r.get(0))
                .unwrap_or(0);
            (skills as u64, schedules as u64)
        }
        Err(_) => (0, 0),
    };

    DoctorCheck {
        name: "storage_stats".to_owned(),
        status: CheckStatus::Pass,
        detail: format!("{sessions} sessions, {skills} skills, {schedules} schedules"),
    }
}

/// Run PRAGMA integrity_check on the database.
fn check_database_integrity(db_path: &Path) -> DoctorCheck {
    match rusqlite::Connection::open(db_path) {
        Ok(conn) => {
            match conn.query_row("PRAGMA integrity_check", [], |row| {
                row.get::<_, String>(0)
            }) {
                Ok(result) if result == "ok" => DoctorCheck {
                    name: "db_integrity".to_owned(),
                    status: CheckStatus::Pass,
                    detail: "integrity check passed".to_owned(),
                },
                Ok(result) => DoctorCheck {
                    name: "db_integrity".to_owned(),
                    status: CheckStatus::Fail,
                    detail: format!("integrity issue: {result}"),
                },
                Err(e) => DoctorCheck {
                    name: "db_integrity".to_owned(),
                    status: CheckStatus::Fail,
                    detail: format!("integrity check failed: {e}"),
                },
            }
        }
        Err(e) => DoctorCheck {
            name: "db_integrity".to_owned(),
            status: CheckStatus::Fail,
            detail: format!("cannot open database: {e}"),
        },
    }
}

/// Report configured MCP servers.
fn check_mcp_servers(loaded: &LoadedConfig) -> DoctorCheck {
    let servers = &loaded.config.mcp_servers;
    if servers.is_empty() {
        DoctorCheck {
            name: "mcp_servers".to_owned(),
            status: CheckStatus::Pass,
            detail: "none configured".to_owned(),
        }
    } else {
        let names: String = servers.keys().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        DoctorCheck {
            name: "mcp_servers".to_owned(),
            status: CheckStatus::Pass,
            detail: format!("{} configured: {}", servers.len(), names),
        }
    }
}

impl SessionPlan {
    pub fn from_config(
        config: &GenesisConfig,
        session_id: impl Into<String>,
        platform: DeliveryPlatform,
    ) -> Self {
        let session_id = session_id.into();
        let model = model_selection_from_config(config);
        let provider_name = config.provider.backend.clone();
        let model_name = config.provider.model.clone();

        Self {
            session_id: session_id.clone(),
            profile: config.profile.clone(),
            platform: platform.clone(),
            model,
            initial_events: vec![
                RuntimeEvent::SessionStarted {
                    session_id: session_id.clone(),
                },
                RuntimeEvent::SessionPlanned {
                    session_id,
                    platform,
                    provider: provider_name,
                    model: model_name,
                },
            ],
        }
    }
}

fn model_selection_from_config(config: &GenesisConfig) -> ModelSelection {
    ModelSelection {
        provider: provider_kind(&config.provider.backend),
        model: config.provider.model.clone(),
        base_url: config.provider.base_url.clone(),
    }
}

fn provider_kind(raw: &str) -> ModelProviderKind {
    match raw.trim().to_ascii_lowercase().as_str() {
        "openai" => ModelProviderKind::OpenAi,
        "openrouter" => ModelProviderKind::OpenRouter,
        "anthropic" => ModelProviderKind::Anthropic,
        "gemini" | "google" => ModelProviderKind::Gemini,
        "local" | "ollama" => ModelProviderKind::Local,
        _ => ModelProviderKind::Compatible,
    }
}

/// Infer the backend and API key env var from a model name.
fn infer_backend(model: &str) -> (&'static str, &'static str) {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") || m.starts_with("anthropic") {
        ("anthropic", "ANTHROPIC_API_KEY")
    } else if m.starts_with("gemini") {
        ("gemini", "GEMINI_API_KEY")
    } else if m.starts_with("gpt") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        ("openai", "OPENAI_API_KEY")
    } else {
        // Default to OpenRouter for unknown models
        ("openrouter", "OPENROUTER_API_KEY")
    }
}

impl ToolRuntime {
    /// Set the default working directory for shell commands.
    /// Used by worktree isolation to redirect tool execution.
    pub fn set_default_working_dir(&mut self, dir: String) {
        self.context.default_working_dir = Some(dir);
    }

    /// Filter the tool registry to only include tools in the given set.
    pub fn retain(&mut self, names: &std::collections::HashSet<String>) {
        self.registry.retain(names);
    }

    /// Returns built-in tool definitions only (sync).
    /// Use `definitions_async()` to include MCP tools.
    pub fn definitions(&self) -> Vec<genesis_types::ToolDefinition> {
        self.registry.definitions()
    }

    /// Returns tool definitions including MCP tools (async-safe).
    pub async fn definitions_async(&self) -> Vec<genesis_types::ToolDefinition> {
        let mut defs = self.registry.definitions();
        if let Some(mcp) = &self.mcp {
            defs.extend(mcp.tool_definitions().await);

            // Add resource/prompt tools if any servers expose them
            if !mcp.resource_definitions().await.is_empty() {
                defs.push(genesis_types::ToolDefinition {
                    name: "mcp_list_resources".to_owned(),
                    description: "List available MCP resources from all connected servers. Returns resource URIs, names, and descriptions.".to_owned(),
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {},
                    })),
                });
                defs.push(genesis_types::ToolDefinition {
                    name: "mcp_read_resource".to_owned(),
                    description: "Read an MCP resource by URI. Use mcp_list_resources first to discover available resources.".to_owned(),
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "server": {
                                "type": "string",
                                "description": "The MCP server name that owns this resource"
                            },
                            "uri": {
                                "type": "string",
                                "description": "The resource URI to read"
                            }
                        },
                        "required": ["server", "uri"]
                    })),
                });
            }

            if !mcp.prompt_definitions().await.is_empty() {
                defs.push(genesis_types::ToolDefinition {
                    name: "mcp_list_prompts".to_owned(),
                    description: "List available MCP prompt templates from all connected servers. Returns prompt names, descriptions, and required arguments.".to_owned(),
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {},
                    })),
                });
                defs.push(genesis_types::ToolDefinition {
                    name: "mcp_get_prompt".to_owned(),
                    description: "Get an MCP prompt template by name with optional arguments. Returns the expanded prompt messages.".to_owned(),
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "server": {
                                "type": "string",
                                "description": "The MCP server name that owns this prompt"
                            },
                            "name": {
                                "type": "string",
                                "description": "The prompt template name"
                            },
                            "arguments": {
                                "type": "object",
                                "description": "Optional arguments to fill in the prompt template",
                                "additionalProperties": { "type": "string" }
                            }
                        },
                        "required": ["server", "name"]
                    })),
                });
            }
        }
        defs
    }

    pub fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        self.registry.execute(call, &self.context)
    }

    /// Execute a tool call, routing MCP-prefixed tools to the MCP manager
    /// and `moa_consult` to the Mixture of Agents engine.
    pub async fn execute_async(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        if call.name.starts_with("mcp_") {
            if let Some(mcp) = &self.mcp {
                // Handle built-in MCP resource/prompt operations
                match call.name.as_str() {
                    "mcp_list_resources" => {
                        let resources = mcp.resource_definitions().await;
                        let mut lines = Vec::new();
                        for (server, res) in &resources {
                            let desc = res.description.as_deref().unwrap_or("");
                            let mime = res.mime_type.as_deref().unwrap_or("unknown");
                            lines.push(format!(
                                "- [{}] {} ({}) — {} [{}]",
                                server, res.name, res.uri, desc, mime
                            ));
                        }
                        let content = if lines.is_empty() {
                            "No MCP resources available.".to_owned()
                        } else {
                            format!("Available MCP resources ({}):\n{}", lines.len(), lines.join("\n"))
                        };
                        return Ok(ToolOutput {
                            content,
                            metadata: std::collections::BTreeMap::new(),
                        });
                    }
                    "mcp_read_resource" => {
                        let server = call.arguments.get("server").ok_or_else(|| {
                            ToolError::MissingArgument { tool: call.name.clone(), argument: "server" }
                        })?;
                        let uri = call.arguments.get("uri").ok_or_else(|| {
                            ToolError::MissingArgument { tool: call.name.clone(), argument: "uri" }
                        })?;
                        let result = mcp.read_resource(server, uri).await.map_err(|e| {
                            ToolError::ExecutionFailed { tool: call.name.clone(), reason: e.to_string() }
                        })?;
                        let content = result.contents.iter()
                            .filter_map(|c| c.text.as_deref())
                            .collect::<Vec<_>>()
                            .join("\n");
                        return Ok(ToolOutput {
                            content,
                            metadata: std::collections::BTreeMap::new(),
                        });
                    }
                    "mcp_list_prompts" => {
                        let prompts = mcp.prompt_definitions().await;
                        let mut lines = Vec::new();
                        for (server, prompt) in &prompts {
                            let desc = prompt.description.as_deref().unwrap_or("");
                            let args = prompt.arguments.as_ref()
                                .map(|a| a.iter().map(|arg| {
                                    let req = if arg.required == Some(true) { " (required)" } else { "" };
                                    format!("{}{}", arg.name, req)
                                }).collect::<Vec<_>>().join(", "))
                                .unwrap_or_default();
                            lines.push(format!(
                                "- [{}] {} — {} [args: {}]",
                                server, prompt.name, desc, if args.is_empty() { "none" } else { &args }
                            ));
                        }
                        let content = if lines.is_empty() {
                            "No MCP prompts available.".to_owned()
                        } else {
                            format!("Available MCP prompts ({}):\n{}", lines.len(), lines.join("\n"))
                        };
                        return Ok(ToolOutput {
                            content,
                            metadata: std::collections::BTreeMap::new(),
                        });
                    }
                    "mcp_get_prompt" => {
                        let server = call.arguments.get("server").ok_or_else(|| {
                            ToolError::MissingArgument { tool: call.name.clone(), argument: "server" }
                        })?;
                        let name = call.arguments.get("name").ok_or_else(|| {
                            ToolError::MissingArgument { tool: call.name.clone(), argument: "name" }
                        })?;
                        let arguments = call.arguments.get("arguments")
                            .and_then(|v| serde_json::from_str::<std::collections::HashMap<String, String>>(v).ok());
                        let result = mcp.get_prompt(server, name, arguments).await.map_err(|e| {
                            ToolError::ExecutionFailed { tool: call.name.clone(), reason: e.to_string() }
                        })?;
                        let mut content = String::new();
                        if let Some(desc) = &result.description {
                            content.push_str(&format!("Description: {desc}\n\n"));
                        }
                        for msg in &result.messages {
                            let text = msg.content.text.as_deref().unwrap_or("");
                            content.push_str(&format!("[{}]: {}\n", msg.role, text));
                        }
                        return Ok(ToolOutput {
                            content,
                            metadata: std::collections::BTreeMap::new(),
                        });
                    }
                    _ => {}
                }

                // Generic MCP tool call dispatch
                let args = if call.arguments.is_empty() {
                    None
                } else {
                    Some(serde_json::to_value(&call.arguments).unwrap_or_default())
                };

                let content = mcp.call_tool(&call.name, args).await.map_err(|e| {
                    ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: e.to_string(),
                    }
                })?;

                return Ok(ToolOutput {
                    content,
                    metadata: std::collections::BTreeMap::from([(
                        "tool".to_owned(),
                        call.name.clone(),
                    )]),
                });
            }
            return Err(ToolError::ToolNotFound(call.name.clone()));
        }

        if call.name == "moa_consult" {
            return self.execute_moa(call).await;
        }

        // PTC: execute_code with tool RPC access via Unix domain socket
        if call.name == "execute_code" {
            if let Some(code) = call.arguments.get("code") {
                let code = code.clone();
                let registry = self.registry.clone();
                let context = self.context.clone();
                return tokio::task::spawn_blocking(move || {
                    genesis_tools::builtins::code_execution::execute_code_ptc(
                        &code, &registry, &context,
                    )
                })
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "execute_code".to_owned(),
                    reason: format!("PTC task panicked: {e}"),
                });
            }
        }

        self.registry.execute(call, &self.context)
    }

    /// Execute the MoA consultation tool.
    async fn execute_moa(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        use crate::moa::{MoaConfig, MoaModelConfig};

        let prompt = call.arguments.get("prompt").ok_or_else(|| {
            ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "prompt",
            }
        })?;

        if prompt.trim().is_empty() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: "prompt cannot be empty".to_owned(),
            });
        }

        let system_prompt = call.arguments.get("system_prompt").map(|s| s.as_str());
        let layers: usize = call
            .arguments
            .get("layers")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1)
            .clamp(1, 3);

        // Parse model list or use defaults
        let models: Vec<&str> = call
            .arguments
            .get("models")
            .map(|m| m.split(',').map(str::trim).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();

        let proposers = if models.is_empty() {
            // Default: use a few well-known models
            vec![
                MoaModelConfig {
                    backend: "openai".to_owned(),
                    model: "gpt-4.1".to_owned(),
                    base_url: None,
                    api_key_env: Some("OPENAI_API_KEY".to_owned()),
                },
                MoaModelConfig {
                    backend: "anthropic".to_owned(),
                    model: "claude-sonnet-4-20250514".to_owned(),
                    base_url: None,
                    api_key_env: Some("ANTHROPIC_API_KEY".to_owned()),
                },
                MoaModelConfig {
                    backend: "openai".to_owned(),
                    model: "gpt-4.1-mini".to_owned(),
                    base_url: None,
                    api_key_env: Some("OPENAI_API_KEY".to_owned()),
                },
            ]
        } else {
            models
                .iter()
                .map(|m| {
                    let (backend, api_key_env) = infer_backend(m);
                    MoaModelConfig {
                        backend: backend.to_owned(),
                        model: m.to_string(),
                        base_url: None,
                        api_key_env: Some(api_key_env.to_owned()),
                    }
                })
                .collect()
        };

        // Use the first proposer as aggregator (common MoA pattern)
        let aggregator = proposers[0].clone();

        let config = MoaConfig {
            proposers,
            aggregator,
            layers,
            temperature: Some(0.7),
            max_tokens: Some(2048),
        };

        let result = moa::run_moa(prompt, system_prompt, &config)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: e.to_string(),
            })?;

        let mut content = result.response;

        // Append proposer attributions
        if !result.proposer_responses.is_empty() {
            content.push_str("\n\n---\n*Consulted models: ");
            let model_names: Vec<&str> = result
                .proposer_responses
                .iter()
                .map(|r| r.model.as_str())
                .collect();
            content.push_str(&model_names.join(", "));
            content.push_str(&format!(" ({} layers)*", result.layers_completed));
        }

        Ok(ToolOutput {
            content,
            metadata: std::collections::BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                (
                    "models_consulted".to_owned(),
                    result
                        .proposer_responses
                        .iter()
                        .map(|r| r.model.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            ]),
        })
    }

    /// Attach an MCP manager for external tool support.
    pub fn set_mcp(&mut self, mcp: Arc<McpManager>) {
        self.mcp = Some(mcp);
    }

    /// Set an interactive approval handler for tools requiring user confirmation.
    pub fn set_approval_handler(&mut self, handler: Arc<dyn genesis_tools::ApprovalHandler>) {
        self.registry.set_approval_handler(handler);
    }

    /// Set the terminal backend for shell command execution.
    pub fn set_terminal_backend(&mut self, backend: genesis_tools::TerminalBackend) {
        self.context.terminal_backend = Some(backend);
    }

    /// Create a new ToolRuntime with a different session ID.
    /// Used when spawning subagent workstreams.
    pub fn with_session_id(&self, session_id: impl Into<String>) -> Self {
        Self {
            registry: self.registry.clone(),
            context: ToolContext {
                session_id: session_id.into(),
                ..self.context.clone()
            },
            mcp: self.mcp.clone(),
        }
    }
}

/// Returns the number of tools in the default registry without constructing
/// a full `ExecutionContext`.
pub fn default_tool_count() -> usize {
    default_registry().definitions().len()
}

#[cfg(test)]
mod tests {
    use super::{
        build_default_tool_runtime, build_execution_context_from_loaded, CheckStatus, SessionPlan,
    };
    use genesis_config::{
        AppPaths, GenesisConfig, LoadedConfig, ProviderConfig, RuntimeConfig, StorageConfig,
    };
    use genesis_tools::ToolCall;
    use genesis_types::{DeliveryPlatform, ModelProviderKind, RuntimeEvent};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn sample_loaded_config() -> LoadedConfig {
        LoadedConfig {
            config: GenesisConfig {
                schema_version: 1,
                profile: "operator".to_owned(),
                provider: ProviderConfig {
                    backend: "openrouter".to_owned(),
                    model: "moonshotai/kimi-k2".to_owned(),
                    base_url: Some("https://openrouter.ai/api/v1".to_owned()),
                    api_key_env: Some("OPENROUTER_API_KEY".to_owned()),
                    extra_body: None,
                    tool_call_parser: None,
                },
                tool_provider: None,
                fallback_providers: Vec::new(),
                mcp_servers: std::collections::HashMap::new(),
                storage: StorageConfig {
                    data_dir: PathBuf::from("/tmp/genesis"),
                    database_path: PathBuf::from("/tmp/genesis/genesis.db"),
                },
                runtime: RuntimeConfig {
                    max_concurrency: 8,
                    allow_destructive_tools: false,
                    max_turns: 20,
                    max_context_messages: None,
                    budget_limit: None,
                    terminal: None,
                    thinking_budget: None,
                    max_context_tokens: None,
                    max_iterations: None,
                    context_security: genesis_config::ContextSecurityPolicy::default(),
                    reasoning_effort: None,
                    cache: None,
                    tool_filter: None,
                    guardrails: None,
                },
                gateway: None,
                toolsets: std::collections::HashMap::new(),
                personality: None,
            },
            paths: AppPaths {
                config_path: PathBuf::from("/tmp/genesis/config.yaml"),
                data_dir: PathBuf::from("/tmp/genesis"),
                database_path: PathBuf::from("/tmp/genesis/genesis.db"),
            },
        }
    }

    #[test]
    fn session_plan_captures_runtime_defaults_from_config() {
        let loaded = sample_loaded_config();

        let plan = SessionPlan::from_config(&loaded.config, "session-42", DeliveryPlatform::Cli);

        assert_eq!(plan.profile, "operator");
        assert_eq!(plan.model.provider, ModelProviderKind::OpenRouter);
        assert_eq!(plan.model.model, "moonshotai/kimi-k2");
        assert_eq!(plan.initial_events.len(), 2);
        assert!(matches!(
            &plan.initial_events[1],
            RuntimeEvent::SessionPlanned { provider, .. } if provider == "openrouter"
        ));
    }

    #[test]
    fn execution_context_exposes_storage_and_runtime_limits() {
        let loaded = sample_loaded_config();

        let context =
            build_execution_context_from_loaded(&loaded, "session-99".to_owned(), DeliveryPlatform::Slack);

        assert_eq!(context.max_concurrency, 8);
        assert!(!context.allow_destructive_tools);
        assert_eq!(context.database_path, "/tmp/genesis/genesis.db");
        assert_eq!(context.plan.platform, DeliveryPlatform::Slack);
    }

    #[test]
    fn check_status_serialization_contract_stays_snake_case() {
        let encoded = serde_json::to_string(&CheckStatus::Warn)
            .expect("status should serialize");

        assert_eq!(encoded, "\"warn\"");
    }

    #[test]
    fn default_tool_runtime_exposes_session_info_for_execution_context() {
        let loaded = sample_loaded_config();
        let context = build_execution_context_from_loaded(
            &loaded,
            "session-42".to_owned(),
            DeliveryPlatform::Cli,
        );
        let runtime = build_default_tool_runtime(&context);

        let output = runtime
            .execute(&ToolCall {
                name: "session_info".to_owned(),
                arguments: BTreeMap::new(),
            })
            .expect("session_info should execute");

        assert!(output.content.contains("session=session-42"));
        assert!(output.content.contains("profile=operator"));
        assert!(output.content.contains("data_dir=/tmp/genesis"));
    }

    #[test]
    fn moa_consult_tool_is_registered() {
        let context = build_execution_context_from_loaded(
            &sample_loaded_config(),
            "s-1".to_owned(),
            DeliveryPlatform::Cli,
        );
        let runtime = build_default_tool_runtime(&context);
        let defs = runtime.definitions();
        assert!(
            defs.iter().any(|d| d.name == "moa_consult"),
            "moa_consult should be registered"
        );
    }

    #[test]
    fn moa_placeholder_returns_error_on_sync_call() {
        let context = build_execution_context_from_loaded(
            &sample_loaded_config(),
            "s-1".to_owned(),
            DeliveryPlatform::Cli,
        );
        let runtime = build_default_tool_runtime(&context);
        let result = runtime.execute(&ToolCall {
            name: "moa_consult".to_owned(),
            arguments: BTreeMap::from([("prompt".to_owned(), "test".to_owned())]),
        });
        assert!(result.is_err(), "sync moa_consult should fail");
    }

    #[test]
    fn infer_backend_identifies_providers() {
        use super::infer_backend;

        assert_eq!(infer_backend("gpt-4.1").0, "openai");
        assert_eq!(infer_backend("claude-sonnet-4-20250514").0, "anthropic");
        assert_eq!(infer_backend("gemini-2.5-pro").0, "gemini");
        assert_eq!(infer_backend("o4-mini").0, "openai");
        assert_eq!(infer_backend("llama-3.1-70b").0, "openrouter");
    }
}
