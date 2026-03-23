use std::io::{self, Write};

use genesis_storage::{MemoryStore, SessionStore};
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;

use crate::commands::model::estimate_token_cost;
use crate::format::{export_session_markdown, format_session_messages};

/// Slash-command completer for the interactive chat TUI.
/// Provides tab-completion and inline hints for `/` commands.
pub(crate) struct SlashCompleter {
    commands: Vec<&'static str>,
}

impl SlashCompleter {
    pub(crate) fn new() -> Self {
        Self {
            commands: vec![
                "/help",
                "/history",
                "/export",
                "/tokens",
                "/session",
                "/new",
                "/undo",
                "/retry",
                "/fork",
                "/resume",
                "/search",
                "/memories",
                "/compress",
                "/tools",
                "/skills",
                "/model",
                "/personality",
                "/system",
                "/cache",
                "/stats",
                "/tag",
                "/title",
                "/tree",
                "/audit",
                "/analytics",
                "/template",
                "/workflow",
                "/bus",
                "/eval",
                "/clear",
                "/paste",
            ],
        }
    }
}

impl Completer for SlashCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if line.starts_with('/') && !line[1..].contains(' ') {
            let matches: Vec<Pair> = self
                .commands
                .iter()
                .filter(|cmd| cmd.starts_with(line))
                .map(|cmd| Pair {
                    display: cmd.to_string(),
                    replacement: cmd.to_string(),
                })
                .collect();
            Ok((0, matches))
        } else {
            Ok((pos, vec![]))
        }
    }
}

impl Hinter for SlashCompleter {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &rustyline::Context<'_>) -> Option<String> {
        if line.starts_with('/') && !line[1..].contains(' ') && pos == line.len() {
            self.commands
                .iter()
                .find(|cmd| cmd.starts_with(line) && cmd.len() > line.len())
                .map(|cmd| cmd[line.len()..].to_owned())
        } else {
            None
        }
    }
}

impl Highlighter for SlashCompleter {}
impl Validator for SlashCompleter {}
impl rustyline::Helper for SlashCompleter {}

pub(crate) fn handle_chat_command(
    input: &str,
    session_id: &str,
    store: &SessionStore,
) -> Option<String> {
    let cmd = input.strip_prefix('/')?;
    let (name, _args) = cmd.split_once(' ').unwrap_or((cmd, ""));

    match name {
        "help" | "h" => Some(
            "/help         - Show this help\n\
             /history      - Show recent conversation history\n\
             /export       - Export session as Markdown\n\
             /tokens       - Show session token usage\n\
             /session      - Show current session ID\n\
             /new          - Start a new session\n\
             /undo         - Undo last turn (remove last user-assistant exchange)\n\
             /retry        - Undo last turn and re-send the user message\n\
             /fork         - Branch conversation into a new session\n\
             /resume [id]  - Switch to another session (lists recent if no ID)\n\
             /search <q>   - Search past sessions for a query\n\
             /memories     - Show stored memories\n\
             /compress     - Trim old messages, keeping recent context\n\
             /tools        - List available tools\n\
             /skills       - List saved skills\n\
             /model [spec] - Show or switch model (e.g. /model anthropic/claude-sonnet-4-20250514)\n\
             /system [txt] - View or set system prompt override (reset to clear)\n\
             /personality  - List or set personality (e.g. /personality pirate)\n\
             /title [text] - View or set session title\n\
             /tree         - Show conversation branch tree\n\
             /tag [name]   - View, add, or remove (-name) session tags\n\
             /stats        - Show session statistics\n\
             /cache        - Show cache stats (clear, prune)\n\
             /audit        - View audit log (stats, purge <days>)\n\
             /analytics    - Tool and LLM usage analytics\n\
             /template     - Apply an agent template (archetype)\n\
             /workflow     - Run a YAML-defined multi-step workflow\n\
             /bus          - Agent message bus (stats, history)\n\
             /eval         - Run evaluation suites against the agent\n\
             /clear        - Clear the screen\n\
             Use \\ at end of line for multi-line input"
                .to_owned(),
        ),
        "history" => {
            let messages = match store.load_messages(session_id) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load messages for /history");
                    return Some(format!("Error loading messages: {e}"));
                }
            };
            let recent = if messages.len() > 10 {
                &messages[messages.len() - 10..]
            } else {
                &messages
            };
            Some(format_session_messages(session_id, recent))
        }
        "export" => {
            let messages = match store.load_messages(session_id) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load messages for /export");
                    return Some(format!("Error loading messages: {e}"));
                }
            };
            Some(export_session_markdown(session_id, &messages))
        }
        "tokens" | "usage" => {
            let session = match store.get_session(session_id) {
                Ok(Some(s)) => s,
                Ok(None) => return Some("Session not found.".to_owned()),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load session for /tokens");
                    return Some(format!("Error loading session: {e}"));
                }
            };
            let total = session.total_input_tokens + session.total_output_tokens;
            // Rough cost estimate based on common pricing
            let cost_estimate = estimate_token_cost(
                session.total_input_tokens as u32,
                session.total_output_tokens as u32,
            );
            let mut output = format!(
                "Session: {}\nInput tokens:  {}\nOutput tokens: {}\nTotal tokens:  {}",
                session_id, session.total_input_tokens, session.total_output_tokens, total
            );
            if let Some((cost, note)) = cost_estimate {
                output.push_str(&format!("\nEst. cost:     ${:.4} ({note})", cost));
            }
            Some(output)
        }
        "session" => Some(format!("Current session: {session_id}")),
        "compress" => {
            let messages = match store.load_messages(session_id) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load messages for /compress");
                    return Some(format!("Error loading messages: {e}"));
                }
            };
            let total = messages.len();
            if total <= 10 {
                return Some(format!(
                    "Session has only {total} message(s). Nothing to compress."
                ));
            }
            // Keep the 10 most recent messages
            let keep = 10;
            match store.truncate_messages(session_id, keep) {
                Ok(deleted) => {
                    // Inject a summary note as the first message
                    let _ = store.append_message(
                        session_id,
                        "system",
                        Some(&format!(
                            "[Context compressed] {deleted} older messages were removed. \
                             {keep} recent messages retained."
                        )),
                        None,
                        None,
                        None,
                    );
                    Some(format!(
                        "Compressed: removed {deleted} old messages, kept {keep} recent."
                    ))
                }
                Err(e) => Some(format!("Compression failed: {e}")),
            }
        }
        "search" => {
            let query = _args.trim();
            if query.is_empty() {
                return Some("Usage: /search <query>".to_owned());
            }
            match store.search_sessions(query) {
                Ok(results) if results.is_empty() => {
                    Some(format!("No sessions found matching '{query}'."))
                }
                Ok(results) => {
                    let mut lines = vec![format!("{} session(s) matching '{query}':", results.len())];
                    for s in results.iter().take(10) {
                        let title = s.title.as_deref().unwrap_or("(untitled)");
                        lines.push(format!("  {} - {} [{}]", s.id, title, s.updated_at));
                    }
                    if results.len() > 10 {
                        lines.push(format!("  ... and {} more", results.len() - 10));
                    }
                    Some(lines.join("\n"))
                }
                Err(_) => Some(format!("No sessions found matching '{query}'.")),
            }
        }
        "memories" => {
            let memory_store = MemoryStore::new(store.database_path());
            let list_result: Result<Vec<genesis_storage::StoredMemory>, _> = memory_store.list(20);
            match list_result {
                Ok(memories) if memories.is_empty() => {
                    Some("No stored memories.".to_owned())
                }
                Ok(memories) => {
                    let mut lines = vec![format!("{} memory/memories:", memories.len())];
                    for m in &memories {
                        lines.push(format!("  [{}] {}", m.kind, m.content));
                    }
                    Some(lines.join("\n"))
                }
                Err(_) => Some("No stored memories.".to_owned()),
            }
        }
        "tree" => {
            // Show the conversation tree rooted at the current session
            let _session = match store.get_session(session_id) {
                Ok(Some(s)) => s,
                Ok(None) => return Some("Session not found.".to_owned()),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load session for /tree");
                    return Some(format!("Error loading session: {e}"));
                }
            };
            // Walk up to the root
            let mut root_id = session_id.to_owned();
            let mut visited = std::collections::HashSet::new();
            while let Ok(Some(s)) = store.get_session(&root_id) {
                if let Some(parent) = s.parent_session_id {
                    if visited.contains(&parent) {
                        break;
                    }
                    visited.insert(root_id.clone());
                    root_id = parent;
                } else {
                    break;
                }
            }
            // Print tree from root
            fn print_tree(
                store: &SessionStore,
                id: &str,
                current: &str,
                prefix: &str,
                is_last: bool,
                lines: &mut Vec<String>,
            ) {
                let connector = if prefix.is_empty() { "" } else if is_last { "└── " } else { "├── " };
                let title = match store.get_session(id) {
                    Ok(s) => s.and_then(|s| s.title).unwrap_or_else(|| "(untitled)".to_owned()),
                    Err(_) => "(untitled)".to_owned(),
                };
                let marker = if id == current { " ← you" } else { "" };
                lines.push(format!("{prefix}{connector}{id} — {title}{marker}"));
                if let Ok(children) = store.list_children(id) {
                    let child_prefix = if prefix.is_empty() {
                        "".to_owned()
                    } else if is_last {
                        format!("{prefix}    ")
                    } else {
                        format!("{prefix}│   ")
                    };
                    for (i, child) in children.iter().enumerate() {
                        let last = i == children.len() - 1;
                        print_tree(store, &child.id, current, &child_prefix, last, lines);
                    }
                }
            }
            let mut lines = Vec::new();
            print_tree(store, &root_id, session_id, "", false, &mut lines);
            if lines.len() <= 1 {
                Some("No conversation branches. Use /fork to create one.".to_owned())
            } else {
                Some(lines.join("\n"))
            }
        }
        "title" => {
            let arg = _args.trim();
            if arg.is_empty() {
                match store.get_session(session_id) {
                    Ok(Some(s)) => Some(format!(
                        "Title: {}",
                        s.title.as_deref().unwrap_or("(untitled)")
                    )),
                    _ => Some("Could not load session.".to_owned()),
                }
            } else {
                match store.update_title(session_id, arg) {
                    Ok(_) => Some(format!("Title set to: {arg}")),
                    Err(e) => Some(format!("Failed to set title: {e}")),
                }
            }
        }
        "tag" => {
            let arg = _args.trim();
            if arg.is_empty() {
                match store.get_tags(session_id) {
                    Ok(tags) if tags.is_empty() => Some("No tags on this session. Add with: /tag <name>".to_owned()),
                    Ok(tags) => Some(format!("Tags: {}\nRemove with: /tag -<name>", tags.join(", "))),
                    Err(e) => Some(format!("Failed to read tags: {e}")),
                }
            } else if let Some(to_remove) = arg.strip_prefix('-') {
                match store.remove_tag(session_id, to_remove.trim()) {
                    Ok(true) => Some(format!("Removed tag '{}'.", to_remove.trim())),
                    Ok(false) => Some(format!("Tag '{}' not found.", to_remove.trim())),
                    Err(e) => Some(format!("Failed to remove tag: {e}")),
                }
            } else {
                match store.add_tag(session_id, arg) {
                    Ok(_) => Some(format!("Added tag '{arg}'.")),
                    Err(e) => Some(format!("Failed to add tag: {e}")),
                }
            }
        }
        "stats" => {
            let messages = match store.load_messages(session_id) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load messages for /stats");
                    return Some(format!("Error loading messages: {e}"));
                }
            };
            let session = match store.get_session(session_id) {
                Ok(Some(s)) => s,
                Ok(None) => return Some("Session not found.".to_owned()),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load session for /stats");
                    return Some(format!("Error loading session: {e}"));
                }
            };
            let user_msgs = messages.iter().filter(|m| m.role == "user").count();
            let assistant_msgs = messages.iter().filter(|m| m.role == "assistant").count();
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            let system_msgs = messages.iter().filter(|m| m.role == "system").count();
            let total_chars: usize = messages
                .iter()
                .filter_map(|m| m.content.as_ref())
                .map(|c| c.len())
                .sum();
            let total_tokens = session.total_input_tokens + session.total_output_tokens;
            let title = session.title.as_deref().unwrap_or("(untitled)");
            let mut lines = vec![
                format!("Session: {} — {}", session_id, title),
                format!("Platform: {}", session.platform),
                format!("Messages: {} total ({} user, {} assistant, {} tool, {} system)",
                    messages.len(), user_msgs, assistant_msgs, tool_msgs, system_msgs),
                format!("Characters: {}", total_chars),
                format!("Tokens: {} ({} in, {} out)",
                    total_tokens, session.total_input_tokens, session.total_output_tokens),
            ];
            if let Some(ref parent) = session.parent_session_id {
                lines.push(format!("Forked from: {parent}"));
            }
            lines.push(format!("Created: {}", session.created_at));
            lines.push(format!("Updated: {}", session.updated_at));
            Some(lines.join("\n"))
        }
        "cache" => {
            let cache_store = genesis_storage::ResponseCacheStore::new(store.database_path());
            let sub = _args.trim();
            if sub == "clear" {
                match cache_store.clear() {
                    Ok(n) => Some(format!("Cache cleared ({n} entries removed).")),
                    Err(e) => Some(format!("Failed to clear cache: {e}")),
                }
            } else if sub == "prune" {
                match cache_store.prune_expired() {
                    Ok(n) => Some(format!("Pruned {n} expired cache entries.")),
                    Err(e) => Some(format!("Failed to prune cache: {e}")),
                }
            } else {
                match cache_store.stats() {
                    Ok((entries, hits)) => Some(format!(
                        "Response cache: {entries} entries, {hits} total hits\n\
                         Commands: /cache clear, /cache prune"
                    )),
                    Err(e) => Some(format!("Cache stats unavailable: {e}")),
                }
            }
        }
        "analytics" => {
            let audit_store = genesis_storage::AuditLogStore::new(store.database_path());
            let sub = _args.trim();
            let days: u32 = if sub == "llm" || sub == "tools" { 30 } else { sub.parse().unwrap_or(30) };
            let is_llm = sub == "llm";

            if is_llm {
                match audit_store.llm_analytics(days) {
                    Ok(analytics) => {
                        if analytics.is_empty() {
                            Some("No LLM analytics data yet. Analytics are populated from audit logs.".into())
                        } else {
                            let mut lines = vec![format!("LLM usage (last {days} days):")];
                            for a in &analytics {
                                let total = a.total_input_tokens + a.total_output_tokens;
                                lines.push(format!(
                                    "  {} - {} calls, {} tokens ({} in / {} out)",
                                    a.model, a.call_count, total, a.total_input_tokens, a.total_output_tokens
                                ));
                            }
                            Some(lines.join("\n"))
                        }
                    }
                    Err(e) => Some(format!("LLM analytics unavailable: {e}")),
                }
            } else {
                match audit_store.tool_analytics(days) {
                    Ok(analytics) => {
                        if analytics.is_empty() {
                            Some("No tool analytics data yet. Analytics are populated from audit logs.".into())
                        } else {
                            let mut lines = vec![format!("Tool usage (last {days} days):")];
                            for a in &analytics {
                                let success_rate = if a.call_count > 0 {
                                    (a.success_count as f64 / a.call_count as f64) * 100.0
                                } else { 0.0 };
                                lines.push(format!(
                                    "  {:20} {:4} calls  {:.0}% success  {:.0}ms avg",
                                    a.tool_name, a.call_count, success_rate, a.avg_duration_ms
                                ));
                            }
                            lines.push("\nCommands: /analytics, /analytics llm".into());
                            Some(lines.join("\n"))
                        }
                    }
                    Err(e) => Some(format!("Tool analytics unavailable: {e}")),
                }
            }
        }
        "audit" => {
            let audit_store = genesis_storage::AuditLogStore::new(store.database_path());
            let sub = _args.trim();
            if sub == "stats" {
                match audit_store.stats() {
                    Ok(stats) => {
                        let total: i64 = stats.iter().map(|(_, c)| c).sum();
                        let mut lines = vec![format!("Audit log: {total} total entries")];
                        for (event_type, count) in &stats {
                            lines.push(format!("  {event_type}: {count}"));
                        }
                        lines.push("Commands: /audit, /audit stats, /audit purge <days>".into());
                        Some(lines.join("\n"))
                    }
                    Err(e) => Some(format!("Audit stats unavailable: {e}")),
                }
            } else if sub.starts_with("purge") {
                let days: u32 = sub.strip_prefix("purge").unwrap_or("90").trim().parse().unwrap_or(90);
                match audit_store.purge_older_than(days) {
                    Ok(n) => Some(format!("Purged {n} audit entries older than {days} days.")),
                    Err(e) => Some(format!("Audit purge failed: {e}")),
                }
            } else {
                // Show recent entries (default: 20)
                let limit: usize = sub.parse().unwrap_or(20);
                match audit_store.recent(limit) {
                    Ok(entries) => {
                        if entries.is_empty() {
                            Some("No audit log entries.".into())
                        } else {
                            let mut lines = vec![format!("Recent audit entries (showing {}):", entries.len())];
                            for entry in &entries {
                                let session = entry.session_id.as_deref().unwrap_or("-");
                                lines.push(format!(
                                    "  [{}] {} session={} {}",
                                    entry.created_at, entry.event_type, session,
                                    entry.details
                                ));
                            }
                            Some(lines.join("\n"))
                        }
                    }
                    Err(e) => Some(format!("Audit log unavailable: {e}")),
                }
            }
        }
        "bus" => {
            let bus_store = genesis_storage::AgentBusStore::new(store.database_path());
            if let Err(e) = bus_store.ensure_table() {
                return Some(format!("Agent bus unavailable: {e}"));
            }
            let sub = _args.trim();
            if sub == "stats" {
                match bus_store.channel_stats() {
                    Ok(stats) => {
                        if stats.is_empty() {
                            Some("No agent bus messages yet.".into())
                        } else {
                            let total: i64 = stats.iter().map(|(_, c)| c).sum();
                            let mut lines = vec![format!("Agent bus: {total} total messages")];
                            for (channel, count) in &stats {
                                lines.push(format!("  {channel}: {count} messages"));
                            }
                            lines.push("Commands: /bus stats, /bus history <channel>".into());
                            Some(lines.join("\n"))
                        }
                    }
                    Err(e) => Some(format!("Bus stats unavailable: {e}")),
                }
            } else if sub.starts_with("history") {
                let channel = sub.strip_prefix("history").unwrap_or("").trim();
                if channel.is_empty() {
                    Some("Usage: /bus history <channel>".into())
                } else {
                    match bus_store.channel_messages(channel, 20) {
                        Ok(messages) => {
                            if messages.is_empty() {
                                Some(format!("No messages on channel '{channel}'."))
                            } else {
                                let mut lines = vec![format!("Channel '{channel}' ({} messages):", messages.len())];
                                for msg in &messages {
                                    lines.push(format!(
                                        "  [{}] {} ({:?}): {}",
                                        msg.timestamp, msg.sender, msg.kind,
                                        if msg.payload.len() > 100 {
                                            format!("{}...", &msg.payload[..100])
                                        } else {
                                            msg.payload.clone()
                                        }
                                    ));
                                }
                                Some(lines.join("\n"))
                            }
                        }
                        Err(e) => Some(format!("Bus history unavailable: {e}")),
                    }
                }
            } else {
                match bus_store.channel_stats() {
                    Ok(stats) => {
                        let total: i64 = stats.iter().map(|(_, c)| c).sum();
                        let mut lines = vec![format!("Agent bus: {total} total messages, {} channels", stats.len())];
                        for (channel, count) in &stats {
                            lines.push(format!("  {channel}: {count} messages"));
                        }
                        lines.push("\nCommands: /bus stats, /bus history <channel>".into());
                        Some(lines.join("\n"))
                    }
                    Err(e) => Some(format!("Bus info unavailable: {e}")),
                }
            }
        }
        "clear" => {
            // ANSI clear screen
            print!("\x1b[2J\x1b[H");
            let _ = io::stdout().flush();
            Some(String::new())
        }
        "tools" => {
            let registry = genesis_tools::default_registry();
            let defs = registry.definitions();
            let mut lines: Vec<String> = defs
                .iter()
                .map(|d| format!("  {} - {}", d.name, d.description))
                .collect();
            lines.sort();
            Some(format!("Available tools ({}):\n{}", defs.len(), lines.join("\n")))
        }
        "skills" => {
            Some("Use `genesis skills list` to see saved skills.".to_owned())
        }
        "model" => None, // handled in chat loop for mutable service access
        "undo" => {
            // Remove the last user-assistant exchange (user message + all
            // subsequent assistant/tool messages until the next user or start).
            let messages = match store.load_messages(session_id) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load messages for /undo");
                    return Some(format!("Error loading messages: {e}"));
                }
            };
            if messages.is_empty() {
                return Some("Nothing to undo.".to_owned());
            }
            // Walk backwards to find the last user message, then count all
            // messages from it to the end — that's the "turn" to remove.
            let mut last_user_idx = None;
            for (i, msg) in messages.iter().enumerate().rev() {
                if msg.role == "user" {
                    last_user_idx = Some(i);
                    break;
                }
            }
            let idx = match last_user_idx {
                Some(i) => i,
                None => return Some("No user messages to undo.".to_owned()),
            };
            let to_remove = messages.len() - idx;
            match store.delete_last_n_messages(session_id, to_remove) {
                Ok(n) => Some(format!("Undid last turn ({n} messages removed).")),
                Err(e) => Some(format!("Undo failed: {e}")),
            }
        }
        _ => Some(format!("Unknown command: /{name}. Type /help for available commands.")),
    }
}
