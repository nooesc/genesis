use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::PluginManifest;
use crate::LuaRuntimeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPlugin {
    pub name: String,
    pub kind: PluginKind,
    pub root: PathBuf,
    pub entrypoint: PathBuf,
    pub manifest: PluginManifest,
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
        .map_err(|source| LuaRuntimeError::ReadPluginDirectory {
            path: root.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| LuaRuntimeError::ReadPluginEntry {
            path: root.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| LuaRuntimeError::ReadPluginEntry {
            path: path.clone(),
            source,
        })?;

        if file_type.is_file() && path.extension().is_some_and(|ext| ext == "lua") {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| LuaRuntimeError::InvalidPluginFilename { path: path.clone() })?
                .to_owned();
            ensure_unique_name(&mut names, &name)?;

            let manifest = PluginManifest::for_single_file(name.clone());
            plugins.push(DiscoveredPlugin {
                name,
                kind: PluginKind::SingleFile,
                root: path.clone(),
                entrypoint: path,
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
                fs::read_to_string(&manifest_path).map_err(|source| {
                    LuaRuntimeError::ReadPluginManifest {
                        path: manifest_path.clone(),
                        source,
                    }
                })?;
            let manifest: PluginManifest = toml::from_str(&manifest_raw).map_err(|source| {
                LuaRuntimeError::ParsePluginManifest {
                    path: manifest_path.clone(),
                    source,
                }
            })?;
            let name = manifest.plugin.name.clone();
            ensure_unique_name(&mut names, &name)?;

            plugins.push(DiscoveredPlugin {
                name,
                kind: PluginKind::Package,
                root: path,
                entrypoint,
                manifest,
            });
        }
    }

    Ok(plugins)
}

fn ensure_unique_name(names: &mut HashSet<String>, name: &str) -> Result<(), LuaRuntimeError> {
    if !names.insert(name.to_owned()) {
        return Err(LuaRuntimeError::DuplicatePluginName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::LuaRuntimeError;

    use super::discover_plugins;

    #[test]
    fn discovers_single_file_plugin_as_untrusted() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("hello.lua"), "genesis.log('hi')").expect("plugin should write");

        let plugins = discover_plugins(dir.path()).expect("discovery should succeed");

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "hello");
        assert!(!plugins[0].manifest.permissions.trusted);
        assert!(plugins[0].manifest.permissions.tools.is_empty());
        assert!(plugins[0].manifest.permissions.hooks.is_empty());
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
        assert_eq!(plugins[0].manifest.permissions.tools, vec!["read_file"]);
        assert_eq!(plugins[0].manifest.permissions.hooks, vec!["PreTurn"]);
        assert!(!plugins[0].manifest.permissions.trusted);
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

        let err = discover_plugins(dir.path()).expect_err("duplicate names should fail");
        assert!(
            matches!(err, LuaRuntimeError::DuplicatePluginName { ref name } if name == "dup"),
            "expected duplicate plugin error, got: {err:?}"
        );
    }

    #[test]
    fn reports_missing_plugin_directory() {
        let missing = PathBuf::from("/definitely/not/a/real/genesis-plugin-dir");
        let err = discover_plugins(&missing).expect_err("missing root should fail");

        assert!(
            matches!(err, LuaRuntimeError::ReadPluginDirectory { ref path, .. } if path == &missing),
            "expected read plugin directory error, got: {err:?}"
        );
    }

    #[test]
    fn reports_malformed_package_manifest() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let package_dir = dir.path().join("broken");
        fs::create_dir(&package_dir).expect("package dir should exist");
        fs::write(package_dir.join("init.lua"), "return true").expect("init.lua should write");
        fs::write(
            package_dir.join("plugin.toml"),
            r#"
[plugin]
name = "broken"
"#,
        )
        .expect("plugin.toml should write");

        let manifest_path = package_dir.join("plugin.toml");
        let err = discover_plugins(dir.path()).expect_err("malformed manifest should fail");

        assert!(
            matches!(err, LuaRuntimeError::ParsePluginManifest { ref path, .. } if path == &manifest_path),
            "expected parse plugin manifest error, got: {err:?}"
        );
    }
}
