use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use genesis_storage::MemoryStore;
use genesis_tools::sandbox::PathValidator;
use mlua::{Function, Lua, LuaSerdeExt, Table, UserData, UserDataFields, Value};

use crate::{
    context_registry::PluginContextRegistry,
    hooks::{HookEvent, HookRegistry},
    manifest::PluginPermissions,
    personality::LuaPersonalityRegistry,
    tools::{LuaHostToolExecutor, LuaToolRegistry},
    LuaRuntimeConfig, LuaRuntimeError, LuaSessionContext,
};

#[derive(Clone)]
pub struct GenesisApi {
    version: String,
    plugin_dir: String,
    database_path: PathBuf,
    session: Arc<Mutex<LuaSessionContext>>,
    config_values: Arc<std::collections::BTreeMap<String, String>>,
    logs: Arc<Mutex<Vec<String>>>,
    hooks: Arc<Mutex<HookRegistry>>,
    tools: Arc<Mutex<LuaToolRegistry>>,
    personalities: Arc<Mutex<LuaPersonalityRegistry>>,
    host_tools: Arc<Mutex<Option<Arc<dyn LuaHostToolExecutor>>>>,
    active_plugin: Arc<Mutex<Vec<PluginContext>>>,
    context_registry: PluginContextRegistry,
    plugin_names: Arc<Mutex<Vec<String>>>,
    plugin_context: Option<PluginContext>,
    pub(crate) path_validator: Option<Arc<PathValidator>>,
    pub(crate) working_dir: Option<PathBuf>,
    pub(crate) http_client: Option<Arc<reqwest::blocking::Client>>,
}

#[derive(Debug, Clone)]
struct SessionView {
    ctx: Arc<Mutex<LuaSessionContext>>,
}

#[derive(Debug, Clone)]
struct ConfigView {
    values: Arc<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct PluginContext {
    pub name: String,
    pub permissions: PluginPermissions,
    load_active: Arc<Mutex<bool>>,
}

impl PluginContext {
    pub(crate) fn new(name: String, permissions: PluginPermissions) -> Self {
        Self {
            name,
            permissions,
            load_active: Arc::new(Mutex::new(true)),
        }
    }

    pub(crate) fn close_tool_registration(&self) {
        *self.load_active.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("lua plugin 'load_active' mutex was poisoned, recovering");
            poisoned.into_inner()
        }) = false;
    }

    fn load_active(&self) -> bool {
        *self.load_active.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("lua plugin 'load_active' mutex was poisoned, recovering");
            poisoned.into_inner()
        })
    }

    pub(crate) fn for_execution(name: String, permissions: PluginPermissions) -> Self {
        let context = Self::new(name, permissions);
        context.close_tool_registration();
        context
    }

    /// Check whether this plugin has access to a given primitive.
    pub(crate) fn has_primitive(&self, name: &str) -> bool {
        self.permissions.trusted || self.permissions.primitives.iter().any(|p| p == name)
    }
}

impl UserData for SessionView {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| {
            Ok(this
                .ctx
                .lock()
                .map_err(|e| mlua::Error::external(format!("session context lock poisoned: {e}")))?
                .id
                .clone())
        });
        fields.add_field_method_get("model", |_, this| {
            Ok(this
                .ctx
                .lock()
                .map_err(|e| mlua::Error::external(format!("session context lock poisoned: {e}")))?
                .model
                .clone())
        });
        fields.add_field_method_get("turn_count", |_, this| {
            Ok(this
                .ctx
                .lock()
                .map_err(|e| mlua::Error::external(format!("session context lock poisoned: {e}")))?
                .turn_count)
        });
        fields.add_field_method_get("total_tokens", |_, this| {
            Ok(this
                .ctx
                .lock()
                .map_err(|e| mlua::Error::external(format!("session context lock poisoned: {e}")))?
                .total_tokens)
        });
        fields.add_field_method_get("platform", |_, this| {
            Ok(this
                .ctx
                .lock()
                .map_err(|e| mlua::Error::external(format!("session context lock poisoned: {e}")))?
                .platform
                .clone())
        });
        fields.add_field_method_get("personality", |_, this| {
            Ok(this
                .ctx
                .lock()
                .map_err(|e| mlua::Error::external(format!("session context lock poisoned: {e}")))?
                .personality
                .clone())
        });
    }
}

const ENV_ALLOWED_PREFIXES: &[&str] = &[
    "GENESIS_",
    "TELEGRAM_",
    "SLACK_",
    "DISCORD_",
    "WHATSAPP_",
    "SIGNAL_",
    "OPENAI_",
    "ANTHROPIC_",
    "OPENROUTER_",
    "HOMEASSISTANT_",
    "GOOGLE_",
    "ELEVENLABS_",
    "DEEPGRAM_",
    "BRAVE_",
    "HASS_",
];

impl UserData for ConfigView {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_function_get("get", |lua, ud| {
            let values = ud.borrow::<ConfigView>()?.values.clone();
            lua.create_function(move |_, key: String| Ok(values.get(&key).cloned()))
        });
        fields.add_field_function_get("env", |lua, _ud| {
            lua.create_function(|lua, key: String| {
                let allowed = ENV_ALLOWED_PREFIXES
                    .iter()
                    .any(|prefix| key.starts_with(prefix));
                if !allowed {
                    return Ok(Value::Nil);
                }
                match std::env::var(&key) {
                    Ok(val) => Ok(Value::String(lua.create_string(&val)?)),
                    Err(_) => Ok(Value::Nil),
                }
            })
        });
    }
}

impl UserData for GenesisApi {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("version", |_, this| Ok(this.version.clone()));
        fields.add_field_method_get("plugin_dir", |_, this| Ok(this.plugin_dir.clone()));
        fields.add_field_method_get("session", |lua, this| {
            lua.create_userdata(SessionView {
                ctx: this.session.clone(),
            })
        });
        fields.add_field_method_get("config", |lua, this| {
            lua.create_userdata(ConfigView {
                values: Arc::clone(&this.config_values),
            })
        });
        fields.add_field_method_get("log", |lua, this| {
            make_logger(lua, Arc::clone(&this.logs), None)
        });
        fields.add_field_method_get("log_warn", |lua, this| {
            make_logger(lua, Arc::clone(&this.logs), Some("[warn] "))
        });
        fields.add_field_method_get("log_error", |lua, this| {
            make_logger(lua, Arc::clone(&this.logs), Some("[error] "))
        });
        fields.add_field_method_get("tools", |lua, this| {
            make_tool_bridge(
                lua,
                Arc::clone(&this.host_tools),
                Arc::clone(&this.active_plugin),
            )
        });
        fields.add_field_method_get("memory", |lua, this| {
            make_memory_bridge(lua, this.database_path.clone(), Arc::clone(&this.session))
        });
        fields.add_field_method_get("json", |lua, _this| {
            crate::primitives::json::make_json_bridge(lua)
        });
        fields.add_field_method_get("fs", |lua, this| {
            if let Some(ref ctx) = this.plugin_context {
                if !ctx.has_primitive("fs") {
                    return Ok(mlua::Value::Nil);
                }
            }
            crate::primitives::fs::make_fs_bridge(lua, this.path_validator.clone())
                .map(mlua::Value::Table)
        });
        fields.add_field_method_get("process", |lua, this| {
            if let Some(ref ctx) = this.plugin_context {
                if !ctx.has_primitive("process") {
                    return Ok(mlua::Value::Nil);
                }
            }
            crate::primitives::process::make_process_bridge(
                lua,
                this.working_dir.clone(),
                this.path_validator.clone(),
            )
            .map(mlua::Value::Table)
        });
        fields.add_field_method_get("http", |lua, this| {
            if let Some(ref ctx) = this.plugin_context {
                if !ctx.has_primitive("http") {
                    return Ok(mlua::Value::Nil);
                }
            }
            crate::primitives::http::make_http_bridge(lua, this.http_client.clone())
                .map(mlua::Value::Table)
        });
        fields.add_field_method_get("search", |lua, this| {
            if let Some(ref ctx) = this.plugin_context {
                if !ctx.has_primitive("search") {
                    return Ok(mlua::Value::Nil);
                }
            }
            crate::primitives::search::make_search_bridge(
                lua,
                this.path_validator.clone(),
                this.working_dir.clone(),
            )
            .map(mlua::Value::Table)
        });
        fields.add_field_method_get("storage", |lua, this| {
            if let Some(ref ctx) = this.plugin_context {
                if !ctx.has_primitive("storage") {
                    return Ok(mlua::Value::Nil);
                }
            }
            crate::primitives::storage::make_storage_bridge(lua, this.database_path.clone())
                .map(mlua::Value::Table)
        });
        fields.add_field_method_get("context", |lua, this| {
            let registry = this.context_registry.clone();
            let plugin_name = this.plugin_context.as_ref().map(|c| c.name.clone());
            let table = lua.create_table()?;

            let add_registry = registry.clone();
            let add_plugin = plugin_name.clone();
            table.set(
                "add",
                lua.create_function(move |_, content: String| {
                    let name = add_plugin.as_deref().unwrap_or("unknown");
                    let accepted = add_registry.add(name, content);
                    Ok(accepted)
                })?,
            )?;

            let clear_registry = registry;
            let clear_plugin = plugin_name;
            table.set(
                "clear",
                lua.create_function(move |_, ()| {
                    let name = clear_plugin.as_deref().unwrap_or("unknown");
                    clear_registry.clear_for_plugin(name);
                    Ok(())
                })?,
            )?;

            Ok(table)
        });
        fields.add_field_method_get("plugins", |lua, this| {
            let plugin_names = Arc::clone(&this.plugin_names);
            let current_plugin = this.plugin_context.as_ref().map(|c| c.name.clone());
            let table = lua.create_table()?;

            table.set(
                "list",
                lua.create_function(move |lua, ()| {
                    let names = plugin_names.lock().unwrap_or_else(|p| p.into_inner());
                    let result = lua.create_table()?;
                    for (i, name) in names.iter().enumerate() {
                        let entry = lua.create_table()?;
                        entry.set("name", name.clone())?;
                        result.set(i + 1, entry)?;
                    }
                    Ok(result)
                })?,
            )?;

            table.set(
                "current",
                lua.create_function(move |lua, ()| match &current_plugin {
                    Some(name) => {
                        let entry = lua.create_table()?;
                        entry.set("name", name.clone())?;
                        Ok(mlua::Value::Table(entry))
                    }
                    None => Ok(mlua::Value::Nil),
                })?,
            )?;

            Ok(table)
        });
        fields.add_field_method_get("tools_list", |lua, this| {
            let tools = Arc::clone(&this.tools);
            lua.create_function(move |lua, ()| {
                let registry = tools.lock().unwrap_or_else(|p| p.into_inner());
                let all_tools = registry.registered_tools();
                let result = lua.create_table()?;
                for (i, tool) in all_tools.iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("name", tool.definition.name.clone())?;
                    entry.set("description", tool.definition.description.clone())?;
                    entry.set("plugin", tool.plugin_name.clone())?;
                    result.set(i + 1, entry)?;
                }
                Ok(result)
            })
        });
        fields.add_field_method_get("on", |lua, this| {
            let hooks = Arc::clone(&this.hooks);
            let plugin_context = this.plugin_context.clone();
            lua.create_function(move |_, (event_name, callback): (String, Function)| {
                let plugin_context = plugin_context.clone().ok_or_else(|| {
                    mlua::Error::external(LuaRuntimeError::HookRegistrationUnavailable)
                })?;
                if !plugin_context.load_active() {
                    return Err(mlua::Error::external(
                        LuaRuntimeError::HookRegistrationUnavailable,
                    ));
                }
                let requested_event = event_name.clone();
                let event = HookEvent::from_name(&requested_event).ok_or_else(|| {
                    mlua::Error::external(LuaRuntimeError::UnsupportedHookEvent {
                        event: requested_event,
                    })
                })?;
                if !plugin_context.permissions.trusted
                    && !plugin_context
                        .permissions
                        .hooks
                        .iter()
                        .any(|hook| HookEvent::from_name(hook) == Some(event))
                {
                    return Err(mlua::Error::external(
                        LuaRuntimeError::HookPermissionDenied {
                            plugin_name: plugin_context.name.clone(),
                            event: event_name.clone(),
                        },
                    ));
                }
                hooks
                    .lock()
                    .unwrap_or_else(|poisoned| {
                        tracing::warn!("lua plugin 'hooks' mutex was poisoned, recovering");
                        poisoned.into_inner()
                    })
                    .register(event, callback, Some(plugin_context));
                Ok(())
            })
        });
        fields.add_field_method_get("register_tool", |lua, this| {
            let tools = Arc::clone(&this.tools);
            let plugin_context = this.plugin_context.clone();
            lua.create_function(move |lua, spec: Table| {
                let plugin_context = plugin_context.clone().ok_or_else(|| {
                    mlua::Error::external(LuaRuntimeError::ToolRegistrationUnavailable)
                })?;
                if !plugin_context.load_active() {
                    return Err(mlua::Error::external(
                        LuaRuntimeError::ToolRegistrationUnavailable,
                    ));
                }
                tools
                    .lock()
                    .unwrap_or_else(|poisoned| {
                        tracing::warn!("lua plugin 'tools' mutex was poisoned, recovering");
                        poisoned.into_inner()
                    })
                    .register(lua, &plugin_context.name, &plugin_context.permissions, spec)
                    .map_err(mlua::Error::external)?;
                Ok(())
            })
        });
        fields.add_field_method_get("register_personality", |lua, this| {
            let personalities = Arc::clone(&this.personalities);
            let plugin_context = this.plugin_context.clone();
            lua.create_function(move |_, spec: Table| {
                let plugin_context = plugin_context.clone().ok_or_else(|| {
                    mlua::Error::external(LuaRuntimeError::PersonalityRegistrationUnavailable)
                })?;
                if !plugin_context.load_active() {
                    return Err(mlua::Error::external(
                        LuaRuntimeError::PersonalityRegistrationUnavailable,
                    ));
                }
                personalities
                    .lock()
                    .unwrap_or_else(|poisoned| {
                        tracing::warn!("lua plugin 'personalities' mutex was poisoned, recovering");
                        poisoned.into_inner()
                    })
                    .register(&plugin_context.name, &plugin_context.permissions, spec)
                    .map_err(mlua::Error::external)?;
                Ok(())
            })
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_genesis_api(
    lua: &Lua,
    config: &LuaRuntimeConfig,
    logs: Arc<Mutex<Vec<String>>>,
    session: Arc<Mutex<LuaSessionContext>>,
    hooks: Arc<Mutex<HookRegistry>>,
    tools: Arc<Mutex<LuaToolRegistry>>,
    personalities: Arc<Mutex<LuaPersonalityRegistry>>,
    host_tools: Arc<Mutex<Option<Arc<dyn LuaHostToolExecutor>>>>,
    active_plugin: Arc<Mutex<Vec<PluginContext>>>,
    context_registry: PluginContextRegistry,
    plugin_names: Arc<Mutex<Vec<String>>>,
    plugin_context: Option<PluginContext>,
    path_validator: Option<Arc<PathValidator>>,
    working_dir: Option<PathBuf>,
    http_client: Option<Arc<reqwest::blocking::Client>>,
) -> Result<mlua::AnyUserData, LuaRuntimeError> {
    Ok(lua.create_userdata(GenesisApi {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        plugin_dir: config.plugin_dir.to_string_lossy().into_owned(),
        database_path: config
            .config_values
            .get("database_path")
            .map(PathBuf::from)
            .unwrap_or_default(),
        session,
        config_values: Arc::new(config.config_values.clone()),
        logs,
        hooks,
        tools,
        personalities,
        host_tools,
        active_plugin,
        context_registry,
        plugin_names,
        plugin_context,
        path_validator,
        working_dir,
        http_client,
    })?)
}

fn make_memory_bridge(
    lua: &Lua,
    database_path: PathBuf,
    session: Arc<Mutex<LuaSessionContext>>,
) -> mlua::Result<Table> {
    let memory = lua.create_table()?;

    let list_path = database_path.clone();
    memory.set(
        "list",
        lua.create_function(move |lua, limit: Option<usize>| {
            let store = MemoryStore::new(&list_path);
            let memories = store
                .list(limit.unwrap_or(10))
                .map_err(mlua::Error::external)?;
            lua.to_value(&memories)
        })?,
    )?;

    let search_path = database_path.clone();
    memory.set(
        "search",
        lua.create_function(move |lua, (query, limit): (String, Option<usize>)| {
            let store = MemoryStore::new(&search_path);
            let memories = store
                .search(&query, limit.unwrap_or(5))
                .map_err(mlua::Error::external)?;
            lua.to_value(&memories)
        })?,
    )?;

    memory.set(
        "create",
        lua.create_function(move |lua, (content, metadata): (String, Option<Value>)| {
            let kind = resolve_memory_kind(metadata)?;
            let session_id = session
                .lock()
                .unwrap_or_else(|poisoned| {
                    tracing::warn!("lua plugin 'session' mutex was poisoned, recovering");
                    poisoned.into_inner()
                })
                .id
                .clone();
            let store = MemoryStore::new(&database_path);
            let created = store
                .create(Some(&session_id), &kind, &content)
                .map_err(mlua::Error::external)?;
            lua.to_value(&created)
        })?,
    )?;

    Ok(memory)
}

fn resolve_memory_kind(metadata: Option<Value>) -> mlua::Result<String> {
    match metadata {
        None | Some(Value::Nil) => Ok("note".to_owned()),
        Some(Value::String(kind)) => Ok(kind.to_str()?.to_owned()),
        Some(Value::Table(table)) => {
            if let Ok(kind) = table.get::<String>("kind") {
                return Ok(kind);
            }
            if let Ok(kind) = table.get::<String>(1) {
                return Ok(kind);
            }
            Ok("note".to_owned())
        }
        Some(other) => Err(mlua::Error::external(format!(
            "unsupported memory metadata: {}",
            other.type_name()
        ))),
    }
}

fn make_logger(
    lua: &Lua,
    logs: Arc<Mutex<Vec<String>>>,
    prefix: Option<&'static str>,
) -> mlua::Result<mlua::Function> {
    lua.create_function(move |_, message: String| {
        let mut stored = logs.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("lua plugin 'logs' mutex was poisoned, recovering");
            poisoned.into_inner()
        });
        stored.push(match prefix {
            Some(prefix) => format!("{prefix}{message}"),
            None => message,
        });
        Ok(())
    })
}

fn make_tool_bridge(
    lua: &Lua,
    host_tools: Arc<Mutex<Option<Arc<dyn LuaHostToolExecutor>>>>,
    active_plugin: Arc<Mutex<Vec<PluginContext>>>,
) -> mlua::Result<Table> {
    let tools = lua.create_table()?;
    let metatable = lua.create_table()?;
    let index = lua.create_function(move |lua, (_table, tool_name): (Table, String)| {
        let host_tools = Arc::clone(&host_tools);
        let active_plugin = Arc::clone(&active_plugin);
        lua.create_function(move |lua, args: Option<Table>| {
            let plugin_context = active_plugin
                .lock()
                .unwrap_or_else(|poisoned| {
                    tracing::warn!("lua plugin 'active_plugin' mutex was poisoned, recovering");
                    poisoned.into_inner()
                })
                .last()
                .cloned()
                .ok_or_else(|| {
                    mlua::Error::external(LuaRuntimeError::HostToolContextUnavailable)
                })?;
            if !plugin_context.permissions.trusted
                && !plugin_context
                    .permissions
                    .tools
                    .iter()
                    .any(|allowed| allowed == &tool_name)
            {
                return Err(mlua::Error::external(
                    LuaRuntimeError::HostToolPermissionDenied {
                        plugin_name: plugin_context.name.clone(),
                        tool_name: tool_name.clone(),
                    },
                ));
            }

            let executor = host_tools
                .lock()
                .unwrap_or_else(|poisoned| {
                    tracing::warn!("lua plugin 'host_tools' mutex was poisoned, recovering");
                    poisoned.into_inner()
                })
                .clone()
                .ok_or_else(|| mlua::Error::external(LuaRuntimeError::HostToolBridgeUnavailable))?;
            let arguments = table_to_string_arguments(lua, args)?;
            let output = executor.execute(&tool_name, arguments).map_err(|reason| {
                mlua::Error::external(LuaRuntimeError::HostToolExecutionFailed {
                    tool_name: tool_name.clone(),
                    reason,
                })
            })?;
            Ok(output)
        })
    })?;
    metatable.set("__index", index)?;
    tools.set_metatable(Some(metatable))?;
    Ok(tools)
}

fn table_to_string_arguments(
    lua: &Lua,
    args: Option<Table>,
) -> mlua::Result<std::collections::BTreeMap<String, String>> {
    let Some(args) = args else {
        return Ok(std::collections::BTreeMap::new());
    };

    let mut flattened = std::collections::BTreeMap::new();
    for pair in args.pairs::<String, Value>() {
        let (key, value) = pair?;
        let value = match value {
            Value::Nil => continue,
            Value::String(text) => text.to_str()?.to_owned(),
            Value::Boolean(flag) => flag.to_string(),
            Value::Integer(number) => number.to_string(),
            Value::Number(number) => number.to_string(),
            other => {
                let json = lua.from_value::<serde_json::Value>(other)?;
                json.to_string()
            }
        };
        flattened.insert(key, value);
    }

    Ok(flattened)
}
