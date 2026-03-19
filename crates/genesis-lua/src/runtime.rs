use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, LuaSerdeExt, Table, Value, VmState};
use thiserror::Error;

use crate::{
    api::{install_genesis_api, PluginContext},
    discovery::{discover_plugins_best_effort, PluginKind},
    hooks::{
        parse_post_hook_result, parse_pre_hook_result, HookEvent, HookRegistry, PostHookOutcome,
        PreHookOutcome,
    },
    personality::{LuaPersonalityEntry, LuaPersonalityRegistry, LuaRegisteredPersonality},
    tools::{LuaHostToolExecutor, LuaRegisteredTool, LuaToolOutput, LuaToolRegistry},
};

const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_AUTO_DISABLE_AFTER: u32 = 3;
const DEFAULT_MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LuaSessionContext {
    pub id: String,
    pub model: String,
    pub turn_count: u32,
    pub total_tokens: u32,
    pub platform: String,
    pub personality: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LuaRuntimeConfig {
    pub plugin_dir: PathBuf,
    pub session: LuaSessionContext,
    pub disabled_plugins: Vec<String>,
    pub plugin_verbose: Option<bool>,
    pub config_values: BTreeMap<String, String>,
}

pub struct LuaRuntime {
    lua: Lua,
    plugin_names: Vec<String>,
    logs: Arc<Mutex<Vec<String>>>,
    plugin_errors: Vec<String>,
    session_state: Arc<Mutex<LuaSessionContext>>,
    hook_registry: Arc<Mutex<HookRegistry>>,
    tool_registry: Arc<Mutex<LuaToolRegistry>>,
    personality_registry: Arc<Mutex<LuaPersonalityRegistry>>,
    host_tool_executor: Arc<Mutex<Option<Arc<dyn LuaHostToolExecutor>>>>,
    active_plugin: Arc<Mutex<Vec<PluginContext>>>,
    execution_control: Arc<Mutex<PluginExecutionControl>>,
    disabled_plugins: Arc<Mutex<HashSet<String>>>,
    plugin_failures: Arc<Mutex<HashMap<String, u32>>>,
    hook_timeout: Duration,
    tool_timeout: Duration,
    auto_disable_after: u32,
    plugin_verbose: bool,
}

impl std::fmt::Debug for LuaRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaRuntime")
            .field("plugin_names", &self.plugin_names)
            .field("logs", &self.logs())
            .field("plugin_errors", &self.plugin_errors)
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct LuaRuntimeBuilder {
    config: LuaRuntimeConfig,
}

#[derive(Debug, Default)]
struct PluginExecutionControl {
    active: Option<ActivePluginExecution>,
}

#[derive(Debug, Clone)]
struct ActivePluginExecution {
    plugin_name: String,
    operation: String,
    started_at: Instant,
    deadline: Instant,
    timeout_ms: u64,
}

#[derive(Debug, Error)]
pub enum LuaRuntimeError {
    #[error("failed to read plugin directory `{path}`: {source}")]
    ReadPluginDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect plugin entry in `{path}`: {source}")]
    ReadPluginEntry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid plugin filename `{path}`")]
    InvalidPluginFilename { path: PathBuf },
    #[error("failed to read plugin manifest `{path}`: {source}")]
    ReadPluginManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse plugin manifest `{path}`: {source}")]
    ParsePluginManifest {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to read plugin source `{path}`: {source}")]
    ReadPluginSource {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("duplicate plugin name `{name}`")]
    DuplicatePluginName { name: String },
    #[error("unsupported hook event `{event}`")]
    UnsupportedHookEvent { event: String },
    #[error("duplicate lua tool name `{name}`")]
    DuplicateLuaToolName { name: String },
    #[error("invalid lua tool definition from plugin `{plugin_name}`: {reason}")]
    InvalidLuaToolDefinition { plugin_name: String, reason: String },
    #[error("lua tool registration is only available during plugin load")]
    ToolRegistrationUnavailable,
    #[error("unknown lua tool `{name}`")]
    UnknownLuaTool { name: String },
    #[error("invalid lua tool result from `{tool_name}`: unsupported `{value_type}` value")]
    InvalidLuaToolResult {
        tool_name: String,
        value_type: String,
    },
    #[error("duplicate lua personality name `{name}`")]
    DuplicateLuaPersonalityName { name: String },
    #[error("invalid lua personality definition from plugin `{plugin_name}`: {reason}")]
    InvalidLuaPersonalityDefinition { plugin_name: String, reason: String },
    #[error("lua personality registration is only available during plugin load")]
    PersonalityRegistrationUnavailable,
    #[error("plugin `{plugin_name}` is disabled for this session")]
    PluginDisabled { plugin_name: String },
    #[error("host tool bridge is not configured")]
    HostToolBridgeUnavailable,
    #[error("host tool bridge is unavailable outside plugin callbacks")]
    HostToolContextUnavailable,
    #[error("plugin `{plugin_name}` is not permitted to call host tool `{tool_name}`")]
    HostToolPermissionDenied {
        plugin_name: String,
        tool_name: String,
    },
    #[error("host tool `{tool_name}` failed: {reason}")]
    HostToolExecutionFailed { tool_name: String, reason: String },
    #[error("lua execution failed: {source}")]
    Lua {
        #[from]
        source: mlua::Error,
    },
    #[error("lua runtime initialization is not implemented yet")]
    NotImplemented,
}

impl LuaRuntime {
    pub fn builder() -> LuaRuntimeBuilder {
        LuaRuntimeBuilder::default()
    }
}

impl LuaRuntimeBuilder {
    pub fn with_config(mut self, config: LuaRuntimeConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self) -> Result<LuaRuntime, LuaRuntimeError> {
        LuaRuntime::new(self.config)
    }
}

fn config_u64(values: &BTreeMap<String, String>, key: &str, default: u64) -> u64 {
    values
        .get(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn config_u32(values: &BTreeMap<String, String>, key: &str, default: u32) -> u32 {
    values
        .get(key)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn config_usize(values: &BTreeMap<String, String>, key: &str, default: usize) -> usize {
    values
        .get(key)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn config_bool(values: &BTreeMap<String, String>, key: &str, default: bool) -> bool {
    values
        .get(key)
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn strip_unsafe_globals(lua: &Lua) -> Result<(), LuaRuntimeError> {
    let globals = lua.globals();
    globals.set("io", Value::Nil)?;
    globals.set("debug", Value::Nil)?;
    if let Ok(os) = globals.get::<Table>("os") {
        os.set("execute", Value::Nil)?;
    }
    Ok(())
}

impl LuaRuntime {
    fn new(config: LuaRuntimeConfig) -> Result<Self, LuaRuntimeError> {
        let lua = Lua::new();
        let override_timeout_ms = std::env::var("GENESIS_PLUGIN_TIMEOUT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok());
        let hook_timeout = Duration::from_millis(
            override_timeout_ms.unwrap_or_else(|| {
                config_u64(
                    &config.config_values,
                    "plugin_hook_timeout_ms",
                    DEFAULT_HOOK_TIMEOUT_MS,
                )
            }),
        );
        let tool_timeout = Duration::from_millis(
            override_timeout_ms.unwrap_or_else(|| {
                config_u64(
                    &config.config_values,
                    "plugin_tool_timeout_ms",
                    DEFAULT_TOOL_TIMEOUT_MS,
                )
            }),
        );
        let auto_disable_after = config_u32(
            &config.config_values,
            "plugin_auto_disable_after",
            DEFAULT_AUTO_DISABLE_AFTER,
        );
        let plugin_verbose = config.plugin_verbose.unwrap_or_else(|| {
            env_bool(
                "GENESIS_PLUGIN_VERBOSE",
                config_bool(&config.config_values, "plugin_verbose", false),
            )
        });
        let memory_limit_bytes = config_usize(
            &config.config_values,
            "plugin_memory_limit_bytes",
            DEFAULT_MEMORY_LIMIT_BYTES,
        );
        lua.set_memory_limit(memory_limit_bytes)?;
        let logs = Arc::new(Mutex::new(Vec::new()));
        let session_state = Arc::new(Mutex::new(config.session.clone()));
        let hook_registry = Arc::new(Mutex::new(HookRegistry::default()));
        let tool_registry = Arc::new(Mutex::new(LuaToolRegistry::default()));
        let personality_registry = Arc::new(Mutex::new(LuaPersonalityRegistry::default()));
        let host_tool_executor = Arc::new(Mutex::new(None));
        let active_plugin = Arc::new(Mutex::new(Vec::new()));
        let execution_control = Arc::new(Mutex::new(PluginExecutionControl::default()));
        let disabled_plugins = Arc::new(Mutex::new(HashSet::new()));
        let plugin_failures = Arc::new(Mutex::new(HashMap::new()));
        let interrupt_control = Arc::clone(&execution_control);
        lua.set_interrupt(move |_| {
            let active = interrupt_control
                .lock()
                .expect("plugin execution control mutex should not be poisoned")
                .active
                .clone();
            let Some(active) = active else {
                return Ok(VmState::Continue);
            };
            if Instant::now() < active.deadline {
                return Ok(VmState::Continue);
            }
            Err(mlua::Error::runtime(format!(
                "plugin `{}` {} timed out after {}ms",
                active.plugin_name, active.operation, active.timeout_ms
            )))
        });
        let genesis = install_genesis_api(
            &lua,
            &config,
            Arc::clone(&logs),
            Arc::clone(&session_state),
            Arc::clone(&hook_registry),
            Arc::clone(&tool_registry),
            Arc::clone(&personality_registry),
            Arc::clone(&host_tool_executor),
            Arc::clone(&active_plugin),
            None,
        )?;
        lua.globals().set("genesis", genesis)?;
        strip_unsafe_globals(&lua)?;
        lua.sandbox(true)?;

        let mut runtime = Self {
            lua,
            plugin_names: Vec::new(),
            logs,
            plugin_errors: Vec::new(),
            session_state,
            hook_registry,
            tool_registry,
            personality_registry,
            host_tool_executor,
            active_plugin,
            execution_control,
            disabled_plugins,
            plugin_failures,
            hook_timeout,
            tool_timeout,
            auto_disable_after,
            plugin_verbose,
        };
        runtime.load_plugins(&config)?;
        Ok(runtime)
    }

    pub fn load_plugins(&mut self, config: &LuaRuntimeConfig) -> Result<(), LuaRuntimeError> {
        if config.plugin_dir.as_os_str().is_empty() {
            return Ok(());
        }

        let report = match discover_plugins_best_effort(&config.plugin_dir) {
            Ok(report) => report,
            Err(LuaRuntimeError::ReadPluginDirectory { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(());
            }
            Err(err) => {
                self.plugin_errors.push(err.to_string());
                return Ok(());
            }
        };
        self.plugin_errors
            .extend(report.errors.into_iter().map(|err| err.to_string()));

        let configured_disabled = config
            .disabled_plugins
            .iter()
            .cloned()
            .collect::<HashSet<_>>();

        for plugin in report.plugins {
            if configured_disabled.contains(&plugin.name) {
                self.disabled_plugins
                    .lock()
                    .expect("disabled plugins mutex should not be poisoned")
                    .insert(plugin.name.clone());
                continue;
            }
            let source = match fs::read_to_string(&plugin.entrypoint).map_err(|source| {
                LuaRuntimeError::ReadPluginSource {
                    path: plugin.entrypoint.clone(),
                    source,
                }
            }) {
                Ok(source) => source,
                Err(err) => {
                    self.plugin_errors.push(err.to_string());
                    continue;
                }
            };
            let (plugin_env, plugin_context) = self.plugin_environment(config, &plugin)?;
            let _guard = self.begin_plugin_execution(&plugin.name, "load", self.hook_timeout);
            let load_result = self
                .lua
                .load(&source)
                .set_name(&plugin.name)
                .set_environment(plugin_env)
                .exec();
            plugin_context.close_tool_registration();
            if let Err(err) = load_result {
                self.tool_registry
                    .lock()
                    .expect("tool registry mutex should not be poisoned")
                    .remove_tools_owned_by(&plugin.name);
                self.personality_registry
                    .lock()
                    .expect("personality registry mutex should not be poisoned")
                    .remove_personalities_owned_by(&plugin.name);
                self.plugin_errors
                    .push(format!("plugin `{}` failed to load: {err}", plugin.name));
                continue;
            }
            let plugin_name = plugin.name.clone();
            self.plugin_names.push(plugin_name.clone());
            let _ = self.run_on_plugin_load(&plugin_name, plugin.kind);
        }
        Ok(())
    }

    pub fn eval_string(&self, source: &str) -> Result<serde_json::Value, LuaRuntimeError> {
        let value = self.lua.load(source).eval()?;
        Ok(self.lua.from_value(value)?)
    }

    pub fn plugin_names(&self) -> Vec<String> {
        self.plugin_names.clone()
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .expect("log sink mutex should not be poisoned")
            .clone()
    }

    pub fn plugin_errors(&self) -> &[String] {
        &self.plugin_errors
    }

    pub fn registered_tools(&self) -> Vec<LuaRegisteredTool> {
        let disabled = self.disabled_plugin_names();
        self.tool_registry
            .lock()
            .expect("tool registry mutex should not be poisoned")
            .registered_tools()
            .into_iter()
            .filter(|tool| !disabled.contains(&tool.plugin_name))
            .collect()
    }

    pub fn registered_personalities(&self) -> Vec<LuaRegisteredPersonality> {
        let disabled = self.disabled_plugin_names();
        self.personality_registry
            .lock()
            .expect("personality registry mutex should not be poisoned")
            .registered_personalities()
            .into_iter()
            .filter(|personality| !disabled.contains(&personality.plugin_name))
            .collect()
    }

    pub fn personality_prompt(&self, name: &str) -> Option<String> {
        let entry = self
            .personality_registry
            .lock()
            .expect("personality registry mutex should not be poisoned")
            .personality_entry(name)?;
        if self.plugin_disabled(&entry.metadata.plugin_name) {
            return None;
        }

        self.resolve_personality_prompt(entry)
    }

    pub fn invoke_tool(
        &self,
        name: &str,
        arguments: BTreeMap<String, String>,
    ) -> Result<LuaToolOutput, LuaRuntimeError> {
        let plugin_context = {
            let registry = self
                .tool_registry
                .lock()
                .expect("tool registry mutex should not be poisoned");
            let tool = registry
                .registered_tools()
                .into_iter()
                .find(|tool| tool.definition.name == name)
                .ok_or_else(|| LuaRuntimeError::UnknownLuaTool {
                    name: name.to_owned(),
                })?;
            if self.plugin_disabled(&tool.plugin_name) {
                return Err(LuaRuntimeError::PluginDisabled {
                    plugin_name: tool.plugin_name,
                });
            }
            PluginContext::for_execution(tool.plugin_name, tool.permissions)
        };
        let plugin_name = plugin_context.name.clone();
        let _active_guard =
            ActivePluginGuard::push(Arc::clone(&self.active_plugin), Some(plugin_context));
        let _guard = self.begin_plugin_execution(&plugin_name, format!("tool `{name}`"), self.tool_timeout);
        let result = self
            .tool_registry
            .lock()
            .expect("tool registry mutex should not be poisoned")
            .invoke(&self.lua, name, arguments);
        if let Err(err) = &result {
            self.record_plugin_failure(&plugin_name, &err.to_string());
        }
        result
    }

    pub fn set_host_tool_executor(&self, executor: Arc<dyn LuaHostToolExecutor>) {
        *self
            .host_tool_executor
            .lock()
            .expect("host tool executor mutex should not be poisoned") = Some(executor);
    }

    pub fn register_hook(
        &self,
        event_name: &str,
        callback: mlua::Function,
    ) -> Result<(), LuaRuntimeError> {
        let event = HookEvent::from_name(event_name).ok_or_else(|| {
            LuaRuntimeError::UnsupportedHookEvent {
                event: event_name.to_owned(),
            }
        })?;
        self.hook_registry
            .lock()
            .expect("hook registry mutex should not be poisoned")
            .register(event, callback, None);
        Ok(())
    }

    pub fn record_completed_turn(&self, tokens: u32) {
        let mut state = self
            .session_state
            .lock()
            .expect("session state mutex should not be poisoned");
        state.turn_count = state.turn_count.saturating_add(1);
        state.total_tokens = state.total_tokens.saturating_add(tokens);
    }

    pub fn run_pre_turn(
        &self,
        user_message: &str,
    ) -> Result<PreHookOutcome<String>, LuaRuntimeError> {
        let context = self.lua.create_table()?;
        context.set("user_message", user_message)?;
        self.run_pre_hook(HookEvent::PreTurn, context, user_message.to_owned())
    }

    pub fn run_pre_tool_call(
        &self,
        tool_name: &str,
        arguments: &str,
    ) -> Result<PreHookOutcome<String>, LuaRuntimeError> {
        let context = self.lua.create_table()?;
        context.set("tool_name", tool_name)?;
        context.set("arguments", arguments)?;
        self.run_pre_hook(HookEvent::PreToolCall, context, arguments.to_owned())
    }

    pub fn run_post_tool_call(
        &self,
        tool_name: &str,
        output: &str,
    ) -> Result<PostHookOutcome<String>, LuaRuntimeError> {
        let context = self.lua.create_table()?;
        context.set("tool_name", tool_name)?;
        context.set("output", output)?;
        self.run_post_hook(HookEvent::PostToolCall, context, output.to_owned())
    }

    pub fn run_post_turn(
        &self,
        response: &str,
    ) -> Result<PostHookOutcome<String>, LuaRuntimeError> {
        let context = self.lua.create_table()?;
        context.set("response", response)?;
        self.run_post_hook(HookEvent::PostTurn, context, response.to_owned())
    }

    pub fn run_on_error(&self, stage: &str, error: &str) -> Result<(), LuaRuntimeError> {
        let context = self.lua.create_table()?;
        context.set("stage", stage)?;
        context.set("error", error)?;
        self.run_observe_hook(HookEvent::OnError, context)
    }

    pub fn run_on_complete(&self) -> Result<(), LuaRuntimeError> {
        let context = self.lua.create_table()?;
        self.run_observe_hook(HookEvent::OnComplete, context)
    }

    pub fn run_on_plugin_load(
        &self,
        plugin_name: &str,
        plugin_kind: PluginKind,
    ) -> Result<(), LuaRuntimeError> {
        let context = self.lua.create_table()?;
        context.set("plugin_name", plugin_name)?;
        context.set("plugin_kind", plugin_kind_name(plugin_kind))?;
        self.run_observe_hook(HookEvent::OnPluginLoad, context)
    }

    fn plugin_environment(
        &self,
        config: &LuaRuntimeConfig,
        plugin: &crate::DiscoveredPlugin,
    ) -> Result<(Table, PluginContext), LuaRuntimeError> {
        let globals = self.lua.globals();
        let mut cloned_tables = HashMap::new();
        let env = self.clone_table(&globals, &mut cloned_tables)?;
        let plugin_context =
            PluginContext::new(plugin.name.clone(), plugin.manifest.permissions.clone());
        let plugin_genesis = install_genesis_api(
            &self.lua,
            config,
            Arc::clone(&self.logs),
            Arc::clone(&self.session_state),
            Arc::clone(&self.hook_registry),
            Arc::clone(&self.tool_registry),
            Arc::clone(&self.personality_registry),
            Arc::clone(&self.host_tool_executor),
            Arc::clone(&self.active_plugin),
            Some(plugin_context.clone()),
        )?;
        env.set("genesis", plugin_genesis)?;
        env.set("_G", env.clone())?;
        env.set("_ENV", env.clone())?;
        Ok((env, plugin_context))
    }

    fn clone_table(
        &self,
        source: &Table,
        cloned_tables: &mut HashMap<*const c_void, Table>,
    ) -> mlua::Result<Table> {
        let pointer = source.to_pointer();
        if let Some(cloned) = cloned_tables.get(&pointer) {
            return Ok(cloned.clone());
        }

        let cloned = self.lua.create_table()?;
        cloned_tables.insert(pointer, cloned.clone());

        for pair in source.pairs::<Value, Value>() {
            let (key, value) = pair?;
            let cloned_key = self.clone_value(key, cloned_tables)?;
            let cloned_value = self.clone_value(value, cloned_tables)?;
            cloned.raw_set(cloned_key, cloned_value)?;
        }

        if let Some(metatable) = source.metatable() {
            let cloned_metatable = self.clone_table(&metatable, cloned_tables)?;
            cloned.set_metatable(Some(cloned_metatable))?;
        }

        Ok(cloned)
    }

    fn clone_value(
        &self,
        value: Value,
        cloned_tables: &mut HashMap<*const c_void, Table>,
    ) -> mlua::Result<Value> {
        match value {
            Value::Table(table) => Ok(Value::Table(self.clone_table(&table, cloned_tables)?)),
            other => Ok(other),
        }
    }

    fn run_pre_hook(
        &self,
        event: HookEvent,
        context: Table,
        mut current: String,
    ) -> Result<PreHookOutcome<String>, LuaRuntimeError> {
        let callbacks = self
            .hook_registry
            .lock()
            .expect("hook registry mutex should not be poisoned")
            .callbacks(event);
        for callback in callbacks {
            if callback
                .plugin_context
                .as_ref()
                .is_some_and(|plugin| self.plugin_disabled(&plugin.name))
            {
                continue;
            }
            let plugin_name = callback
                .plugin_context
                .as_ref()
                .map(|plugin| plugin.name.clone());
            let _guard = ActivePluginGuard::push(
                Arc::clone(&self.active_plugin),
                callback.plugin_context.clone(),
            );
            let _execution_guard = plugin_name
                .as_deref()
                .map(|plugin| self.begin_plugin_execution(plugin, format!("hook `{event:?}`"), self.hook_timeout));
            if let Some(plugin_name) = plugin_name.as_deref() {
                self.push_verbose_hook_log(event, plugin_name, "invoke");
            }
            let value: Result<mlua::MultiValue, mlua::Error> =
                callback.function.call(context.clone());
            match value {
                Ok(values) => match parse_pre_hook_result(values, current.clone()) {
                    Ok(PreHookOutcome::Allow(next)) => {
                        if let Some(plugin_name) = plugin_name.as_deref() {
                            self.push_verbose_hook_log(
                                event,
                                plugin_name,
                                &format!("allow {next:?}"),
                            );
                        }
                        current = next;
                    }
                    Ok(PreHookOutcome::Veto { reason }) => {
                        if let Some(plugin_name) = plugin_name.as_deref() {
                            self.push_verbose_hook_log(
                                event,
                                plugin_name,
                                &format!("veto {:?}", reason),
                            );
                        }
                        return Ok(PreHookOutcome::Veto { reason })
                    }
                    Err(err) => {
                        self.push_hook_error(event, err.to_string());
                        if let Some(plugin_name) = plugin_name.as_deref() {
                            self.record_plugin_failure(plugin_name, &err.to_string());
                        }
                    }
                },
                Err(err) => {
                    self.push_hook_error(event, err.to_string());
                    if let Some(plugin_name) = plugin_name.as_deref() {
                        self.record_plugin_failure(plugin_name, &err.to_string());
                    }
                }
            }
        }
        Ok(PreHookOutcome::Allow(current))
    }

    fn run_post_hook(
        &self,
        event: HookEvent,
        context: Table,
        mut current: String,
    ) -> Result<PostHookOutcome<String>, LuaRuntimeError> {
        let callbacks = self
            .hook_registry
            .lock()
            .expect("hook registry mutex should not be poisoned")
            .callbacks(event);
        for callback in callbacks {
            if callback
                .plugin_context
                .as_ref()
                .is_some_and(|plugin| self.plugin_disabled(&plugin.name))
            {
                continue;
            }
            let plugin_name = callback
                .plugin_context
                .as_ref()
                .map(|plugin| plugin.name.clone());
            let _guard = ActivePluginGuard::push(
                Arc::clone(&self.active_plugin),
                callback.plugin_context.clone(),
            );
            let _execution_guard = plugin_name
                .as_deref()
                .map(|plugin| self.begin_plugin_execution(plugin, format!("hook `{event:?}`"), self.hook_timeout));
            if let Some(plugin_name) = plugin_name.as_deref() {
                self.push_verbose_hook_log(event, plugin_name, "invoke");
            }
            let value: Result<mlua::MultiValue, mlua::Error> =
                callback.function.call(context.clone());
            match value {
                Ok(values) => match parse_post_hook_result(values, current.clone()) {
                    Ok(PostHookOutcome::Keep(next)) => {
                        if let Some(plugin_name) = plugin_name.as_deref() {
                            self.push_verbose_hook_log(
                                event,
                                plugin_name,
                                &format!("keep {next:?}"),
                            );
                        }
                        current = next;
                    }
                    Ok(PostHookOutcome::Rewrite(next)) => {
                        if let Some(plugin_name) = plugin_name.as_deref() {
                            self.push_verbose_hook_log(
                                event,
                                plugin_name,
                                &format!("rewrite {next:?}"),
                            );
                        }
                        current = next;
                    }
                    Err(err) => {
                        self.push_hook_error(event, err.to_string());
                        if let Some(plugin_name) = plugin_name.as_deref() {
                            self.record_plugin_failure(plugin_name, &err.to_string());
                        }
                    }
                },
                Err(err) => {
                    self.push_hook_error(event, err.to_string());
                    if let Some(plugin_name) = plugin_name.as_deref() {
                        self.record_plugin_failure(plugin_name, &err.to_string());
                    }
                }
            }
        }
        Ok(PostHookOutcome::Rewrite(current))
    }

    fn run_observe_hook(&self, event: HookEvent, context: Table) -> Result<(), LuaRuntimeError> {
        let callbacks = self
            .hook_registry
            .lock()
            .expect("hook registry mutex should not be poisoned")
            .callbacks(event);
        for callback in callbacks {
            if callback
                .plugin_context
                .as_ref()
                .is_some_and(|plugin| self.plugin_disabled(&plugin.name))
            {
                continue;
            }
            let plugin_name = callback
                .plugin_context
                .as_ref()
                .map(|plugin| plugin.name.clone());
            let _guard = ActivePluginGuard::push(
                Arc::clone(&self.active_plugin),
                callback.plugin_context.clone(),
            );
            let _execution_guard = plugin_name
                .as_deref()
                .map(|plugin| self.begin_plugin_execution(plugin, format!("hook `{event:?}`"), self.hook_timeout));
            if let Some(plugin_name) = plugin_name.as_deref() {
                self.push_verbose_hook_log(event, plugin_name, "invoke");
            }
            let value: Result<mlua::MultiValue, mlua::Error> =
                callback.function.call(context.clone());
            match value {
                Ok(_) => {
                    if let Some(plugin_name) = plugin_name.as_deref() {
                        self.push_verbose_hook_log(event, plugin_name, "ok");
                    }
                }
                Err(err) => {
                    self.push_hook_error(event, err.to_string());
                    if let Some(plugin_name) = plugin_name.as_deref() {
                        self.record_plugin_failure(plugin_name, &err.to_string());
                    }
                }
            }
        }
        Ok(())
    }

    fn push_hook_error(&self, event: HookEvent, message: String) {
        let mut logs = self
            .logs
            .lock()
            .expect("log sink mutex should not be poisoned");
        logs.push(format!("[hook::{event:?}] {message}"));
    }

    fn push_verbose_hook_log(&self, event: HookEvent, plugin_name: &str, message: &str) {
        if !self.plugin_verbose {
            return;
        }
        let mut logs = self
            .logs
            .lock()
            .expect("log sink mutex should not be poisoned");
        logs.push(format!(
            "[hook::{event:?}::{plugin_name}] {message}"
        ));
    }

    fn resolve_personality_prompt(&self, entry: LuaPersonalityEntry) -> Option<String> {
        let fallback = entry.metadata.system_prompt.clone();
        let Some(build_prompt) = entry.build_prompt else {
            return fallback;
        };

        let session = self
            .session_state
            .lock()
            .expect("session state mutex should not be poisoned")
            .clone();
        let context = match self.build_personality_context(&session) {
            Ok(context) => context,
            Err(error) => {
                self.push_personality_error(
                    &entry.metadata.plugin_name,
                    format!("failed to build prompt context: {error}"),
                );
                return fallback;
            }
        };
        let plugin_context = PluginContext::for_execution(
            entry.metadata.plugin_name.clone(),
            entry.permissions.clone(),
        );
        let _guard = ActivePluginGuard::push(Arc::clone(&self.active_plugin), Some(plugin_context));
        let _execution_guard = self.begin_plugin_execution(
            &entry.metadata.plugin_name,
            format!("personality `{}`", entry.metadata.name),
            self.hook_timeout,
        );

        match build_prompt.call::<Value>(context) {
            Ok(Value::Nil) => fallback,
            Ok(Value::String(text)) => match text.to_str() {
                Ok(text) => Some(text.to_owned()),
                Err(error) => {
                    self.push_personality_error(
                        &entry.metadata.plugin_name,
                        format!("build_prompt returned invalid utf-8: {error}"),
                    );
                    self.record_plugin_failure(&entry.metadata.plugin_name, &error.to_string());
                    fallback
                }
            },
            Ok(other) => match other.to_string() {
                Ok(text) => Some(text),
                Err(error) => {
                    self.push_personality_error(
                        &entry.metadata.plugin_name,
                        format!("build_prompt returned unsupported value: {error}"),
                    );
                    fallback
                }
            },
            Err(error) => {
                self.push_personality_error(
                    &entry.metadata.plugin_name,
                    format!("build_prompt failed: {error}"),
                );
                self.record_plugin_failure(&entry.metadata.plugin_name, &error.to_string());
                fallback
            }
        }
    }

    fn build_personality_context(
        &self,
        session: &LuaSessionContext,
    ) -> Result<Table, LuaRuntimeError> {
        let context = self.lua.create_table()?;
        context.set("id", session.id.clone())?;
        context.set("model", session.model.clone())?;
        context.set("turn_count", session.turn_count)?;
        context.set("total_tokens", session.total_tokens)?;
        context.set("platform", session.platform.clone())?;
        context.set("personality", session.personality.clone())?;
        Ok(context)
    }

    fn push_personality_error(&self, plugin_name: &str, message: String) {
        let mut logs = self
            .logs
            .lock()
            .expect("log sink mutex should not be poisoned");
        logs.push(format!("[personality::{plugin_name}] {message}"));
    }

    fn begin_plugin_execution(
        &self,
        plugin_name: &str,
        operation: impl Into<String>,
        timeout: Duration,
    ) -> PluginExecutionGuard {
        let operation = operation.into();
        let started_at = Instant::now();
        let timeout_ms = timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        let active = ActivePluginExecution {
            plugin_name: plugin_name.to_owned(),
            operation: operation.clone(),
            started_at,
            deadline: started_at + timeout,
            timeout_ms,
        };
        let previous = self
            .execution_control
            .lock()
            .expect("plugin execution control mutex should not be poisoned")
            .active
            .replace(active);
        PluginExecutionGuard {
            control: Arc::clone(&self.execution_control),
            previous,
            logs: Arc::clone(&self.logs),
            plugin_name: plugin_name.to_owned(),
            operation,
            plugin_verbose: self.plugin_verbose,
        }
    }

    fn plugin_disabled(&self, plugin_name: &str) -> bool {
        self.disabled_plugins
            .lock()
            .expect("disabled plugins mutex should not be poisoned")
            .contains(plugin_name)
    }

    fn disabled_plugin_names(&self) -> HashSet<String> {
        self.disabled_plugins
            .lock()
            .expect("disabled plugins mutex should not be poisoned")
            .clone()
    }

    fn record_plugin_failure(&self, plugin_name: &str, _message: &str) {
        if self.auto_disable_after == 0 {
            return;
        }
        if self.plugin_disabled(plugin_name) {
            return;
        }

        let failures = {
            let mut failures = self
                .plugin_failures
                .lock()
                .expect("plugin failures mutex should not be poisoned");
            let count = failures.entry(plugin_name.to_owned()).or_insert(0);
            *count += 1;
            *count
        };

        if failures < self.auto_disable_after {
            return;
        }

        let newly_disabled = self
            .disabled_plugins
            .lock()
            .expect("disabled plugins mutex should not be poisoned")
            .insert(plugin_name.to_owned());
        if newly_disabled {
            let mut logs = self
                .logs
                .lock()
                .expect("log sink mutex should not be poisoned");
            logs.push(format!(
                "[plugin::{plugin_name}] disabled for this session after {failures} failures"
            ));
        }
    }
}

fn plugin_kind_name(kind: PluginKind) -> &'static str {
    match kind {
        PluginKind::SingleFile => "single_file",
        PluginKind::Package => "package",
    }
}

struct ActivePluginGuard {
    stack: Arc<Mutex<Vec<PluginContext>>>,
    pushed: bool,
}

struct PluginExecutionGuard {
    control: Arc<Mutex<PluginExecutionControl>>,
    previous: Option<ActivePluginExecution>,
    logs: Arc<Mutex<Vec<String>>>,
    plugin_name: String,
    operation: String,
    plugin_verbose: bool,
}

impl ActivePluginGuard {
    fn push(stack: Arc<Mutex<Vec<PluginContext>>>, plugin_context: Option<PluginContext>) -> Self {
        let pushed = if let Some(plugin_context) = plugin_context {
            stack
                .lock()
                .expect("active plugin mutex should not be poisoned")
                .push(plugin_context);
            true
        } else {
            false
        };
        Self { stack, pushed }
    }
}

impl Drop for ActivePluginGuard {
    fn drop(&mut self) {
        if self.pushed {
            self.stack
                .lock()
                .expect("active plugin mutex should not be poisoned")
            .pop();
        }
    }
}

impl Drop for PluginExecutionGuard {
    fn drop(&mut self) {
        let finished = Instant::now();
        let previous = {
            let mut control = self
                .control
                .lock()
                .expect("plugin execution control mutex should not be poisoned");
            let current = control.active.take();
            control.active = self.previous.take();
            current
        };

        if self.plugin_verbose {
            if let Some(current) = previous {
                let elapsed_ms = finished
                    .saturating_duration_since(current.started_at)
                    .as_millis();
                self.logs
                    .lock()
                    .expect("log sink mutex should not be poisoned")
                    .push(format!(
                        "[plugin::{}] {} completed in {}ms",
                        self.plugin_name, self.operation, elapsed_ms
                    ));
            }
        }
    }
}
