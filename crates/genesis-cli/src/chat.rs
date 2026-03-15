use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use genesis_config::{load, LoadedConfig};
use genesis_core::agent_loop::StreamEvent;
use genesis_core::execution::{SessionExecutionService, SessionTurnInput};
use genesis_storage::{bootstrap, SessionStore};
use genesis_types::DeliveryPlatform;
use genesis_ui::UiContext;

use crate::clipboard;
use crate::slash::{SlashCompleter, handle_chat_command};
use crate::{CliError, mcp_startup_strict, is_exit_command};

/// Interactive approval handler for CLI mode. Prompts the user via stdin
/// when a tool requires explicit confirmation (e.g., send_message).
pub(crate) struct CliApprovalHandler;

impl genesis_tools::ApprovalHandler for CliApprovalHandler {
    fn request_approval(&self, tool_name: &str, arguments: &std::collections::BTreeMap<String, String>) -> bool {
        eprintln!("\n[Tool approval required] {tool_name}");
        for (key, value) in arguments {
            let display = match value.char_indices().nth(100) {
                Some((i, _)) => format!("{}...", &value[..i]),
                None => value.clone(),
            };
            eprintln!("  {key}: {display}");
        }
        eprint!("Allow this tool call? [y/N] ");
        let _ = io::stderr().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return false;
        }
        matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
    }
}

pub(crate) async fn build_session_service<'a>(
    loaded: &'a LoadedConfig,
    strict_startup: bool,
    approval_handler: bool,
) -> Result<SessionExecutionService<'a>, CliError> {
    let mut service = SessionExecutionService::with_mcp(loaded, strict_startup).await?;
    if approval_handler {
        service.set_approval_handler(std::sync::Arc::new(CliApprovalHandler));
    }
    Ok(service)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_chat(
    config_path: Option<PathBuf>,
    session_id: Option<String>,
    resume: Option<String>,
    initial_prompt: Option<String>,
    system_override: Option<String>,
    last: bool,
    worktree: bool,
    clipboard: bool,
    ui: &UiContext,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;
    let strict_startup = mcp_startup_strict(&loaded)?;
    let mut service = build_session_service(&loaded, strict_startup, true).await?;
    if let Some(ref sys) = system_override {
        service.set_system_prompt_override(sys.clone());
    }

    // Set up git worktree isolation if requested.
    let _worktree_guard = if worktree {
        let guard = create_worktree()?;
        service.set_default_working_dir(guard.path.clone());
        println!("Working in isolated worktree: {}", guard.path);
        Some(guard)
    } else {
        None
    };

    let store = SessionStore::new(&loaded.config.storage.database_path);

    let (session_id, is_resumed) = if last {
        let sessions = store.list_recent_sessions(1)?;
        match sessions.first() {
            Some(s) => (s.id.clone(), true),
            None => return Err(CliError::Other("no previous sessions found".to_owned())),
        }
    } else {
        match resume {
            Some(resume_id) => {
                let session = store
                    .get_session(&resume_id)?
                    .ok_or_else(|| CliError::SessionNotFound(resume_id.clone()))?;
                (session.id, true)
            }
            None => (session_id.unwrap_or_else(default_session_id), false),
        }
    };

    if !is_resumed {
        service.ensure_session(&session_id, "cli", None)?;
    }

    // Show the Eve banner with animation and session info.
    {
        use genesis_ui::banner::{show_banner, BannerInfo};

        let info = BannerInfo {
            session_id: session_id.clone(),
            model: loaded.config.provider.model.clone(),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "unknown".into()),
            builtin_tools: 60,
            mcp_tools: 0,
        };
        show_banner(ui, &info);
    }

    if is_resumed {
        println!(
            "{}",
            ui.format_metadata(&format!(
                "Resuming session `{session_id}`. Type `exit` or `quit` to leave.",
            ))
        );
    }

    let mut rl = rustyline::Editor::new()
        .map_err(|e| CliError::Other(format!("readline init failed: {e}")))?;
    rl.set_helper(Some(SlashCompleter::new()));

    // Load persistent readline history
    let history_path = loaded.config.storage.data_dir.join("chat_history.txt");
    let _ = rl.load_history(&history_path);

    let model = &loaded.config.provider.model;
    let mut session_id = session_id;

    // Extract clipboard image if --clipboard was passed
    let mut pending_clipboard_images: Vec<genesis_provider::ImageUrl> = Vec::new();
    if clipboard {
        match extract_clipboard_as_image_url(&loaded.config.storage.data_dir) {
            Ok(img) => {
                println!("     [clipboard image attached]");
                pending_clipboard_images.push(img);
            }
            Err(e) => {
                println!("     [clipboard: {e}]");
            }
        }
    } else if clipboard::has_clipboard_image().unwrap_or(false) {
        println!("     [clipboard image detected — use /paste to attach it]");
    }

    // Process initial prompt if provided
    if let Some(initial) = initial_prompt {
        println!("{}{}",  ui.you_prompt(), initial);
        let images = std::mem::take(&mut pending_clipboard_images);
        run_streaming_turn(&service, &session_id, &initial, model, images, ui).await?;
    }

    while let Some(input) = read_multiline_input(&mut rl, "you> ", "  .. ") {
        let trimmed = input.trim();

        if trimmed.is_empty() {
            continue;
        }

        if is_exit_command(trimmed) {
            break;
        }

        // Handle /new — start a fresh session
        if trimmed == "/new" {
            session_id = default_session_id();
            service.ensure_session(&session_id, "cli", None)?;
            println!("Started new session: {session_id}");
            continue;
        }

        // Handle /retry — undo last turn and re-send the user message
        if trimmed == "/retry" {
            let messages = store.load_messages(&session_id).unwrap_or_default();
            match messages.iter().rposition(|m| m.role == "user") {
                Some(idx) => {
                    let prompt_text = messages[idx].content.clone().unwrap_or_default();
                    if prompt_text.is_empty() {
                        println!("Last user message has no text content to retry.");
                    } else {
                        let to_remove = messages.len() - idx;
                        let _ = store.delete_last_n_messages(&session_id, to_remove);
                        println!("Retrying: {prompt_text}");
                        run_streaming_turn(&service, &session_id, &prompt_text, model, Vec::new(), ui).await?;
                    }
                }
                None => println!("No user message to retry."),
            }
            continue;
        }

        // Handle /resume <session_id> — switch to a different session
        if trimmed.starts_with("/resume") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            if parts.len() < 2 || parts[1].trim().is_empty() {
                // No arg — list recent sessions to pick from
                match store.list_recent_sessions(10) {
                    Ok(sessions) if !sessions.is_empty() => {
                        println!("Recent sessions:");
                        for s in &sessions {
                            let title = s.title.as_deref().unwrap_or("(untitled)");
                            println!("  {} — {} ({})", s.id, title, s.platform);
                        }
                        println!("\nUsage: /resume <session_id>");
                    }
                    Ok(_) => println!("No sessions found."),
                    Err(e) => println!("Failed to list sessions: {e}"),
                }
            } else {
                let target_id = parts[1].trim();
                match store.get_session(target_id) {
                    Ok(Some(_)) => {
                        let old_id = session_id.clone();
                        session_id = target_id.to_owned();
                        let msgs = store.load_messages(&session_id).unwrap_or_default();
                        println!("Switched from {old_id} → {session_id} ({} messages)", msgs.len());
                    }
                    Ok(None) => println!("Session '{target_id}' not found."),
                    Err(e) => println!("Error looking up session: {e}"),
                }
            }
            continue;
        }

        // Handle /system — view or set system prompt override
        if trimmed.starts_with("/system") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if arg.is_empty() {
                println!("Current system prompt: (use the default agent prompt)");
                println!("Override with: /system <prompt text>");
                println!("Clear override: /system reset");
            } else if arg == "reset" {
                service.clear_system_prompt_override();
                println!("System prompt override cleared. Using default.");
            } else {
                service.set_system_prompt_override(arg.to_owned());
                println!("System prompt override set. Takes effect on next turn.");
            }
            continue;
        }

        // Handle /personality — list or set personality
        // Handle /template — apply an agent template (personality + system prompt + guidelines)
        if trimmed.starts_with("/template") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if arg.is_empty() {
                let templates = genesis_core::templates::list_templates();
                println!("Available agent templates:");
                for t in templates {
                    println!("  {:12} - {} [personality: {}]", t.name, t.description, t.personality);
                }
                println!("\nApply with: /template <name>");
            } else {
                match genesis_core::templates::get_template(arg) {
                    Some(t) => {
                        let prompt = genesis_core::templates::format_template_prompt(t);
                        service.set_personality_override(t.personality.to_owned());
                        service.set_system_prompt_override(prompt);
                        println!("Applied template '{}' (personality: {}).", t.name, t.personality);
                        println!("Guidelines:");
                        for g in t.guidelines {
                            println!("  - {g}");
                        }
                        println!("Takes effect on next turn.");
                    }
                    None => {
                        let names: Vec<&str> =
                            genesis_core::templates::list_templates()
                                .iter()
                                .map(|t| t.name)
                                .collect();
                        println!("Unknown template '{arg}'. Available: {}", names.join(", "));
                    }
                }
            }
            continue;
        }

        // Handle /workflow — run a YAML-defined multi-step workflow
        if trimmed.starts_with("/workflow") {
            let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
            let sub = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if sub.is_empty() || sub == "help" {
                println!(
                    "Usage:\n\
                     /workflow run <file.yaml> [input]  - Run a workflow from YAML file\n\
                     /workflow validate <file.yaml>     - Validate a workflow definition\n\
                     /workflow show <file.yaml>         - Show workflow steps\n\
                     \n\
                     Example workflow YAML:\n\
                     name: research_pipeline\n\
                     description: Research and summarize a topic\n\
                     steps:\n\
                       - name: research\n\
                         prompt: \"Research: {{{{input}}}}\"\n\
                       - name: summarize\n\
                         prompt: \"Summarize: {{{{research}}}}\"\n\
                         terminal: true"
                );
            } else if sub == "validate" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /workflow validate <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::workflow::parse_workflow(&yaml) {
                            Ok(wf) => {
                                let issues = genesis_core::workflow::validate_workflow(&wf);
                                if issues.is_empty() {
                                    println!("Workflow '{}' is valid ({} steps).", wf.name, wf.steps.len());
                                } else {
                                    println!("Workflow '{}' has {} issue(s):", wf.name, issues.len());
                                    for issue in &issues {
                                        println!("  - {issue}");
                                    }
                                }
                            }
                            Err(e) => println!("Failed to parse workflow: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else if sub == "show" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /workflow show <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::workflow::parse_workflow(&yaml) {
                            Ok(wf) => {
                                println!("Workflow: {} — {}", wf.name, wf.description);
                                for (i, step) in wf.steps.iter().enumerate() {
                                    let model_str = step.model.as_deref().unwrap_or("default");
                                    let terminal_str = if step.terminal { " [terminal]" } else { "" };
                                    println!("  Step {}: {} (model: {}){}", i + 1, step.name, model_str, terminal_str);
                                    // Show truncated prompt
                                    let prompt_preview = if step.prompt.len() > 80 {
                                        format!("{}...", &step.prompt[..80])
                                    } else {
                                        step.prompt.clone()
                                    };
                                    println!("    Prompt: {prompt_preview}");
                                }
                            }
                            Err(e) => println!("Failed to parse workflow: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else if sub == "run" {
                let rest = parts.get(2).map(|s| s.trim()).unwrap_or("");
                let (file, input_text) = rest.split_once(' ').unwrap_or((rest, ""));
                if file.is_empty() {
                    println!("Usage: /workflow run <file.yaml> [input text]");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::workflow::parse_workflow(&yaml) {
                            Ok(wf) => {
                                let issues = genesis_core::workflow::validate_workflow(&wf);
                                if !issues.is_empty() {
                                    println!("Workflow has validation issues:");
                                    for issue in &issues {
                                        println!("  - {issue}");
                                    }
                                    println!("Fix these before running.");
                                } else {
                                    println!("Running workflow '{}' ({} steps)...", wf.name, wf.steps.len());
                                    let wf_session_id = format!("{session_id}__wf__{}", wf.name);
                                    match service.run_workflow(&wf, input_text, &wf_session_id).await {
                                        Ok(result) => {
                                            println!("\nWorkflow '{}' complete!", result.workflow_name);
                                            println!("Steps completed: {}/{}", result.steps_completed(), wf.steps.len());
                                            println!("Total tokens: {} in / {} out", result.total_input_tokens, result.total_output_tokens);
                                            for sr in &result.step_results {
                                                println!("\n--- Step: {} ---", sr.step_name);
                                                // Show first 500 chars of output
                                                let preview = if sr.output.len() > 500 {
                                                    format!("{}...", &sr.output[..500])
                                                } else {
                                                    sr.output.clone()
                                                };
                                                println!("{preview}");
                                            }
                                            println!("\n--- Final Output ---");
                                            println!("{}", result.final_output);
                                        }
                                        Err(e) => println!("Workflow execution failed: {e}"),
                                    }
                                }
                            }
                            Err(e) => println!("Failed to parse workflow: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else {
                println!("Unknown workflow subcommand '{sub}'. Use /workflow help.");
            }
            continue;
        }

        // Handle /eval — run evaluation suites
        if trimmed.starts_with("/eval") {
            let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
            let sub = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if sub.is_empty() || sub == "help" {
                println!(
                    "Usage:\n\
                     /eval run <file.yaml>      - Run an evaluation suite\n\
                     /eval validate <file.yaml>  - Validate a suite definition\n\
                     /eval show <file.yaml>      - Show suite test cases\n\
                     \n\
                     Example eval suite YAML:\n\
                     name: basic_math\n\
                     cases:\n\
                       - id: addition\n\
                         prompt: \"What is 2 + 2?\"\n\
                         criteria:\n\
                           must_contain: [\"4\"]"
                );
            } else if sub == "validate" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /eval validate <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::eval::parse_suite(&yaml) {
                            Ok(suite) => {
                                let issues = genesis_core::eval::validate_suite(&suite);
                                if issues.is_empty() {
                                    println!("Suite '{}' is valid ({} cases).", suite.name, suite.cases.len());
                                } else {
                                    println!("Suite '{}' has {} issue(s):", suite.name, issues.len());
                                    for issue in &issues {
                                        println!("  - {issue}");
                                    }
                                }
                            }
                            Err(e) => println!("Failed to parse suite: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else if sub == "show" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /eval show <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::eval::parse_suite(&yaml) {
                            Ok(suite) => {
                                println!("Suite: {} v{} — {}", suite.name, suite.version, suite.description);
                                println!("Cases: {}", suite.cases.len());
                                for case in &suite.cases {
                                    let criteria_count = case.criteria.must_contain.len()
                                        + case.criteria.must_not_contain.len()
                                        + if case.criteria.exact_match.is_some() { 1 } else { 0 }
                                        + if case.criteria.regex_match.is_some() { 1 } else { 0 };
                                    println!(
                                        "  {} (difficulty: {}, {} criteria, tags: [{}])",
                                        case.id,
                                        case.difficulty,
                                        criteria_count,
                                        case.tags.join(", ")
                                    );
                                    let prompt_preview = if case.prompt.len() > 60 {
                                        format!("{}...", &case.prompt[..60])
                                    } else {
                                        case.prompt.clone()
                                    };
                                    println!("    Prompt: {prompt_preview}");
                                }
                            }
                            Err(e) => println!("Failed to parse suite: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else if sub == "run" {
                let file = parts.get(2).map(|s| s.trim()).unwrap_or("");
                if file.is_empty() {
                    println!("Usage: /eval run <file.yaml>");
                } else {
                    match std::fs::read_to_string(file) {
                        Ok(yaml) => match genesis_core::eval::parse_suite(&yaml) {
                            Ok(suite) => {
                                let issues = genesis_core::eval::validate_suite(&suite);
                                if !issues.is_empty() {
                                    println!("Suite has validation issues:");
                                    for issue in &issues {
                                        println!("  - {issue}");
                                    }
                                    println!("Fix these before running.");
                                } else {
                                    println!("Running eval suite '{}' ({} cases)...", suite.name, suite.cases.len());
                                    match service.run_eval(&suite).await {
                                        Ok(report) => {
                                            println!("\n=== Eval Report: {} v{} ===", report.suite_name, report.suite_version);
                                            println!("Model: {}", report.model);
                                            println!("Duration: {}ms", report.total_duration_ms);
                                            println!("Results: {} passed, {} failed, {} errored ({:.0}% pass rate)",
                                                report.passed, report.failed, report.errored, report.pass_rate * 100.0);
                                            println!("Avg score: {:.2}", report.avg_score);
                                            println!("Tokens: {} in / {} out\n", report.total_input_tokens, report.total_output_tokens);

                                            for r in &report.results {
                                                let status = if r.passed { "PASS" } else if r.error.is_some() { "ERROR" } else { "FAIL" };
                                                println!("[{status}] {} — score: {:.2}, {}ms, {} turns",
                                                    r.case_id, r.score, r.duration_ms, r.turns_used);
                                                for check in &r.checks {
                                                    let mark = if check.passed { "✓" } else { "✗" };
                                                    println!("  {mark} {}: {}", check.criterion, check.detail);
                                                }
                                                if let Some(ref err) = r.error {
                                                    println!("  Error: {err}");
                                                }
                                            }

                                            if !report.tag_results.is_empty() {
                                                println!("\nBy tag:");
                                                for (tag, tr) in &report.tag_results {
                                                    println!("  {tag}: {}/{} passed ({:.0}%)", tr.passed, tr.total, tr.pass_rate * 100.0);
                                                }
                                            }
                                        }
                                        Err(e) => println!("Eval run failed: {e}"),
                                    }
                                }
                            }
                            Err(e) => println!("Failed to parse suite: {e}"),
                        },
                        Err(e) => println!("Failed to read file '{file}': {e}"),
                    }
                }
            } else {
                println!("Unknown eval subcommand '{sub}'. Use /eval help.");
            }
            continue;
        }

        if trimmed.starts_with("/personality") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if arg.is_empty() {
                let all = genesis_core::personality::list_personalities();
                println!("Available personalities:");
                for p in &all {
                    println!("  {:12} - {}", p.name, p.description);
                }
                println!("\nSet with: /personality <name>");
            } else {
                match genesis_core::personality::get_personality(arg) {
                    Some(p) => {
                        service.set_personality_override(p.name.to_owned());
                        println!("Personality set to '{}'. Takes effect on next turn.", p.name);
                    }
                    None => {
                        let names: Vec<&str> =
                            genesis_core::personality::list_personalities()
                                .iter()
                                .map(|p| p.name)
                                .collect();
                        println!("Unknown personality '{arg}'. Available: {}", names.join(", "));
                    }
                }
            }
            continue;
        }

        // Handle /model — show or switch model at runtime
        if trimmed.starts_with("/model") {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if arg.is_empty() {
                println!("Active model: {}", model);
                println!("Set with: /model <backend>/<model>  (e.g. /model anthropic/claude-sonnet-4-20250514)");
            } else if let Some((backend, new_model)) = arg.split_once('/') {
                service.set_model_override(backend.to_owned(), new_model.to_owned());
                println!("Model switched to {backend}/{new_model}. Takes effect on next turn.");
            } else {
                // Assume same backend, just changing model name
                let backend = &loaded.config.provider.backend;
                service.set_model_override(backend.clone(), arg.to_owned());
                println!("Model switched to {backend}/{arg}. Takes effect on next turn.");
            }
            continue;
        }

        // Handle /fork — branch the conversation into a new session
        if trimmed == "/fork" {
            let new_id = default_session_id();
            match store.fork_session(&session_id, &new_id) {
                Ok(_) => {
                    let old_id = session_id.clone();
                    session_id = new_id;
                    println!("Forked from {old_id} → new session: {session_id}");
                }
                Err(e) => println!("Fork failed: {e}"),
            }
            continue;
        }

        // Handle /paste — attach clipboard image to the next message
        if trimmed == "/paste" {
            match extract_clipboard_as_image_url(&loaded.config.storage.data_dir) {
                Ok(img) => {
                    pending_clipboard_images.push(img);
                    println!("     [clipboard image attached — send a message to include it]");
                }
                Err(e) => println!("     [clipboard: {e}]"),
            }
            continue;
        }

        // Handle in-chat slash commands
        if let Some(handled) = handle_chat_command(trimmed, &session_id, &store) {
            println!("{handled}");
            continue;
        }

        let images = std::mem::take(&mut pending_clipboard_images);
        run_streaming_turn(&service, &session_id, trimmed, model, images, ui).await?;
    }

    // Save readline history for next session
    let _ = rl.save_history(&history_path);

    Ok(format!("chat session saved as {session_id}"))
}

/// Run a single prompt non-interactively and return the response.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_oneshot(
    config_path: Option<PathBuf>,
    prompt: &str,
    session_id: Option<String>,
    raw: bool,
    json: bool,
    system_override: Option<String>,
    stream: bool,
    image_paths: &[String],
    ui: &UiContext,
) -> Result<String, CliError> {
    // Support piping: `echo "prompt" | genesis run -`
    let prompt = if prompt == "-" {
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map_err(|e| CliError::Other(format!("stdin read error: {e}")))?;
        buf.trim().to_owned()
    } else {
        prompt.to_owned()
    };

    // Resolve image paths/URLs to ImageUrl objects
    let images = resolve_image_inputs(image_paths)?;

    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;
    let strict_startup = mcp_startup_strict(&loaded)?;
    let mut service = build_session_service(&loaded, strict_startup, true).await?;
    if let Some(sys) = system_override {
        service.set_system_prompt_override(sys);
    }

    let session_id = session_id.unwrap_or_else(default_session_id);
    service.ensure_session(&session_id, "cli", None)?;

    if stream && !json {
        // Streaming mode — print output as it arrives
        run_streaming_turn(&service, &session_id, &prompt, &loaded.config.provider.model, images, ui).await?;
        return Ok(String::new());
    }

    let outcome = service
        .run_turn(SessionTurnInput {
            session_id: &session_id,
            session_platform: "cli",
            delivery_platform: DeliveryPlatform::Cli,
            prompt: &prompt,
            title: None,
            images,
        })
        .await?;

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "session_id": outcome.session_id,
            "response": outcome.result.response,
            "turns_used": outcome.result.turns_used,
            "tool_calls_made": outcome.result.tool_calls_made,
            "total_input_tokens": outcome.result.total_input_tokens,
            "total_output_tokens": outcome.result.total_output_tokens,
        }))?);
    }

    if raw {
        Ok(outcome.result.response)
    } else {
        let mut output = outcome.result.response.clone();
        let r = &outcome.result;
        if r.total_input_tokens > 0 || r.total_output_tokens > 0 {
            let cost_str = match r.estimated_cost {
                Some(c) if c > 0.0 => {
                    if c < 0.01 {
                        format!(", ~${c:.4}")
                    } else {
                        format!(", ~${c:.2}")
                    }
                }
                _ => genesis_provider::pricing::estimate_cost(
                    &loaded.config.provider.model,
                    r.total_input_tokens,
                    r.total_output_tokens,
                )
                .map(|c| format!(", ~{c}"))
                .unwrap_or_default(),
            };
            output.push_str(&format!(
                "\n\n[{} in / {} out tokens, {} turns, {} tool calls{cost_str}]",
                r.total_input_tokens, r.total_output_tokens, r.turns_used, r.tool_calls_made
            ));
        }
        Ok(output)
    }
}

pub(crate) fn read_multiline_input(
    rl: &mut rustyline::Editor<SlashCompleter, rustyline::history::DefaultHistory>,
    prompt: &str,
    continuation: &str,
) -> Option<String> {
    let first = read_user_input(rl, prompt)?;
    if !first.ends_with('\\') {
        return Some(first);
    }

    let mut buf = String::new();
    buf.push_str(first.trim_end_matches('\\'));
    buf.push('\n');

    loop {
        let line = read_user_input(rl, continuation)?;
        if line.ends_with('\\') {
            buf.push_str(line.trim_end_matches('\\'));
            buf.push('\n');
        } else {
            buf.push_str(&line);
            break;
        }
    }

    // Add the full multi-line input as a single history entry
    if !buf.trim().is_empty() {
        let _ = rl.add_history_entry(&buf);
    }

    Some(buf)
}

pub(crate) fn read_user_input(rl: &mut rustyline::Editor<SlashCompleter, rustyline::history::DefaultHistory>, prompt: &str) -> Option<String> {
    match rl.readline(prompt) {
        Ok(line) => {
            if !line.trim().is_empty() {
                let _ = rl.add_history_entry(&line);
            }
            Some(line)
        }
        Err(rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof) => {
            None
        }
        Err(_) => None,
    }
}

/// Resolve image inputs (file paths or URLs) to ImageUrl objects.
///
/// Supports:
/// - URLs (http/https) — passed through directly
pub(crate) fn resolve_image_inputs(inputs: &[String]) -> Result<Vec<genesis_provider::ImageUrl>, CliError> {
    use base64::Engine;

    let mut images = Vec::new();
    for input in inputs {
        if input.starts_with("http://") || input.starts_with("https://") {
            images.push(genesis_provider::ImageUrl {
                url: input.clone(),
                detail: None,
            });
        } else {
            // Local file path — read and encode as data URI
            let path = std::path::Path::new(input);
            let data = std::fs::read(path)
                .map_err(|e| CliError::Other(format!("failed to read image {input}: {e}")))?;

            let mime = match path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|s| s.to_lowercase())
                .as_deref()
            {
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("gif") => "image/gif",
                Some("webp") => "image/webp",
                Some("svg") => "image/svg+xml",
                _ => "image/png", // default
            };

            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
            images.push(genesis_provider::ImageUrl {
                url: format!("data:{mime};base64,{encoded}"),
                detail: None,
            });
        }
    }
    Ok(images)
}

/// Extract the clipboard image, save it to a temp file under `data_dir`, and
/// return an [`ImageUrl`] with the base64-encoded data URI.
pub(crate) fn extract_clipboard_as_image_url(
    data_dir: &Path,
) -> Result<genesis_provider::ImageUrl, clipboard::ClipboardError> {
    use base64::Engine;

    let clip_dir = data_dir.join("clipboard");
    std::fs::create_dir_all(&clip_dir).map_err(|e| {
        clipboard::ClipboardError::ExtractionFailed(format!(
            "failed to create clipboard dir: {e}"
        ))
    })?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let dest = clip_dir.join(format!("clip-{timestamp}.png"));

    clipboard::save_clipboard_image(&dest)?;

    let data = std::fs::read(&dest).map_err(clipboard::ClipboardError::Io)?;
    // Clean up the temp file — we only need the base64 data
    let _ = std::fs::remove_file(&dest);

    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
    Ok(genesis_provider::ImageUrl {
        url: format!("data:image/png;base64,{encoded}"),
        detail: None,
    })
}

pub(crate) fn default_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("cli-{timestamp}")
}

/// Guard that cleans up a git worktree when dropped.
pub(crate) struct WorktreeGuard {
    path: String,
    branch: String,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        // Check for uncommitted changes before cleaning up
        let has_changes = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.path)
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(true);

        if has_changes {
            eprintln!(
                "Worktree has uncommitted changes, keeping at: {}\n\
                 Branch: {}\n\
                 To clean up: git worktree remove {}",
                self.path, self.branch, self.path
            );
        } else {
            // Remove the worktree
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", &self.path])
                .output();
            // Delete the temporary branch
            let _ = std::process::Command::new("git")
                .args(["branch", "-d", &self.branch])
                .output();
        }
    }
}


pub(crate) fn create_worktree() -> Result<WorktreeGuard, CliError> {
    // Verify we're in a git repo
    let toplevel = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| CliError::Other(format!("git not found: {e}")))?;
    if !toplevel.status.success() {
        return Err(CliError::Other(
            "--worktree requires a git repository".to_owned(),
        ));
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let branch_name = format!("genesis-worktree-{timestamp}");

    // Create a temp directory for the worktree
    let repo_root = String::from_utf8_lossy(&toplevel.stdout).trim().to_owned();
    let worktree_path = format!("{repo_root}/.git/genesis-worktrees/{branch_name}");

    // Ensure the worktrees directory exists
    let _ = std::fs::create_dir_all(format!("{repo_root}/.git/genesis-worktrees"));

    let output = std::process::Command::new("git")
        .args(["worktree", "add", "-b", &branch_name, &worktree_path])
        .output()
        .map_err(|e| CliError::Other(format!("failed to create worktree: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::Other(format!(
            "git worktree add failed: {stderr}"
        )));
    }

    Ok(WorktreeGuard {
        path: worktree_path,
        branch: branch_name,
    })
}

pub(crate) async fn run_streaming_turn(
    service: &SessionExecutionService<'_>,
    session_id: &str,
    prompt: &str,
    model: &str,
    images: Vec<genesis_provider::ImageUrl>,
    ui: &UiContext,
) -> Result<(), CliError> {
    use std::sync::atomic::{AtomicBool, Ordering};

    let eve_prompt = ui.eve_prompt();
    let streamed = AtomicBool::new(false);
    let turn_future = service.run_turn_streaming(
        SessionTurnInput {
            session_id,
            session_platform: "cli",
            delivery_platform: DeliveryPlatform::Cli,
            prompt,
            title: None,
            images,
        },
        |event| match event {
            StreamEvent::Chunk(chunk) => {
                if !streamed.swap(true, Ordering::Relaxed) {
                    print!("{eve_prompt}");
                }
                print!("{chunk}");
                let _ = io::stdout().flush();
            }
            StreamEvent::ToolCallStart { name } => {
                if streamed.load(Ordering::Relaxed) {
                    println!();
                }
                println!("{}", ui.format_metadata(&format!("     [calling {name}...]")));
                streamed.store(false, Ordering::Relaxed);
            }
            StreamEvent::ToolCallEnd { .. } => {}
            StreamEvent::ClarificationNeeded { question } => {
                println!("\n{}{question}", ui.eve_prompt());
            }
        },
    );

    tokio::select! {
        result = turn_future => {
            let outcome = result?;
            if outcome.result.pending_clarification.is_some() {
                // Clarification was already printed via stream event
            } else if streamed.load(Ordering::Relaxed) {
                println!();
            } else {
                println!("{}{}", ui.eve_prompt(), outcome.result.response);
            }
            let r = &outcome.result;
            if r.total_input_tokens > 0 || r.total_output_tokens > 0 {
                let cost_str = match r.estimated_cost {
                    Some(c) if c > 0.0 => {
                        if c < 0.01 {
                            format!(", ~${c:.4}")
                        } else {
                            format!(", ~${c:.2}")
                        }
                    }
                    _ => genesis_provider::pricing::estimate_cost(
                        model,
                        r.total_input_tokens,
                        r.total_output_tokens,
                    )
                    .map(|c| format!(", ~{c}"))
                    .unwrap_or_default(),
                };
                println!(
                    "{}",
                    ui.format_metadata(&format!(
                        "     [{} in / {} out tokens, {} turns, {} tool calls{cost_str}]",
                        r.total_input_tokens, r.total_output_tokens, r.turns_used, r.tool_calls_made
                    ))
                );
            }
        }
        _ = tokio::signal::ctrl_c() => {
            if streamed.load(Ordering::Relaxed) {
                println!();
            }
            println!("{}", ui.format_warning("     [interrupted]"));
        }
    }

    Ok(())
}
