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
    Bundled,
}

#[derive(Debug, Default)]
pub struct DiscoveryReport {
    pub plugins: Vec<DiscoveredPlugin>,
    pub errors: Vec<LuaRuntimeError>,
}

pub fn discover_plugins(root: &Path) -> Result<Vec<DiscoveredPlugin>, LuaRuntimeError> {
    Ok(scan_plugins(root, false)?.plugins)
}

pub fn discover_plugins_best_effort(root: &Path) -> Result<DiscoveryReport, LuaRuntimeError> {
    scan_plugins(root, true)
}

fn ensure_unique_name(names: &mut HashSet<String>, name: &str) -> Result<(), LuaRuntimeError> {
    if !names.insert(name.to_owned()) {
        return Err(LuaRuntimeError::DuplicatePluginName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

fn scan_plugins(root: &Path, best_effort: bool) -> Result<DiscoveryReport, LuaRuntimeError> {
    let mut report = DiscoveryReport::default();
    let mut names = HashSet::new();
    let read_dir = fs::read_dir(root).map_err(|source| LuaRuntimeError::ReadPluginDirectory {
        path: root.to_path_buf(),
        source,
    })?;
    let (entries, entry_errors) = collect_plugin_entry_paths(
        read_dir.map(|entry| entry.map(|entry| entry.path())),
        root,
        best_effort,
    )?;
    report.errors.extend(entry_errors);

    for entry in entries {
        match discover_plugin_entry(entry, &mut names) {
            Ok(Some(plugin)) => report.plugins.push(plugin),
            Ok(None) => {}
            Err(err) if best_effort => report.errors.push(err),
            Err(err) => return Err(err),
        }
    }

    Ok(report)
}

fn collect_plugin_entry_paths<I>(
    entries: I,
    root: &Path,
    best_effort: bool,
) -> Result<(Vec<PathBuf>, Vec<LuaRuntimeError>), LuaRuntimeError>
where
    I: IntoIterator<Item = std::io::Result<PathBuf>>,
{
    let mut paths = Vec::new();
    let mut errors = Vec::new();

    for entry in entries {
        match entry {
            Ok(path) => paths.push(path),
            Err(source) if best_effort => errors.push(LuaRuntimeError::ReadPluginEntry {
                path: root.to_path_buf(),
                source,
            }),
            Err(source) => {
                return Err(LuaRuntimeError::ReadPluginEntry {
                    path: root.to_path_buf(),
                    source,
                });
            }
        }
    }

    paths.sort();
    Ok((paths, errors))
}

fn discover_plugin_entry(
    path: PathBuf,
    names: &mut HashSet<String>,
) -> Result<Option<DiscoveredPlugin>, LuaRuntimeError> {
    let file_type = fs::metadata(&path)
        .map(|meta| meta.file_type())
        .map_err(|source| LuaRuntimeError::ReadPluginEntry {
            path: path.clone(),
            source,
        })?;

    if file_type.is_file() && path.extension().is_some_and(|ext| ext == "lua") {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| LuaRuntimeError::InvalidPluginFilename { path: path.clone() })?
            .to_owned();
        ensure_unique_name(names, &name)?;

        let manifest = PluginManifest::for_single_file(name.clone());
        return Ok(Some(DiscoveredPlugin {
            name,
            kind: PluginKind::SingleFile,
            root: path.clone(),
            entrypoint: path,
            manifest,
        }));
    }

    if file_type.is_dir() {
        let entrypoint = path.join("init.lua");
        let manifest_path = path.join("plugin.toml");
        if !(entrypoint.is_file() && manifest_path.is_file()) {
            return Ok(None);
        }

        let manifest_raw = fs::read_to_string(&manifest_path).map_err(|source| {
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
        ensure_unique_name(names, &name)?;

        return Ok(Some(DiscoveredPlugin {
            name,
            kind: PluginKind::Package,
            root: path,
            entrypoint,
            manifest,
        }));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::ErrorKind;
    use std::path::PathBuf;

    use crate::LuaRuntimeError;

    use super::{collect_plugin_entry_paths, discover_plugins};

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

    #[test]
    fn best_effort_collects_entry_errors_without_dropping_valid_paths() {
        let root = PathBuf::from("/tmp/plugins");
        let alpha = root.join("alpha.lua");
        let gamma = root.join("gamma.lua");

        let (paths, errors) = collect_plugin_entry_paths(
            vec![
                Ok(gamma.clone()),
                Err(std::io::Error::new(ErrorKind::PermissionDenied, "denied")),
                Ok(alpha.clone()),
            ],
            &root,
            true,
        )
        .expect("best-effort path collection should succeed");

        assert_eq!(
            paths,
            vec![alpha, gamma],
            "valid paths should be retained and sorted"
        );
        assert_eq!(errors.len(), 1, "iterator error should be recorded");
        assert!(
            matches!(errors[0], LuaRuntimeError::ReadPluginEntry { ref path, .. } if path == &root),
            "expected entry-read error rooted at the plugin dir, got: {:?}",
            errors[0]
        );
    }
}
