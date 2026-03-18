use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::{PluginManifest, PluginPermissions};
use crate::LuaRuntimeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPlugin {
    pub name: String,
    pub kind: PluginKind,
    pub root: PathBuf,
    pub entrypoint: PathBuf,
    pub manifest: PluginManifest,
    pub permissions: PluginPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    SingleFile,
    Package,
}

pub fn discover_plugins(root: &Path) -> Result<Vec<DiscoveredPlugin>, LuaRuntimeError> {
    let mut plugins = Vec::new();
    let mut names = HashSet::new();

    let mut entries = fs::read_dir(root)
        .map_err(|_| LuaRuntimeError::NotImplemented)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| LuaRuntimeError::NotImplemented)?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|_| LuaRuntimeError::NotImplemented)?;

        if file_type.is_file() && path.extension().is_some_and(|ext| ext == "lua") {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or(LuaRuntimeError::NotImplemented)?
                .to_owned();
            ensure_unique_name(&mut names, &name)?;

            let manifest = PluginManifest::for_single_file(name.clone());
            plugins.push(DiscoveredPlugin {
                name,
                kind: PluginKind::SingleFile,
                root: path.clone(),
                entrypoint: path,
                permissions: manifest.permissions.clone(),
                manifest,
            });
            continue;
        }

        if file_type.is_dir() {
            let entrypoint = path.join("init.lua");
            let manifest_path = path.join("plugin.toml");
            if !(entrypoint.is_file() && manifest_path.is_file()) {
                continue;
            }

            let manifest_raw =
                fs::read_to_string(&manifest_path).map_err(|_| LuaRuntimeError::NotImplemented)?;
            let manifest: PluginManifest =
                toml::from_str(&manifest_raw).map_err(|_| LuaRuntimeError::NotImplemented)?;
            let name = manifest.plugin.name.clone();
            ensure_unique_name(&mut names, &name)?;

            plugins.push(DiscoveredPlugin {
                name,
                kind: PluginKind::Package,
                root: path,
                entrypoint,
                permissions: manifest.permissions.clone(),
                manifest,
            });
        }
    }

    Ok(plugins)
}

fn ensure_unique_name(names: &mut HashSet<String>, name: &str) -> Result<(), LuaRuntimeError> {
    if !names.insert(name.to_owned()) {
        return Err(LuaRuntimeError::NotImplemented);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::discover_plugins;

    #[test]
    fn discovers_single_file_plugin_as_untrusted() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("hello.lua"), "genesis.log('hi')").expect("plugin should write");

        let plugins = discover_plugins(dir.path()).expect("discovery should succeed");

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "hello");
        assert!(!plugins[0].permissions.trusted);
        assert!(plugins[0].permissions.tools.is_empty());
        assert!(plugins[0].permissions.hooks.is_empty());
    }

    #[test]
    fn discovers_package_plugin() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let package_dir = dir.path().join("weather");
        fs::create_dir(&package_dir).expect("package dir should exist");
        fs::write(package_dir.join("init.lua"), "return true").expect("init.lua should write");
        fs::write(
            package_dir.join("plugin.toml"),
            r#"
[plugin]
name = "weather"
version = "0.1.0"

[permissions]
tools = ["read_file"]
hooks = ["PreTurn"]
"#,
        )
        .expect("plugin.toml should write");

        let plugins = discover_plugins(dir.path()).expect("discovery should succeed");

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "weather");
        assert_eq!(plugins[0].permissions.tools, vec!["read_file"]);
        assert_eq!(plugins[0].permissions.hooks, vec!["PreTurn"]);
        assert!(!plugins[0].permissions.trusted);
    }

    #[test]
    fn rejects_duplicate_plugin_names() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("dup.lua"), "return true").expect("single file should write");

        let package_dir = dir.path().join("dup-plugin");
        fs::create_dir(&package_dir).expect("package dir should exist");
        fs::write(package_dir.join("init.lua"), "return true").expect("init.lua should write");
        fs::write(
            package_dir.join("plugin.toml"),
            r#"
[plugin]
name = "dup"
version = "0.1.0"
"#,
        )
        .expect("plugin.toml should write");

        assert!(
            discover_plugins(dir.path()).is_err(),
            "duplicate names should fail"
        );
    }
}
