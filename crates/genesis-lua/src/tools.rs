use std::collections::BTreeMap;

use genesis_types::ToolDefinition;
use mlua::{Function, Lua, LuaSerdeExt, RegistryKey, Table, Value};

use crate::{PluginPermissions, LuaRuntimeError};

#[derive(Debug, Clone, PartialEq)]
pub enum LuaToolOutput {
    Text(String),
    Json(serde_json::Value),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LuaRegisteredTool {
    pub definition: ToolDefinition,
    pub plugin_name: String,
    pub permissions: PluginPermissions,
}

struct LuaToolEntry {
    registration: LuaRegisteredTool,
    handler: RegistryKey,
}

#[derive(Default)]
pub struct LuaToolRegistry {
    tools: BTreeMap<String, LuaToolEntry>,
}

impl std::fmt::Debug for LuaToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaToolRegistry")
            .field("tool_names", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl LuaToolRegistry {
    pub fn register(
        &mut self,
        lua: &Lua,
        plugin_name: &str,
        permissions: &PluginPermissions,
        spec: Table,
    ) -> Result<(), LuaRuntimeError> {
        let name = required_string(&spec, "name", plugin_name)?;
        let description = required_string(&spec, "description", plugin_name)?;

        if self.tools.contains_key(&name) {
            return Err(LuaRuntimeError::DuplicateLuaToolName { name });
        }

        let parameters = optional_parameter_schema(lua, &spec, plugin_name)?;
        let run = required_function(&spec, "run", plugin_name)?;
        let handler = lua.create_registry_value(run)?;
        let registration = LuaRegisteredTool {
            definition: ToolDefinition {
                name: name.clone(),
                description,
                parameters,
            },
            plugin_name: plugin_name.to_owned(),
            permissions: permissions.clone(),
        };

        self.tools.insert(
            name,
            LuaToolEntry {
                registration,
                handler,
            },
        );
        Ok(())
    }

    pub fn registered_tools(&self) -> Vec<LuaRegisteredTool> {
        self.tools
            .values()
            .map(|entry| entry.registration.clone())
            .collect()
    }

    pub fn invoke(
        &self,
        lua: &Lua,
        name: &str,
        args: BTreeMap<String, String>,
    ) -> Result<LuaToolOutput, LuaRuntimeError> {
        let entry = self
            .tools
            .get(name)
            .ok_or_else(|| LuaRuntimeError::UnknownLuaTool {
                name: name.to_owned(),
            })?;
        let function: Function = lua.registry_value(&entry.handler)?;
        let arg_table = lua.create_table()?;
        for (key, value) in args {
            arg_table.set(key, value)?;
        }

        let result = function.call(arg_table)?;
        to_output(lua, name, result)
    }
}

fn required_string(spec: &Table, field: &str, plugin_name: &str) -> Result<String, LuaRuntimeError> {
    spec.get::<String>(field).map_err(|source| LuaRuntimeError::InvalidLuaToolDefinition {
        plugin_name: plugin_name.to_owned(),
        reason: format!("missing or invalid `{field}`: {source}"),
    })
}

fn required_function(
    spec: &Table,
    field: &str,
    plugin_name: &str,
) -> Result<Function, LuaRuntimeError> {
    spec.get::<Function>(field)
        .map_err(|source| LuaRuntimeError::InvalidLuaToolDefinition {
            plugin_name: plugin_name.to_owned(),
            reason: format!("missing or invalid `{field}`: {source}"),
        })
}

fn optional_parameter_schema(
    lua: &Lua,
    spec: &Table,
    plugin_name: &str,
) -> Result<Option<serde_json::Value>, LuaRuntimeError> {
    let value = spec
        .get::<Option<Value>>("parameters")
        .map_err(|source| LuaRuntimeError::InvalidLuaToolDefinition {
            plugin_name: plugin_name.to_owned(),
            reason: format!("invalid `parameters`: {source}"),
        })?;

    let Some(value) = value else {
        return Ok(None);
    };

    let table = match value {
        Value::Table(table) => table,
        _ => {
            return Err(LuaRuntimeError::InvalidLuaToolDefinition {
                plugin_name: plugin_name.to_owned(),
                reason: "`parameters` must be a table".to_owned(),
            })
        }
    };

    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for pair in table.pairs::<String, Table>() {
        let (name, definition) =
            pair.map_err(|source| LuaRuntimeError::InvalidLuaToolDefinition {
                plugin_name: plugin_name.to_owned(),
                reason: format!("invalid parameter definition: {source}"),
            })?;
        let mut schema = match lua.from_value::<serde_json::Value>(Value::Table(definition.clone())) {
            Ok(serde_json::Value::Object(object)) => object,
            Ok(_) => {
                return Err(LuaRuntimeError::InvalidLuaToolDefinition {
                    plugin_name: plugin_name.to_owned(),
                    reason: format!("parameter `{name}` must serialize to an object"),
                })
            }
            Err(source) => {
                return Err(LuaRuntimeError::InvalidLuaToolDefinition {
                    plugin_name: plugin_name.to_owned(),
                    reason: format!("parameter `{name}` could not be serialized: {source}"),
                })
            }
        };

        if matches!(schema.remove("required"), Some(serde_json::Value::Bool(true))) {
            required.push(serde_json::Value::String(name.clone()));
        }

        properties.insert(name, serde_json::Value::Object(schema));
    }

    let mut root = serde_json::Map::new();
    root.insert("type".to_owned(), serde_json::Value::String("object".to_owned()));
    root.insert("properties".to_owned(), serde_json::Value::Object(properties));
    if !required.is_empty() {
        root.insert("required".to_owned(), serde_json::Value::Array(required));
    }

    Ok(Some(serde_json::Value::Object(root)))
}

fn to_output(lua: &Lua, tool_name: &str, value: Value) -> Result<LuaToolOutput, LuaRuntimeError> {
    match value {
        Value::Nil => Ok(LuaToolOutput::Text(String::new())),
        Value::String(text) => Ok(LuaToolOutput::Text(text.to_str()?.to_owned())),
        Value::Boolean(_)
        | Value::Integer(_)
        | Value::Number(_)
        | Value::Table(_) => Ok(LuaToolOutput::Json(lua.from_value(value)?)),
        other => Err(LuaRuntimeError::InvalidLuaToolResult {
            tool_name: tool_name.to_owned(),
            value_type: other.type_name().to_owned(),
        }),
    }
}
