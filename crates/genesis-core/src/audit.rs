use std::path::Path;
use std::sync::Arc;

use genesis_storage::AuditLogStore;
use serde_json::json;
use tracing::warn;

use crate::agent_loop::{AgentHooks, AgentResult};

/// `AgentHooks` implementation that writes all lifecycle events to the audit log.
///
/// Events recorded:
/// - `turn_start` — user turn begins
/// - `turn_end` — turn completes with result summary
/// - `tool_call_start` — tool execution begins
/// - `tool_call_end` — tool execution completes (success/failure + duration)
/// - `llm_request` — LLM API call initiated
/// - `llm_response` — LLM API response received with token counts
/// - `context_prune` — context compression triggered
/// - `stuck_loop` — repeated tool failure detected
pub struct AuditHooks {
    store: AuditLogStore,
}

impl AuditHooks {
    pub fn new(database_path: &Path) -> Self {
        Self {
            store: AuditLogStore::new(database_path),
        }
    }

    /// Convenience: wrap in Arc for use with `agent.set_hooks()`.
    pub fn shared(database_path: &Path) -> Arc<Self> {
        Arc::new(Self::new(database_path))
    }

    fn log_event(&self, session_id: &str, event_type: &str, details: &serde_json::Value) {
        if let Err(e) = self.store.log(Some(session_id), event_type, details) {
            warn!(error = %e, event_type, "failed to write audit log entry");
        }
    }
}

impl AgentHooks for AuditHooks {
    fn on_turn_start(&self, session_id: &str, user_message: &str) {
        // Truncate very long messages to keep the audit log manageable
        let preview = if user_message.len() > 200 {
            format!("{}...", &user_message[..200])
        } else {
            user_message.to_owned()
        };
        self.log_event(session_id, "turn_start", &json!({
            "message_preview": preview,
        }));
    }

    fn on_turn_end(&self, session_id: &str, result: &AgentResult) {
        self.log_event(session_id, "turn_end", &json!({
            "turns_used": result.turns_used,
            "tool_calls_made": result.tool_calls_made,
            "finished_naturally": result.finished_naturally,
            "input_tokens": result.total_input_tokens,
            "output_tokens": result.total_output_tokens,
            "estimated_cost": result.estimated_cost,
        }));
    }

    fn on_tool_call_start(&self, session_id: &str, tool_name: &str) {
        self.log_event(session_id, "tool_call_start", &json!({
            "tool": tool_name,
        }));
    }

    fn on_tool_call_end(
        &self,
        session_id: &str,
        tool_name: &str,
        success: bool,
        duration_ms: u64,
    ) {
        self.log_event(session_id, "tool_call_end", &json!({
            "tool": tool_name,
            "success": success,
            "duration_ms": duration_ms,
        }));
    }

    fn on_llm_request(&self, session_id: &str, model: &str, turn: usize) {
        self.log_event(session_id, "llm_request", &json!({
            "model": model,
            "turn": turn,
        }));
    }

    fn on_llm_response(
        &self,
        session_id: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) {
        self.log_event(session_id, "llm_response", &json!({
            "model": model,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        }));
    }

    fn on_context_prune(&self, session_id: &str, messages_before: usize, messages_after: usize) {
        self.log_event(session_id, "context_prune", &json!({
            "messages_before": messages_before,
            "messages_after": messages_after,
        }));
    }

    fn on_stuck_loop(&self, session_id: &str, tool_name: &str, failure_count: usize) {
        self.log_event(session_id, "stuck_loop", &json!({
            "tool": tool_name,
            "failure_count": failure_count,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn audit_hooks_records_turn_lifecycle() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).unwrap();

        let hooks = AuditHooks::new(&db_path);
        hooks.on_turn_start("s1", "Hello world");
        hooks.on_tool_call_start("s1", "shell_execute");
        hooks.on_tool_call_end("s1", "shell_execute", true, 150);
        hooks.on_llm_request("s1", "gpt-4", 1);
        hooks.on_llm_response("s1", "gpt-4", 500, 200);
        hooks.on_turn_end("s1", &AgentResult {
            response: "Done".into(),
            turns_used: 2,
            tool_calls_made: 1,
            finished_naturally: true,
            total_input_tokens: 500,
            total_output_tokens: 200,
            estimated_cost: Some(0.001),
            pending_clarification: None,
        });

        let store = AuditLogStore::new(&db_path);
        let entries = store.by_session("s1", 100).unwrap();
        assert_eq!(entries.len(), 6);

        // Most recent first
        assert_eq!(entries[0].event_type, "turn_end");
        assert_eq!(entries[5].event_type, "turn_start");
    }

    #[test]
    fn audit_hooks_truncates_long_messages() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).unwrap();

        let hooks = AuditHooks::new(&db_path);
        let long_msg = "x".repeat(500);
        hooks.on_turn_start("s1", &long_msg);

        let store = AuditLogStore::new(&db_path);
        let entries = store.by_session("s1", 1).unwrap();
        let details: serde_json::Value = serde_json::from_str(&entries[0].details).unwrap();
        let preview = details["message_preview"].as_str().unwrap();
        assert!(preview.len() < 250);
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn audit_hooks_records_stuck_loop() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).unwrap();

        let hooks = AuditHooks::new(&db_path);
        hooks.on_stuck_loop("s1", "broken_tool", 3);

        let store = AuditLogStore::new(&db_path);
        let entries = store.by_event_type("stuck_loop", 10).unwrap();
        assert_eq!(entries.len(), 1);
        let details: serde_json::Value = serde_json::from_str(&entries[0].details).unwrap();
        assert_eq!(details["tool"], "broken_tool");
        assert_eq!(details["failure_count"], 3);
    }
}
