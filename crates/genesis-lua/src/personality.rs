use std::collections::BTreeMap;

use mlua::{Function, Table};

use crate::{manifest::PluginPermissions, LuaRuntimeError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaRegisteredPersonality {
    pub name: String,
    pub description: String,
    pub system_prompt: Option<String>,
    pub plugin_name: String,
}

#[derive(Debug, Default)]
pub struct LuaPersonalityRegistry {
    personalities: BTreeMap<String, LuaPersonalityEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct LuaPersonalityEntry {
    pub(crate) metadata: LuaRegisteredPersonality,
    pub(crate) build_prompt: Option<Function>,
    pub(crate) transform_response: Option<Function>,
    pub(crate) permissions: PluginPermissions,
}

impl LuaPersonalityRegistry {
    pub fn register(
        &mut self,
        plugin_name: &str,
        permissions: &PluginPermissions,
        spec: Table,
    ) -> Result<(), LuaRuntimeError> {
        let name = required_string(&spec, "name", plugin_name)?;
        let description = required_string(&spec, "description", plugin_name)?;
        let system_prompt = optional_string(&spec, "system_prompt", plugin_name)?;
        let build_prompt = optional_function(&spec, "build_prompt", plugin_name)?;
        let transform_response = optional_function(&spec, "transform_response", plugin_name)?;

        if self.personalities.contains_key(&name) {
            return Err(LuaRuntimeError::DuplicateLuaPersonalityName { name });
        }
        if system_prompt.is_none() && build_prompt.is_none() {
            return Err(LuaRuntimeError::InvalidLuaPersonalityDefinition {
                plugin_name: plugin_name.to_owned(),
                reason: "expected `system_prompt` or `build_prompt`".to_owned(),
            });
        }

        self.personalities.insert(
            name.clone(),
            LuaPersonalityEntry {
                metadata: LuaRegisteredPersonality {
                    name,
                    description,
                    system_prompt,
                    plugin_name: plugin_name.to_owned(),
                },
                build_prompt,
                transform_response,
                permissions: permissions.clone(),
            },
        );
        Ok(())
    }

    pub fn registered_personalities(&self) -> Vec<LuaRegisteredPersonality> {
        self.personalities
            .values()
            .map(|entry| entry.metadata.clone())
            .collect()
    }

    pub(crate) fn personality_entry(&self, name: &str) -> Option<LuaPersonalityEntry> {
        self.personalities.get(name).cloned()
    }

    pub fn remove_personalities_owned_by(&mut self, plugin_name: &str) {
        self.personalities
            .retain(|_, entry| entry.metadata.plugin_name != plugin_name);
    }
}

fn required_string(
    spec: &Table,
    field: &str,
    plugin_name: &str,
) -> Result<String, LuaRuntimeError> {
    spec.get::<String>(field)
        .map_err(|source| LuaRuntimeError::InvalidLuaPersonalityDefinition {
            plugin_name: plugin_name.to_owned(),
            reason: format!("missing or invalid `{field}`: {source}"),
        })
}

fn optional_string(
    spec: &Table,
    field: &str,
    plugin_name: &str,
) -> Result<Option<String>, LuaRuntimeError> {
    spec.get::<Option<String>>(field).map_err(|source| {
        LuaRuntimeError::InvalidLuaPersonalityDefinition {
            plugin_name: plugin_name.to_owned(),
            reason: format!("invalid `{field}`: {source}"),
        }
    })
}

fn optional_function(
    spec: &Table,
    field: &str,
    plugin_name: &str,
) -> Result<Option<Function>, LuaRuntimeError> {
    spec.get::<Option<Function>>(field).map_err(|source| {
        LuaRuntimeError::InvalidLuaPersonalityDefinition {
            plugin_name: plugin_name.to_owned(),
            reason: format!("invalid `{field}`: {source}"),
        }
    })
}
