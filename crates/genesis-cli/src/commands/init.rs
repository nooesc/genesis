use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use genesis_config::load;
use genesis_storage::bootstrap;

use crate::commands::model::{detect_codex_model, known_models};
use crate::CliError;

pub(crate) async fn run_login(config_path: Option<PathBuf>) -> Result<String, CliError> {
    use genesis_config::{update_provider_in_file, AppPaths};

    let auth_path = genesis_auth::default_auth_path()?;
    let paths = AppPaths::resolve(config_path.as_deref())?;

    // Check for existing valid credentials
    if let Ok(existing_store) = genesis_auth::store::read_store(&auth_path) {
        if genesis_auth::store::get_codex_state(&existing_store).is_some() {
            eprintln!("  Existing Codex credentials found in auth store.");
            eprint!("  Use existing credentials? [Y/n]: ");

            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .map_err(CliError::Io)?;
            let input = input.trim().to_lowercase();

            if input.is_empty() || input == "y" || input == "yes" {
                update_provider_in_file(
                    &paths.config_path,
                    Some("openai-codex"),
                    None,
                    Some(Some(genesis_auth::provider::CODEX_INFERENCE_URL)),
                    None,
                )?;
                return Ok(format!(
                    "\n  Login successful!\n  Config updated: {} (backend=openai-codex)",
                    paths.config_path.display()
                ));
            }
        }
    }

    // Check for Codex CLI migration
    if let Some(cli_tokens) = genesis_auth::codex::import_codex_cli_tokens() {
        eprintln!("  Found existing Codex CLI credentials (~/.codex/auth.json)");
        eprintln!("  Genesis will create its own independent session.");
        eprint!("  Import these credentials? [y/N]: ");

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(CliError::Io)?;
        let input = input.trim().to_lowercase();

        if input == "y" || input == "yes" {
            genesis_auth::store::save_codex_tokens(
                &auth_path,
                cli_tokens,
                genesis_auth::store::CredentialSource::CodexMigration,
            )?;
            update_provider_in_file(
                &paths.config_path,
                Some("openai-codex"),
                None,
                Some(Some(genesis_auth::provider::CODEX_INFERENCE_URL)),
                None,
            )?;
            return Ok(format!(
                "\n  Credentials imported!\n  Config updated: {} (backend=openai-codex)\n\n  Note: Genesis maintains its own session — won't affect Codex CLI.",
                paths.config_path.display()
            ));
        }
    }

    // Run device code flow
    eprintln!();
    eprintln!("  Signing in to OpenAI Codex...");
    eprintln!("  (Genesis creates its own session — won't affect Codex CLI or VS Code)");

    let creds = genesis_auth::codex::login(&auth_path).await?;

    update_provider_in_file(
        &paths.config_path,
        Some("openai-codex"),
        None,
        Some(Some(&creds.base_url)),
        None,
    )?;

    Ok(format!(
        "\n  Login successful!\n  Auth state: {}\n  Config updated: {} (backend=openai-codex)",
        auth_path.display(),
        paths.config_path.display()
    ))
}

pub(crate) fn run_logout() -> Result<String, CliError> {
    let auth_path = genesis_auth::default_auth_path()?;
    let removed = genesis_auth::store::clear_active_provider(&auth_path)?;

    match removed {
        Some(provider_id) => Ok(format!(
            "  Logged out from '{provider_id}'.\n  Credentials cleared from {}",
            auth_path.display()
        )),
        None => Ok("  No active authentication session to clear.".to_owned()),
    }
}

pub(crate) async fn run_init(
    config_path: Option<PathBuf>,
    backend: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
) -> Result<String, CliError> {
    use std::io::IsTerminal;

    // If no flags provided and stdin is a TTY, run interactive wizard
    let is_interactive = backend.is_none()
        && model.is_none()
        && base_url.is_none()
        && api_key_env.is_none()
        && io::stdin().is_terminal();

    if is_interactive {
        return run_init_wizard(config_path).await;
    }

    run_init_non_interactive(config_path, backend, model, base_url, api_key_env)
}

/// Interactive setup wizard — prompts the user to choose a provider, model,
/// and verify their API key. Invoked when `genesis init` is run with no flags.
pub(crate) async fn run_init_wizard(config_path: Option<PathBuf>) -> Result<String, CliError> {
    use genesis_config::{render_example_yaml, update_provider_in_file, AppPaths};

    eprintln!();
    eprintln!("  Welcome to Genesis setup!");
    eprintln!("  ========================");
    eprintln!();

    // Step 1: Choose provider
    let providers: Vec<(&str, &str, &str)> = vec![
        ("openai", "OpenAI", "OPENAI_API_KEY"),
        ("anthropic", "Anthropic (Claude)", "ANTHROPIC_API_KEY"),
        ("google", "Google (Gemini)", "GEMINI_API_KEY"),
        (
            "openrouter",
            "OpenRouter (200+ models)",
            "OPENROUTER_API_KEY",
        ),
        ("openai-codex", "Sign in with ChatGPT (OAuth)", ""),
        ("local", "Local / Self-hosted (vLLM, Ollama, etc.)", ""),
        ("compatible", "Custom OpenAI-compatible endpoint", ""),
    ];

    eprintln!("  Choose your LLM provider:");
    eprintln!();
    for (i, (_, label, _)) in providers.iter().enumerate() {
        eprintln!("    {}. {}", i + 1, label);
    }
    eprintln!();

    let provider_idx = prompt_choice("  Provider", providers.len())?;
    let (backend, _provider_label, default_key_env) = providers[provider_idx];
    eprintln!();

    // If user chose OAuth, detect model then delegate to login flow
    if backend == "openai-codex" {
        eprintln!("  Starting OAuth login flow...");
        eprintln!();

        // Detect codex model from user's config
        let codex_model = detect_codex_model();
        let chosen_model = if let Some(model) = codex_model {
            eprintln!("  Detected Codex model: {}", model);
            eprintln!();
            model
        } else {
            // Fall back to known models picker
            let models = known_models();
            let backend_models: Vec<_> = models
                .iter()
                .filter(|(p, _, _)| p.eq_ignore_ascii_case("openai-codex"))
                .collect();
            if backend_models.is_empty() {
                prompt_line("  Model")?
            } else {
                eprintln!("  Choose a model:");
                eprintln!();
                for (i, (_, model_id, desc)) in backend_models.iter().enumerate() {
                    eprintln!("    {}. {} — {}", i + 1, model_id, desc);
                }
                eprintln!(
                    "    {}. Enter a custom model name",
                    backend_models.len() + 1
                );
                eprintln!();
                let model_idx = prompt_choice("  Model", backend_models.len() + 1)?;
                if model_idx < backend_models.len() {
                    backend_models[model_idx].1.to_owned()
                } else {
                    prompt_line("  Custom model")?
                }
            }
        };

        // Ensure config and storage exist first
        let paths = AppPaths::resolve(config_path.as_deref())?;
        if !paths.config_path.exists() {
            if let Some(parent) = paths.config_path.parent() {
                std::fs::create_dir_all(parent).map_err(CliError::Io)?;
            }
            let yaml = render_example_yaml(config_path.as_deref())?;
            std::fs::write(&paths.config_path, &yaml).map_err(CliError::Io)?;
        }
        std::fs::create_dir_all(&paths.data_dir).map_err(CliError::Io)?;
        let _ = bootstrap(&paths.database_path)?;

        // Write the model to config
        update_provider_in_file(
            &paths.config_path,
            Some("openai-codex"),
            Some(&chosen_model),
            None,
            None,
        )?;

        return run_login(config_path).await;
    }

    // Step 2: Choose model
    let models = known_models();
    let backend_models: Vec<_> = models
        .iter()
        .filter(|(p, _, _)| p.eq_ignore_ascii_case(backend))
        .collect();

    let chosen_model = if backend_models.is_empty() {
        // No known models for this backend — ask for free-form input
        eprintln!("  Enter the model name:");
        prompt_line("  Model")?
    } else {
        eprintln!("  Choose a model:");
        eprintln!();
        for (i, (_, model_id, desc)) in backend_models.iter().enumerate() {
            eprintln!("    {}. {} — {}", i + 1, model_id, desc);
        }
        eprintln!(
            "    {}. Enter a custom model name",
            backend_models.len() + 1
        );
        eprintln!();

        let model_idx = prompt_choice("  Model", backend_models.len() + 1)?;
        if model_idx < backend_models.len() {
            backend_models[model_idx].1.to_owned()
        } else {
            prompt_line("  Custom model name")?
        }
    };
    eprintln!();

    // Step 3: Base URL (only for local/compatible)
    let base_url = if backend == "local" || backend == "compatible" {
        eprintln!("  Enter the API base URL (e.g. http://localhost:11434/v1):");
        let url = prompt_line("  Base URL")?;
        eprintln!();
        Some(url)
    } else {
        None
    };

    // Step 4: API key
    let api_key_env = if !default_key_env.is_empty() {
        eprintln!("  API key env var [default: {}]:", default_key_env);
        let input = prompt_line_or_default("  Env var", default_key_env)?;
        eprintln!();

        // Check if the key is actually set
        if genesis_config::env::is_present(&input) {
            eprintln!("  [ok] ${} is set", input);
        } else {
            eprintln!("  [!!] ${} is NOT set — set it before chatting:", input);
            eprintln!("       export {}=your-api-key-here", input);
        }
        eprintln!();

        Some(input)
    } else {
        None
    };

    // Now run the actual init with the chosen values
    let mut steps = Vec::new();
    steps.push(String::new());

    let paths = AppPaths::resolve(config_path.as_deref())?;

    // Create config if needed
    if !paths.config_path.exists() {
        if let Some(parent) = paths.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(CliError::Io)?;
        }
        let yaml = render_example_yaml(config_path.as_deref())?;
        std::fs::write(&paths.config_path, &yaml).map_err(CliError::Io)?;
        steps.push(format!(
            "  [+] Created config: {}",
            paths.config_path.display()
        ));
    } else {
        steps.push(format!(
            "  [ok] Config exists: {}",
            paths.config_path.display()
        ));
    }

    // Apply choices
    update_provider_in_file(
        &paths.config_path,
        Some(backend),
        Some(&chosen_model),
        base_url.as_ref().map(|u| Some(u.as_str())),
        api_key_env.as_ref().map(|k| Some(k.as_str())),
    )?;
    steps.push(format!("  [+] Provider: {} / {}", backend, chosen_model));

    // Bootstrap storage
    std::fs::create_dir_all(&paths.data_dir).map_err(CliError::Io)?;
    let storage_result = bootstrap(&paths.database_path)?;
    steps.push(format!(
        "  [+] Storage ready: {} (schema v{})",
        paths.database_path.display(),
        storage_result.schema_version
    ));

    let tool_count = genesis_core::default_tool_count();
    steps.push(String::new());
    steps.push(format!(
        "  Genesis is ready! {} tools available.",
        tool_count
    ));
    steps.push("  Run `genesis chat` to start talking to Eve.".to_owned());

    Ok(steps.join("\n"))
}

/// Non-interactive init — used when CLI flags are provided or stdin is not a TTY.
pub(crate) fn run_init_non_interactive(
    config_path: Option<PathBuf>,
    backend: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
) -> Result<String, CliError> {
    use genesis_config::{render_example_yaml, update_provider_in_file, AppPaths};

    let paths = AppPaths::resolve(config_path.as_deref())?;
    let mut steps = Vec::new();

    // Step 1: Create config file if it doesn't exist
    let config_existed = paths.config_path.exists();
    if !config_existed {
        // Create parent directory
        if let Some(parent) = paths.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(CliError::Io)?;
        }

        // Write default config
        let yaml = render_example_yaml(config_path.as_deref())?;
        std::fs::write(&paths.config_path, &yaml).map_err(CliError::Io)?;
        steps.push(format!(
            "[+] Created config: {}",
            paths.config_path.display()
        ));
    } else {
        steps.push(format!(
            "[ok] Config exists: {}",
            paths.config_path.display()
        ));
    }

    // Step 2: Apply provider overrides if specified
    if backend.is_some() || model.is_some() || base_url.is_some() || api_key_env.is_some() {
        update_provider_in_file(
            &paths.config_path,
            backend.as_deref(),
            model.as_deref(),
            base_url.as_ref().map(|u| Some(u.as_str())),
            api_key_env.as_ref().map(|k| Some(k.as_str())),
        )?;

        let mut parts = Vec::new();
        if let Some(ref b) = backend {
            parts.push(format!("backend={b}"));
        }
        if let Some(ref m) = model {
            parts.push(format!("model={m}"));
        }
        steps.push(format!("[+] Updated provider: {}", parts.join(", ")));
    }

    // Step 3: Bootstrap storage
    std::fs::create_dir_all(&paths.data_dir).map_err(CliError::Io)?;
    let storage_result = bootstrap(&paths.database_path)?;
    steps.push(format!(
        "[+] Storage ready: {} (schema v{})",
        paths.database_path.display(),
        storage_result.schema_version
    ));

    // Step 4: Load and verify config
    let loaded = load(config_path.as_deref())?;
    steps.push(format!(
        "[ok] Profile: {}, Provider: {} / {}",
        loaded.config.profile, loaded.config.provider.backend, loaded.config.provider.model
    ));

    // Step 5: Check API key
    let api_key_var = loaded
        .config
        .provider
        .api_key_env
        .as_deref()
        .unwrap_or("OPENAI_API_KEY");
    if genesis_config::env::is_present(api_key_var) {
        steps.push(format!("[ok] API key: ${api_key_var} is set"));
    } else {
        steps.push(format!(
            "[!!] API key: ${api_key_var} is NOT set — set it to start chatting"
        ));
    }

    // Summary
    let tool_count = genesis_core::default_tool_count();
    steps.push(String::new());
    steps.push(format!("Genesis is ready! {} tools available.", tool_count));
    steps.push("Run `genesis chat` to start talking to Eve.".to_owned());

    Ok(steps.join("\n"))
}

/// Prompt the user to enter a number (1-based) and return 0-based index.
pub(crate) fn prompt_choice(label: &str, max: usize) -> Result<usize, CliError> {
    loop {
        eprint!("{} [1-{}]: ", label, max);
        let _ = io::stderr().flush();

        let mut input = String::new();
        io::stdin().read_line(&mut input).map_err(CliError::Io)?;

        match input.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= max => return Ok(n - 1),
            _ => eprintln!("  Please enter a number between 1 and {}.", max),
        }
    }
}

/// Prompt the user for a free-form line of input.
pub(crate) fn prompt_line(label: &str) -> Result<String, CliError> {
    loop {
        eprint!("{}: ", label);
        let _ = io::stderr().flush();

        let mut input = String::new();
        io::stdin().read_line(&mut input).map_err(CliError::Io)?;

        let trimmed = input.trim().to_owned();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
        eprintln!("  Please enter a value.");
    }
}

/// Prompt the user for a line of input with a default value.
pub(crate) fn prompt_line_or_default(label: &str, default: &str) -> Result<String, CliError> {
    eprint!("{} [{}]: ", label, default);
    let _ = io::stderr().flush();

    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(CliError::Io)?;

    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

pub(crate) async fn run_update() -> Result<String, CliError> {
    use std::process::Command as StdCommand;

    let exe = std::env::current_exe().map_err(CliError::Io)?;
    let repo_dir = exe
        .ancestors()
        .find(|p| p.join(".git").exists() || p.join("Cargo.toml").exists())
        .map(|p| p.to_path_buf())
        .ok_or_else(|| {
            CliError::Other(
                "cannot locate genesis source repo — update requires a source install".into(),
            )
        })?;

    let mut steps = Vec::new();

    // Step 1: git pull
    steps.push("[*] Pulling latest changes…".to_owned());
    let pull = StdCommand::new("git")
        .args(["pull", "--rebase", "origin", "main"])
        .current_dir(&repo_dir)
        .output()
        .map_err(CliError::Io)?;

    let pull_out = String::from_utf8_lossy(&pull.stdout);
    let pull_err = String::from_utf8_lossy(&pull.stderr);

    if !pull.status.success() {
        return Err(CliError::Other(format!(
            "git pull failed:\n{pull_out}{pull_err}"
        )));
    }
    steps.push(format!("    {}", pull_out.trim()));

    // Step 2: cargo build --release
    steps.push("[*] Building release binary…".to_owned());
    let build = StdCommand::new("cargo")
        .args(["build", "--release"])
        .current_dir(&repo_dir)
        .output()
        .map_err(CliError::Io)?;

    if !build.status.success() {
        let build_err = String::from_utf8_lossy(&build.stderr);
        return Err(CliError::Other(format!("cargo build failed:\n{build_err}")));
    }
    steps.push("[ok] Build succeeded.".to_owned());

    // Step 3: Report new version
    let version_out = match StdCommand::new(repo_dir.join("target/release/genesis"))
        .arg("--version")
        .output()
    {
        Ok(o) => String::from_utf8(o.stdout).unwrap_or_else(|_| "unknown".to_owned()),
        Err(e) => {
            tracing::warn!(error = %e, "failed to get version from new binary");
            "unknown".to_owned()
        }
    };
    steps.push(format!("[ok] Updated to: {}", version_out.trim()));

    Ok(steps.join("\n"))
}

pub(crate) fn run_uninstall(
    config_override: Option<&Path>,
    remove_data: bool,
    remove_config: bool,
    force: bool,
) -> Result<String, CliError> {
    use std::io::IsTerminal;

    let exe_path = std::env::current_exe().map_err(CliError::Io)?;

    // Try loading the full config (respects storage.data_dir and GENESIS_DATA_DIR).
    // Fall back to platform-default paths if the config file is already gone or unreadable.
    let (config_dir, data_dir) = match load(config_override) {
        Ok(loaded) => {
            let cd = loaded.paths.config_path.parent().map(|p| p.to_path_buf());
            (cd, loaded.paths.data_dir)
        }
        Err(_) => {
            let paths = genesis_config::AppPaths::resolve(config_override)?;
            let cd = paths.config_path.parent().map(|p| p.to_path_buf());
            (cd, paths.data_dir)
        }
    };

    // Build the plan of what will be removed
    let mut plan: Vec<String> = Vec::new();

    if exe_path.exists() {
        plan.push(format!("  Binary:  {}", exe_path.display()));
    }
    if remove_data && data_dir.exists() {
        plan.push(format!("  Data:    {}", data_dir.display()));
    }
    if remove_config {
        if let Some(ref cd) = config_dir {
            if cd.exists() {
                plan.push(format!("  Config:  {}", cd.display()));
            }
        }
    }

    if plan.is_empty() {
        return Ok("Nothing to remove — Genesis does not appear to be installed.".to_owned());
    }

    let mut output = Vec::new();
    output.push("The following will be removed:".to_owned());
    output.extend(plan.iter().cloned());

    // Prompt for confirmation unless --force is set
    if !force {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "uninstall requires --force when stdin is not a terminal".into(),
            ));
        }

        eprintln!();
        for line in &output {
            eprintln!("{line}");
        }
        eprintln!();
        eprint!("Proceed with uninstall? [y/N] ");
        let _ = io::stderr().flush();

        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(CliError::Io)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            return Ok("Uninstall cancelled.".to_owned());
        }
    }

    // Perform removals
    let mut results = Vec::new();

    // Remove data directory first (if requested), since it's the most expendable
    if remove_data && data_dir.exists() {
        fs::remove_dir_all(&data_dir).map_err(|e| {
            CliError::Other(format!(
                "failed to remove data directory {}: {e}",
                data_dir.display()
            ))
        })?;
        results.push(format!(
            "[ok] Removed data directory: {}",
            data_dir.display()
        ));
    }

    // Remove config directory (if requested)
    if remove_config {
        if let Some(ref cd) = config_dir {
            if cd.exists() {
                fs::remove_dir_all(cd).map_err(|e| {
                    CliError::Other(format!(
                        "failed to remove config directory {}: {e}",
                        cd.display()
                    ))
                })?;
                results.push(format!("[ok] Removed config directory: {}", cd.display()));
            }
        }
    }

    // Remove the binary last so the process can finish writing output
    if exe_path.exists() {
        fs::remove_file(&exe_path).map_err(|e| {
            CliError::Other(format!(
                "failed to remove binary {}: {e}",
                exe_path.display()
            ))
        })?;
        results.push(format!("[ok] Removed binary: {}", exe_path.display()));
    }

    if results.is_empty() {
        Ok("Nothing was removed.".to_owned())
    } else {
        results.push(String::new());
        results.push("Genesis has been uninstalled.".to_owned());
        Ok(results.join("\n"))
    }
}
