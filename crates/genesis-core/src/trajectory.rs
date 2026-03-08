use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_TRAJECTORY_STEPS: usize = 10_000;
const MAX_STEP_CONTENT: usize = 8 * 1024; // 8 KiB per field

/// A single step in a trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryStep {
    pub step_index: usize,
    pub timestamp: String,
    pub action_type: ActionType,
    pub content: String,
    /// Tool name if this is a tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool arguments as JSON string if tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_arguments: Option<String>,
    /// Tool result if this is a tool result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
    /// Token usage for this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    SystemMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u32,
    pub output: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    pub session_id: String,
    pub model: String,
    pub system_prompt_hash: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub steps: Vec<TrajectoryStep>,
    pub outcome: Option<TrajectoryOutcome>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TrajectoryOutcome {
    Success,
    Failure { reason: String },
    Abandoned,
}

/// Records trajectory steps as they happen in the agent loop.
pub struct TrajectoryRecorder {
    trajectory: Trajectory,
    enabled: bool,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

fn truncate_field(s: &str) -> String {
    if s.len() <= MAX_STEP_CONTENT {
        s.to_owned()
    } else {
        let mut t = s[..MAX_STEP_CONTENT].to_string();
        t.push_str("... (truncated)");
        t
    }
}

impl TrajectoryRecorder {
    /// Create a new enabled recorder for a session.
    pub fn new(session_id: &str, model: &str, system_prompt: &str) -> Self {
        Self {
            trajectory: Trajectory {
                session_id: session_id.to_owned(),
                model: model.to_owned(),
                system_prompt_hash: sha256_hex(system_prompt),
                started_at: now_rfc3339(),
                completed_at: None,
                steps: Vec::new(),
                outcome: None,
                tags: Vec::new(),
            },
            enabled: true,
        }
    }

    /// Returns a no-op recorder that discards all events.
    pub fn disabled() -> Self {
        Self {
            trajectory: Trajectory {
                session_id: String::new(),
                model: String::new(),
                system_prompt_hash: String::new(),
                started_at: String::new(),
                completed_at: None,
                steps: Vec::new(),
                outcome: None,
                tags: Vec::new(),
            },
            enabled: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn record_user_message(&mut self, content: &str) {
        if !self.enabled {
            return;
        }
        self.push_step(ActionType::UserMessage, content, None, None, None, None);
    }

    pub fn record_assistant_message(&mut self, content: &str) {
        if !self.enabled {
            return;
        }
        self.push_step(
            ActionType::AssistantMessage,
            content,
            None,
            None,
            None,
            None,
        );
    }

    pub fn record_tool_call(&mut self, name: &str, arguments: &str) {
        if !self.enabled {
            return;
        }
        self.push_step(
            ActionType::ToolCall,
            &format!("tool_call: {name}"),
            Some(name),
            Some(arguments),
            None,
            None,
        );
    }

    pub fn record_tool_result(&mut self, name: &str, result: &str) {
        if !self.enabled {
            return;
        }
        self.push_step(
            ActionType::ToolResult,
            &format!("tool_result: {name}"),
            Some(name),
            None,
            Some(result),
            None,
        );
    }

    pub fn record_system_message(&mut self, content: &str) {
        if !self.enabled {
            return;
        }
        self.push_step(ActionType::SystemMessage, content, None, None, None, None);
    }

    pub fn set_outcome(&mut self, outcome: TrajectoryOutcome) {
        if !self.enabled {
            return;
        }
        self.trajectory.outcome = Some(outcome);
    }

    pub fn add_tag(&mut self, tag: &str) {
        if !self.enabled {
            return;
        }
        if !self.trajectory.tags.contains(&tag.to_owned()) {
            self.trajectory.tags.push(tag.to_owned());
        }
    }

    /// Mark the trajectory as complete by setting `completed_at`.
    pub fn finish(&mut self) {
        if !self.enabled {
            return;
        }
        self.trajectory.completed_at = Some(now_rfc3339());
    }

    /// Export the trajectory as a pretty-printed JSON string.
    pub fn export_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.trajectory)
    }

    /// Export the trajectory in ShareGPT format for training data.
    ///
    /// Produces a JSON array of `{"from": "<role>", "value": "<content>"}` objects.
    /// Roles: `"human"`, `"gpt"`, `"tool_call"`, `"tool_result"`, `"system"`.
    pub fn export_sharegpt(&self) -> Result<String, serde_json::Error> {
        let entries: Vec<ShareGptEntry> = self
            .trajectory
            .steps
            .iter()
            .map(|step| {
                let from = match step.action_type {
                    ActionType::UserMessage => "human",
                    ActionType::AssistantMessage => "gpt",
                    ActionType::ToolCall => "tool_call",
                    ActionType::ToolResult => "tool_result",
                    ActionType::SystemMessage => "system",
                };

                let value = match step.action_type {
                    ActionType::ToolCall => {
                        // Include tool name and arguments in the value for richer training signal.
                        let name = step.tool_name.as_deref().unwrap_or("unknown");
                        let args = step.tool_arguments.as_deref().unwrap_or("{}");
                        format!("{name}: {args}")
                    }
                    ActionType::ToolResult => {
                        let name = step.tool_name.as_deref().unwrap_or("unknown");
                        let result = step.tool_result.as_deref().unwrap_or("");
                        format!("{name}: {result}")
                    }
                    _ => step.content.clone(),
                };

                ShareGptEntry {
                    from: from.to_owned(),
                    value,
                }
            })
            .collect();

        serde_json::to_string_pretty(&entries)
    }

    pub fn step_count(&self) -> usize {
        self.trajectory.steps.len()
    }

    /// Borrow the underlying trajectory for inspection.
    pub fn trajectory(&self) -> &Trajectory {
        &self.trajectory
    }

    /// Save the trajectory to a JSON file. Creates the parent directory if needed.
    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        if !self.enabled {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.trajectory)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    /// Consume the recorder and return the trajectory.
    pub fn into_trajectory(self) -> Trajectory {
        self.trajectory
    }

    fn push_step(
        &mut self,
        action_type: ActionType,
        content: &str,
        tool_name: Option<&str>,
        tool_arguments: Option<&str>,
        tool_result: Option<&str>,
        tokens: Option<TokenUsage>,
    ) {
        if !self.enabled || self.trajectory.steps.len() >= MAX_TRAJECTORY_STEPS {
            return;
        }
        let step_index = self.trajectory.steps.len();
        self.trajectory.steps.push(TrajectoryStep {
            step_index,
            timestamp: now_rfc3339(),
            action_type,
            content: truncate_field(content),
            tool_name: tool_name.map(|s| s.to_owned()),
            tool_arguments: tool_arguments.map(|s| truncate_field(s)),
            tool_result: tool_result.map(|s| truncate_field(s)),
            tokens,
        });
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ShareGptEntry {
    from: String,
    value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_recorder_is_enabled_with_correct_metadata() {
        let recorder = TrajectoryRecorder::new("session-1", "gpt-4", "You are a helpful assistant.");
        assert!(recorder.is_enabled());
        assert_eq!(recorder.trajectory().session_id, "session-1");
        assert_eq!(recorder.trajectory().model, "gpt-4");
        assert!(!recorder.trajectory().system_prompt_hash.is_empty());
        assert!(!recorder.trajectory().started_at.is_empty());
        assert!(recorder.trajectory().completed_at.is_none());
        assert_eq!(recorder.step_count(), 0);
    }

    #[test]
    fn disabled_recorder_ignores_all_operations() {
        let mut recorder = TrajectoryRecorder::disabled();
        assert!(!recorder.is_enabled());

        recorder.record_user_message("hello");
        recorder.record_assistant_message("hi");
        recorder.record_tool_call("shell_exec", r#"{"command":"ls"}"#);
        recorder.record_tool_result("shell_exec", "file.txt");
        recorder.record_system_message("nudge");
        recorder.set_outcome(TrajectoryOutcome::Success);
        recorder.add_tag("test");
        recorder.finish();

        assert_eq!(recorder.step_count(), 0);
        assert!(recorder.trajectory().outcome.is_none());
        assert!(recorder.trajectory().tags.is_empty());
        assert!(recorder.trajectory().completed_at.is_none());
    }

    #[test]
    fn system_prompt_is_hashed_not_stored_verbatim() {
        let prompt = "You are a helpful assistant.";
        let recorder = TrajectoryRecorder::new("s-1", "m-1", prompt);

        let hash = &recorder.trajectory().system_prompt_hash;
        assert_ne!(hash, prompt);
        assert_eq!(hash.len(), 64); // SHA-256 hex is 64 chars

        // Same input produces same hash.
        let recorder2 = TrajectoryRecorder::new("s-2", "m-2", prompt);
        assert_eq!(
            recorder.trajectory().system_prompt_hash,
            recorder2.trajectory().system_prompt_hash,
        );
    }

    #[test]
    fn record_user_message_creates_step() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_user_message("hello world");

        assert_eq!(recorder.step_count(), 1);
        let step = &recorder.trajectory().steps[0];
        assert_eq!(step.step_index, 0);
        assert_eq!(step.action_type, ActionType::UserMessage);
        assert_eq!(step.content, "hello world");
        assert!(step.tool_name.is_none());
        assert!(step.tool_arguments.is_none());
        assert!(step.tool_result.is_none());
    }

    #[test]
    fn record_assistant_message_creates_step() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_assistant_message("I can help with that.");

        assert_eq!(recorder.step_count(), 1);
        let step = &recorder.trajectory().steps[0];
        assert_eq!(step.action_type, ActionType::AssistantMessage);
        assert_eq!(step.content, "I can help with that.");
    }

    #[test]
    fn record_tool_call_captures_name_and_arguments() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_tool_call("read_file", r#"{"path":"/tmp/foo.txt"}"#);

        assert_eq!(recorder.step_count(), 1);
        let step = &recorder.trajectory().steps[0];
        assert_eq!(step.action_type, ActionType::ToolCall);
        assert_eq!(step.tool_name.as_deref(), Some("read_file"));
        assert_eq!(
            step.tool_arguments.as_deref(),
            Some(r#"{"path":"/tmp/foo.txt"}"#)
        );
        assert!(step.tool_result.is_none());
    }

    #[test]
    fn record_tool_result_captures_name_and_result() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_tool_result("read_file", "contents of file");

        assert_eq!(recorder.step_count(), 1);
        let step = &recorder.trajectory().steps[0];
        assert_eq!(step.action_type, ActionType::ToolResult);
        assert_eq!(step.tool_name.as_deref(), Some("read_file"));
        assert_eq!(step.tool_result.as_deref(), Some("contents of file"));
        assert!(step.tool_arguments.is_none());
    }

    #[test]
    fn record_system_message_creates_step() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_system_message("memory consolidation nudge");

        assert_eq!(recorder.step_count(), 1);
        let step = &recorder.trajectory().steps[0];
        assert_eq!(step.action_type, ActionType::SystemMessage);
        assert_eq!(step.content, "memory consolidation nudge");
    }

    #[test]
    fn step_indices_increment_correctly() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_user_message("first");
        recorder.record_assistant_message("second");
        recorder.record_tool_call("echo", "{}");
        recorder.record_tool_result("echo", "done");
        recorder.record_assistant_message("third");

        assert_eq!(recorder.step_count(), 5);
        for (i, step) in recorder.trajectory().steps.iter().enumerate() {
            assert_eq!(step.step_index, i);
        }
    }

    #[test]
    fn set_outcome_and_finish() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        assert!(recorder.trajectory().outcome.is_none());
        assert!(recorder.trajectory().completed_at.is_none());

        recorder.set_outcome(TrajectoryOutcome::Success);
        recorder.finish();

        assert_eq!(recorder.trajectory().outcome, Some(TrajectoryOutcome::Success));
        assert!(recorder.trajectory().completed_at.is_some());
    }

    #[test]
    fn set_outcome_failure_with_reason() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.set_outcome(TrajectoryOutcome::Failure {
            reason: "budget exceeded".to_owned(),
        });

        assert_eq!(
            recorder.trajectory().outcome,
            Some(TrajectoryOutcome::Failure {
                reason: "budget exceeded".to_owned(),
            })
        );
    }

    #[test]
    fn set_outcome_abandoned() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.set_outcome(TrajectoryOutcome::Abandoned);
        assert_eq!(
            recorder.trajectory().outcome,
            Some(TrajectoryOutcome::Abandoned)
        );
    }

    #[test]
    fn add_tag_deduplicates() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.add_tag("demo");
        recorder.add_tag("training");
        recorder.add_tag("demo"); // duplicate

        assert_eq!(recorder.trajectory().tags, vec!["demo", "training"]);
    }

    #[test]
    fn export_json_round_trips() {
        let mut recorder = TrajectoryRecorder::new("s-1", "gpt-4", "system");
        recorder.record_user_message("hello");
        recorder.record_assistant_message("hi");
        recorder.add_tag("test");
        recorder.set_outcome(TrajectoryOutcome::Success);
        recorder.finish();

        let json = recorder.export_json().expect("export_json should succeed");
        let parsed: Trajectory =
            serde_json::from_str(&json).expect("exported JSON should parse back");

        assert_eq!(parsed.session_id, "s-1");
        assert_eq!(parsed.model, "gpt-4");
        assert_eq!(parsed.steps.len(), 2);
        assert_eq!(parsed.steps[0].action_type, ActionType::UserMessage);
        assert_eq!(parsed.steps[1].action_type, ActionType::AssistantMessage);
        assert_eq!(parsed.tags, vec!["test"]);
        assert_eq!(parsed.outcome, Some(TrajectoryOutcome::Success));
        assert!(parsed.completed_at.is_some());
    }

    #[test]
    fn export_sharegpt_produces_correct_format() {
        let mut recorder = TrajectoryRecorder::new("s-1", "gpt-4", "system");
        recorder.record_user_message("What files are in /tmp?");
        recorder.record_tool_call("shell_exec", r#"{"command":"ls /tmp"}"#);
        recorder.record_tool_result("shell_exec", "foo.txt\nbar.txt");
        recorder.record_assistant_message("There are two files: foo.txt and bar.txt.");

        let json = recorder
            .export_sharegpt()
            .expect("export_sharegpt should succeed");
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&json).expect("sharegpt JSON should parse");

        assert_eq!(entries.len(), 4);

        assert_eq!(entries[0]["from"], "human");
        assert_eq!(entries[0]["value"], "What files are in /tmp?");

        assert_eq!(entries[1]["from"], "tool_call");
        assert!(entries[1]["value"]
            .as_str()
            .unwrap()
            .contains("shell_exec"));

        assert_eq!(entries[2]["from"], "tool_result");
        assert!(entries[2]["value"]
            .as_str()
            .unwrap()
            .contains("foo.txt\nbar.txt"));

        assert_eq!(entries[3]["from"], "gpt");
        assert!(entries[3]["value"]
            .as_str()
            .unwrap()
            .contains("two files"));
    }

    #[test]
    fn export_sharegpt_system_message_role() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_system_message("consolidation nudge");

        let json = recorder
            .export_sharegpt()
            .expect("export_sharegpt should succeed");
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&json).expect("sharegpt JSON should parse");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["from"], "system");
        assert_eq!(entries[0]["value"], "consolidation nudge");
    }

    #[test]
    fn into_trajectory_consumes_recorder() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_user_message("test");

        let trajectory = recorder.into_trajectory();
        assert_eq!(trajectory.session_id, "s-1");
        assert_eq!(trajectory.steps.len(), 1);
    }

    #[test]
    fn full_conversation_trajectory() {
        let mut recorder = TrajectoryRecorder::new("session-42", "claude-sonnet-4-20250514", "You are Eve.");
        recorder.add_tag("demo");

        recorder.record_user_message("Read the config file.");
        recorder.record_tool_call("read_file", r#"{"path":"config.yaml"}"#);
        recorder.record_tool_result("read_file", "profile: operator\nprovider: openrouter");
        recorder.record_assistant_message("The config has profile 'operator' with openrouter.");
        recorder.record_user_message("Great, now list sessions.");
        recorder.record_tool_call("session_search", r#"{"query":"*"}"#);
        recorder.record_tool_result("session_search", "session-1, session-2");
        recorder.record_assistant_message("Found 2 sessions.");
        recorder.set_outcome(TrajectoryOutcome::Success);
        recorder.finish();

        assert_eq!(recorder.step_count(), 8);

        // Verify JSON export is valid.
        let json = recorder.export_json().expect("json export");
        let trajectory: Trajectory = serde_json::from_str(&json).expect("json parse");
        assert_eq!(trajectory.steps.len(), 8);
        assert!(trajectory.completed_at.is_some());

        // Verify ShareGPT export.
        let sharegpt = recorder.export_sharegpt().expect("sharegpt export");
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&sharegpt).expect("sharegpt parse");
        assert_eq!(entries.len(), 8);

        // Check alternation pattern: human, tool_call, tool_result, gpt, human, tool_call, tool_result, gpt
        let expected_roles = [
            "human",
            "tool_call",
            "tool_result",
            "gpt",
            "human",
            "tool_call",
            "tool_result",
            "gpt",
        ];
        for (i, expected) in expected_roles.iter().enumerate() {
            assert_eq!(
                entries[i]["from"].as_str().unwrap(),
                *expected,
                "step {i} should have role {expected}"
            );
        }
    }

    #[test]
    fn sha256_hex_produces_correct_hash() {
        // Known SHA-256 of empty string.
        let hash = sha256_hex("");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        // Known SHA-256 of "hello".
        let hash = sha256_hex("hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn timestamps_are_rfc3339() {
        let mut recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        recorder.record_user_message("check timestamp");

        let ts = &recorder.trajectory().steps[0].timestamp;
        // RFC 3339 timestamps contain 'T' and end with timezone offset or 'Z'.
        assert!(ts.contains('T'), "timestamp should contain 'T': {ts}");
        assert!(
            ts.ends_with('Z') || ts.contains('+'),
            "timestamp should end with Z or have offset: {ts}"
        );
    }

    #[test]
    fn save_to_file_creates_file() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let path = dir.path().join("trajectories").join("test.json");

        let mut recorder = TrajectoryRecorder::new("s-1", "gpt-4", "system");
        recorder.record_user_message("hello");
        recorder.record_assistant_message("hi");
        recorder.finish();

        recorder.save_to_file(&path).expect("save_to_file should succeed");
        assert!(path.exists(), "trajectory file should exist");
    }

    #[test]
    fn save_to_file_disabled_is_noop() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let path = dir.path().join("should_not_exist.json");

        let recorder = TrajectoryRecorder::disabled();
        recorder.save_to_file(&path).expect("save_to_file should succeed for disabled");
        assert!(!path.exists(), "file should not be created for disabled recorder");
    }

    #[test]
    fn save_to_file_produces_valid_json() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let path = dir.path().join("valid.json");

        let mut recorder = TrajectoryRecorder::new("s-42", "claude-sonnet-4-20250514", "You are Eve.");
        recorder.record_user_message("What files are here?");
        recorder.record_tool_call("shell_exec", r#"{"command":"ls"}"#);
        recorder.record_tool_result("shell_exec", "foo.txt\nbar.txt");
        recorder.record_assistant_message("Found foo.txt and bar.txt.");
        recorder.set_outcome(TrajectoryOutcome::Success);
        recorder.finish();

        recorder.save_to_file(&path).expect("save_to_file should succeed");

        let contents = std::fs::read_to_string(&path).expect("should read file");
        let parsed: Trajectory =
            serde_json::from_str(&contents).expect("file contents should deserialize as Trajectory");

        assert_eq!(parsed.session_id, "s-42");
        assert_eq!(parsed.model, "claude-sonnet-4-20250514");
        assert_eq!(parsed.steps.len(), 4);
        assert_eq!(parsed.outcome, Some(TrajectoryOutcome::Success));
        assert!(parsed.completed_at.is_some());
    }

    #[test]
    fn export_json_empty_trajectory() {
        let recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        let json = recorder.export_json().expect("should work for empty trajectory");
        let parsed: Trajectory = serde_json::from_str(&json).expect("should parse");
        assert!(parsed.steps.is_empty());
        assert!(parsed.outcome.is_none());
        assert!(parsed.completed_at.is_none());
    }

    #[test]
    fn export_sharegpt_empty_trajectory() {
        let recorder = TrajectoryRecorder::new("s-1", "m-1", "sys");
        let json = recorder
            .export_sharegpt()
            .expect("should work for empty trajectory");
        let entries: Vec<serde_json::Value> = serde_json::from_str(&json).expect("should parse");
        assert!(entries.is_empty());
    }

    #[test]
    fn outcome_serialization_round_trips() {
        let outcomes = vec![
            TrajectoryOutcome::Success,
            TrajectoryOutcome::Failure {
                reason: "timeout".to_owned(),
            },
            TrajectoryOutcome::Abandoned,
        ];

        for outcome in outcomes {
            let json = serde_json::to_string(&outcome).expect("should serialize");
            let parsed: TrajectoryOutcome =
                serde_json::from_str(&json).expect("should deserialize");
            assert_eq!(parsed, outcome);
        }
    }
}
