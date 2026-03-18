use std::sync::{Arc, Mutex};

use mlua::{Lua, UserData, UserDataFields};

use crate::{LuaRuntimeConfig, LuaRuntimeError, LuaSessionContext};

#[derive(Debug, Clone)]
pub struct GenesisApi {
    version: String,
    plugin_dir: String,
    session: LuaSessionContext,
    config_values: Arc<std::collections::BTreeMap<String, String>>,
    logs: Arc<Mutex<Vec<String>>>,
}

#[derive(Debug, Clone)]
struct SessionView {
    ctx: LuaSessionContext,
}

#[derive(Debug, Clone)]
struct ConfigView {
    values: Arc<std::collections::BTreeMap<String, String>>,
}

impl UserData for SessionView {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| Ok(this.ctx.id.clone()));
        fields.add_field_method_get("model", |_, this| Ok(this.ctx.model.clone()));
        fields.add_field_method_get("turn_count", |_, this| Ok(this.ctx.turn_count));
        fields.add_field_method_get("total_tokens", |_, this| Ok(this.ctx.total_tokens));
        fields.add_field_method_get("platform", |_, this| Ok(this.ctx.platform.clone()));
        fields.add_field_method_get("personality", |_, this| Ok(this.ctx.personality.clone()));
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
    }
}

pub(crate) fn install_genesis_api(
    lua: &Lua,
    config: &LuaRuntimeConfig,
    logs: Arc<Mutex<Vec<String>>>,
) -> Result<mlua::AnyUserData, LuaRuntimeError> {
    Ok(lua.create_userdata(GenesisApi {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        plugin_dir: config.plugin_dir.to_string_lossy().into_owned(),
        session: config.session.clone(),
        config_values: Arc::new(config.config_values.clone()),
        logs,
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
