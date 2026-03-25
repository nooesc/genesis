use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use genesis_provider::ToolCallEntry;
use genesis_tools::ToolCall;
use tracing::{debug, info, info_span, warn};

use super::{AgentError, SubagentSpawner};
use crate::ToolRuntime;

/// Check whether a tool result string represents an error.
///
/// This is the single source of truth for the `"Error:"` convention used
/// throughout the agent loop, trajectory scorer, and auto-tagger.
/// Handles both `"Error:"` (title-case) and `"error:"` (lowercase) prefixes.
pub(crate) fn is_tool_error(content: &str) -> bool {
    content.starts_with("Error:") || content.starts_with("error:")
}

/// Check whether a `find_tools` result indicates no matches.
/// Used to avoid treating empty results as successful discovery.
pub(crate) fn is_find_tools_empty(content: &str) -> bool {
    content.starts_with("No tools")
}

/// Produce a short summary string (max ~40 chars) from a tool call's JSON
/// arguments. Tries to show the first key-value pair; falls back to truncating
/// the raw string.
pub(crate) fn summarize_args(args_json: &str) -> String {
    if args_json.is_empty() || args_json == "{}" {
        return String::new();
    }
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(args_json)
    {
        if let Some((key, val)) = map.iter().next() {
            let v = match val {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let combined = format!("{key}: {v}");
            if combined.len() <= 40 {
                return combined;
            }
            let truncated: String = combined.chars().take(37).collect();
            return format!("{truncated}...");
        }
    }
    let raw = args_json.trim_matches(|c| c == '{' || c == '}').trim();
    if raw.len() <= 40 {
        return raw.to_owned();
    }
    let truncated: String = raw.chars().take(37).collect();
    format!("{truncated}...")
}

pub(crate) fn parse_tool_arguments(raw: &str) -> Result<BTreeMap<String, String>, AgentError> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| AgentError::ArgumentParse(format!("{raw}: {e}")))?;

    let obj = value
        .as_object()
        .ok_or_else(|| AgentError::ArgumentParse(format!("expected JSON object, got: {raw}")))?;

    Ok(obj
        .iter()
        .map(|(k, v)| {
            let string_value = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), string_value)
        })
        .collect())
}

/// Execute multiple tool calls concurrently up to the given concurrency limit.
///
/// Results are returned in the same order as the input `tool_calls`, preserving
/// the tool-call-to-result correspondence required by the LLM message format.
/// If any tool call fails with a hard error (e.g., tool not found), that error
/// is propagated and the remaining results are discarded.
pub(crate) async fn execute_tool_calls_parallel(
    tools: &ToolRuntime,
    subagent_spawner: &Option<Arc<dyn SubagentSpawner>>,
    tool_calls: &[ToolCallEntry],
    max_concurrency: usize,
    timeout_secs: u64,
    policy: Option<&crate::tool_policy::ToolPolicy>,
) -> Result<Vec<(String, bool)>, AgentError> {
    let timeout_duration = std::time::Duration::from_secs(timeout_secs);

    if tool_calls.len() == 1 {
        // Check tool policy before execution.
        if let Some(policy) = policy {
            if let Some(denial) = check_tool_policy(policy, &tool_calls[0]) {
                return Ok(vec![(denial, false)]);
            }
        }

        // Fast path: avoid semaphore overhead for single tool calls.
        // execute_single_tool converts all errors to soft "Error:" content,
        // so the Ok(r) branch always succeeds and timeouts are the only
        // additional failure mode to handle.
        let result = match tokio::time::timeout(
            timeout_duration,
            execute_single_tool(tools, subagent_spawner, &tool_calls[0]),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => {
                // Defensive: execute_single_tool should never return Err
                // after error-as-data conversion, but handle it gracefully.
                (
                    format!(
                        "Error: tool `{}` encountered an unexpected error. \
                         Try a different approach.",
                        tool_calls[0].function.name
                    ),
                    false,
                )
            }
            Err(_) => {
                warn!(
                    tool_name = tool_calls[0].function.name.as_str(),
                    timeout_secs, "tool call timed out"
                );
                (
                    format!(
                        "Error: tool `{}` timed out after {timeout_secs}s. \
                         The operation took too long. Try a simpler approach \
                         or break the task into smaller steps.",
                        tool_calls[0].function.name
                    ),
                    false,
                )
            }
        };
        return Ok(vec![result]);
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrency.max(1)));
    let futs: Vec<_> = tool_calls
        .iter()
        .map(|tc| {
            let sem = Arc::clone(&semaphore);
            let tool_name = tc.function.name.clone();
            // Pre-check tool policy so denied calls never reach execution.
            let denial = policy.and_then(|p| check_tool_policy(p, tc));
            async move {
                if let Some(denial_msg) = denial {
                    return Ok((denial_msg, false));
                }
                let Ok(_permit) = sem.acquire().await else {
                    return Ok((
                        format!("Error: tool `{tool_name}` skipped — concurrency semaphore closed"),
                        false,
                    ));
                };
                match tokio::time::timeout(
                    timeout_duration,
                    execute_single_tool(tools, subagent_spawner, tc),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        warn!(
                            tool_name = tool_name.as_str(),
                            timeout_secs, "tool call timed out"
                        );
                        Ok((
                            format!(
                                "Error: tool `{tool_name}` timed out after {timeout_secs}s. \
                                 The operation took too long. Try a simpler approach \
                                 or break the task into smaller steps."
                            ),
                            false,
                        ))
                    }
                }
            }
        })
        .collect();

    let results = futures_util::future::join_all(futs).await;

    // Collect results, short-circuiting on the first hard error.
    results.into_iter().collect()
}

/// Check a single tool call against the policy, returning a denial message
/// if the call is blocked, or `None` if it is allowed.
pub(crate) fn check_tool_policy(
    policy: &crate::tool_policy::ToolPolicy,
    tc: &ToolCallEntry,
) -> Option<String> {
    let parse_result = serde_json::from_str::<std::collections::BTreeMap<String, serde_json::Value>>(
        &tc.function.arguments,
    );
    let raw_map = match parse_result {
        Ok(m) => m,
        Err(e) => {
            warn!(
                tool = %tc.function.name,
                error = %e,
                "failed to parse tool arguments for policy check; denying call"
            );
            return Some(format!(
                "Error: tool `{}` denied: could not parse arguments for policy evaluation",
                tc.function.name
            ));
        }
    };
    let args: std::collections::BTreeMap<String, String> = raw_map
        .into_iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            (k, s)
        })
        .collect();
    let decision = policy.evaluate(&tc.function.name, &args);
    match decision {
        crate::tool_policy::PolicyDecision::Deny(reason) => {
            warn!(
                tool = tc.function.name.as_str(),
                reason = reason.as_str(),
                "tool call blocked by policy"
            );
            Some(format!(
                "Error: {reason}\n\n\
                 This tool call was blocked by the tool policy. \
                 Review the policy file configured at `runtime.tool_policy_path` to adjust permissions."
            ))
        }
        crate::tool_policy::PolicyDecision::Allow => None,
    }
}

/// Execute a single tool call against the provided runtime, returning the
/// content string for the LLM and whether the tool requests user input.
///
/// This is a free function (not a method) so it can be used for concurrent
/// execution from `&mut self` methods via field-level borrow splitting.
pub(crate) async fn execute_single_tool(
    tools: &ToolRuntime,
    subagent_spawner: &Option<Arc<dyn SubagentSpawner>>,
    tc: &ToolCallEntry,
) -> Result<(String, bool), AgentError> {
    let span = info_span!(
        "agent.tool_call",
        tool_name = tc.function.name.as_str(),
        tool_call_id = tc.id.as_str()
    );
    let started_at = Instant::now();
    let tool_name = &tc.function.name;

    // Parse arguments — malformed JSON from the LLM is a recoverable error
    // (feed it back so the model can self-correct) rather than a hard failure.
    let arguments = {
        let _entered = span.enter();
        match parse_tool_arguments(&tc.function.arguments) {
            Ok(args) => {
                debug!(argument_count = args.len(), "parsed tool arguments");
                args
            }
            Err(e) => {
                warn!(
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    tool_name = tool_name.as_str(),
                    error = %e,
                    "tool argument parse failed, feeding error back to LLM"
                );
                return Ok((
                    format!(
                        "Error: tool `{tool_name}` received invalid arguments: {e}\n\n\
                         Please fix the JSON arguments and try again."
                    ),
                    false,
                ));
            }
        }
    };

    let call = ToolCall {
        name: tool_name.clone(),
        arguments,
    };

    match tools.execute_async(&call).await {
        Ok(output) => {
            info!(
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                output_bytes = output.content.len(),
                "tool call succeeded"
            );
            // Check for subagent spawn metadata.
            if let Some(spawner) = subagent_spawner {
                if output.metadata.get("__subagent_spawn").map(String::as_str) == Some("true") {
                    if let (Some(child_session_id), Some(subagent_id), Some(task)) = (
                        output.metadata.get("child_session_id"),
                        output.metadata.get("subagent_id"),
                        output.metadata.get("task"),
                    ) {
                        info!(
                            subagent_id = subagent_id.as_str(),
                            child_session_id = child_session_id.as_str(),
                            "spawning subagent workstream"
                        );
                        spawner.spawn(child_session_id, subagent_id, task);
                    }
                }
            }
            let requires_input = output
                .metadata
                .get("requires_input")
                .map(|v| v == "true")
                .unwrap_or(false);
            Ok((output.content, requires_input))
        }
        Err(err) => {
            warn!(
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                tool_name = tool_name.as_str(),
                error = %err,
                "tool call failed, feeding error back to LLM"
            );
            match &err {
                genesis_tools::ToolError::ToolNotFound(name) => {
                    let suggestions = suggest_similar_tools(name, tools);
                    let msg = if suggestions.is_empty() {
                        format!(
                            "Error: tool `{name}` not found. \
                             Use only tools listed in the system prompt."
                        )
                    } else {
                        format!(
                            "Error: tool `{name}` not found. Did you mean: {}?\n\n\
                             Try calling one of the suggested tools instead.",
                            suggestions.join(", ")
                        )
                    };
                    Ok((msg, false))
                }
                genesis_tools::ToolError::MissingArgument { tool, argument } => Ok((
                    format!(
                        "Error: tool `{tool}` is missing required argument `{argument}`.\n\n\
                         Please include the `{argument}` parameter and try again."
                    ),
                    false,
                )),
                genesis_tools::ToolError::ApprovalDenied { tool, reason } => Ok((
                    format!(
                        "Error: tool `{tool}` was denied: {reason}\n\n\
                         Try a different approach that doesn't require this operation."
                    ),
                    false,
                )),
                genesis_tools::ToolError::ExecutionFailed { tool, reason } => Ok((
                    format!(
                        "Error: tool `{tool}` execution failed: {reason}\n\n\
                         You can try a different approach or use an alternative tool."
                    ),
                    false,
                )),
            }
        }
    }
}

/// Suggest tool names similar to `name` using edit distance.
/// Returns up to 3 suggestions sorted by similarity.
fn suggest_similar_tools(name: &str, tools: &ToolRuntime) -> Vec<String> {
    let name_lower = name.to_lowercase();
    let mut scored: Vec<(String, usize)> = tools
        .definitions()
        .iter()
        .filter_map(|def| {
            let def_lower = def.name.to_lowercase();
            let dist = edit_distance(&name_lower, &def_lower);
            let max_len = name.len().max(def.name.len());
            // Only suggest if within 40% edit distance
            if max_len > 0 && dist <= max_len * 2 / 5 {
                Some((def.name.clone(), dist))
            } else {
                // Also match if one is a substring of the other
                if def_lower.contains(&name_lower) || name_lower.contains(&def_lower) {
                    Some((def.name.clone(), dist))
                } else {
                    None
                }
            }
        })
        .collect();
    scored.sort_by_key(|(_, d)| *d);
    scored.truncate(3);
    scored.into_iter().map(|(n, _)| format!("`{n}`")).collect()
}

/// Simple Levenshtein edit distance.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}
