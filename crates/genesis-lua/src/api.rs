use std::sync::{Arc, Mutex};

use mlua::{Lua, Table, UserData, UserDataFields};

use crate::{LuaRuntimeConfig, LuaRuntimeError, LuaSessionContext};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenesisApi;

#[derive(Debug, Clone)]
struct SessionView {
    ctx: LuaSessionContext,
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

pub(crate) fn install_genesis_api(
    lua: &Lua,
    config: &LuaRuntimeConfig,
    logs: Arc<Mutex<Vec<String>>>,
) -> Result<Table, LuaRuntimeError> {
    let genesis = lua.create_table()?;
    genesis.set("version", env!("CARGO_PKG_VERSION"))?;
    genesis.set("plugin_dir", config.plugin_dir.to_string_lossy().into_owned())?;

    let session = lua.create_userdata(SessionView {
        ctx: config.session.clone(),
    })?;
    genesis.set("session", session)?;

    let config_table = lua.create_table()?;
    let config_values = config.config_values.clone();
    let get = lua.create_function(move |_, key: String| Ok(config_values.get(&key).cloned()))?;
    config_table.set("get", get)?;
    genesis.set("config", config_table)?;

    genesis.set("log", make_logger(lua, Arc::clone(&logs), None)?)?;
    genesis.set(
        "log_warn",
        make_logger(lua, Arc::clone(&logs), Some("[warn] "))?,
    )?;
    genesis.set("log_error", make_logger(lua, logs, Some("[error] "))?)?;

    Ok(genesis)
}

fn make_logger(
    lua: &Lua,
    logs: Arc<Mutex<Vec<String>>>,
    prefix: Option<&'static str>,
) -> Result<mlua::Function, LuaRuntimeError> {
    Ok(lua.create_function(move |_, message: String| {
        let mut stored = logs.lock().expect("log sink mutex should not be poisoned");
        stored.push(match prefix {
            Some(prefix) => format!("{prefix}{message}"),
            None => message,
        });
        Ok(())
    })?)
}
