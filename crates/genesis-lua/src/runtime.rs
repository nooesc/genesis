use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt, Table, Value};
use thiserror::Error;

use crate::{api::install_genesis_api, discovery::discover_plugins_best_effort};

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
    pub config_values: BTreeMap<String, String>,
}

pub struct LuaRuntime {
    lua: Lua,
    plugin_names: Vec<String>,
    logs: Arc<Mutex<Vec<String>>>,
    plugin_errors: Vec<String>,
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

impl LuaRuntime {
    fn new(config: LuaRuntimeConfig) -> Result<Self, LuaRuntimeError> {
        let lua = Lua::new();
        let logs = Arc::new(Mutex::new(Vec::new()));
        let genesis = install_genesis_api(&lua, &config, Arc::clone(&logs))?;
        lua.globals().set("genesis", genesis)?;

        let mut runtime = Self {
            lua,
            plugin_names: Vec::new(),
            logs,
            plugin_errors: Vec::new(),
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

        for plugin in report.plugins {
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
            let plugin_env = self.plugin_environment()?;
            if let Err(err) = self
                .lua
                .load(&source)
                .set_name(&plugin.name)
                .set_environment(plugin_env)
                .exec()
            {
                self.plugin_errors
                    .push(format!("plugin `{}` failed to load: {err}", plugin.name));
                continue;
            }
            self.plugin_names.push(plugin.name);
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

    fn plugin_environment(&self) -> Result<Table, LuaRuntimeError> {
        let globals = self.lua.globals();
        let mut cloned_tables = HashMap::new();
        let env = self.clone_table(&globals, &mut cloned_tables)?;
        env.set("_G", env.clone())?;
        env.set("_ENV", env.clone())?;
        Ok(env)
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
}
