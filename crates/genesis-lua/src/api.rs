use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, Table, UserData, UserDataFields};

use crate::{
    hooks::{HookEvent, HookRegistry},
    manifest::PluginPermissions,
    tools::LuaToolRegistry,
    LuaRuntimeConfig, LuaRuntimeError, LuaSessionContext,
};

#[derive(Debug, Clone)]
pub struct GenesisApi {
    version: String,
    plugin_dir: String,
    session: Arc<Mutex<LuaSessionContext>>,
    config_values: Arc<std::collections::BTreeMap<String, String>>,
    logs: Arc<Mutex<Vec<String>>>,
    hooks: Arc<Mutex<HookRegistry>>,
    tools: Arc<Mutex<LuaToolRegistry>>,
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

    fn tool_registration_open(&self) -> bool {
        *self
            .load_active
            .lock()
            .expect("plugin load state mutex should not be poisoned")
    }
}

impl UserData for SessionView {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| Ok(this.ctx.lock().unwrap().id.clone()));
        fields.add_field_method_get("model", |_, this| Ok(this.ctx.lock().unwrap().model.clone()));
        fields.add_field_method_get("turn_count", |_, this| Ok(this.ctx.lock().unwrap().turn_count));
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
        fields.add_field_method_get("on", |lua, this| {
            let hooks = Arc::clone(&this.hooks);
            lua.create_function(move |_, (event_name, callback): (String, Function)| {
                let event = HookEvent::from_name(&event_name).ok_or_else(|| {
                    mlua::Error::external(LuaRuntimeError::UnsupportedHookEvent {
                        event: event_name,
                    })
                })?;
                hooks
                    .lock()
                    .expect("hook registry mutex should not be poisoned")
                    .register(event, callback);
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
                if !plugin_context.tool_registration_open() {
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
    }
}

pub(crate) fn install_genesis_api(
    lua: &Lua,
    config: &LuaRuntimeConfig,
    logs: Arc<Mutex<Vec<String>>>,
    session: Arc<Mutex<LuaSessionContext>>,
    hooks: Arc<Mutex<HookRegistry>>,
    tools: Arc<Mutex<LuaToolRegistry>>,
    plugin_context: Option<PluginContext>,
) -> Result<mlua::AnyUserData, LuaRuntimeError> {
    Ok(lua.create_userdata(GenesisApi {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        plugin_dir: config.plugin_dir.to_string_lossy().into_owned(),
        session,
        config_values: Arc::new(config.config_values.clone()),
        logs,
        hooks,
        tools,
        plugin_context,
    })?)
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
