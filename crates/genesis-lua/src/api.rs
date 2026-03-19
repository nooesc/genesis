use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use genesis_storage::MemoryStore;
use mlua::{Function, Lua, LuaSerdeExt, Table, UserData, UserDataFields, Value};

use crate::{
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
    plugin_context: Option<PluginContext>,
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
        *self
            .load_active
            .lock()
            .expect("plugin load state mutex should not be poisoned") = false;
    }

    fn load_active(&self) -> bool {
        *self
            .load_active
            .lock()
            .expect("plugin load state mutex should not be poisoned")
    }

    pub(crate) fn for_execution(name: String, permissions: PluginPermissions) -> Self {
        let context = Self::new(name, permissions);
        context.close_tool_registration();
        context
    }
}

impl UserData for SessionView {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| Ok(this.ctx.lock().unwrap().id.clone()));
        fields.add_field_method_get("model", |_, this| {
            Ok(this.ctx.lock().unwrap().model.clone())
        });
        fields.add_field_method_get("turn_count", |_, this| {
            Ok(this.ctx.lock().unwrap().turn_count)
        });
        fields.add_field_method_get("total_tokens", |_, this| {
            Ok(this.ctx.lock().unwrap().total_tokens)
        });
        fields.add_field_method_get("platform", |_, this| {
            Ok(this.ctx.lock().unwrap().platform.clone())
        });
        fields.add_field_method_get("personality", |_, this| {
            Ok(this.ctx.lock().unwrap().personality.clone())
        });
    }
}

impl UserData for ConfigView {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_function_get("get", |lua, ud| {
            let values = ud.borrow::<ConfigView>()?.values.clone();
            lua.create_function(move |_, key: String| Ok(values.get(&key).cloned()))
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
                    .expect("hook registry mutex should not be poisoned")
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
                    .expect("tool registry mutex should not be poisoned")
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
                    .expect("personality registry mutex should not be poisoned")
                    .register(&plugin_context.name, &plugin_context.permissions, spec)
                    .map_err(mlua::Error::external)?;
                Ok(())
            })
        });
    }
}

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
    plugin_context: Option<PluginContext>,
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
        plugin_context,
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
                .expect("session state mutex should not be poisoned")
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
        let mut stored = logs.lock().expect("log sink mutex should not be poisoned");
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
                .expect("active plugin mutex should not be poisoned")
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
                .expect("host tools mutex should not be poisoned")
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
