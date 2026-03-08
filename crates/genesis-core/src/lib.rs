pub mod agent_loop;
pub mod prompt;

use std::path::Path;

use genesis_config::{load, GenesisConfig, LoadedConfig};
use genesis_storage::{bootstrap, inspect, StorageHealth};
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
        },
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
    pub fn definitions(&self) -> Vec<genesis_types::ToolDefinition> {
        self.registry.definitions()
    }

    pub fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        self.registry.execute(call, &self.context)
    }
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
                storage: StorageConfig {
                    data_dir: PathBuf::from("/tmp/genesis"),
                    database_path: PathBuf::from("/tmp/genesis/genesis.db"),
                },
                runtime: RuntimeConfig {
                    max_concurrency: 8,
                    allow_destructive_tools: false,
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
