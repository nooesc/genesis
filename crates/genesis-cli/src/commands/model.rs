use std::path::PathBuf;

use genesis_config::{load, LoadedConfig};

use crate::{CliError, ModelCommand};

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
                let loaded = match load(config_path.as_deref()) {
                    Ok(l) => Some(l),
                    Err(e) => {
                        tracing::debug!(error = %e, "failed to load config for model listing");
                        None
                    }
                };
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
            let api_key = genesis_config::env::get_opt(genesis_config::env::OPENROUTER_API_KEY);

            // Fetch models.
            let mut models =
                genesis_provider::openrouter_models::fetch_models(api_key.as_deref(), &cache_dir)
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
            lines.push(format!("Sort: {}  |  Use: genesis model set <id>", sort));

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
