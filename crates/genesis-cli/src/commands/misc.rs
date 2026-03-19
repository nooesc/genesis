use std::path::PathBuf;
use std::time::Duration;

use genesis_config::{load, set_plugin_disabled_in_file, LoadedConfig};
use genesis_core::prompt::load_context_file;
use genesis_lua::discovery::{discover_plugins_best_effort, DiscoveryReport};
use genesis_lua::{DiscoveredPlugin, LuaRuntimeError, PluginKind};
use genesis_storage::{PairingStore, ScheduleStore, SessionStore, SkillStore, UserModelStore};
use genesis_types::DeliveryPlatform;

use crate::format::context_template;
use crate::{
    CliError, ContextCommand, McpCommand, ModelCommand, PairingCommand, PersonalityCommand,
    PluginsCommand, ToolsetCommand,
};

pub(crate) struct RegistryMcpBackend {
    registry: genesis_tools::ToolRegistry,
    context: genesis_tools::ToolContext,
}

impl genesis_mcp::McpToolBackend for RegistryMcpBackend {
    fn list_tools(&self) -> Vec<genesis_mcp::McpServerToolDef> {
        self.registry
            .definitions()
            .into_iter()
            .map(|def| genesis_mcp::McpServerToolDef {
                name: def.name,
                description: Some(def.description),
                input_schema: def
                    .parameters
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
            })
            .collect()
    }

    fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<String, String> {
        // Convert JSON arguments to BTreeMap<String, String>
        let mut args = std::collections::BTreeMap::new();
        if let Some(obj) = arguments.as_object() {
            for (k, v) in obj {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => continue,
                    other => other.to_string(),
                };
                args.insert(k.clone(), s);
            }
        }

        let call = genesis_tools::ToolCall {
            name: name.to_owned(),
            arguments: args,
        };

        match self.registry.execute(&call, &self.context) {
            Ok(output) => Ok(output.content),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub(crate) async fn run_mcp(
    config_path: Option<PathBuf>,
    command: McpCommand,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;

    match command {
        McpCommand::List => {
            let servers = &loaded.config.mcp_servers;
            if servers.is_empty() {
                return Ok("no MCP servers configured".to_owned());
            }

            if json {
                return Ok(serde_json::to_string_pretty(servers)?);
            }

            let mut lines = Vec::new();
            for (name, cfg) in servers {
                let transport = if cfg.command.is_some() {
                    "stdio"
                } else if cfg.url.is_some() {
                    "http"
                } else {
                    "unknown"
                };

                let endpoint = cfg.command.as_deref().or(cfg.url.as_deref()).unwrap_or("-");

                let timeout = cfg.timeout.unwrap_or(120);
                let connect_timeout = cfg.connect_timeout.unwrap_or(60);

                lines.push(format!(
                    "{name}  [{transport}]  {endpoint}  timeout={timeout}s connect={connect_timeout}s"
                ));
            }
            Ok(lines.join("\n"))
        }
        McpCommand::Serve => {
            // Run Genesis as an MCP server on stdio
            let registry = genesis_tools::default_registry();
            let context = genesis_tools::ToolContext {
                session_id: format!("mcp-server-{}", std::process::id()),
                profile: loaded.config.profile.clone(),
                data_dir: loaded.config.storage.data_dir.to_string_lossy().to_string(),
                allow_destructive_tools: false,
                terminal_backend: None,
                default_working_dir: None,
                sandbox_manager: None,
                approval_mode: genesis_config::ApprovalMode::Auto,
            };

            let backend = std::sync::Arc::new(RegistryMcpBackend { registry, context });
            let config = genesis_mcp::McpServeConfig::default();

            genesis_mcp::run_stdio_server(config, backend)
                .await
                .map_err(|e| CliError::Other(format!("MCP server error: {e}")))?;

            Ok("MCP server exited".to_owned())
        }
        McpCommand::Test => {
            let servers = &loaded.config.mcp_servers;
            if servers.is_empty() {
                return Ok("no MCP servers configured".to_owned());
            }

            let configs = genesis_mcp::build_server_configs(servers);
            if configs.is_empty() {
                return Ok("no valid MCP server configs found".to_owned());
            }

            let mut lines = Vec::new();
            let manager = genesis_mcp::McpManager::connect_all(configs).await;
            let server_count = manager.server_count().await;
            let tool_count = manager.tool_count().await;

            if server_count == 0 {
                lines.push("no MCP servers responded".to_owned());
            } else {
                lines.push(format!(
                    "{server_count} server(s) connected, {tool_count} tool(s) available"
                ));

                let tool_defs = manager.tool_definitions().await;
                for tool in &tool_defs {
                    lines.push(format!("  - {}: {}", tool.name, tool.description));
                }
            }

            Ok(lines.join("\n"))
        }
    }
}

pub(crate) async fn run_pairing(
    config_path: Option<PathBuf>,
    command: PairingCommand,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let store = PairingStore::new(&loaded.config.storage.database_path);

    match command {
        PairingCommand::List { platform } => {
            let users = store
                .list_approved(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "approved": users,
                    "count": users.len(),
                }))
                .unwrap());
            }

            if users.is_empty() {
                return Ok("No approved users.".to_owned());
            }

            let mut lines = vec![format!(
                "{:<12} {:<20} {:<20} {}",
                "PLATFORM", "USER_ID", "NAME", "APPROVED_AT"
            )];
            for u in &users {
                lines.push(format!(
                    "{:<12} {:<20} {:<20} {}",
                    u.platform, u.user_id, u.user_name, u.approved_at
                ));
            }
            lines.push(format!("\n{} approved user(s)", users.len()));
            Ok(lines.join("\n"))
        }
        PairingCommand::Pending { platform } => {
            let pending = store
                .list_pending(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "pending": pending,
                    "count": pending.len(),
                }))
                .unwrap());
            }

            if pending.is_empty() {
                return Ok("No pending pairing requests.".to_owned());
            }

            let mut lines = vec![format!(
                "{:<12} {:<10} {:<20} {:<20} {}",
                "PLATFORM", "CODE", "USER_ID", "NAME", "CREATED_AT"
            )];
            for p in &pending {
                lines.push(format!(
                    "{:<12} {:<10} {:<20} {:<20} {}",
                    p.platform, p.code, p.user_id, p.user_name, p.created_at
                ));
            }
            lines.push(format!("\n{} pending request(s)", pending.len()));
            Ok(lines.join("\n"))
        }
        PairingCommand::Approve { platform, code } => {
            let result = store
                .approve_code(&platform, &code)
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            match result {
                Some(user) => {
                    if json {
                        Ok(serde_json::to_string_pretty(&serde_json::json!({
                            "approved": true,
                            "user": user,
                        }))
                        .unwrap())
                    } else {
                        Ok(format!(
                            "Approved {} ({}) on {}",
                            user.user_name, user.user_id, user.platform
                        ))
                    }
                }
                None => Err(CliError::Other(
                    "Invalid or expired pairing code.".to_owned(),
                )),
            }
        }
        PairingCommand::Revoke { platform, user_id } => {
            let revoked = store
                .revoke(&platform, &user_id)
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "revoked": revoked,
                    "platform": platform,
                    "user_id": user_id,
                }))
                .unwrap());
            }

            if revoked {
                Ok(format!("Revoked access for {} on {}", user_id, platform))
            } else {
                Err(CliError::Other(format!(
                    "No approved user '{}' on platform '{}'",
                    user_id, platform
                )))
            }
        }
        PairingCommand::ClearPending { platform } => {
            let cleared = store
                .clear_pending(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "cleared": cleared,
                    "platform": platform,
                }))
                .unwrap());
            }

            match platform {
                Some(p) => Ok(format!("Cleared {} pending code(s) for {}", cleared, p)),
                None => Ok(format!(
                    "Cleared {} pending code(s) across all platforms",
                    cleared
                )),
            }
        }
    }
}

pub(crate) fn run_toolset(command: ToolsetCommand, json: bool) -> Result<String, CliError> {
    use genesis_core::toolset;
    use rand::SeedableRng;

    match command {
        ToolsetCommand::List => {
            let names = toolset::builtin_distribution_names();
            if json {
                let distributions: Vec<serde_json::Value> = names
                    .iter()
                    .filter_map(|name| {
                        toolset::builtin_distribution(name).map(|d| {
                            serde_json::json!({
                                "name": d.name,
                                "description": d.description,
                                "tool_count": d.possible_tools().len(),
                            })
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&distributions).unwrap())
            } else {
                let mut lines = vec![format!("{:<18} {:<5} {}", "NAME", "TOOLS", "DESCRIPTION")];
                for name in &names {
                    if let Some(d) = toolset::builtin_distribution(name) {
                        lines.push(format!(
                            "{:<18} {:<5} {}",
                            d.name,
                            d.possible_tools().len(),
                            d.description
                        ));
                    }
                }
                Ok(lines.join("\n"))
            }
        }
        ToolsetCommand::Show { name } => {
            let dist = toolset::builtin_distribution(&name).ok_or_else(|| {
                CliError::Other(format!(
                    "unknown distribution '{name}'. Available: {}",
                    toolset::builtin_distribution_names().join(", ")
                ))
            })?;

            if json {
                Ok(serde_json::to_string_pretty(&dist).unwrap())
            } else {
                let mut lines = vec![
                    format!("Distribution: {}", dist.name),
                    format!("Description:  {}", dist.description),
                    format!("Tools ({}):", dist.tools.len()),
                ];
                for (tool, prob) in &dist.tools {
                    lines.push(format!("  {:<30} {:.0}%", tool, prob * 100.0));
                }
                Ok(lines.join("\n"))
            }
        }
        ToolsetCommand::Sample { name, seed } => {
            let dist = toolset::builtin_distribution(&name).ok_or_else(|| {
                CliError::Other(format!(
                    "unknown distribution '{name}'. Available: {}",
                    toolset::builtin_distribution_names().join(", ")
                ))
            })?;

            let mut rng = match seed {
                Some(s) => rand::rngs::StdRng::seed_from_u64(s),
                None => rand::rngs::StdRng::from_os_rng(),
            };
            let selected = dist.sample(&mut rng);
            let mut tools: Vec<&str> = selected.iter().map(|s| s.as_str()).collect();
            tools.sort();

            if json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "distribution": dist.name,
                    "seed": seed,
                    "selected_count": tools.len(),
                    "possible_count": dist.possible_tools().len(),
                    "selected": tools,
                }))
                .unwrap())
            } else {
                let mut lines = vec![format!(
                    "Sampled {} tools from '{}' ({} possible):",
                    tools.len(),
                    dist.name,
                    dist.possible_tools().len()
                )];
                for tool in &tools {
                    lines.push(format!("  {tool}"));
                }
                Ok(lines.join("\n"))
            }
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::plugin_kind_name;
    use genesis_lua::PluginKind;

    #[test]
    fn plugin_kind_name_reports_bundled_plugins() {
        assert_eq!(plugin_kind_name(PluginKind::Bundled), "bundled");
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
        .filter(|candidate| {
            !same_plugin_entry(candidate, entry) && candidate.status == "shadowed"
        })
        .collect();
    let alternates: Vec<&PluginCommandEntry> = matches
        .into_iter()
        .filter(|candidate| {
            !same_plugin_entry(candidate, entry) && candidate.status != "shadowed"
        })
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

pub(crate) fn run_personality(command: PersonalityCommand, json: bool) -> Result<String, CliError> {
    use genesis_core::personality;

    match command {
        PersonalityCommand::List => {
            let personalities = personality::list_personalities();
            if json {
                let items: Vec<serde_json::Value> = personalities
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "description": p.description,
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&items).unwrap())
            } else {
                let mut lines = vec![format!("{:<16} {:<10} {}", "NAME", "SOURCE", "DESCRIPTION")];
                for p in &personalities {
                    lines.push(format!(
                        "{:<16} {:<10} {}",
                        p.name, "bundled", p.description
                    ));
                }
                Ok(lines.join("\n"))
            }
        }
        PersonalityCommand::Show { name } => {
            let p = personality::get_personality(&name).ok_or_else(|| {
                let available: Vec<String> = personality::list_personalities()
                    .iter()
                    .map(|p| p.name.to_owned())
                    .collect();
                CliError::Other(format!(
                    "unknown personality '{name}'. Available: {}",
                    available.join(", ")
                ))
            })?;

            if json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "name": p.name,
                    "source": "bundled",
                    "description": p.description,
                    "system_prompt_prefix": p.system_prompt_prefix,
                }))
                .unwrap())
            } else {
                Ok(format!(
                    "Personality: {}\nSource: bundled\nDescription: {}\n\nSystem prompt prefix:\n{}",
                    p.name, p.description, p.system_prompt_prefix
                ))
            }
        }
    }
}

pub(crate) async fn run_benchmark(
    config_path: Option<PathBuf>,
    runs: usize,
    include_tool_provider: bool,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;

    let mut providers = vec![(
        "primary",
        loaded.config.provider.backend.clone(),
        loaded.config.provider.model.clone(),
        genesis_provider::client_from_config(
            &loaded.config.provider.backend,
            &loaded.config.provider.model,
            loaded.config.provider.base_url.as_deref(),
            loaded.config.provider.api_key_env.as_deref(),
        )
        .await?,
    )];

    if include_tool_provider {
        if let Some(tp) = &loaded.config.tool_provider {
            providers.push((
                "tool",
                tp.backend.clone(),
                tp.model.clone(),
                genesis_provider::client_from_config(
                    &tp.backend,
                    &tp.model,
                    tp.base_url.as_deref(),
                    tp.api_key_env.as_deref(),
                )
                .await?,
            ));
        }
    }

    let test_prompt = "Say exactly: ping";
    let runs = runs.clamp(1, 20);
    let mut results = Vec::new();

    for (label, backend, model, client) in &providers {
        eprintln!("benchmarking {label} ({backend}/{model}) × {runs}...");

        let mut latencies = Vec::with_capacity(runs);
        let mut ttft_times = Vec::new(); // time to first token (streaming)
        let mut errors = 0;

        for i in 0..runs {
            let request = genesis_provider::ChatCompletionRequest::new(
                "",
                vec![genesis_provider::ChatMessage::user(test_prompt)],
            );

            let start = std::time::Instant::now();
            match client.complete(request).await {
                Ok(response) => {
                    let elapsed = start.elapsed();
                    latencies.push(elapsed);

                    let tokens = response
                        .usage
                        .as_ref()
                        .map(|u| u.completion_tokens)
                        .unwrap_or(0);
                    eprintln!(
                        "  run {}: {:.0}ms ({tokens} tokens)",
                        i + 1,
                        elapsed.as_secs_f64() * 1000.0,
                    );
                }
                Err(e) => {
                    errors += 1;
                    eprintln!("  run {}: ERROR — {e}", i + 1);
                }
            }

            // Also do a streaming TTFT test on the first run.
            if i == 0 {
                let request = genesis_provider::ChatCompletionRequest::new(
                    "",
                    vec![genesis_provider::ChatMessage::user(test_prompt)],
                );
                let stream_start = std::time::Instant::now();
                if let Ok(mut stream) = client.complete_stream(request).await {
                    use futures_util::TryStreamExt;
                    if let Some(_chunk) = stream.try_next().await.ok().flatten() {
                        ttft_times.push(stream_start.elapsed());
                    }
                }
            }
        }

        let successful = latencies.len();
        let (min, max, avg, p50) = if !latencies.is_empty() {
            latencies.sort();
            let min = latencies[0];
            let max = latencies[latencies.len() - 1];
            let total: Duration = latencies.iter().sum();
            let avg = total / successful as u32;
            let p50 = latencies[successful / 2];
            (min, max, avg, p50)
        } else {
            (
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            )
        };

        results.push(serde_json::json!({
            "label": label,
            "backend": backend,
            "model": model,
            "runs": runs,
            "successful": successful,
            "errors": errors,
            "min_ms": min.as_millis(),
            "max_ms": max.as_millis(),
            "avg_ms": avg.as_millis(),
            "p50_ms": p50.as_millis(),
            "ttft_ms": ttft_times.first().map(|d| d.as_millis()),
        }));
    }

    if json {
        return Ok(serde_json::to_string_pretty(&results)?);
    }

    let mut lines = Vec::new();
    for r in &results {
        lines.push(format!(
            "\n{} ({}/{})",
            r["label"].as_str().unwrap_or("-"),
            r["backend"].as_str().unwrap_or("-"),
            r["model"].as_str().unwrap_or("-"),
        ));
        lines.push(format!(
            "  {} successful / {} errors",
            r["successful"], r["errors"]
        ));
        lines.push(format!(
            "  avg: {}ms  p50: {}ms  min: {}ms  max: {}ms",
            r["avg_ms"], r["p50_ms"], r["min_ms"], r["max_ms"]
        ));
        if let Some(ttft) = r["ttft_ms"].as_u64() {
            lines.push(format!("  ttft (time to first token): {ttft}ms"));
        }
    }

    Ok(lines.join("\n"))
}

pub(crate) fn run_info(config_path: Option<PathBuf>, json: bool) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let db_path = &loaded.config.storage.database_path;

    let tool_count = genesis_core::default_tool_count();

    let session_count = SessionStore::new(db_path).count_sessions().unwrap_or(0);

    let skill_count = SkillStore::new(db_path)
        .list_all()
        .map(|s| s.len())
        .unwrap_or(0);

    let trait_count = UserModelStore::new(db_path)
        .list_all()
        .map(|t| t.len())
        .unwrap_or(0);

    let schedule_count = ScheduleStore::new(db_path)
        .list_all()
        .map(|s| s.len())
        .unwrap_or(0);

    let mcp_server_count = loaded.config.mcp_servers.len();
    let version = env!("CARGO_PKG_VERSION");

    if json {
        let info = serde_json::json!({
            "version": version,
            "profile": loaded.config.profile,
            "provider": {
                "backend": loaded.config.provider.backend,
                "model": loaded.config.provider.model,
            },
            "tools": tool_count,
            "mcp_servers": mcp_server_count,
            "sessions": session_count,
            "skills": skill_count,
            "user_traits": trait_count,
            "schedules": schedule_count,
            "config_path": loaded.paths.config_path.display().to_string(),
            "database_path": db_path.display().to_string(),
        });
        Ok(serde_json::to_string_pretty(&info)?)
    } else {
        Ok(format!(
            "genesis v{version}\n\
             profile:     {}\n\
             provider:    {} / {}\n\
             tools:       {tool_count}\n\
             mcp servers: {mcp_server_count}\n\
             sessions:    {session_count}\n\
             skills:      {skill_count}\n\
             user traits: {trait_count}\n\
             schedules:   {schedule_count}\n\
             config:      {}\n\
             database:    {}",
            loaded.config.profile,
            loaded.config.provider.backend,
            loaded.config.provider.model,
            loaded.paths.config_path.display(),
            db_path.display(),
        ))
    }
}

pub(crate) fn run_tools(config_path: Option<PathBuf>, json: bool) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let ctx = genesis_core::build_execution_context_from_loaded(
        &loaded,
        "tools-list".to_owned(),
        DeliveryPlatform::Cli,
    );
    let runtime = genesis_core::build_default_tool_runtime(&ctx);
    let defs = runtime.definitions();

    if json {
        Ok(serde_json::to_string_pretty(&defs)?)
    } else {
        let mut lines = vec![format!("genesis tools ({} registered)", defs.len())];
        for def in &defs {
            lines.push(format!("  {:<20} {}", def.name, def.description));
        }
        Ok(lines.join("\n"))
    }
}

pub(crate) fn run_context(command: ContextCommand) -> Result<String, CliError> {
    let current_dir = std::env::current_dir()?;

    match command {
        ContextCommand::Show => Ok(
            match load_context_file(&current_dir, &genesis_config::ContextSecurityPolicy::Warn) {
                Some(contents) => contents,
                None => "no context file found in current directory".to_owned(),
            },
        ),
        ContextCommand::Init => {
            let context_dir = current_dir.join(".genesis");
            let context_path = context_dir.join("context.md");

            if context_path.exists() {
                return Ok(format!(
                    "context file already exists: {}",
                    context_path.display()
                ));
            }

            std::fs::create_dir_all(&context_dir)?;
            std::fs::write(&context_path, context_template())?;

            Ok(format!("created context file: {}", context_path.display()))
        }
        ContextCommand::Edit => {
            let context_dir = current_dir.join(".genesis");
            let context_path = context_dir.join("context.md");

            if !context_path.exists() {
                // Create with template if it doesn't exist
                std::fs::create_dir_all(&context_dir)?;
                std::fs::write(&context_path, context_template())?;
            }

            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_owned());
            let path_str = context_path.display().to_string();
            let status = std::process::Command::new(&editor)
                .arg(&path_str)
                .status()
                .map_err(|e| CliError::Other(format!("failed to launch {editor}: {e}")))?;
            if status.success() {
                Ok(format!("context saved: {path_str}"))
            } else {
                Err(CliError::Other(format!(
                    "{editor} exited with status {status}"
                )))
            }
        }
    }
}

pub(crate) async fn run_model(
    config_path: Option<PathBuf>,
    command: ModelCommand,
    json: bool,
) -> Result<String, CliError> {
    match command {
        ModelCommand::Show => {
            let loaded = load(config_path.as_deref())?;
            if json {
                Ok(serde_json::to_string_pretty(&loaded.config.provider)?)
            } else {
                let mut lines = vec![
                    format!("backend: {}", loaded.config.provider.backend),
                    format!("model: {}", loaded.config.provider.model),
                ];
                if let Some(url) = &loaded.config.provider.base_url {
                    lines.push(format!("base_url: {url}"));
                }
                if let Some(key_env) = &loaded.config.provider.api_key_env {
                    lines.push(format!("api_key_env: {key_env}"));
                }
                Ok(lines.join("\n"))
            }
        }
        ModelCommand::List { backend } => {
            let models = known_models();
            if json {
                let filtered: Vec<_> = if let Some(ref b) = backend {
                    models
                        .iter()
                        .filter(|(provider, _, _)| provider.eq_ignore_ascii_case(b))
                        .collect()
                } else {
                    models.iter().collect()
                };
                let json_models: Vec<_> = filtered
                    .iter()
                    .map(|(provider, model, desc)| {
                        serde_json::json!({
                            "provider": provider,
                            "model": model,
                            "description": desc,
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&json_models)?)
            } else {
                let loaded = load(config_path.as_deref()).ok();
                let active_model = loaded.as_ref().map(|l| l.config.provider.model.as_str());
                let mut current_provider = String::new();
                let mut lines = Vec::new();
                for (provider, model, desc) in &models {
                    if let Some(ref b) = backend {
                        if !provider.eq_ignore_ascii_case(b) {
                            continue;
                        }
                    }
                    if *provider != current_provider {
                        if !current_provider.is_empty() {
                            lines.push(String::new());
                        }
                        lines.push(format!("[{provider}]"));
                        current_provider = provider.to_string();
                    }
                    let marker = if active_model == Some(model) {
                        " *"
                    } else {
                        ""
                    };
                    lines.push(format!("  {model}{marker}  — {desc}"));
                }
                if lines.is_empty() {
                    Ok("No models found for the specified backend.".to_owned())
                } else {
                    lines.push(String::new());
                    lines.push("* = currently active".to_owned());
                    Ok(lines.join("\n"))
                }
            }
        }
        ModelCommand::Set {
            model,
            backend,
            base_url,
            api_key_env,
        } => {
            let loaded = load(config_path.as_deref())?;
            let config_file = config_path.unwrap_or_else(|| loaded.paths.config_path.clone());

            genesis_config::update_provider_in_file(
                &config_file,
                backend.as_deref(),
                Some(&model),
                base_url.as_ref().map(|u| Some(u.as_str())),
                api_key_env.as_ref().map(|k| Some(k.as_str())),
            )?;

            let updated = load(Some(&config_file))?;
            if json {
                Ok(serde_json::to_string_pretty(&updated.config.provider)?)
            } else {
                Ok(format!(
                    "model set to {} / {}\nconfig: {}",
                    updated.config.provider.backend,
                    updated.config.provider.model,
                    config_file.display()
                ))
            }
        }
        ModelCommand::Browse {
            query,
            tools,
            vision,
            reasoning,
            sort,
            limit,
            json: json_output,
        } => {
            let loaded = load(config_path.as_deref())?;
            let cache_dir = loaded.paths.data_dir.join("cache");
            let _ = std::fs::create_dir_all(&cache_dir);

            // Resolve API key for OpenRouter.
            let api_key = std::env::var("OPENROUTER_API_KEY").ok();

            // Fetch models.
            let mut models = genesis_provider::openrouter_models::fetch_models(
                api_key.as_deref(),
                &cache_dir,
            )
            .await
            .map_err(|e| CliError::Other(format!("Failed to fetch models: {e}")))?;

            // Filter by query.
            if let Some(ref q) = query {
                let q_lower = q.to_lowercase();
                models.retain(|m| {
                    m.id.to_lowercase().contains(&q_lower)
                        || m.name.to_lowercase().contains(&q_lower)
                });
            }

            // Filter by capabilities.
            if tools {
                models.retain(|m| m.supports_tools());
            }
            if vision {
                models.retain(|m| m.supports_vision());
            }
            if reasoning {
                models.retain(|m| m.supports_reasoning());
            }

            // Sort.
            match sort.as_str() {
                "cheapest" | "price" => {
                    models.sort_by(|a, b| {
                        let pa = a.pricing.prompt.parse::<f64>().unwrap_or(f64::MAX);
                        let pb = b.pricing.prompt.parse::<f64>().unwrap_or(f64::MAX);
                        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                "context" | "largest" => {
                    models.sort_by(|a, b| b.context_length.cmp(&a.context_length));
                }
                _ => {
                    // Default: newest first.
                    models.sort_by(|a, b| b.created.cmp(&a.created));
                }
            }

            // Limit.
            models.truncate(limit);

            if json_output {
                let output = serde_json::to_string_pretty(&models)?;
                return Ok(output);
            }

            // Human-readable output.
            if models.is_empty() {
                return Ok("No models found matching your criteria.".to_owned());
            }

            let mut lines = Vec::new();
            lines.push(format!("Found {} models:\n", models.len()));

            for m in &models {
                let mut badges = String::new();
                if m.supports_tools() {
                    badges.push_str(" [T]");
                }
                if m.supports_vision() {
                    badges.push_str(" [V]");
                }
                if m.supports_reasoning() {
                    badges.push_str(" [R]");
                }

                lines.push(format!(
                    "  {:<45} {:>12}  {:>6}{}",
                    if m.id.len() > 45 {
                        let truncated: String = m.id.chars().take(44).collect();
                        format!("{truncated}…")
                    } else {
                        m.id.clone()
                    },
                    m.price_display(),
                    m.context_display(),
                    badges,
                ));
            }

            lines.push(String::new());
            lines.push("Badges: [T] tools  [V] vision  [R] reasoning".to_owned());
            lines.push(format!(
                "Sort: {}  |  Use: genesis model set <id>",
                sort
            ));

            Ok(lines.join("\n"))
        }
    }
}

/// Verify API connectivity by sending a minimal completion request.
pub(crate) async fn verify_api_connectivity(loaded: &LoadedConfig) -> Result<u128, String> {
    use genesis_provider::{ChatCompletionRequest, ChatMessage as ProviderMessage};

    let client = genesis_provider::client_from_config(
        &loaded.config.provider.backend,
        &loaded.config.provider.model,
        loaded.config.provider.base_url.as_deref(),
        loaded.config.provider.api_key_env.as_deref(),
    )
    .await
    .map_err(|e| format!("failed to create client: {e}"))?;

    let mut request = ChatCompletionRequest::new(
        &loaded.config.provider.model,
        vec![ProviderMessage::user("Say: ok")],
    );
    request.max_tokens = Some(5);

    let start = std::time::Instant::now();
    client.complete(request).await.map_err(|e| format!("{e}"))?;

    Ok(start.elapsed().as_millis())
}

/// Rough cost estimate based on typical per-million-token pricing.
pub(crate) fn estimate_token_cost(
    input_tokens: u32,
    output_tokens: u32,
) -> Option<(f64, &'static str)> {
    if input_tokens == 0 && output_tokens == 0 {
        return None;
    }
    // Use GPT-4.1-mini pricing as a reasonable middle-ground estimate:
    // $0.40 / 1M input, $1.60 / 1M output
    let input_cost = (input_tokens as f64 / 1_000_000.0) * 0.40;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * 1.60;
    Some((input_cost + output_cost, "GPT-4.1-mini pricing"))
}

pub(crate) fn known_models() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        // Anthropic
        (
            "anthropic",
            "claude-opus-4-6",
            "Most capable, complex reasoning",
        ),
        (
            "anthropic",
            "claude-sonnet-4-6",
            "Balanced speed and capability",
        ),
        (
            "anthropic",
            "claude-haiku-4-5-20251001",
            "Fastest, lightweight tasks",
        ),
        // OpenAI
        ("openai", "gpt-4.1", "Flagship GPT model"),
        ("openai", "gpt-4.1-mini", "Fast and affordable"),
        ("openai", "gpt-4.1-nano", "Fastest, simplest tasks"),
        ("openai", "o3", "Advanced reasoning"),
        ("openai", "o4-mini", "Fast reasoning"),
        // OpenAI Codex (ChatGPT subscription)
        ("openai-codex", "gpt-5.4", "Latest GPT-5.4"),
        ("openai-codex", "o3-pro", "Most capable reasoning"),
        ("openai-codex", "o3", "Advanced reasoning"),
        ("openai-codex", "gpt-4.1", "Flagship GPT model"),
        ("openai-codex", "o4-mini", "Fast reasoning"),
        // Google
        ("google", "gemini-2.5-pro", "Best for complex tasks"),
        ("google", "gemini-2.5-flash", "Fast and versatile"),
        // OpenRouter (aggregator — any model)
        (
            "openrouter",
            "anthropic/claude-sonnet-4-6",
            "Claude via OpenRouter",
        ),
        ("openrouter", "openai/gpt-4.1", "GPT-4.1 via OpenRouter"),
        (
            "openrouter",
            "google/gemini-2.5-pro",
            "Gemini via OpenRouter",
        ),
        (
            "openrouter",
            "deepseek/deepseek-r1",
            "DeepSeek R1 reasoning",
        ),
        (
            "openrouter",
            "meta-llama/llama-4-maverick",
            "Llama 4 Maverick",
        ),
    ]
}

/// Detect the user's preferred Codex model from `~/.codex/config.toml` or
/// `~/.codex/models_cache.json`.
pub(crate) fn detect_codex_model() -> Option<String> {
    let home = dirs::home_dir()?;

    // Try config.toml first
    let config_path = home.join(".codex").join("config.toml");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(model) = table.get("model").and_then(|v| v.as_str()) {
                return Some(model.to_owned());
            }
        }
    }

    // Try models_cache.json
    let cache_path = home.join(".codex").join("models_cache.json");
    if let Ok(content) = std::fs::read_to_string(&cache_path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
            // The cache is typically an array of model objects
            if let Some(models) = value.as_array() {
                if let Some(first) = models.first() {
                    if let Some(id) = first
                        .get("id")
                        .or_else(|| first.get("model"))
                        .and_then(|v| v.as_str())
                    {
                        return Some(id.to_owned());
                    }
                    // If it's just a string
                    if let Some(s) = first.as_str() {
                        return Some(s.to_owned());
                    }
                }
            }
        }
    }

    None
}

pub(crate) fn run_compress(
    input: String,
    output: Option<String>,
    level: Option<String>,
    format: Option<String>,
    training: bool,
) -> Result<String, CliError> {
    let level = parse_compression_level(level.as_deref())?;
    let format = parse_compression_format(format.as_deref())?;

    let raw = std::fs::read_to_string(&input)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", input)))?;
    let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
        .map_err(|e| CliError::Other(format!("invalid trajectory JSON in {}: {e}", input)))?;

    let compressed = if training {
        genesis_core::compress::TrajectoryCompressor::default().compress_for_training(&trajectory)
    } else {
        genesis_core::compress::compress(&trajectory, level)
    };
    let rendered = match format {
        CompressionFormat::Json => serde_json::to_string_pretty(&compressed)?,
        CompressionFormat::ShareGpt => {
            serde_json::to_string_pretty(&genesis_core::compress::to_sharegpt(&compressed))?
        }
        CompressionFormat::ChatMl => genesis_core::compress::to_chatml(&compressed),
    };

    match output {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        CliError::Other(format!(
                            "failed to create parent directory for {}: {e}",
                            path
                        ))
                    })?;
                }
            }
            std::fs::write(&path, rendered)
                .map_err(|e| CliError::Other(format!("failed to write {}: {e}", path)))?;
            Ok(format!("wrote compressed trajectory to {path}"))
        }
        None => Ok(rendered),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompressionFormat {
    Json,
    ShareGpt,
    ChatMl,
}

pub(crate) fn parse_compression_level(
    raw: Option<&str>,
) -> Result<genesis_core::compress::CompressionLevel, CliError> {
    match raw.unwrap_or("medium").trim().to_ascii_lowercase().as_str() {
        "light" => Ok(genesis_core::compress::CompressionLevel::Light),
        "medium" => Ok(genesis_core::compress::CompressionLevel::Medium),
        "heavy" => Ok(genesis_core::compress::CompressionLevel::Heavy),
        other => Err(CliError::Other(format!(
            "unknown compression level '{other}', expected light, medium, or heavy"
        ))),
    }
}

pub(crate) fn parse_compression_format(raw: Option<&str>) -> Result<CompressionFormat, CliError> {
    match raw.unwrap_or("json").trim().to_ascii_lowercase().as_str() {
        "json" => Ok(CompressionFormat::Json),
        "sharegpt" => Ok(CompressionFormat::ShareGpt),
        "chatml" => Ok(CompressionFormat::ChatMl),
        other => Err(CliError::Other(format!(
            "unknown compression format '{other}', expected json, sharegpt, or chatml"
        ))),
    }
}
