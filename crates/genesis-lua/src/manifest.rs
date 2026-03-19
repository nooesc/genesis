use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifest {
    pub plugin: PluginMetadata,
    #[serde(default)]
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub genesis: PluginGenesis,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginMetadata {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginPermissions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub trusted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginGenesis {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
}

impl PluginManifest {
    pub fn for_single_file(name: impl Into<String>) -> Self {
        Self {
            plugin: PluginMetadata {
                name: name.into(),
                version: "0.0.0".to_owned(),
                description: None,
                author: None,
            },
            permissions: PluginPermissions::default(),
            genesis: PluginGenesis::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PluginManifest;

    #[test]
    fn parses_package_manifest() {
        let raw = r#"
[plugin]
name = "my-plugin"
version = "0.1.0"
description = "Does something useful"
author = "user"

[permissions]
tools = ["read_file", "write_file"]
hooks = ["PreToolCall", "PostToolCall"]
trusted = true

[genesis]
min_version = "0.1.0"
"#;

        let manifest: PluginManifest = toml::from_str(raw).expect("manifest should parse");
        assert_eq!(manifest.plugin.name, "my-plugin");
        assert_eq!(manifest.plugin.version, "0.1.0");
        assert_eq!(manifest.plugin.description.as_deref(), Some("Does something useful"));
        assert_eq!(manifest.plugin.author.as_deref(), Some("user"));
        assert_eq!(manifest.permissions.tools, vec!["read_file", "write_file"]);
        assert_eq!(
            manifest.permissions.hooks,
            vec!["PreToolCall", "PostToolCall"]
        );
        assert!(manifest.permissions.trusted);
        assert_eq!(manifest.genesis.min_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn rejects_malformed_manifest() {
        let raw = r#"
[permissions]
trusted = false
"#;

        let err = toml::from_str::<PluginManifest>(raw).expect_err("manifest should fail");
        assert!(
            err.to_string().contains("plugin"),
            "expected parse error to mention missing plugin section, got: {err}"
        );
    }
}
