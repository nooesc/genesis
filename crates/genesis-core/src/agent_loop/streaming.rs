use std::time::Instant;

use futures_util::StreamExt;
use genesis_provider::{
    ChatCompletionChunk, ChatCompletionRequest, ChatMessage, ChatTool, MessageContent,
    ProviderError, ToolCallEntry,
};
use tracing::{debug, info, warn};

use super::tools::{execute_tool_calls_parallel, summarize_args};
use super::types::{format_blocked_reasons, StreamEvent};
use super::{AgentError, AgentLoop, AgentResult};
use crate::hooks::HookEvent;
use crate::sanitize;

pub(crate) struct StreamUpdate {
    pub(crate) contents: Vec<String>,
    pub(crate) tool_calls: Vec<ToolCallEntry>,
    pub(crate) finish_reason: Option<String>,
    pub(crate) usage: Option<genesis_provider::ChatUsage>,
    pub(crate) provider_metadata: Option<serde_json::Value>,
}

pub(crate) fn collect_stream_update(chunk: ChatCompletionChunk) -> StreamUpdate {
    let mut contents = Vec::new();
    let mut tool_calls = Vec::new();
    let mut finish_reason = None;

    for choice in chunk.choices {
        if let Some(content) = choice.delta.content {
            contents.push(content);
        }
        if let Some(delta_tool_calls) = choice.delta.tool_calls {
            tool_calls.extend(delta_tool_calls);
        }
        if choice.finish_reason.is_some() {
            finish_reason = choice.finish_reason;
        }
    }

    StreamUpdate {
        contents,
        tool_calls,
        finish_reason,
        usage: chunk.usage,
        provider_metadata: chunk.provider_metadata,
    }
}

/// Merge streaming tool call deltas into the accumulated tool calls list.
///
/// In SSE streaming (both Chat Completions and Responses API), tool calls
/// arrive as incremental chunks:
///   1. First chunk: `id` + `name` + empty `arguments` → new entry
///   2. Subsequent chunks: empty `id` + empty `name` + argument fragment → append
///
/// This function appends argument fragments to the last matching tool call
/// instead of creating separate ghost entries with empty names.
pub(crate) fn merge_streamed_tool_calls(
    accumulated: &mut Vec<ToolCallEntry>,
    deltas: Vec<ToolCallEntry>,
) {
    for delta in deltas {
        if !delta.id.is_empty() && !delta.function.name.is_empty() {
            // New tool call — push as a new entry
            accumulated.push(delta);
        } else if !delta.function.arguments.is_empty() {
            // Argument fragment — append to the last tool call
            if let Some(last) = accumulated.last_mut() {
                last.function.arguments.push_str(&delta.function.arguments);
            }
        }
        // Ignore entries with empty id, empty name, AND empty arguments
    }
}

impl AgentLoop {
    pub async fn run_turn_streaming<F>(
        &mut self,
        user_message: &str,
        on_event: F,
    ) -> Result<AgentResult, AgentError>
    where
        F: FnMut(StreamEvent<'_>),
    {
        self.run_turn_streaming_with_images(user_message, Vec::new(), on_event)
            .await
    }

    /// Run a streaming turn with optional image attachments.
    pub async fn run_turn_streaming_with_images<F>(
        &mut self,
        user_message: &str,
        images: Vec<genesis_provider::ImageUrl>,
        mut on_event: F,
    ) -> Result<AgentResult, AgentError>
    where
        F: FnMut(StreamEvent<'_>),
    {
        use genesis_lua::hooks::PreHookOutcome;

        // Reset stuck-loop state at the start of each new user turn so
        // stale failure counts from a previous turn don't cause false positives.
        self.tool_failure_counts.clear();
        self.nudge_sent = false;

        let hook_session = self.session_id_str().to_owned();
        let lua_pre_turn = self.run_lua_pre_turn(user_message);
        let user_message = match lua_pre_turn {
            PreHookOutcome::Allow(message) => {
                self.fire_shell_hooks(
                    HookEvent::PreTurn,
                    serde_json::json!({
                        "session_id": hook_session,
                        "user_message": message,
                        "image_count": images.len(),
                        "streaming": true,
                    }),
                );
                message
            }
            PreHookOutcome::Veto { reason } => {
                self.fire_shell_hooks(
                    HookEvent::PreTurn,
                    serde_json::json!({
                        "session_id": hook_session,
                        "user_message": user_message,
                        "image_count": images.len(),
                        "streaming": true,
                        "lua_vetoed": true,
                        "lua_reason": reason.as_deref(),
                    }),
                );
                let result = AgentResult {
                    response: format!(
                        "Your input was blocked by Lua hook: {}",
                        reason.unwrap_or_else(|| "request rejected".to_owned())
                    ),
                    turns_used: 0,
                    tool_calls_made: 0,
                    finished_naturally: true,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    estimated_cost: None,
                    pending_clarification: None,
                };
                return Ok(self.finalize_turn(&hook_session, result, false));
            }
        };

        // Run input guardrails if configured (streaming path)
        let user_message = if let Some(ref cg) = self.compiled_guardrails {
            let result = cg.check_input(&user_message);
            if !result.passed {
                let agent_result = AgentResult {
                    response: format!(
                        "Your input was blocked by guardrails: {}",
                        format_blocked_reasons(&result)
                    ),
                    turns_used: 0,
                    tool_calls_made: 0,
                    finished_naturally: true,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    estimated_cost: None,
                    pending_clarification: None,
                };
                return Ok(self.finalize_turn(&hook_session, agent_result, false));
            }
            result.content
        } else {
            user_message.to_owned()
        };

        let user_message = if images.is_empty() {
            self.push_message_with_lua_hooks(&hook_session, ChatMessage::user(&user_message))
                .and_then(|message| message.content_text().map(str::to_owned))
                .unwrap_or_default()
        } else {
            self.push_message_with_lua_hooks(
                &hook_session,
                ChatMessage::user_with_images(user_message.clone(), images),
            )
            .and_then(|message| message.content_text().map(str::to_owned))
            .unwrap_or_default()
        };

        if !user_message.is_empty() {
            self.trajectory.record_user_message(&user_message);
        }

        // Fire turn-start hook (streaming)
        self.hooks.on_turn_start(&hook_session, &user_message);
        on_event(StreamEvent::TurnStarted);

        let all_tool_defs: Vec<ChatTool> = self
            .tools
            .definitions_async()
            .await
            .iter()
            .map(ChatTool::from)
            .collect();

        let mut turns_used = 0;
        let mut tool_calls_made = 0;
        let mut total_input_tokens = 0u32;
        let mut total_output_tokens = 0u32;

        loop {
            let tool_defs = if self.config.core_tools.is_some() {
                self.filter_tool_defs(&all_tool_defs)
            } else {
                all_tool_defs.clone()
            };

            if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                info!("agent loop cancelled by external signal (streaming)");
                self.save_trajectory();
                let result = AgentResult {
                    response: "The operation was cancelled.".to_owned(),
                    turns_used,
                    tool_calls_made,
                    finished_naturally: false,
                    total_input_tokens,
                    total_output_tokens,
                    estimated_cost: Some(self.cost.total_cost),
                    pending_clarification: None,
                };
                return Ok(self.finalize_turn(&hook_session, result, false));
            }

            turns_used += 1;
            if turns_used > self.config.max_turns {
                warn!(
                    max_turns = self.config.max_turns,
                    "agent loop reached turn limit (streaming)"
                );
                let msg = format!(
                    "I've reached the maximum of {} turns for this request. \
                     The work so far has been saved. You can continue by sending another message.",
                    self.config.max_turns
                );
                on_event(StreamEvent::Chunk(&msg));
                self.save_trajectory();
                let result = AgentResult {
                    response: msg,
                    turns_used: turns_used - 1,
                    tool_calls_made,
                    finished_naturally: false,
                    total_input_tokens,
                    total_output_tokens,
                    estimated_cost: Some(self.cost.total_cost),
                    pending_clarification: None,
                };
                return Ok(self.finalize_turn(&hook_session, result, false));
            }

            // Check iteration budget (lifetime cap across all user turns)
            if let Some(limit) = self.config.max_iterations {
                if self.iterations_used >= limit {
                    warn!(
                        iterations = self.iterations_used,
                        limit, "iteration budget exhausted (streaming)"
                    );
                    let msg = format!(
                        "Iteration budget exhausted ({limit} iterations). \
                         The work so far has been saved."
                    );
                    on_event(StreamEvent::Chunk(&msg));
                    self.save_trajectory();
                    let result = AgentResult {
                        response: msg,
                        turns_used,
                        tool_calls_made,
                        finished_naturally: false,
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    };
                    return Ok(self.finalize_turn(&hook_session, result, false));
                }
            }
            self.iterations_used += 1;

            debug!(
                turn = turns_used,
                mode = "streaming",
                prompt_version = crate::prompt::PROMPT_VERSION,
                "starting agent turn iteration"
            );

            self.prune_context().await;
            let mut request = ChatCompletionRequest::new("", self.messages.clone());
            request.tools = tool_defs;
            request.temperature = self.config.temperature;
            request.max_tokens = self.config.max_tokens;
            request.thinking = self.config.thinking.clone();
            request.response_format = self.config.response_format.clone();
            self.inject_reasoning_effort(&mut request);

            // Adaptive routing: classify and override model if enabled.
            let had_failure = self.tool_failure_counts.values().any(|&c| c > 0);
            self.apply_routing(
                &mut request,
                &user_message,
                tool_calls_made,
                turns_used,
                had_failure,
            );

            self.hooks
                .on_llm_request(&hook_session, self.active_client().model(), turns_used);
            let stream_result = self.complete_stream_with_failover(request.clone()).await;
            match stream_result {
                Ok((mut stream, active_model)) => {
                    let mut response_text = String::new();
                    let mut streamed_tool_calls = Vec::new();
                    let mut finished_naturally = true;
                    let mut turn_input_tokens = 0u32;
                    let mut turn_output_tokens = 0u32;
                    let mut streamed_provider_metadata: Option<serde_json::Value> = None;

                    while let Some(chunk) = stream.next().await {
                        let chunk = match chunk {
                            Ok(chunk) => chunk,
                            Err(err) => {
                                return Err(self.report_error(
                                    &hook_session,
                                    "stream_chunk",
                                    err.into(),
                                ));
                            }
                        };
                        let update = collect_stream_update(chunk);

                        for content in update.contents {
                            on_event(StreamEvent::Chunk(&content));
                            response_text.push_str(&content);
                        }

                        if let Some(reason) = update.finish_reason {
                            finished_naturally =
                                !matches!(reason.as_str(), "length" | "incomplete");
                        }

                        if let Some(usage) = update.usage {
                            turn_input_tokens =
                                turn_input_tokens.saturating_add(usage.prompt_tokens);
                            turn_output_tokens =
                                turn_output_tokens.saturating_add(usage.completion_tokens);
                        }

                        if update.provider_metadata.is_some() {
                            streamed_provider_metadata = update.provider_metadata;
                        }

                        merge_streamed_tool_calls(&mut streamed_tool_calls, update.tool_calls);
                    }

                    total_input_tokens = total_input_tokens.saturating_add(turn_input_tokens);
                    total_output_tokens = total_output_tokens.saturating_add(turn_output_tokens);
                    self.last_prompt_tokens = turn_input_tokens;
                    if let Err(err) = self.record_usage_with_model(
                        &active_model,
                        turns_used,
                        turn_input_tokens,
                        turn_output_tokens,
                    ) {
                        return Err(self.report_error(&hook_session, "usage_record", err));
                    }
                    self.hooks.on_llm_response(
                        &hook_session,
                        &active_model,
                        turn_input_tokens,
                        turn_output_tokens,
                    );
                    on_event(StreamEvent::TokenUsage {
                        input_tokens: total_input_tokens,
                        output_tokens: total_output_tokens,
                    });

                    // If streaming didn't produce native tool calls, try parsing from text
                    if streamed_tool_calls.is_empty() && !response_text.is_empty() {
                        if let Some(parser) = self.resolve_parser(&active_model) {
                            if let Some(result) = parser.parse(&response_text) {
                                streamed_tool_calls = result.tool_calls;
                                response_text = result.content.unwrap_or_default();
                            }
                        }
                    }

                    if !streamed_tool_calls.is_empty() {
                        // Propagate provider_metadata (e.g. reasoning items) from
                        // the streaming completion event for multi-turn continuity.
                        let mut msg = ChatMessage::assistant_with_tool_calls(
                            if response_text.is_empty() {
                                None
                            } else {
                                Some(MessageContent::Text(response_text.clone()))
                            },
                            streamed_tool_calls.clone(),
                        );
                        // Only response.completed emits provider_metadata today; last write wins is intentional.
                        msg.provider_metadata = streamed_provider_metadata;
                        if let Some(message) =
                            self.push_message_with_lua_hooks(&hook_session, msg)
                        {
                            if let Some(text) = message.content_text() {
                                if !text.is_empty() {
                                    self.trajectory.record_assistant_message(text);
                                }
                            }
                        }

                        let (effective_tool_calls, veto_reasons) =
                            self.prepare_tool_calls(&hook_session, &streamed_tool_calls, true);
                        streamed_tool_calls = effective_tool_calls;
                        // Emit start events and execute tool calls.
                        tool_calls_made += streamed_tool_calls.len();
                        for tc in &streamed_tool_calls {
                            on_event(StreamEvent::ToolCallStart {
                                name: &tc.function.name,
                                call_id: &tc.id,
                                args_summary: summarize_args(&tc.function.arguments),
                            });
                        }

                        let executable_tool_calls: Vec<ToolCallEntry> = streamed_tool_calls
                            .iter()
                            .zip(veto_reasons.iter())
                            .filter(|(_, veto)| veto.is_none())
                            .map(|(tc, _)| tc.clone())
                            .collect();
                        let tool_exec_start = Instant::now();
                        let executed_results = if executable_tool_calls.is_empty() {
                            Vec::new()
                        } else {
                            match execute_tool_calls_parallel(
                                &self.tools,
                                &self.subagent_spawner,
                                &executable_tool_calls,
                                self.config.max_concurrency,
                                self.config.tool_timeout_secs,
                                self.config.tool_policy.as_ref(),
                            )
                            .await
                            {
                                Ok(results) => results,
                                Err(err) => {
                                    return Err(self.report_error(
                                        &hook_session,
                                        "tool_execution",
                                        err,
                                    ));
                                }
                            }
                        };
                        let tool_exec_duration = tool_exec_start.elapsed();

                        let tool_exec_duration_ms = tool_exec_duration.as_millis() as u64;
                        let mut clarification = None;
                        let mut executed_results = executed_results.into_iter();
                        for (tc, veto_reason) in
                            streamed_tool_calls.iter().zip(veto_reasons.into_iter())
                        {
                            let lua_vetoed = veto_reason.is_some();
                            let (mut result, requires_input) = match veto_reason {
                                Some(reason) => (
                                    format!("Error: tool call blocked by Lua hook: {reason}"),
                                    false,
                                ),
                                None => executed_results.next().expect(
                                    "executed tool results should align with allowed calls",
                                ),
                            };
                            result = sanitize::sanitize_credentials(&result);
                            // Extract discovered tool names from find_tools results
                            // (only when core set filtering is active).
                            if self.config.core_tools.is_some()
                                && tc.function.name == "find_tools"
                                && !result.starts_with("Error:")
                                && !result.starts_with("No tools")
                            {
                                for line in result.lines() {
                                    let trimmed = line.trim();
                                    if let Some(rest) = trimmed.strip_prefix("**") {
                                        if let Some(name_end) = rest.find("**") {
                                            self.discover_tool(&rest[..name_end]);
                                        }
                                    }
                                }
                            }
                            let tool_success = !result.starts_with("Error:");
                            let result = self.run_lua_post_tool_call(&tc.function.name, &result);
                            on_event(StreamEvent::ToolCallEnd {
                                name: &tc.function.name,
                                call_id: &tc.id,
                                duration_ms: tool_exec_duration_ms,
                                success: tool_success,
                            });
                            if !tool_success {
                                let count = self
                                    .tool_failure_counts
                                    .entry(tc.function.name.clone())
                                    .or_insert(0);
                                *count += 1;
                            } else {
                                self.tool_failure_counts.remove(&tc.function.name);
                                // Auto-discover tools called outside core set.
                                if self.config.core_tools.is_some() {
                                    self.discover_tool(&tc.function.name);
                                }
                            }
                            self.fire_shell_hooks(
                                HookEvent::PostToolCall,
                                serde_json::json!({
                                    "session_id": hook_session,
                                    "tool_name": tc.function.name,
                                    "tool_call_id": tc.id,
                                    "success": tool_success,
                                    "result": result,
                                    "requires_input": requires_input,
                                    "streaming": true,
                                    "lua_vetoed": lua_vetoed,
                                }),
                            );
                            let result = self
                                .push_message_with_lua_hooks(
                                    &hook_session,
                                    ChatMessage::tool_result(&tc.id, result),
                                )
                                .and_then(|message| message.content_text().map(str::to_owned))
                                .unwrap_or_default();
                            self.trajectory
                                .record_tool_result(&tc.function.name, &result);
                            if requires_input {
                                on_event(StreamEvent::ClarificationNeeded { question: &result });
                                clarification = Some(result.clone());
                            }
                        }

                        self.maybe_inject_stuck_nudge();

                        if let Some(question) = clarification {
                            self.save_trajectory();
                            let result = AgentResult {
                                response: String::new(),
                                turns_used,
                                tool_calls_made,
                                finished_naturally: false,
                                total_input_tokens,
                                total_output_tokens,
                                estimated_cost: Some(self.cost.total_cost),
                                pending_clarification: Some(question),
                            };
                            return Ok(self.finalize_turn(&hook_session, result, false));
                        }

                        self.maybe_inject_memory_nudge(tool_calls_made);
                        continue;
                    }

                    let mut text_msg = ChatMessage::assistant(&response_text);
                    text_msg.provider_metadata = streamed_provider_metadata;
                    let response_text = self
                        .push_message_with_lua_hooks(&hook_session, text_msg)
                        .and_then(|message| message.content_text().map(str::to_owned))
                        .unwrap_or_default();
                    if !response_text.is_empty() {
                        self.trajectory.record_assistant_message(&response_text);
                    }

                    self.maybe_inject_skill_nudge(tool_calls_made);
                    self.save_trajectory();
                    let result = AgentResult {
                        response: response_text,
                        turns_used,
                        tool_calls_made,
                        finished_naturally,
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    };
                    return Ok(self.finalize_turn(&hook_session, result, true));
                }
                Err(_) => {
                    warn!(
                        turn = turns_used,
                        "streaming provider request failed; falling back to blocking completion"
                    );
                    let (mut response, fb_model) =
                        match self.complete_with_failover(request).await {
                            Ok(result) => result,
                            Err(err) => {
                                return Err(self.report_error(
                                    &hook_session,
                                    "llm_request_fallback",
                                    err.into(),
                                ));
                            }
                        };

                    // Apply tool call parser for models that embed tool calls in text
                    self.apply_tool_call_parser(&mut response, &fb_model);

                    if let Some(usage) = &response.usage {
                        total_input_tokens =
                            total_input_tokens.saturating_add(usage.prompt_tokens);
                        total_output_tokens =
                            total_output_tokens.saturating_add(usage.completion_tokens);
                        self.last_prompt_tokens = usage.prompt_tokens;
                        if let Err(err) = self.record_usage_with_model(
                            &fb_model,
                            turns_used,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                        ) {
                            return Err(self.report_error(&hook_session, "usage_record", err));
                        }
                        on_event(StreamEvent::TokenUsage {
                            input_tokens: total_input_tokens,
                            output_tokens: total_output_tokens,
                        });
                    }

                    let choice = response.choices.first().ok_or_else(|| {
                        self.report_error(
                            &hook_session,
                            "empty_response",
                            AgentError::Provider(ProviderError::EmptyChoices),
                        )
                    })?;
                    let assistant_msg = &choice.message;

                    if let Some(tool_calls) = &assistant_msg.tool_calls {
                        if !tool_calls.is_empty() {
                            let (effective_tool_calls, veto_reasons) =
                                self.prepare_tool_calls(&hook_session, tool_calls, false);
                            let tool_calls = effective_tool_calls;

                            let mut msg = ChatMessage::assistant_with_tool_calls(
                                assistant_msg.content.clone(),
                                tool_calls.clone(),
                            );
                            msg.provider_metadata = assistant_msg.provider_metadata.clone();
                            if let Some(message) =
                                self.push_message_with_lua_hooks(&hook_session, msg)
                            {
                                if let Some(text) = message.content_text() {
                                    if !text.is_empty() {
                                        self.trajectory.record_assistant_message(text);
                                    }
                                }
                            }

                            // Emit start events.
                            for tc in tool_calls.iter() {
                                on_event(StreamEvent::ToolCallStart {
                                    name: &tc.function.name,
                                    call_id: &tc.id,
                                    args_summary: summarize_args(&tc.function.arguments),
                                });
                            }

                            // Execute tool calls in parallel.
                            tool_calls_made += tool_calls.len();
                            let executable_tool_calls: Vec<ToolCallEntry> = tool_calls
                                .iter()
                                .zip(veto_reasons.iter())
                                .filter(|(_, veto)| veto.is_none())
                                .map(|(tc, _)| tc.clone())
                                .collect();
                            let tool_exec_start = Instant::now();
                            let executed_results = if executable_tool_calls.is_empty() {
                                Vec::new()
                            } else {
                                match execute_tool_calls_parallel(
                                    &self.tools,
                                    &self.subagent_spawner,
                                    &executable_tool_calls,
                                    self.config.max_concurrency,
                                    self.config.tool_timeout_secs,
                                    self.config.tool_policy.as_ref(),
                                )
                                .await
                                {
                                    Ok(results) => results,
                                    Err(err) => {
                                        return Err(self.report_error(
                                            &hook_session,
                                            "tool_execution",
                                            err,
                                        ));
                                    }
                                }
                            };
                            let tool_exec_duration = tool_exec_start.elapsed();
                            let tool_exec_duration_ms = tool_exec_duration.as_millis() as u64;

                            let mut clarification = None;
                            let mut executed_results = executed_results.into_iter();
                            for (tc, veto_reason) in
                                tool_calls.iter().zip(veto_reasons.into_iter())
                            {
                                let lua_vetoed = veto_reason.is_some();
                                let (mut result, requires_input) = match veto_reason {
                                    Some(reason) => (
                                        format!(
                                            "Error: tool call blocked by Lua hook: {reason}"
                                        ),
                                        false,
                                    ),
                                    None => executed_results.next().expect(
                                        "executed tool results should align with allowed calls",
                                    ),
                                };
                                result = sanitize::sanitize_credentials(&result);
                                if self.config.core_tools.is_some()
                                    && tc.function.name == "find_tools"
                                    && !result.starts_with("Error:")
                                    && !result.starts_with("No tools")
                                {
                                    for line in result.lines() {
                                        let trimmed = line.trim();
                                        if let Some(rest) = trimmed.strip_prefix("**") {
                                            if let Some(name_end) = rest.find("**") {
                                                self.discover_tool(&rest[..name_end]);
                                            }
                                        }
                                    }
                                }
                                let tool_success = !result.starts_with("Error:");
                                let result =
                                    self.run_lua_post_tool_call(&tc.function.name, &result);
                                on_event(StreamEvent::ToolCallEnd {
                                    name: &tc.function.name,
                                    call_id: &tc.id,
                                    duration_ms: tool_exec_duration_ms,
                                    success: tool_success,
                                });
                                if !tool_success {
                                    let count = self
                                        .tool_failure_counts
                                        .entry(tc.function.name.clone())
                                        .or_insert(0);
                                    *count += 1;
                                } else {
                                    self.tool_failure_counts.remove(&tc.function.name);
                                }
                                self.fire_shell_hooks(
                                    HookEvent::PostToolCall,
                                    serde_json::json!({
                                        "session_id": hook_session,
                                        "tool_name": tc.function.name,
                                        "tool_call_id": tc.id,
                                        "success": tool_success,
                                        "result": result,
                                        "requires_input": requires_input,
                                        "streaming": false,
                                        "lua_vetoed": lua_vetoed,
                                    }),
                                );
                                let result = self
                                    .push_message_with_lua_hooks(
                                        &hook_session,
                                        ChatMessage::tool_result(&tc.id, result),
                                    )
                                    .and_then(|message| message.content_text().map(str::to_owned))
                                    .unwrap_or_default();
                                self.trajectory
                                    .record_tool_result(&tc.function.name, &result);
                                if requires_input {
                                    on_event(StreamEvent::ClarificationNeeded {
                                        question: &result,
                                    });
                                    clarification = Some(result.clone());
                                }
                            }

                            self.maybe_inject_stuck_nudge();

                            if let Some(question) = clarification {
                                self.save_trajectory();
                                let result = AgentResult {
                                    response: String::new(),
                                    turns_used,
                                    tool_calls_made,
                                    finished_naturally: false,
                                    total_input_tokens,
                                    total_output_tokens,
                                    estimated_cost: Some(self.cost.total_cost),
                                    pending_clarification: Some(question),
                                };
                                return Ok(self.finalize_turn(&hook_session, result, false));
                            }

                            self.maybe_inject_memory_nudge(tool_calls_made);
                            continue;
                        }
                    }

                    let mut response_text = assistant_msg.content_text().unwrap_or("").to_owned();

                    // Run output guardrails if configured (streaming path)
                    if let Some(ref cg) = self.compiled_guardrails {
                        let gr = cg.check_output(&response_text);
                        if !gr.passed {
                            response_text = format!(
                                "Response blocked by guardrails: {}",
                                format_blocked_reasons(&gr)
                            );
                        } else {
                            response_text = gr.content;
                        }
                    }

                    let mut msg = ChatMessage::assistant(&response_text);
                    msg.provider_metadata = assistant_msg.provider_metadata.clone();
                    let response_text = self
                        .push_message_with_lua_hooks(&hook_session, msg)
                        .and_then(|message| message.content_text().map(str::to_owned))
                        .unwrap_or_default();
                    if !response_text.is_empty() {
                        self.trajectory.record_assistant_message(&response_text);
                    }

                    self.maybe_inject_skill_nudge(tool_calls_made);
                    self.save_trajectory();
                    let result = AgentResult {
                        response: response_text,
                        turns_used,
                        tool_calls_made,
                        finished_naturally: !matches!(
                            choice.finish_reason.as_deref(),
                            Some("length") | Some("incomplete")
                        ),
                        total_input_tokens,
                        total_output_tokens,
                        estimated_cost: Some(self.cost.total_cost),
                        pending_clarification: None,
                    };
                    return Ok(self.finalize_turn(&hook_session, result, true));
                }
            }
        }
    }
}
