use std::collections::BTreeMap;

use genesis_types::ToolDefinition;
use mlua::{Function, Lua, LuaSerdeExt, RegistryKey, Table, Value};

use crate::{LuaRuntimeError, PluginPermissions};

pub trait LuaHostToolExecutor: Send + Sync {
    fn execute(
        &self,
        tool_name: &str,
        arguments: BTreeMap<String, String>,
    ) -> Result<String, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum LuaToolOutput {
    Text(String),
    Json(serde_json::Value),
    TextWithMetadata {
        content: String,
        metadata: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct LuaRegisteredTool {
    pub definition: ToolDefinition,
    pub plugin_name: String,
    pub permissions: PluginPermissions,
    pub approval_policy: Option<String>,
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
        let approval_policy: Option<String> = spec.get("approval").ok();
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
            approval_policy,
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

    pub fn remove_tools_owned_by(&mut self, plugin_name: &str) {
        self.tools
            .retain(|_, entry| entry.registration.plugin_name != plugin_name);
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

fn required_string(
    spec: &Table,
    field: &str,
    plugin_name: &str,
) -> Result<String, LuaRuntimeError> {
    spec.get::<String>(field)
        .map_err(|source| LuaRuntimeError::InvalidLuaToolDefinition {
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
    let value = spec.get::<Option<Value>>("parameters").map_err(|source| {
        LuaRuntimeError::InvalidLuaToolDefinition {
            plugin_name: plugin_name.to_owned(),
            reason: format!("invalid `parameters`: {source}"),
        }
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
        let mut schema = match lua.from_value::<serde_json::Value>(Value::Table(definition.clone()))
        {
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

        if matches!(
            schema.remove("required"),
            Some(serde_json::Value::Bool(true))
        ) {
            required.push(serde_json::Value::String(name.clone()));
        }

        properties.insert(name, serde_json::Value::Object(schema));
    }

    let mut root = serde_json::Map::new();
    root.insert(
        "type".to_owned(),
        serde_json::Value::String("object".to_owned()),
    );
    root.insert(
        "properties".to_owned(),
        serde_json::Value::Object(properties),
    );
    if !required.is_empty() {
        root.insert("required".to_owned(), serde_json::Value::Array(required));
    }

    Ok(Some(serde_json::Value::Object(root)))
}

fn to_output(lua: &Lua, tool_name: &str, value: Value) -> Result<LuaToolOutput, LuaRuntimeError> {
    match value {
        Value::Nil => Ok(LuaToolOutput::Text(String::new())),
        Value::String(text) => Ok(LuaToolOutput::Text(text.to_str()?.to_owned())),
        Value::Boolean(_) | Value::Integer(_) | Value::Number(_) => {
            Ok(LuaToolOutput::Json(lua.from_value(value)?))
        }
        Value::Table(ref t) => {
            // Check if this is a {content, metadata} response.
            // Both `content` (string) and `metadata` (table) keys must be present
            // to distinguish from a plain structured table that happens to have
            // a `content` field.
            if let (Ok(Some(content)), Ok(Some(metadata_table))) = (
                t.get::<Option<String>>("content"),
                t.get::<Option<Table>>("metadata"),
            ) {
                let mut metadata = BTreeMap::new();
                for (k, v) in metadata_table.pairs::<String, String>().flatten() {
                    metadata.insert(k, v);
                }
                return Ok(LuaToolOutput::TextWithMetadata { content, metadata });
            }
            // Otherwise treat as JSON
            Ok(LuaToolOutput::Json(lua.from_value(value)?))
        }
        other => Err(LuaRuntimeError::InvalidLuaToolResult {
            tool_name: tool_name.to_owned(),
            value_type: other.type_name().to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    fn make_lua() -> Lua {
        Lua::new()
    }

    fn make_permissions() -> PluginPermissions {
        PluginPermissions::default()
    }

    #[test]
    fn register_extracts_approval_policy_from_spec() {
        let lua = make_lua();
        let mut registry = LuaToolRegistry::default();

        lua.scope(|scope| {
            let spec = lua.create_table().unwrap();
            spec.set("name", "send_msg").unwrap();
            spec.set("description", "Send a message").unwrap();
            spec.set("approval", "always").unwrap();
            spec.set(
                "run",
                scope
                    .create_function(|_, _args: Value| Ok(Value::Nil))
                    .unwrap(),
            )
            .unwrap();
            registry
                .register(&lua, "test_plugin", &make_permissions(), spec)
                .unwrap();
            Ok(())
        })
        .unwrap();

        let tools = registry.registered_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].approval_policy.as_deref(), Some("always"));
    }

    #[test]
    fn register_approval_policy_defaults_to_none() {
        let lua = make_lua();
        let mut registry = LuaToolRegistry::default();

        lua.scope(|scope| {
            let spec = lua.create_table().unwrap();
            spec.set("name", "echo").unwrap();
            spec.set("description", "Echo text").unwrap();
            spec.set(
                "run",
                scope
                    .create_function(|_, _args: Value| Ok(Value::Nil))
                    .unwrap(),
            )
            .unwrap();
            registry
                .register(&lua, "test_plugin", &make_permissions(), spec)
                .unwrap();
            Ok(())
        })
        .unwrap();

        let tools = registry.registered_tools();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].approval_policy.is_none());
    }

    #[test]
    fn to_output_returns_text_with_metadata_for_content_metadata_table() {
        let lua = make_lua();
        let result = lua
            .load(
                r#"
            return {
                content = "Please clarify your request.",
                metadata = { requires_input = "true" },
            }
        "#,
            )
            .eval::<Value>()
            .unwrap();

        let output = to_output(&lua, "clarify", result).unwrap();
        match output {
            LuaToolOutput::TextWithMetadata { content, metadata } => {
                assert_eq!(content, "Please clarify your request.");
                assert_eq!(metadata.get("requires_input").unwrap(), "true");
            }
            other => panic!("expected TextWithMetadata, got {:?}", other),
        }
    }

    #[test]
    fn to_output_returns_text_with_metadata_with_empty_metadata() {
        let lua = make_lua();
        let result = lua
            .load(
                r#"
            return {
                content = "hello",
                metadata = {},
            }
        "#,
            )
            .eval::<Value>()
            .unwrap();

        let output = to_output(&lua, "test", result).unwrap();
        match output {
            LuaToolOutput::TextWithMetadata { content, metadata } => {
                assert_eq!(content, "hello");
                assert!(metadata.is_empty());
            }
            other => panic!("expected TextWithMetadata, got {:?}", other),
        }
    }

    #[test]
    fn to_output_returns_json_for_table_without_content_key() {
        let lua = make_lua();
        let result = lua
            .load(
                r#"
            return { ok = true, count = 42 }
        "#,
            )
            .eval::<Value>()
            .unwrap();

        let output = to_output(&lua, "test", result).unwrap();
        assert!(matches!(output, LuaToolOutput::Json(_)));
    }

    #[test]
    fn to_output_returns_json_for_table_with_content_but_no_metadata_key() {
        let lua = make_lua();
        let result = lua
            .load(
                r#"
            return { content = "hello", other = "field" }
        "#,
            )
            .eval::<Value>()
            .unwrap();

        let output = to_output(&lua, "test", result).unwrap();
        // Has content string but no metadata key — treated as plain JSON to avoid
        // breaking existing tools that return structured tables with a content field.
        assert!(
            matches!(output, LuaToolOutput::Json(_)),
            "table with content but no metadata key should be Json"
        );
    }

    #[test]
    fn to_output_returns_text_with_metadata_multiple_metadata_entries() {
        let lua = make_lua();
        let result = lua
            .load(
                r#"
            return {
                content = "done",
                metadata = {
                    requires_input = "true",
                    priority = "high",
                },
            }
        "#,
            )
            .eval::<Value>()
            .unwrap();

        let output = to_output(&lua, "test", result).unwrap();
        match output {
            LuaToolOutput::TextWithMetadata { content, metadata } => {
                assert_eq!(content, "done");
                assert_eq!(metadata.len(), 2);
                assert_eq!(metadata["requires_input"], "true");
                assert_eq!(metadata["priority"], "high");
            }
            other => panic!("expected TextWithMetadata, got {:?}", other),
        }
    }

    #[test]
    fn invoke_returns_text_with_metadata() {
        let lua = make_lua();
        let mut registry = LuaToolRegistry::default();

        lua.scope(|scope| {
            let spec = lua.create_table().unwrap();
            spec.set("name", "clarify").unwrap();
            spec.set("description", "Ask for clarification").unwrap();
            spec.set(
                "run",
                scope
                    .create_function(|lua, _args: Value| {
                        let t = lua.create_table()?;
                        t.set("content", "What did you mean?")?;
                        let meta = lua.create_table()?;
                        meta.set("requires_input", "true")?;
                        t.set("metadata", meta)?;
                        Ok(Value::Table(t))
                    })
                    .unwrap(),
            )
            .unwrap();
            registry
                .register(&lua, "test_plugin", &make_permissions(), spec)
                .unwrap();

            let output = registry.invoke(&lua, "clarify", BTreeMap::new()).unwrap();
            match output {
                LuaToolOutput::TextWithMetadata { content, metadata } => {
                    assert_eq!(content, "What did you mean?");
                    assert_eq!(metadata["requires_input"], "true");
                }
                other => panic!("expected TextWithMetadata, got {:?}", other),
            }
            Ok(())
        })
        .unwrap();
    }
}
