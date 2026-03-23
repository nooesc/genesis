use std::path::PathBuf;
use std::time::Duration;

use genesis_config::load;

use crate::CliError;

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
                    if let Some(_chunk) = match stream.try_next().await {
                        Ok(chunk) => chunk,
                        Err(e) => {
                            tracing::debug!(error = %e, "TTFT stream read failed");
                            None
                        }
                    } {
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
