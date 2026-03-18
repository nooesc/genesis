use std::sync::{Arc, Mutex};

use mlua::{Lua, Table, UserData, UserDataFields};

use crate::{LuaRuntimeConfig, LuaRuntimeError, LuaSessionContext};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenesisApi;

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

    let config_view = lua.create_userdata(ConfigView {
        values: Arc::new(config.config_values.clone()),
    })?;
    genesis.set("config", config_view)?;

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
