use std::time::{SystemTime, UNIX_EPOCH};

use genesis_config::LoadedConfig;
use genesis_storage::{
    bootstrap, InsightsData, MemoryStore, SessionStore, SessionSummary, SkillStore,
    StoredSchedule, UsageStats,
};
use genesis_ui::UiContext;

use crate::{CliError, HubCommand, TapCommand};

pub(crate) fn context_template() -> &'static str {
    "# Project Context

## Purpose
- Describe what this project does.

## Constraints
- Note any technical, product, or operational constraints.

## Working Agreements
- Capture coding standards, review expectations, and collaboration rules.

## Priorities
- Explain what matters most right now.
"
}

pub(crate) fn format_session_list(sessions: &[SessionSummary], ui: &UiContext) -> String {
    if sessions.is_empty() {
        return "no saved sessions".to_owned();
    }

    let mut lines = vec!["genesis sessions".to_owned()];
    for session in sessions {
        let tokens = session.total_input_tokens + session.total_output_tokens;
        let token_info = if tokens > 0 {
            ui.format_metadata(&format!("  {}tok", tokens))
        } else {
            String::new()
        };
        let title_info = session
            .title
            .as_deref()
            .map(|t| format!("  \"{t}\""))
            .unwrap_or_default();
        lines.push(format!(
            "{}  {}",
            ui.format_accent(&session.id),
            ui.format_metadata(&format!("{}  {}{title_info}{token_info}", session.platform, session.created_at))
        ));
    }
    lines.join("\n")
}

pub(crate) fn format_usage_stats(stats: &UsageStats) -> String {
    let total_tokens = stats.total_input_tokens + stats.total_output_tokens;
    let mut lines = vec!["genesis usage stats".to_owned()];
    lines.push(format!("  sessions:      {}", stats.total_sessions));
    lines.push(format!("  input tokens:  {}", stats.total_input_tokens));
    lines.push(format!("  output tokens: {}", stats.total_output_tokens));
    lines.push(format!("  total tokens:  {total_tokens}"));
    lines.join("\n")
}

pub(crate) fn format_insights(data: &InsightsData, model: &str) -> String {
    let total_tokens = data.total_input_tokens + data.total_output_tokens;
    let avg_per_session = if data.sessions_count > 0 {
        total_tokens / data.sessions_count
    } else {
        0
    };

    let mut lines = vec![format!(
        "genesis insights (last {} days)",
        data.period_days
    )];
    lines.push(format!("  model:           {model}"));
    lines.push(format!("  sessions:        {}", data.sessions_count));
    lines.push(format!("  input tokens:    {}", data.total_input_tokens));
    lines.push(format!("  output tokens:   {}", data.total_output_tokens));
    lines.push(format!("  total tokens:    {total_tokens}"));
    lines.push(format!("  avg per session: {avg_per_session}"));

    if let Some(cost) = genesis_provider::pricing::estimate_cost(
        model,
        data.total_input_tokens as u32,
        data.total_output_tokens as u32,
    ) {
        lines.push(format!("  estimated cost:  ~{cost}"));
    }

    if !data.platform_breakdown.is_empty() {
        lines.push(String::new());
        lines.push("  platforms:".to_owned());
        for (platform, count) in &data.platform_breakdown {
            lines.push(format!("    {platform}: {count} sessions"));
        }
    }

    if !data.tool_usage.is_empty() {
        lines.push(String::new());
        lines.push("  top tools:".to_owned());
        for (name, count) in data.tool_usage.iter().take(10) {
            lines.push(format!("    {name}: {count} calls"));
        }
    }

    if !data.sessions_per_day.is_empty() {
        lines.push(String::new());
        lines.push("  activity:".to_owned());
        let max_count = data
            .sessions_per_day
            .iter()
            .map(|(_, c)| *c)
            .max()
            .unwrap_or(1);
        for (day, count) in &data.sessions_per_day {
            let bar_len = if max_count > 0 {
                ((*count as f64 / max_count as f64) * 20.0) as usize
            } else {
                0
            };
            let bar: String = std::iter::repeat_n('#', bar_len).collect();
            lines.push(format!("    {day}  {bar} ({count})"));
        }
    }

    lines.join("\n")
}

pub(crate) fn format_memory_list(memories: &[genesis_storage::StoredMemory]) -> String {
    if memories.is_empty() {
        return "no stored memories".to_owned();
    }

    let mut lines = vec![format!("memories ({})", memories.len())];
    for m in memories {
        let content_preview = if m.content.len() > 80 {
            format!("{}...", &m.content[..77])
        } else {
            m.content.clone()
        };
        lines.push(format!("[{}] {} ({})", m.kind, content_preview, m.created_at));
    }
    lines.join("\n")
}

pub(crate) fn build_status_text(loaded: &LoadedConfig, ui: &UiContext) -> String {
    let mut lines = vec!["genesis status".to_owned()];
    lines.push(String::new());

    // Config
    let config_exists = loaded.paths.config_path.exists();
    lines.push(format!(
        "{}  {} ({})",
        ui.format_metadata("  config:   "),
        loaded.paths.config_path.display(),
        if config_exists { "found" } else { "not found" }
    ));

    // Provider
    lines.push(format!(
        "{}  {} / {}",
        ui.format_metadata("  provider: "),
        loaded.config.provider.backend, loaded.config.provider.model
    ));

    // Database
    let db_exists = loaded.config.storage.database_path.exists();
    lines.push(format!(
        "{}  {} ({})",
        ui.format_metadata("  database: "),
        loaded.config.storage.database_path.display(),
        if db_exists { "found" } else { "not found" }
    ));

    // Counts (best-effort)
    if db_exists && bootstrap(&loaded.config.storage.database_path).is_ok() {
        let session_store = SessionStore::new(&loaded.config.storage.database_path);
        let skill_store = genesis_storage::SkillStore::new(&loaded.config.storage.database_path);
        let schedule_store =
            genesis_storage::ScheduleStore::new(&loaded.config.storage.database_path);
        let memory_store =
            MemoryStore::new(&loaded.config.storage.database_path);

        if let Ok(stats) = session_store.usage_stats() {
            lines.push(format!("{}  {}", ui.format_metadata("  sessions: "), stats.total_sessions));
            let total_tokens = stats.total_input_tokens + stats.total_output_tokens;
            lines.push(format!("{}  {total_tokens} total", ui.format_metadata("  tokens:   ")));
        }
        if let Ok(skills) = skill_store.list_all() {
            lines.push(format!("{}  {}", ui.format_metadata("  skills:   "), skills.len()));
        }
        if let Ok(schedules) = schedule_store.list_all() {
            lines.push(format!("{}  {}", ui.format_metadata("  schedules:"), schedules.len()));
        }
        if let Ok(memories) = memory_store.list(usize::MAX) {
            lines.push(format!("{}  {}", ui.format_metadata("  memories: "), memories.len()));
        }
    }

    // MCP servers
    let mcp_count = loaded.config.mcp_servers.len();
    lines.push(format!("{}  {mcp_count} server(s) configured", ui.format_metadata("  mcp:      ")));

    // Runtime
    lines.push(format!(
        "{}  max_turns={}, destructive={}",
        ui.format_metadata("  runtime:  "),
        loaded.config.runtime.max_turns, loaded.config.runtime.allow_destructive_tools
    ));

    lines.join("\n")
}

pub(crate) fn build_status_json(loaded: &LoadedConfig) -> serde_json::Value {
    let mut data = serde_json::json!({
        "config_path": loaded.paths.config_path.display().to_string(),
        "config_exists": loaded.paths.config_path.exists(),
        "database_path": loaded.config.storage.database_path.display().to_string(),
        "database_exists": loaded.config.storage.database_path.exists(),
        "provider_backend": loaded.config.provider.backend,
        "provider_model": loaded.config.provider.model,
        "mcp_servers": loaded.config.mcp_servers.len(),
        "max_turns": loaded.config.runtime.max_turns,
        "allow_destructive_tools": loaded.config.runtime.allow_destructive_tools,
    });

    if loaded.config.storage.database_path.exists()
        && bootstrap(&loaded.config.storage.database_path).is_ok()
    {
        let session_store = SessionStore::new(&loaded.config.storage.database_path);
        if let Ok(stats) = session_store.usage_stats() {
            data["total_sessions"] = serde_json::json!(stats.total_sessions);
            data["total_tokens"] =
                serde_json::json!(stats.total_input_tokens + stats.total_output_tokens);
        }
    }

    data
}

pub(crate) fn format_session_messages(session_id: &str, messages: &[genesis_storage::StoredMessage]) -> String {
    if messages.is_empty() {
        return format!("session {session_id}: no messages");
    }

    let mut lines = vec![format!("session {session_id} ({} messages)", messages.len())];
    for msg in messages {
        let content = msg.content.as_deref().unwrap_or("[no content]");
        let truncated = if content.len() > 200 {
            format!("{}...", &content[..200])
        } else {
            content.to_owned()
        };
        lines.push(format!("[{}] {}: {}", msg.created_at, msg.role, truncated));
    }
    lines.join("\n")
}

pub(crate) fn export_session_markdown(session_id: &str, messages: &[genesis_storage::StoredMessage]) -> String {
    let mut lines = vec![format!("# Session {session_id}\n")];
    for msg in messages {
        let role = match msg.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "system" => "System",
            "tool" => "Tool Result",
            other => other,
        };
        lines.push(format!("## {role}\n"));
        if let Some(content) = &msg.content {
            lines.push(content.clone());
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

pub(crate) fn run_session_import(
    store: &genesis_storage::SessionStore,
    file: &str,
    format: Option<&str>,
    title: Option<&str>,
) -> Result<String, CliError> {
    let path = std::path::Path::new(file);
    let detected_format = match format {
        Some(f) => f.to_owned(),
        None => match path.extension().and_then(|e| e.to_str()) {
            Some("json") => "sharegpt".to_owned(),
            Some("jsonl") => "jsonl".to_owned(),
            _ => {
                return Err(CliError::Other(
                    "cannot auto-detect format: use --format sharegpt or --format jsonl".to_owned(),
                ))
            }
        },
    };

    let contents = std::fs::read_to_string(path)
        .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;

    let messages: Vec<(String, String)> = match detected_format.as_str() {
        "sharegpt" => {
            let entries: Vec<serde_json::Value> = serde_json::from_str(&contents)
                .map_err(|e| CliError::Other(format!("invalid ShareGPT JSON: {e}")))?;
            entries
                .into_iter()
                .filter_map(|entry| {
                    let from = entry.get("from")?.as_str()?;
                    let value = entry.get("value")?.as_str()?;
                    let role = match from {
                        "human" => "user",
                        "gpt" => "assistant",
                        _ => return None, // skip system, thought, etc.
                    };
                    Some((role.to_owned(), value.to_owned()))
                })
                .collect()
        }
        "jsonl" => {
            contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    let entry: serde_json::Value = serde_json::from_str(line)
                        .map_err(|e| CliError::Other(format!("invalid JSONL line: {e}")))?;
                    let role = entry
                        .get("role")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| CliError::Other("JSONL line missing 'role' field".to_owned()))?
                        .to_owned();
                    let content = entry
                        .get("content")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| CliError::Other("JSONL line missing 'content' field".to_owned()))?
                        .to_owned();
                    Ok((role, content))
                })
                .collect::<Result<Vec<_>, CliError>>()?
        }
        other => {
            return Err(CliError::Other(format!(
                "unknown import format '{other}', expected 'sharegpt' or 'jsonl'"
            )));
        }
    };

    let count = messages.len();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let session_id = format!("import-{timestamp}");

    store.import_session(&session_id, title, messages)?;

    Ok(format!("Imported {count} messages into session {session_id}"))
}

pub(crate) fn format_skill_list(skills: &[genesis_storage::StoredSkill]) -> String {
    if skills.is_empty() {
        return "no saved skills".to_owned();
    }

    let mut lines = vec!["genesis skills".to_owned()];
    for skill in skills {
        lines.push(format!(
            "{}\tv{}\t{}",
            skill.name, skill.version, skill.description
        ));
    }
    lines.join("\n")
}

pub(crate) fn format_skill(skill: &genesis_storage::StoredSkill) -> String {
    let mut lines = vec![
        format!("skill: {}", skill.name),
        format!("description: {}", skill.description),
        format!("version: {}", skill.version),
        format!("created_at: {}", skill.created_at),
        format!("updated_at: {}", skill.updated_at),
    ];

    if let Some(trigger_hint) = &skill.trigger_hint {
        lines.push(format!("trigger_hint: {trigger_hint}"));
    }

    if !skill.tags.is_empty() {
        lines.push(format!("tags: {}", skill.tags.join(", ")));
    }

    lines.push("instructions:".to_owned());
    lines.push(skill.instructions.clone());
    lines.join("\n")
}

pub(crate) fn run_skills_scan(dir: &str, json: bool) -> Result<String, CliError> {
    let entries = genesis_core::skill_manifest::scan_skills_dir(std::path::Path::new(dir))
        .map_err(|e| CliError::Other(format!("failed to scan skills directory: {e}")))?;

    if json {
        return Ok(serde_json::to_string_pretty(&entries)?);
    }

    if entries.is_empty() {
        return Ok(format!("no SKILL.md files found in {dir}"));
    }

    let mut lines = vec![format!("found {} skill(s) in {dir}", entries.len())];
    for entry in &entries {
        let tags = if entry.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", entry.tags.join(", "))
        };
        lines.push(format!(
            "  {} v{}: {}{}",
            entry.name, entry.version, entry.description, tags
        ));
    }
    Ok(lines.join("\n"))
}

pub(crate) fn run_skills_search(
    store: &SkillStore,
    query: &str,
    dir: Option<&str>,
    json: bool,
) -> Result<String, CliError> {
    // Search stored skills
    let stored = store.find_matching(query, 50).unwrap_or_default();

    // Optionally search SKILL.md files on disk
    let disk_entries = if let Some(dir) = dir {
        let all = genesis_core::skill_manifest::scan_skills_dir(std::path::Path::new(dir))
            .unwrap_or_default();
        genesis_core::skill_manifest::search_entries(&all, query)
    } else {
        vec![]
    };

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "stored": stored,
            "disk": disk_entries,
        }))?);
    }

    let mut lines = Vec::new();
    if !stored.is_empty() {
        lines.push(format!("stored skills matching \"{}\":", query));
        for s in &stored {
            lines.push(format!("  {} v{}: {}", s.name, s.version, s.description));
        }
    }
    if !disk_entries.is_empty() {
        lines.push(format!("SKILL.md files matching \"{}\":", query));
        for e in &disk_entries {
            lines.push(format!(
                "  {} v{}: {} ({})",
                e.name,
                e.version,
                e.description,
                e.path.display()
            ));
        }
    }
    if lines.is_empty() {
        return Ok(format!("no skills matching \"{query}\""));
    }
    Ok(lines.join("\n"))
}

pub(crate) fn run_skills_install_local(store: &SkillStore, path: &str) -> Result<String, CliError> {
    let skill_file = std::path::Path::new(path).join("SKILL.md");
    let parsed = genesis_core::skill_manifest::parse_skill_file(&skill_file)
        .map_err(|e| CliError::Other(format!("failed to parse SKILL.md: {e}")))?;

    let tags: Vec<&str> = parsed.frontmatter.tags.iter().map(|s| s.as_str()).collect();
    let trigger = if parsed.frontmatter.description.is_empty() {
        None
    } else {
        Some(parsed.frontmatter.description.as_str())
    };

    store.upsert(
        &parsed.frontmatter.name,
        &parsed.frontmatter.description,
        &parsed.instructions,
        trigger,
        &tags,
    )?;

    Ok(format!(
        "installed skill '{}' v{} from {}",
        parsed.frontmatter.name, parsed.frontmatter.version, path
    ))
}

pub(crate) fn run_skills_hub(
    command: HubCommand,
    loaded: &LoadedConfig,
    json: bool,
) -> Result<String, CliError> {
    let data_dir = &loaded.config.storage.data_dir;
    let mut hub = genesis_core::skills_hub::SkillsHub::new(data_dir);

    // Add optional bundled skills source
    let optional_dir = data_dir.join("skills").join("optional");
    hub.add_optional_source(&optional_dir);

    // Load configured taps
    if let Err(e) = hub.load_taps() {
        eprintln!("warning: failed to load taps: {e}");
    }

    match command {
        HubCommand::Browse { page, size, source } => {
            let (manifests, total) = hub
                .browse(page, size, source.as_deref())
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "skills": manifests,
                    "total": total,
                    "page": page,
                    "page_size": size,
                }))?);
            }

            if manifests.is_empty() {
                return Ok("no skills found".to_owned());
            }

            let total_pages = total.div_ceil(size);
            let mut lines = vec![format!(
                "skills hub  (page {page}/{total_pages}, {total} total)"
            )];
            for m in &manifests {
                let tags = if m.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", m.tags.join(", "))
                };
                lines.push(format!(
                    "  {} v{}  {}{}\n    source: {}  author: {}  license: {}",
                    m.name, m.version, m.description, tags, m.source, m.author, m.license
                ));
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Search { query, source, limit } => {
            let results = hub
                .search(&query, source.as_deref(), limit)
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&results)?);
            }

            if results.is_empty() {
                return Ok(format!("no skills matching \"{query}\""));
            }

            let mut lines = vec![format!("found {} skill(s) matching \"{}\"", results.len(), query)];
            for m in &results {
                let tags = if m.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", m.tags.join(", "))
                };
                lines.push(format!(
                    "  {} v{}: {}{}  ({})",
                    m.name, m.version, m.description, tags, m.source
                ));
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Inspect { name } => {
            let (manifest, report) = hub
                .inspect(&name)
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "manifest": manifest,
                    "security": {
                        "verdict": format!("{:?}", report.verdict),
                        "findings": report.findings.len(),
                        "summary": report.summary(),
                    },
                }))?);
            }

            let mut lines = vec![
                format!("skill: {}", manifest.name),
                format!("description: {}", manifest.description),
                format!("version: {}", manifest.version),
                format!("author: {}", manifest.author),
                format!("license: {}", manifest.license),
                format!("source: {}", manifest.source),
            ];
            if !manifest.tags.is_empty() {
                lines.push(format!("tags: {}", manifest.tags.join(", ")));
            }
            lines.push(String::new());
            lines.push("security scan:".to_owned());
            lines.push(format!("  {}", report.summary()));
            for finding in &report.findings {
                let file = finding.file.as_deref().unwrap_or("(unknown)");
                let line = finding
                    .line
                    .map(|l| format!(":{l}"))
                    .unwrap_or_default();
                lines.push(format!(
                    "  [{:?}] {}{}: {} ({})",
                    finding.severity, file, line, finding.description, finding.category
                ));
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Install { name, force } => {
            let (lock, report) = hub
                .install(&name, force)
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "installed": lock,
                    "security": {
                        "verdict": format!("{:?}", report.verdict),
                        "findings": report.findings.len(),
                    },
                }))?);
            }

            let mut lines = vec![format!(
                "installed skill '{}' v{} from {}",
                lock.name, lock.version, lock.source
            )];
            lines.push(format!("  content hash: {}", lock.content_hash));
            lines.push(format!("  security: {}", report.summary()));
            Ok(lines.join("\n"))
        }

        HubCommand::Uninstall { name } => {
            hub.uninstall(&name)
                .map_err(|e| CliError::Other(e.to_string()))?;
            Ok(format!("uninstalled skill '{name}'"))
        }

        HubCommand::Audit => {
            let results = hub
                .audit()
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&results)?);
            }

            if results.is_empty() {
                return Ok("no hub-installed skills to audit".to_owned());
            }

            let mut lines = vec![format!("auditing {} installed skill(s)", results.len())];
            for r in &results {
                let integrity = if r.integrity_ok { "ok" } else { "MODIFIED" };
                lines.push(format!(
                    "  {} — verdict: {}  findings: {}  integrity: {}",
                    r.name, r.verdict, r.finding_count, integrity
                ));
                if !r.integrity_ok {
                    lines.push("    WARNING: content has been modified since installation".to_owned());
                }
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Installed => {
            let installed = hub
                .list_installed()
                .map_err(|e| CliError::Other(e.to_string()))?;

            if json {
                return Ok(serde_json::to_string_pretty(&installed)?);
            }

            if installed.is_empty() {
                return Ok("no hub-installed skills".to_owned());
            }

            let mut lines = vec![format!("{} hub-installed skill(s)", installed.len())];
            for lock in &installed {
                let hash_preview = &lock.hash_value()[..12.min(lock.hash_value().len())];
                lines.push(format!(
                    "  {} v{}  {}  hash: {}...",
                    lock.name, lock.version, lock.source, hash_preview
                ));
            }
            Ok(lines.join("\n"))
        }

        HubCommand::Tap(tap_cmd) => match tap_cmd {
            TapCommand::List => {
                let taps = hub
                    .list_taps()
                    .map_err(|e| CliError::Other(e.to_string()))?;

                if json {
                    return Ok(serde_json::to_string_pretty(&taps)?);
                }

                if taps.is_empty() {
                    return Ok("no taps configured".to_owned());
                }

                let mut lines = vec![format!("{} tap(s) configured", taps.len())];
                for tap in &taps {
                    lines.push(format!("  {tap}"));
                }
                Ok(lines.join("\n"))
            }
            TapCommand::Add { name, repo, path } => {
                let parts: Vec<&str> = repo.splitn(2, '/').collect();
                if parts.len() != 2 {
                    return Err(CliError::Other(
                        "repo must be in 'owner/repo' format (e.g. 'nooesc/genesis-skills')".to_owned(),
                    ));
                }

                let tap = genesis_core::skills_hub::Tap {
                    name: name.clone(),
                    owner: parts[0].to_owned(),
                    repo: parts[1].to_owned(),
                    path,
                };

                hub.add_tap(tap)
                    .map_err(|e| CliError::Other(e.to_string()))?;
                Ok(format!("added tap '{name}'"))
            }
            TapCommand::Remove { name } => {
                let removed = hub
                    .remove_tap(&name)
                    .map_err(|e| CliError::Other(e.to_string()))?;
                if removed {
                    Ok(format!("removed tap '{name}'"))
                } else {
                    Err(CliError::Other(format!("tap '{name}' not found")))
                }
            }
        },
    }
}

pub(crate) fn format_subagent_list(subs: &[genesis_storage::StoredSubagent]) -> String {
    if subs.is_empty() {
        return "no subagents found".to_owned();
    }

    let mut lines = vec!["genesis subagents".to_owned()];
    for sub in subs {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            sub.id, sub.name, sub.status, sub.created_at
        ));
    }
    lines.join("\n")
}

pub(crate) fn format_subagent(sub: &genesis_storage::StoredSubagent) -> String {
    let mut lines = vec![
        format!("subagent: {}", sub.id),
        format!("name: {}", sub.name),
        format!("status: {}", sub.status),
        format!("parent_session: {}", sub.parent_session_id),
        format!("child_session: {}", sub.child_session_id),
        format!("task: {}", sub.task),
        format!("created_at: {}", sub.created_at),
    ];
    if let Some(ref result) = sub.result {
        lines.push(format!("result: {result}"));
    }
    if let Some(ref error) = sub.error {
        lines.push(format!("error: {error}"));
    }
    if let Some(ref completed_at) = sub.completed_at {
        lines.push(format!("completed_at: {completed_at}"));
    }
    lines.join("\n")
}

pub(crate) fn format_created_schedule(schedule: &StoredSchedule) -> String {
    format!(
        "created schedule {}\ncron: {}\ndestination: {}\nprompt: {}\ncreated_at: {}",
        schedule.id,
        schedule.cron_expression,
        schedule.destination,
        schedule.prompt,
        schedule.created_at
    )
}

pub(crate) fn format_schedule_list(schedules: &[StoredSchedule]) -> String {
    if schedules.is_empty() {
        return "no saved schedules".to_owned();
    }

    let mut lines = vec!["genesis schedules".to_owned()];
    for schedule in schedules {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            schedule.id, schedule.destination, schedule.cron_expression, schedule.created_at
        ));
    }
    lines.join("\n")
}

pub(crate) fn format_doctor_report(report: &genesis_core::DoctorReport, ui: &UiContext) -> String {
    let mut lines = vec![
        "genesis doctor".to_owned(),
        format!("profile: {}", report.profile),
        format!("provider: {} / {}", report.provider_backend, report.model),
        format!("config: {}", report.config_path),
        format!("data: {}", report.data_dir),
        format!("database: {}", report.database_path),
    ];

    for check in &report.checks {
        lines.push(format!(
            "- [{}] {}: {}",
            status_marker(&check.status, ui),
            check.name,
            check.detail
        ));
    }

    lines.push(format!(
        "next-event-preview: {}",
        serde_json::to_string(&report.next_event_preview)
            .expect("runtime event preview should always serialize")
    ));

    lines.join("\n")
}

pub(crate) fn status_marker(status: &genesis_core::CheckStatus, ui: &UiContext) -> String {
    match status {
        genesis_core::CheckStatus::Pass => ui.format_success("ok"),
        genesis_core::CheckStatus::Warn => ui.format_warning("warn"),
        genesis_core::CheckStatus::Fail => ui.format_error("FAIL"),
    }
}

pub(crate) fn format_bootstrap_report(report: &genesis_core::DoctorReport, ui: &UiContext) -> String {
    let mut lines = vec![
        "genesis storage bootstrap".to_owned(),
        format!("database: {}", report.database_path),
        format!("provider: {} / {}", report.provider_backend, report.model),
    ];

    for check in &report.checks {
        lines.push(format!(
            "- [{}] {}: {}",
            status_marker(&check.status, ui),
            check.name,
            check.detail
        ));
    }

    lines.join("\n")
}
