use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt};
use thiserror::Error;

use crate::{api::install_genesis_api, discover_plugins};

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
}

impl std::fmt::Debug for LuaRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaRuntime")
            .field("plugin_names", &self.plugin_names)
            .field("logs", &self.logs())
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
        };
        runtime.load_plugins(&config)?;
        Ok(runtime)
    }

    pub fn load_plugins(&mut self, config: &LuaRuntimeConfig) -> Result<(), LuaRuntimeError> {
        if config.plugin_dir.as_os_str().is_empty() {
            return Ok(());
        }

        let plugins = discover_plugins(&config.plugin_dir)?;
        for plugin in plugins {
            let source = fs::read_to_string(&plugin.entrypoint).map_err(|source| {
                LuaRuntimeError::ReadPluginSource {
                    path: plugin.entrypoint.clone(),
                    source,
                }
            })?;
            self.lua.load(&source).set_name(&plugin.name).exec()?;
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
}
