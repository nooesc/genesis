use genesis_lua::hooks::{PostHookOutcome, PreHookOutcome};
use genesis_provider::{ChatMessage, ContentPart, MessageContent};
use tracing::warn;

use super::{AgentError, AgentLoop, AgentResult};
use crate::hooks::HookEvent;

pub(crate) fn message_image_count(message: &ChatMessage) -> usize {
    match message.content.as_ref() {
        Some(MessageContent::Parts(parts)) => parts
            .iter()
            .filter(|part| matches!(part, ContentPart::ImageUrl { .. }))
            .count(),
        _ => 0,
    }
}

pub(crate) fn set_message_text(message: &mut ChatMessage, text: String) {
    match message.content.as_mut() {
        Some(MessageContent::Text(current)) => *current = text,
        Some(MessageContent::Parts(parts)) => {
            if let Some(ContentPart::Text { text: current }) = parts
                .iter_mut()
                .find(|part| matches!(part, ContentPart::Text { .. }))
            {
                *current = text;
            } else {
                parts.insert(0, ContentPart::Text { text });
            }
        }
        None => {
            message.content = Some(MessageContent::Text(text));
        }
    }
}

pub(crate) fn suppress_message_text(message: &mut ChatMessage) -> bool {
    if message
        .tool_calls
        .as_ref()
        .is_some_and(|tool_calls| !tool_calls.is_empty())
    {
        message.content = None;
        return true;
    }

    match message.content.as_mut() {
        Some(MessageContent::Parts(parts)) => {
            parts.retain(|part| !matches!(part, ContentPart::Text { .. }));
            !parts.is_empty()
        }
        _ => false,
    }
}

impl AgentLoop {
    pub(crate) fn fire_shell_hooks(&mut self, event: HookEvent, context: serde_json::Value) {
        let results = self.hook_runner.run_hooks(event, &context);
        self.hook_results.extend(results);
    }

    pub(crate) fn run_lua_on_message(
        &self,
        role: &str,
        content: &str,
        tool_call_count: usize,
        image_count: usize,
    ) -> PreHookOutcome<String> {
        let Some(runtime) = self.lua_runtime.as_ref() else {
            return PreHookOutcome::Allow(content.to_owned());
        };

        match runtime.run_on_message(role, content, tool_call_count, image_count) {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!(error = %error, role = %role, "lua on_message hook failed");
                PreHookOutcome::Allow(content.to_owned())
            }
        }
    }

    pub(crate) fn run_lua_pre_turn(&self, user_message: &str) -> PreHookOutcome<String> {
        let Some(runtime) = self.lua_runtime.as_ref() else {
            return PreHookOutcome::Allow(user_message.to_owned());
        };

        match runtime.run_pre_turn(user_message) {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!(error = %error, "lua pre-turn hook failed");
                PreHookOutcome::Allow(user_message.to_owned())
            }
        }
    }

    pub(crate) fn run_lua_pre_tool_call(
        &self,
        tool_name: &str,
        arguments: &str,
    ) -> PreHookOutcome<String> {
        let Some(runtime) = self.lua_runtime.as_ref() else {
            return PreHookOutcome::Allow(arguments.to_owned());
        };

        match runtime.run_pre_tool_call(tool_name, arguments) {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!(error = %error, tool_name = %tool_name, "lua pre-tool-call hook failed");
                PreHookOutcome::Allow(arguments.to_owned())
            }
        }
    }

    pub(crate) fn run_lua_post_tool_call(&self, tool_name: &str, output: &str) -> String {
        let Some(runtime) = self.lua_runtime.as_ref() else {
            return output.to_owned();
        };

        match runtime.run_post_tool_call(tool_name, output) {
            Ok(PostHookOutcome::Keep(current)) | Ok(PostHookOutcome::Rewrite(current)) => current,
            Err(error) => {
                warn!(error = %error, tool_name = %tool_name, "lua post-tool-call hook failed");
                output.to_owned()
            }
        }
    }

    pub(crate) fn run_lua_post_turn(&self, response: &str) -> String {
        let Some(runtime) = self.lua_runtime.as_ref() else {
            return response.to_owned();
        };

        match runtime.run_post_turn(response) {
            Ok(PostHookOutcome::Keep(current)) | Ok(PostHookOutcome::Rewrite(current)) => current,
            Err(error) => {
                warn!(error = %error, "lua post-turn hook failed");
                response.to_owned()
            }
        }
    }

    pub(crate) fn run_lua_personality_transform(&self, response: &str) -> String {
        let Some(runtime) = self.lua_runtime.as_ref() else {
            return response.to_owned();
        };

        runtime.transform_personality_response(response)
    }

    pub(crate) fn record_lua_completed_turn(&self, result: &AgentResult) {
        if let Some(runtime) = &self.lua_runtime {
            runtime.record_completed_turn(
                result
                    .total_input_tokens
                    .saturating_add(result.total_output_tokens),
            );
        }
    }

    pub(crate) fn run_lua_on_error(&self, stage: &str, error: &AgentError) {
        if let Some(runtime) = &self.lua_runtime {
            if let Err(lua_error) = runtime.run_on_error(stage, &error.to_string()) {
                warn!(error = %lua_error, stage = %stage, "lua on_error hook failed");
            }
        }
    }

    pub(crate) fn run_lua_on_complete(&self) {
        if let Some(runtime) = &self.lua_runtime {
            if let Err(error) = runtime.run_on_complete() {
                warn!(error = %error, "lua on_complete hook failed");
            }
        }
    }

    pub(crate) fn push_message_with_lua_hooks(
        &mut self,
        session_id: &str,
        mut message: ChatMessage,
    ) -> Option<ChatMessage> {
        let Some(original_content) = message.content_text().map(str::to_owned) else {
            self.messages.push(message.clone());
            return Some(message);
        };
        let tool_call_count = message.tool_calls.as_ref().map_or(0, Vec::len);
        let image_count = message_image_count(&message);

        match self.run_lua_on_message(
            &message.role,
            &original_content,
            tool_call_count,
            image_count,
        ) {
            PreHookOutcome::Allow(rewritten) => {
                set_message_text(&mut message, rewritten.clone());
                self.fire_shell_hooks(
                    HookEvent::OnMessage,
                    serde_json::json!({
                        "session_id": session_id,
                        "role": &message.role,
                        "content": rewritten,
                        "tool_call_count": tool_call_count,
                        "image_count": image_count,
                        "lua_vetoed": false,
                    }),
                );
                self.messages.push(message.clone());
                Some(message)
            }
            PreHookOutcome::Veto { reason } => {
                self.fire_shell_hooks(
                    HookEvent::OnMessage,
                    serde_json::json!({
                        "session_id": session_id,
                        "role": &message.role,
                        "content": original_content,
                        "tool_call_count": tool_call_count,
                        "image_count": image_count,
                        "lua_vetoed": true,
                        "lua_reason": reason,
                    }),
                );

                if suppress_message_text(&mut message) {
                    self.messages.push(message.clone());
                    Some(message)
                } else {
                    None
                }
            }
        }
    }

    pub(crate) fn finalize_turn(
        &mut self,
        session_id: &str,
        mut result: AgentResult,
        fire_complete: bool,
    ) -> AgentResult {
        self.record_lua_completed_turn(&result);
        result.response = self.run_lua_post_turn(&result.response);
        result.response = self.run_lua_personality_transform(&result.response);
        self.fire_shell_hooks(
            HookEvent::PostTurn,
            self.turn_result_context(session_id, &result),
        );
        if fire_complete {
            self.run_lua_on_complete();
            self.fire_shell_hooks(
                HookEvent::OnComplete,
                self.turn_result_context(session_id, &result),
            );
        }
        self.hooks.on_turn_end(session_id, &result);
        result
    }

    pub(crate) fn prepare_tool_calls(
        &mut self,
        hook_session: &str,
        tool_calls: &[genesis_provider::ToolCallEntry],
        streaming: bool,
    ) -> (Vec<genesis_provider::ToolCallEntry>, Vec<Option<String>>) {
        let mut effective_calls = Vec::with_capacity(tool_calls.len());
        let mut veto_reasons = Vec::with_capacity(tool_calls.len());

        for tc in tool_calls {
            self.trajectory
                .record_tool_call(&tc.function.name, &tc.function.arguments);
            self.hooks
                .on_tool_call_start(hook_session, &tc.function.name);

            match self.run_lua_pre_tool_call(&tc.function.name, &tc.function.arguments) {
                PreHookOutcome::Allow(arguments) => {
                    let mut effective = tc.clone();
                    effective.function.arguments = arguments;
                    self.fire_shell_hooks(
                        HookEvent::PreToolCall,
                        serde_json::json!({
                            "session_id": hook_session,
                            "tool_name": &effective.function.name,
                            "tool_call_id": &effective.id,
                            "arguments": &effective.function.arguments,
                            "lua_vetoed": false,
                            "streaming": streaming,
                        }),
                    );
                    effective_calls.push(effective);
                    veto_reasons.push(None);
                }
                PreHookOutcome::Veto { reason } => {
                    self.fire_shell_hooks(
                        HookEvent::PreToolCall,
                        serde_json::json!({
                            "session_id": hook_session,
                            "tool_name": &tc.function.name,
                            "tool_call_id": &tc.id,
                            "arguments": &tc.function.arguments,
                            "lua_vetoed": true,
                            "lua_reason": reason,
                            "streaming": streaming,
                        }),
                    );
                    effective_calls.push(tc.clone());
                    veto_reasons.push(reason);
                }
            }
        }

        (effective_calls, veto_reasons)
    }

    pub(crate) fn report_error(
        &mut self,
        session_id: &str,
        stage: &str,
        error: AgentError,
    ) -> AgentError {
        self.run_lua_on_error(stage, &error);
        self.fire_shell_hooks(
            HookEvent::OnError,
            serde_json::json!({
                "session_id": session_id,
                "stage": stage,
                "error": error.to_string(),
            }),
        );
        error
    }

    pub(crate) fn turn_result_context(
        &self,
        session_id: &str,
        result: &AgentResult,
    ) -> serde_json::Value {
        serde_json::json!({
            "session_id": session_id,
            "response": result.response,
            "turns_used": result.turns_used,
            "tool_calls_made": result.tool_calls_made,
            "finished_naturally": result.finished_naturally,
            "total_input_tokens": result.total_input_tokens,
            "total_output_tokens": result.total_output_tokens,
            "estimated_cost": result.estimated_cost,
            "pending_clarification": result.pending_clarification,
        })
    }
}
