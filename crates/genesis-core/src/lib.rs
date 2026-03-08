pub mod agent_loop;
pub mod cost;
pub mod execution;
pub mod moa;
pub mod nudge;
pub mod prompt;
pub mod scheduler;
pub mod skills;

use std::path::Path;
use std::sync::Arc;

use genesis_config::{load, GenesisConfig, LoadedConfig};
use genesis_provider::resolve;
use genesis_storage::{
    bootstrap, inspect, ScheduleStore, SessionStore, SkillStore, StorageHealth,
};
use genesis_mcp::McpManager;
use genesis_tools::{default_registry, ToolCall, ToolContext, ToolError, ToolOutput, ToolRegistry};
use genesis_types::{DeliveryPlatform, ModelProviderKind, ModelSelection, RuntimeEvent};
use serde::{Deserialize, Serialize};
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
    ToolRuntime {
        registry: default_registry(),
        context: ToolContext {
            session_id: execution_context.plan.session_id.clone(),
            profile: execution_context.plan.profile.clone(),
            data_dir: execution_context.data_dir.clone(),
            allow_destructive_tools: execution_context.allow_destructive_tools,
            terminal_backend: None,
        },
        mcp: None,
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
        // Show masked key (first 8 chars + ...)
        let masked = if resolved.api_key.len() > 8 {
            format!("{}...", &resolved.api_key[..8])
        } else {
            "****".to_owned()
        };
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
    let session_store = SessionStore::new(db_path);
    let skill_store = SkillStore::new(db_path);
    let schedule_store = ScheduleStore::new(db_path);

    let sessions = session_store.count_sessions().unwrap_or(0);
    let skills = skill_store.list_all().map(|v| v.len()).unwrap_or(0);
    let schedules = schedule_store.list_all().map(|v| v.len()).unwrap_or(0);

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
        let names: Vec<&String> = servers.keys().collect();
        DoctorCheck {
            name: "mcp_servers".to_owned(),
            status: CheckStatus::Pass,
            detail: format!("{} configured: {}", names.len(), names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
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

impl ToolRuntime {
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
        }
        defs
    }

    pub fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        self.registry.execute(call, &self.context)
    }

    /// Execute a tool call, routing MCP-prefixed tools to the MCP manager.
    pub async fn execute_async(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        if call.name.starts_with("mcp_") {
            if let Some(mcp) = &self.mcp {
                // Convert BTreeMap<String,String> arguments to JSON Value
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

        self.registry.execute(call, &self.context)
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
                },
                tool_provider: None,
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
                    terminal: None,
                },
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
}
