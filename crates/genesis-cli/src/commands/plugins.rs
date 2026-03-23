use std::path::PathBuf;

use genesis_config::{load, set_plugin_disabled_in_file, LoadedConfig};
use genesis_lua::discovery::{discover_plugins_best_effort, DiscoveryReport};
use genesis_lua::{DiscoveredPlugin, LuaRuntimeError, PluginKind};

use crate::{CliError, PluginsCommand};

#[derive(Debug, Clone)]
struct PluginCommandEntry {
    name: String,
    status: String,
    kind: String,
    source: String,
    version: Option<String>,
    description: Option<String>,
    author: Option<String>,
    root: Option<String>,
    entrypoint: Option<String>,
    trusted: bool,
    tools: Vec<String>,
    hooks: Vec<String>,
    discovered: bool,
}

pub(crate) fn run_plugins(
    config_path: Option<PathBuf>,
    command: PluginsCommand,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let (entries, errors) = collect_plugin_entries(&loaded)?;

    match command {
        PluginsCommand::List => render_plugin_list(&entries, &errors, json),
        PluginsCommand::Info { name } => render_plugin_info(&entries, &errors, &name, json),
        PluginsCommand::Disable { name } => {
            let entry = entries.iter().find(|entry| entry.name == name);
            let already_disabled = loaded
                .config
                .plugins
                .disabled
                .iter()
                .any(|item| item == &name);
            if entry.is_none() && !already_disabled {
                return Err(CliError::Other(unknown_plugin_message(&entries, &name)));
            }
            if !already_disabled {
                set_plugin_disabled_in_file(&loaded.paths.config_path, &name, true)?;
            }

            if json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "plugin": name,
                    "disabled": true,
                    "changed": !already_disabled,
                }))?)
            } else if already_disabled {
                Ok(format!("plugin `{name}` is already disabled"))
            } else {
                Ok(format!("disabled plugin `{name}` in config"))
            }
        }
        PluginsCommand::Enable { name } => {
            let entry = entries.iter().find(|entry| entry.name == name);
            let already_disabled = loaded
                .config
                .plugins
                .disabled
                .iter()
                .any(|item| item == &name);
            if entry.is_none() && !already_disabled {
                return Err(CliError::Other(unknown_plugin_message(&entries, &name)));
            }
            if already_disabled {
                set_plugin_disabled_in_file(&loaded.paths.config_path, &name, false)?;
            }

            if json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "plugin": name,
                    "disabled": false,
                    "changed": already_disabled,
                }))?)
            } else if already_disabled {
                Ok(format!("enabled plugin `{name}` in config"))
            } else {
                Ok(format!("plugin `{name}` is already enabled"))
            }
        }
    }
}

fn collect_plugin_entries(
    loaded: &LoadedConfig,
) -> Result<(Vec<PluginCommandEntry>, Vec<String>), CliError> {
    let report = match discover_plugins_best_effort(&loaded.paths.plugin_dir) {
        Ok(report) => report,
        Err(LuaRuntimeError::ReadPluginDirectory { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            DiscoveryReport::default()
        }
        Err(error) => {
            return Err(CliError::Other(format!(
                "failed to inspect plugin directory {}: {error}",
                loaded.paths.plugin_dir.display()
            )))
        }
    };

    let disabled: std::collections::BTreeSet<_> =
        loaded.config.plugins.disabled.iter().cloned().collect();
    let DiscoveryReport { plugins, errors } = report;
    let mut entries: Vec<PluginCommandEntry> = plugins
        .into_iter()
        .map(|plugin| {
            let is_disabled = disabled.contains(&plugin.name);
            plugin_entry_from_discovery(plugin, is_disabled)
        })
        .collect();
    let active_names: std::collections::BTreeSet<_> = entries
        .iter()
        .filter(|entry| entry.status == "enabled")
        .map(|entry| entry.name.clone())
        .collect();

    for bundled in genesis_lua::bundled_personalities() {
        let status = if disabled.contains(bundled.name) {
            "disabled"
        } else if active_names.contains(bundled.name) {
            "shadowed"
        } else {
            "enabled"
        };
        entries.push(PluginCommandEntry {
            name: bundled.name.to_owned(),
            status: status.to_owned(),
            kind: plugin_kind_name(PluginKind::Bundled).to_owned(),
            source: "built-in".to_owned(),
            version: None,
            description: Some(bundled.description.to_owned()),
            author: None,
            root: None,
            entrypoint: None,
            trusted: false,
            tools: Vec::new(),
            hooks: Vec::new(),
            discovered: true,
        });
    }

    for missing in disabled {
        if entries.iter().any(|entry| entry.name == missing) {
            continue;
        }
        entries.push(PluginCommandEntry {
            name: missing,
            status: "disabled".to_owned(),
            kind: "missing".to_owned(),
            source: "config".to_owned(),
            version: None,
            description: Some("disabled in config, plugin not found on disk".to_owned()),
            author: None,
            root: None,
            entrypoint: None,
            trusted: false,
            tools: Vec::new(),
            hooks: Vec::new(),
            discovered: false,
        });
    }

    entries.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| plugin_status_rank(&left.status).cmp(&plugin_status_rank(&right.status)))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    let errors: Vec<String> = errors
        .into_iter()
        .map(|error: LuaRuntimeError| error.to_string())
        .collect();
    Ok((entries, errors))
}

fn plugin_entry_from_discovery(plugin: DiscoveredPlugin, disabled: bool) -> PluginCommandEntry {
    PluginCommandEntry {
        name: plugin.name,
        status: if disabled { "disabled" } else { "enabled" }.to_owned(),
        kind: plugin_kind_name(plugin.kind).to_owned(),
        source: plugin.root.display().to_string(),
        version: Some(plugin.manifest.plugin.version),
        description: plugin.manifest.plugin.description,
        author: plugin.manifest.plugin.author,
        root: Some(plugin.root.display().to_string()),
        entrypoint: Some(plugin.entrypoint.display().to_string()),
        trusted: plugin.manifest.permissions.trusted,
        tools: plugin.manifest.permissions.tools,
        hooks: plugin.manifest.permissions.hooks,
        discovered: true,
    }
}

fn plugin_status_rank(status: &str) -> u8 {
    match status {
        "enabled" => 0,
        "shadowed" => 1,
        "disabled" => 2,
        "missing" => 3,
        _ => 4,
    }
}

fn plugin_kind_name(kind: PluginKind) -> &'static str {
    match kind {
        PluginKind::SingleFile => "single_file",
        PluginKind::Package => "package",
        PluginKind::Bundled => "bundled",
    }
}

fn render_plugin_list(
    entries: &[PluginCommandEntry],
    errors: &[String],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "plugins": entries.iter().map(plugin_entry_json).collect::<Vec<_>>(),
            "errors": errors,
        }))?);
    }

    if entries.is_empty() && errors.is_empty() {
        return Ok("no Lua plugins discovered".to_owned());
    }

    let mut lines = vec![format!(
        "{:<18} {:<9} {:<12} {:<10} {:<12} {}",
        "NAME", "STATUS", "KIND", "VERSION", "SOURCE", "DESCRIPTION"
    )];
    for entry in entries {
        lines.push(format!(
            "{:<18} {:<9} {:<12} {:<10} {:<12} {}",
            entry.name,
            entry.status,
            entry.kind,
            entry.version.as_deref().unwrap_or("-"),
            short_plugin_source(&entry.source),
            entry.description.as_deref().unwrap_or("-"),
        ));
    }
    if !errors.is_empty() {
        lines.push(String::new());
        lines.push("Discovery warnings:".to_owned());
        for error in errors {
            lines.push(format!("  - {error}"));
        }
    }
    Ok(lines.join("\n"))
}

fn render_plugin_info(
    entries: &[PluginCommandEntry],
    errors: &[String],
    name: &str,
    json: bool,
) -> Result<String, CliError> {
    let matches: Vec<&PluginCommandEntry> =
        entries.iter().filter(|entry| entry.name == name).collect();
    if matches.is_empty() {
        return Err(CliError::Other(unknown_plugin_message(entries, name)));
    }
    let entry = matches
        .iter()
        .copied()
        .find(|entry| entry.status != "shadowed")
        .unwrap_or(matches[0]);
    let shadowed: Vec<&PluginCommandEntry> = matches
        .iter()
        .copied()
        .filter(|candidate| !same_plugin_entry(candidate, entry) && candidate.status == "shadowed")
        .collect();
    let alternates: Vec<&PluginCommandEntry> = matches
        .into_iter()
        .filter(|candidate| !same_plugin_entry(candidate, entry) && candidate.status != "shadowed")
        .collect();

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "plugin": plugin_entry_json(entry),
            "shadowed": shadowed.iter().map(|entry| plugin_entry_json(entry)).collect::<Vec<_>>(),
            "alternates": alternates.iter().map(|entry| plugin_entry_json(entry)).collect::<Vec<_>>(),
            "errors": errors,
        }))?);
    }

    let mut lines = vec![
        format!("Plugin: {}", entry.name),
        format!("Status: {}", entry.status),
        format!("Kind: {}", entry.kind),
        format!("Source: {}", entry.source),
    ];
    if let Some(version) = &entry.version {
        lines.push(format!("Version: {version}"));
    }
    if let Some(description) = &entry.description {
        lines.push(format!("Description: {description}"));
    }
    if let Some(author) = &entry.author {
        lines.push(format!("Author: {author}"));
    }
    lines.push(format!("Discovered: {}", entry.discovered));
    lines.push(format!("Trusted: {}", entry.trusted));
    if let Some(root) = &entry.root {
        lines.push(format!("Root: {root}"));
    }
    if let Some(entrypoint) = &entry.entrypoint {
        lines.push(format!("Entrypoint: {entrypoint}"));
    }
    lines.push(format!("Allowed tools: {}", join_or_none(&entry.tools)));
    lines.push(format!("Allowed hooks: {}", join_or_none(&entry.hooks)));
    if !shadowed.is_empty() {
        lines.push(String::new());
        lines.push("Shadowed entries:".to_owned());
        for shadowed_entry in shadowed {
            lines.push(format!(
                "  - {} [{}]",
                shadowed_entry.name, shadowed_entry.kind
            ));
            lines.push(format!("    Source: {}", shadowed_entry.source));
            lines.push(format!("    Status: {}", shadowed_entry.status));
        }
    }
    if !alternates.is_empty() {
        lines.push(String::new());
        lines.push("Other entries:".to_owned());
        for alternate_entry in alternates {
            lines.push(format!(
                "  - {} [{}]",
                alternate_entry.name, alternate_entry.kind
            ));
            lines.push(format!("    Source: {}", alternate_entry.source));
            lines.push(format!("    Status: {}", alternate_entry.status));
        }
    }
    if !errors.is_empty() {
        lines.push(String::new());
        lines.push("Discovery warnings:".to_owned());
        for error in errors {
            lines.push(format!("  - {error}"));
        }
    }
    Ok(lines.join("\n"))
}

fn plugin_entry_json(entry: &PluginCommandEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.name,
        "status": entry.status,
        "kind": entry.kind,
        "source": entry.source,
        "version": entry.version,
        "description": entry.description,
        "author": entry.author,
        "root": entry.root,
        "entrypoint": entry.entrypoint,
        "trusted": entry.trusted,
        "tools": entry.tools,
        "hooks": entry.hooks,
        "discovered": entry.discovered,
    })
}

fn short_plugin_source(source: &str) -> &str {
    if source == "built-in" {
        "built-in"
    } else if source == "config" {
        "config"
    } else {
        "local"
    }
}

fn same_plugin_entry(left: &PluginCommandEntry, right: &PluginCommandEntry) -> bool {
    left.name == right.name && left.kind == right.kind && left.source == right.source
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

fn unknown_plugin_message(entries: &[PluginCommandEntry], name: &str) -> String {
    let available = entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    if available.is_empty() {
        format!("unknown plugin `{name}`; no plugins are currently discovered")
    } else {
        format!(
            "unknown plugin `{name}`. Available: {}",
            available.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::plugin_kind_name;
    use genesis_lua::PluginKind;

    #[test]
    fn plugin_kind_name_reports_bundled_plugins() {
        assert_eq!(plugin_kind_name(PluginKind::Bundled), "bundled");
    }
}
