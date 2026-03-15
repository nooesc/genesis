use std::collections::HashSet;
use std::path::PathBuf;

use genesis_config::load;
use genesis_core::prompt::load_context_file;
use genesis_storage::{bootstrap, SessionStore};
use genesis_types::DeliveryPlatform;

use crate::CliError;

#[derive(Debug)]
pub(crate) struct BatchInputLine {
    pub(crate) prompt: String,
    pub(crate) tags: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_batch(
    config_path: Option<PathBuf>,
    input: String,
    output: String,
    model_override: Option<String>,
    max_turns: Option<usize>,
    concurrency: Option<usize>,
    toolset: Option<String>,
    quality_filter: Option<f64>,
    auto_tag: bool,
) -> Result<String, CliError> {
    if let Some(score) = quality_filter {
        if !(0.0..=1.0).contains(&score) {
            return Err(CliError::Other(format!(
                "quality filter must be between 0.0 and 1.0, got {score}"
            )));
        }
    }

    let loaded = std::sync::Arc::new(load(config_path.as_deref())?);
    bootstrap(&loaded.config.storage.database_path)?;

    let distribution = match &toolset {
        Some(name) => {
            let dist = genesis_core::toolset::resolve_distribution(
                name,
                &loaded.config.toolsets,
            )
            .ok_or_else(|| {
                let mut available: Vec<String> = genesis_core::toolset::builtin_distribution_names()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                available.extend(loaded.config.toolsets.keys().cloned());
                CliError::Other(format!(
                    "unknown toolset distribution '{name}'. Available: {}",
                    available.join(", ")
                ))
            })?;
            Some(std::sync::Arc::new(dist))
        }
        None => None,
    };

    let input_file = std::fs::File::open(&input)
        .map_err(|e| CliError::Other(format!("failed to open {input}: {e}")))?;
    let reader = std::io::BufReader::new(input_file);
    let mut items = Vec::new();

    for (line_no, line) in std::io::BufRead::lines(reader).enumerate() {
        let line = line.map_err(|e| {
            CliError::Other(format!("failed to read line {} from {}: {e}", line_no + 1, input))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let item = parse_batch_input_line(&line).map_err(|e| {
            CliError::Other(format!("invalid JSONL at {} line {}: {}", input, line_no + 1, e))
        })?;
        items.push(item);
    }

    if items.is_empty() {
        return Ok(format!("no prompts found in {input}"));
    }

    std::fs::create_dir_all(&output).map_err(|e| {
        CliError::Other(format!("failed to create output directory {}: {e}", output))
    })?;

    let total = items.len();
    let limit = concurrency
        .unwrap_or(loaded.config.runtime.max_concurrency)
        .max(1);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(limit));
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    let mut seen_hashes = HashSet::new();
    let mut skipped = 0usize;

    for item in items.into_iter() {
        let prompt_hash = sha256_hex(&item.prompt);
        let output_path = batch_output_path(&output, &prompt_hash);

        if !seen_hashes.insert(prompt_hash.clone()) || output_path.exists() {
            skipped += 1;
            let done = completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            eprintln!("[{done}/{total}] skip {prompt_hash}");
            continue;
        }

        let permit = semaphore.clone().acquire_owned().await.map_err(|e| {
            CliError::Other(format!("failed to acquire concurrency permit: {e}"))
        })?;
        let loaded = loaded.clone();
        let output_dir = output.clone();
        let model_override = model_override.clone();
        let completed = completed.clone();
        let distribution = distribution.clone();

        tasks.spawn(async move {
            let _permit = permit;
            let result = run_batch_item(
                &loaded,
                &prompt_hash,
                &item,
                &output_dir,
                model_override.as_deref(),
                max_turns,
                distribution.as_ref().map(|d| d.as_ref()),
                quality_filter,
                auto_tag,
            )
            .await;

            let done = completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            eprintln!("[{done}/{total}] {prompt_hash}");
            result
        });
    }

    let mut succeeded = 0usize;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(())) => succeeded += 1,
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                return Err(CliError::Other(format!("batch task join error: {error}")));
            }
        }
    }

    Ok(format!(
        "generated {succeeded}/{total} trajectories in {} (skipped {skipped})",
        output
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_batch_item(
    loaded: &genesis_config::LoadedConfig,
    session_id: &str,
    item: &BatchInputLine,
    output_dir: &str,
    model_override: Option<&str>,
    max_turns: Option<usize>,
    distribution: Option<&genesis_core::toolset::ToolsetDistribution>,
    quality_filter: Option<f64>,
    auto_tag: bool,
) -> Result<(), CliError> {
    let session_store = SessionStore::new(&loaded.config.storage.database_path);
    let _ = session_store.create_session(session_id, "batch", None);

    let execution_context = genesis_core::build_execution_context_from_loaded(
        loaded,
        session_id.to_owned(),
        DeliveryPlatform::Cli,
    );
    let mut tool_runtime = genesis_core::build_default_tool_runtime(&execution_context);

    // Apply toolset distribution filtering if specified.
    if let Some(dist) = distribution {
        let mut rng = rand::rng();
        let selected = dist.sample(&mut rng);
        tool_runtime.retain(&selected);
    }
    let skills_section = genesis_core::skills::load_skills_prompt_for_prompt(
        &loaded.config.storage.database_path,
        &item.prompt,
    );
    let context_section = load_context_file(
        std::path::Path::new("."),
        &loaded.config.runtime.context_security,
    );
    let system_prompt = genesis_core::prompt::build_system_prompt_complete(
        &execution_context.plan.profile,
        &tool_runtime.definitions(),
        None,
        skills_section.as_deref(),
        None,
        context_section.as_deref(),
    );

    let model = model_override.unwrap_or(&loaded.config.provider.model);
    let client = genesis_provider::client_from_config(
        &loaded.config.provider.backend,
        model,
        loaded.config.provider.base_url.as_deref(),
        loaded.config.provider.api_key_env.as_deref(),
    )
    .await?;

    let mut agent = genesis_core::agent_loop::AgentLoop::new(
        client,
        tool_runtime,
        genesis_core::agent_loop::AgentLoopConfig {
            system_prompt: Some(system_prompt),
            max_turns: max_turns.unwrap_or(loaded.config.runtime.max_turns),
            max_context_messages: loaded.config.runtime.max_context_messages,
            budget_limit: loaded.config.runtime.budget_limit,
            max_concurrency: loaded.config.runtime.max_concurrency,
            enable_trajectory: true,
            trajectory_dir: Some(output_dir.to_owned()),
            session_id: Some(session_id.to_owned()),
            ..genesis_core::agent_loop::AgentLoopConfig::default()
        },
        genesis_core::hooks::HookRunner::default(),
    );

    if let Some(tp) = &loaded.config.tool_provider {
        let tool_client = genesis_provider::client_from_config(
            &tp.backend,
            &tp.model,
            tp.base_url.as_deref(),
            tp.api_key_env.as_deref(),
        )
        .await?;
        agent.set_tool_client(tool_client);
    }

    if !loaded.config.fallback_providers.is_empty() {
        let mut fallbacks = Vec::new();
        for fp in &loaded.config.fallback_providers {
            let fb_client = genesis_provider::client_from_config(
                &fp.backend,
                &fp.model,
                fp.base_url.as_deref(),
                fp.api_key_env.as_deref(),
            )
            .await?;
            fallbacks.push(fb_client);
        }
        agent.set_fallback_clients(fallbacks);
    }

    for tag in &item.tags {
        agent.trajectory_mut().add_tag(tag);
    }

    let _ = agent.run_turn(&item.prompt).await?;
    if auto_tag {
        apply_auto_tags(output_dir, session_id)?;
    }
    if let Some(min_quality) = quality_filter {
        discard_low_quality_trajectory(output_dir, session_id, min_quality)?;
    }
    Ok(())
}

pub(crate) fn discard_low_quality_trajectory(
    output_dir: &str,
    session_id: &str,
    min_quality: f64,
) -> Result<(), CliError> {
    let output_path = batch_output_path(output_dir, session_id);
    if !output_path.exists() {
        return Ok(());
    }

    let raw = std::fs::read_to_string(&output_path)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", output_path.display())))?;
    let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw).map_err(|e| {
        CliError::Other(format!(
            "invalid trajectory JSON in {}: {e}",
            output_path.display()
        ))
    })?;
    let quality = genesis_core::quality::score(&trajectory);
    if quality.overall < min_quality {
        std::fs::remove_file(&output_path).map_err(|e| {
            CliError::Other(format!(
                "failed to discard low-quality trajectory {}: {e}",
                output_path.display()
            ))
        })?;
    }

    Ok(())
}

pub(crate) fn apply_auto_tags(output_dir: &str, session_id: &str) -> Result<(), CliError> {
    let output_path = batch_output_path(output_dir, session_id);
    if !output_path.exists() {
        return Ok(());
    }

    let raw = std::fs::read_to_string(&output_path)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", output_path.display())))?;
    let mut trajectory: genesis_core::trajectory::Trajectory =
        serde_json::from_str(&raw).map_err(|e| {
            CliError::Other(format!(
                "invalid trajectory JSON in {}: {e}",
                output_path.display()
            ))
        })?;

    let auto_tags = genesis_core::tagger::auto_tag(&trajectory);
    let existing: HashSet<String> = trajectory.tags.iter().cloned().collect();
    for tag in auto_tags {
        if !existing.contains(&tag) {
            trajectory.tags.push(tag);
        }
    }
    trajectory.tags.sort();

    let updated = serde_json::to_string_pretty(&trajectory)?;
    std::fs::write(&output_path, updated).map_err(|e| {
        CliError::Other(format!(
            "failed to write auto-tagged trajectory {}: {e}",
            output_path.display()
        ))
    })?;

    Ok(())
}

pub(crate) fn parse_batch_input_line(line: &str) -> Result<BatchInputLine, String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| e.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "expected JSON object".to_owned())?;

    let prompt = object
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing string field `prompt`".to_owned())?
        .to_owned();

    let tags = object
        .get("tags")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| "field `tags` must be an array of strings".to_owned())?
                .iter()
                .map(|tag| {
                    tag.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "field `tags` must be an array of strings".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok(BatchInputLine { prompt, tags })
}

pub(crate) fn batch_output_path(output_dir: &str, prompt_hash: &str) -> PathBuf {
    std::path::Path::new(output_dir).join(format!("{prompt_hash}.json"))
}

pub(crate) fn sha256_hex(input: &str) -> String {
    crate::sha256_hex(input)
}
