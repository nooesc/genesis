use crate::trajectory::{ActionType, Trajectory, TrajectoryStep, TrajectoryOutcome};
use serde::{Deserialize, Serialize};

/// Maximum length for tool result content in Light compression.
const LIGHT_RESULT_TRUNCATE: usize = 512;

/// Maximum length for tool arguments summary in Medium compression.
const MEDIUM_ARGS_TRUNCATE: usize = 80;
const DEFAULT_PROTECT_EDGE_TURNS: usize = 2;
const DEFAULT_MIDDLE_HIGHLIGHTS: usize = 4;
const DEFAULT_MIDDLE_SNIPPET_TRUNCATE: usize = 120;

/// A compressed trajectory optimized for model training.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedTrajectory {
    pub session_id: String,
    pub model: String,
    pub turns: Vec<CompressedTurn>,
    pub outcome: Option<TrajectoryOutcome>,
    pub tags: Vec<String>,
}

/// A single compressed turn — either user input or agent action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedTurn {
    pub role: CompressedRole,
    pub content: String,
    /// If this is an action turn, summarize what tools were used.
    pub tools_used: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CompressedRole {
    User,
    Assistant,
    /// A collapsed sequence of tool calls + results.
    ActionSummary,
}

/// Compression strategies.
#[derive(Debug, Clone, Copy)]
pub enum CompressionLevel {
    /// Keep everything but remove system messages and truncate tool results.
    Light,
    /// Collapse tool-call chains into action summaries.
    Medium,
    /// Only keep user messages and final assistant responses.
    Heavy,
}

/// Configuration for post-processing trajectories into training-oriented
/// compressed conversations while protecting the beginning and end.
#[derive(Debug, Clone)]
pub struct TrajectoryCompressionConfig {
    pub protect_first_turns: usize,
    pub protect_last_turns: usize,
    pub max_middle_highlights: usize,
    pub max_snippet_chars: usize,
}

impl Default for TrajectoryCompressionConfig {
    fn default() -> Self {
        Self {
            protect_first_turns: DEFAULT_PROTECT_EDGE_TURNS,
            protect_last_turns: DEFAULT_PROTECT_EDGE_TURNS,
            max_middle_highlights: DEFAULT_MIDDLE_HIGHLIGHTS,
            max_snippet_chars: DEFAULT_MIDDLE_SNIPPET_TRUNCATE,
        }
    }
}

/// Post-processes trajectories for training: keep the first and last N turns
/// verbatim, and collapse the middle region into a single summary turn.
#[derive(Debug, Clone, Default)]
pub struct TrajectoryCompressor {
    config: TrajectoryCompressionConfig,
}

/// Compress a trajectory into a training-ready format.
pub fn compress(trajectory: &Trajectory, level: CompressionLevel) -> CompressedTrajectory {
    let turns = match level {
        CompressionLevel::Light => compress_light(&trajectory.steps),
        CompressionLevel::Medium => compress_medium(&trajectory.steps),
        CompressionLevel::Heavy => compress_heavy(&trajectory.steps),
    };

    CompressedTrajectory {
        session_id: trajectory.session_id.clone(),
        model: trajectory.model.clone(),
        turns,
        outcome: trajectory.outcome.clone(),
        tags: trajectory.tags.clone(),
    }
}

impl TrajectoryCompressor {
    pub fn new(config: TrajectoryCompressionConfig) -> Self {
        Self { config }
    }

    pub fn compress_for_training(&self, trajectory: &Trajectory) -> CompressedTrajectory {
        let mut compressed = compress(trajectory, CompressionLevel::Medium);
        compressed.turns = self.compress_turns(&compressed.turns);
        compressed
    }

    fn compress_turns(&self, turns: &[CompressedTurn]) -> Vec<CompressedTurn> {
        let protect_first = self.config.protect_first_turns.min(turns.len());
        let protect_last = self
            .config
            .protect_last_turns
            .min(turns.len().saturating_sub(protect_first));

        if turns.len() <= protect_first + protect_last + 1 {
            return turns.to_vec();
        }

        let middle_start = protect_first;
        let middle_end = turns.len() - protect_last;
        let middle = &turns[middle_start..middle_end];

        let mut output = Vec::new();
        output.extend_from_slice(&turns[..protect_first]);
        output.push(self.summarize_middle(middle));
        output.extend_from_slice(&turns[middle_end..]);
        output
    }

    fn summarize_middle(&self, turns: &[CompressedTurn]) -> CompressedTurn {
        let mut user_turns = 0usize;
        let mut assistant_turns = 0usize;
        let mut action_turns = 0usize;
        let mut tools_used = Vec::<String>::new();
        let mut highlights = Vec::<String>::new();

        for turn in turns {
            match turn.role {
                CompressedRole::User => user_turns += 1,
                CompressedRole::Assistant => assistant_turns += 1,
                CompressedRole::ActionSummary => action_turns += 1,
            }

            for tool in &turn.tools_used {
                if !tools_used.contains(tool) {
                    tools_used.push(tool.clone());
                }
            }

            if highlights.len() < self.config.max_middle_highlights {
                highlights.push(format!(
                    "{}: {}",
                    turn_role_label(&turn.role),
                    truncate(&turn.content, self.config.max_snippet_chars)
                ));
            }
        }

        let tools_fragment = if tools_used.is_empty() {
            "none".to_owned()
        } else {
            tools_used.join(", ")
        };

        let mut content = format!(
            "Summary of {} middle turns: user={} assistant={} action={}.\nTools used: {}.",
            turns.len(),
            user_turns,
            assistant_turns,
            action_turns,
            tools_fragment
        );

        if !highlights.is_empty() {
            content.push_str("\nHighlights:\n");
            for highlight in highlights {
                content.push_str("- ");
                content.push_str(&highlight);
                content.push('\n');
            }
            content.pop();
        }

        CompressedTurn {
            role: CompressedRole::ActionSummary,
            content,
            tools_used,
        }
    }
}

/// Export compressed trajectory as ShareGPT format for training.
///
/// Produces a JSON array of `{"from": "<role>", "value": "<content>"}` objects.
/// Roles: `"human"` for User, `"gpt"` for Assistant, `"thought"` for ActionSummary.
pub fn to_sharegpt(compressed: &CompressedTrajectory) -> Vec<serde_json::Value> {
    compressed
        .turns
        .iter()
        .map(|turn| {
            let from = match turn.role {
                CompressedRole::User => "human",
                CompressedRole::Assistant => "gpt",
                CompressedRole::ActionSummary => "thought",
            };
            serde_json::json!({
                "from": from,
                "value": turn.content,
            })
        })
        .collect()
}

/// Export as ChatML format.
///
/// Produces a string of `<|im_start|>role\ncontent<|im_end|>` blocks.
/// ActionSummary turns are emitted as `assistant` role (they represent agent reasoning).
pub fn to_chatml(compressed: &CompressedTrajectory) -> String {
    let mut output = String::new();
    for turn in &compressed.turns {
        let role = match turn.role {
            CompressedRole::User => "user",
            CompressedRole::Assistant => "assistant",
            CompressedRole::ActionSummary => "assistant",
        };
        output.push_str(&format!(
            "<|im_start|>{role}\n{}<|im_end|>\n",
            turn.content
        ));
    }
    output
}

// ---------------------------------------------------------------------------
// Light compression
// ---------------------------------------------------------------------------

/// Light: drop SystemMessage steps, truncate tool results to 512 chars,
/// keep everything else as separate turns.
fn compress_light(steps: &[TrajectoryStep]) -> Vec<CompressedTurn> {
    steps
        .iter()
        .filter(|s| s.action_type != ActionType::SystemMessage)
        .map(|step| {
            let (role, content, tools_used) = match step.action_type {
                ActionType::UserMessage => (
                    CompressedRole::User,
                    step.content.clone(),
                    vec![],
                ),
                ActionType::AssistantMessage => (
                    CompressedRole::Assistant,
                    step.content.clone(),
                    vec![],
                ),
                ActionType::ToolCall => {
                    let tool_name = step.tool_name.clone().unwrap_or_default();
                    let args = step.tool_arguments.as_deref().unwrap_or("{}");
                    (
                        CompressedRole::ActionSummary,
                        format!("tool_call: {tool_name}({args})"),
                        vec![tool_name],
                    )
                }
                ActionType::ToolResult => {
                    let tool_name = step.tool_name.clone().unwrap_or_default();
                    let result = step.tool_result.as_deref().unwrap_or("");
                    let truncated = truncate(result, LIGHT_RESULT_TRUNCATE);
                    (
                        CompressedRole::ActionSummary,
                        format!("tool_result: {tool_name} -> {truncated}"),
                        vec![tool_name],
                    )
                }
                // SystemMessage is already filtered out above, but exhaustive match.
                ActionType::SystemMessage => unreachable!(),
            };
            CompressedTurn {
                role,
                content,
                tools_used,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Medium compression
// ---------------------------------------------------------------------------

/// Medium: drop SystemMessage, collapse consecutive ToolCall+ToolResult pairs
/// into a single ActionSummary turn, keep UserMessage and AssistantMessage.
fn compress_medium(steps: &[TrajectoryStep]) -> Vec<CompressedTurn> {
    let mut turns: Vec<CompressedTurn> = Vec::new();
    let mut i = 0;

    while i < steps.len() {
        let step = &steps[i];
        match step.action_type {
            ActionType::SystemMessage => {
                i += 1;
            }
            ActionType::UserMessage => {
                turns.push(CompressedTurn {
                    role: CompressedRole::User,
                    content: step.content.clone(),
                    tools_used: vec![],
                });
                i += 1;
            }
            ActionType::AssistantMessage => {
                turns.push(CompressedTurn {
                    role: CompressedRole::Assistant,
                    content: step.content.clone(),
                    tools_used: vec![],
                });
                i += 1;
            }
            ActionType::ToolCall | ActionType::ToolResult => {
                // Collect consecutive tool call/result steps into a single
                // action summary.
                let mut tool_names: Vec<String> = Vec::new();
                let mut summaries: Vec<String> = Vec::new();

                while i < steps.len()
                    && matches!(
                        steps[i].action_type,
                        ActionType::ToolCall | ActionType::ToolResult
                    )
                {
                    let s = &steps[i];
                    if s.action_type == ActionType::ToolCall {
                        let name = s.tool_name.as_deref().unwrap_or("unknown");
                        let args = s.tool_arguments.as_deref().unwrap_or("{}");
                        let brief_args = truncate(args, MEDIUM_ARGS_TRUNCATE);
                        summaries.push(format!("{name}({brief_args})"));
                        if !tool_names.contains(&name.to_owned()) {
                            tool_names.push(name.to_owned());
                        }
                    }
                    // ToolResult steps are consumed but not individually summarized;
                    // they are part of the chain the ToolCall initiated.
                    i += 1;
                }

                let content = if summaries.is_empty() {
                    "Used tools".to_owned()
                } else {
                    format!("Used {}", summaries.join(", "))
                };

                turns.push(CompressedTurn {
                    role: CompressedRole::ActionSummary,
                    content,
                    tools_used: tool_names,
                });
            }
        }
    }

    turns
}

// ---------------------------------------------------------------------------
// Heavy compression
// ---------------------------------------------------------------------------

/// Heavy: only keep UserMessage and the LAST AssistantMessage before the next
/// UserMessage (or the end of the trajectory). Produces clean user->assistant
/// pairs ideal for SFT training.
fn compress_heavy(steps: &[TrajectoryStep]) -> Vec<CompressedTurn> {
    // Strategy: scan through the steps and build user->assistant pairs.
    // For each UserMessage, find the last AssistantMessage before the next
    // UserMessage (or end of list). Pair them up.
    let mut turns: Vec<CompressedTurn> = Vec::new();
    let mut i = 0;

    while i < steps.len() {
        // Skip non-user messages until we find a UserMessage.
        if steps[i].action_type != ActionType::UserMessage {
            i += 1;
            continue;
        }

        // Found a UserMessage.
        turns.push(CompressedTurn {
            role: CompressedRole::User,
            content: steps[i].content.clone(),
            tools_used: vec![],
        });
        i += 1;

        // Now scan forward to find the last AssistantMessage before the next
        // UserMessage or end of list.
        let mut last_assistant: Option<&TrajectoryStep> = None;
        let mut j = i;
        while j < steps.len() && steps[j].action_type != ActionType::UserMessage {
            if steps[j].action_type == ActionType::AssistantMessage {
                last_assistant = Some(&steps[j]);
            }
            j += 1;
        }

        if let Some(assistant) = last_assistant {
            turns.push(CompressedTurn {
                role: CompressedRole::Assistant,
                content: assistant.content.clone(),
                tools_used: vec![],
            });
        }

        i = j;
    }

    turns
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate a string to `max_len` characters, appending "..." if truncated.
pub(crate) fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_owned()
    } else {
        let mut t = s[..max_len].to_string();
        t.push_str("...");
        t
    }
}

fn turn_role_label(role: &CompressedRole) -> &'static str {
    match role {
        CompressedRole::User => "user",
        CompressedRole::Assistant => "assistant",
        CompressedRole::ActionSummary => "action",
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::{ActionType, TokenUsage, Trajectory, TrajectoryOutcome, TrajectoryStep};

    /// Build a sample trajectory with a realistic agent conversation:
    ///   User -> ToolCall -> ToolResult -> ToolCall -> ToolResult -> Assistant
    ///   User -> Assistant
    ///   (plus a SystemMessage somewhere)
    fn sample_trajectory() -> Trajectory {
        Trajectory {
            session_id: "sess-1".to_owned(),
            model: "claude-sonnet-4-20250514".to_owned(),
            system_prompt_hash: "abc123".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: Some("2026-01-01T00:01:00Z".to_owned()),
            steps: vec![
                TrajectoryStep {
                    step_index: 0,
                    timestamp: "2026-01-01T00:00:01Z".to_owned(),
                    action_type: ActionType::UserMessage,
                    content: "What files are in /tmp?".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 1,
                    timestamp: "2026-01-01T00:00:02Z".to_owned(),
                    action_type: ActionType::SystemMessage,
                    content: "Memory consolidation nudge".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 2,
                    timestamp: "2026-01-01T00:00:03Z".to_owned(),
                    action_type: ActionType::ToolCall,
                    content: "tool_call: shell_exec".to_owned(),
                    tool_name: Some("shell_exec".to_owned()),
                    tool_arguments: Some(r#"{"command":"ls /tmp"}"#.to_owned()),
                    tool_result: None,
                    tokens: Some(TokenUsage {
                        input: 100,
                        output: 50,
                    }),
                },
                TrajectoryStep {
                    step_index: 3,
                    timestamp: "2026-01-01T00:00:04Z".to_owned(),
                    action_type: ActionType::ToolResult,
                    content: "tool_result: shell_exec".to_owned(),
                    tool_name: Some("shell_exec".to_owned()),
                    tool_arguments: None,
                    tool_result: Some("foo.txt\nbar.txt\nbaz.txt".to_owned()),
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 4,
                    timestamp: "2026-01-01T00:00:05Z".to_owned(),
                    action_type: ActionType::ToolCall,
                    content: "tool_call: read_file".to_owned(),
                    tool_name: Some("read_file".to_owned()),
                    tool_arguments: Some(r#"{"path":"/tmp/foo.txt"}"#.to_owned()),
                    tool_result: None,
                    tokens: Some(TokenUsage {
                        input: 150,
                        output: 30,
                    }),
                },
                TrajectoryStep {
                    step_index: 5,
                    timestamp: "2026-01-01T00:00:06Z".to_owned(),
                    action_type: ActionType::ToolResult,
                    content: "tool_result: read_file".to_owned(),
                    tool_name: Some("read_file".to_owned()),
                    tool_arguments: None,
                    tool_result: Some("Hello from foo!".to_owned()),
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 6,
                    timestamp: "2026-01-01T00:00:07Z".to_owned(),
                    action_type: ActionType::AssistantMessage,
                    content: "There are 3 files in /tmp: foo.txt, bar.txt, baz.txt. foo.txt contains 'Hello from foo!'.".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: Some(TokenUsage {
                        input: 200,
                        output: 80,
                    }),
                },
                TrajectoryStep {
                    step_index: 7,
                    timestamp: "2026-01-01T00:00:08Z".to_owned(),
                    action_type: ActionType::UserMessage,
                    content: "Thanks!".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 8,
                    timestamp: "2026-01-01T00:00:09Z".to_owned(),
                    action_type: ActionType::AssistantMessage,
                    content: "You're welcome!".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: Some(TokenUsage {
                        input: 50,
                        output: 10,
                    }),
                },
            ],
            outcome: Some(TrajectoryOutcome::Success),
            tags: vec!["demo".to_owned(), "file-browsing".to_owned()],
        }
    }

    fn empty_trajectory() -> Trajectory {
        Trajectory {
            session_id: "sess-empty".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "def456".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![],
            outcome: None,
            tags: vec![],
        }
    }

    fn simple_trajectory() -> Trajectory {
        // Just user -> assistant, no tool calls.
        Trajectory {
            session_id: "sess-simple".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "ghi789".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: Some("2026-01-01T00:00:05Z".to_owned()),
            steps: vec![
                TrajectoryStep {
                    step_index: 0,
                    timestamp: "2026-01-01T00:00:01Z".to_owned(),
                    action_type: ActionType::UserMessage,
                    content: "What is 2+2?".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 1,
                    timestamp: "2026-01-01T00:00:02Z".to_owned(),
                    action_type: ActionType::AssistantMessage,
                    content: "2+2 equals 4.".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: Some(TokenUsage {
                        input: 10,
                        output: 5,
                    }),
                },
            ],
            outcome: Some(TrajectoryOutcome::Success),
            tags: vec!["math".to_owned()],
        }
    }

    // -----------------------------------------------------------------------
    // compress() metadata tests
    // -----------------------------------------------------------------------

    #[test]
    fn compress_preserves_metadata() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Light);

        assert_eq!(compressed.session_id, "sess-1");
        assert_eq!(compressed.model, "claude-sonnet-4-20250514");
        assert_eq!(compressed.outcome, Some(TrajectoryOutcome::Success));
        assert_eq!(compressed.tags, vec!["demo", "file-browsing"]);
    }

    #[test]
    fn trajectory_compressor_preserves_edges_and_summarizes_middle() {
        let traj = sample_trajectory();
        let compressor = TrajectoryCompressor::new(TrajectoryCompressionConfig {
            protect_first_turns: 1,
            protect_last_turns: 1,
            max_middle_highlights: 3,
            max_snippet_chars: 60,
        });

        let compressed = compressor.compress_for_training(&traj);

        assert_eq!(compressed.turns.len(), 3);
        assert_eq!(compressed.turns[0].role, CompressedRole::User);
        assert_eq!(compressed.turns[0].content, "What files are in /tmp?");
        assert_eq!(compressed.turns[2].role, CompressedRole::Assistant);
        assert_eq!(compressed.turns[2].content, "You're welcome!");
        assert_eq!(compressed.turns[1].role, CompressedRole::ActionSummary);
        assert!(compressed.turns[1]
            .content
            .contains("Summary of 3 middle turns"));
        assert!(compressed.turns[1].content.contains("Tools used: shell_exec, read_file"));
        assert!(compressed.turns[1]
            .tools_used
            .contains(&"shell_exec".to_owned()));
        assert!(compressed.turns[1]
            .tools_used
            .contains(&"read_file".to_owned()));
    }

    #[test]
    fn trajectory_compressor_leaves_short_trajectories_alone() {
        let traj = simple_trajectory();
        let compressor = TrajectoryCompressor::default();

        let compressed = compressor.compress_for_training(&traj);

        assert_eq!(compressed.turns.len(), 2);
        assert_eq!(compressed.turns[0].role, CompressedRole::User);
        assert_eq!(compressed.turns[1].role, CompressedRole::Assistant);
        assert_eq!(compressed.turns[0].content, "What is 2+2?");
        assert_eq!(compressed.turns[1].content, "2+2 equals 4.");
    }

    // -----------------------------------------------------------------------
    // Light compression tests
    // -----------------------------------------------------------------------

    #[test]
    fn light_drops_system_messages() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Light);

        // Original has 9 steps, one is SystemMessage, so Light should have 8.
        assert_eq!(compressed.turns.len(), 8);

        // No turn should mention system content.
        for turn in &compressed.turns {
            assert!(
                !turn.content.contains("Memory consolidation nudge"),
                "system message should be dropped"
            );
        }
    }

    #[test]
    fn light_preserves_tool_calls_as_separate_turns() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Light);

        // Turns should include ActionSummary turns for each tool call and result.
        let action_turns: Vec<_> = compressed
            .turns
            .iter()
            .filter(|t| t.role == CompressedRole::ActionSummary)
            .collect();
        // 2 ToolCalls + 2 ToolResults = 4 ActionSummary turns.
        assert_eq!(action_turns.len(), 4);
    }

    #[test]
    fn light_truncates_tool_results() {
        let long_result = "x".repeat(1000);
        let traj = Trajectory {
            session_id: "sess-trunc".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "hash".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![TrajectoryStep {
                step_index: 0,
                timestamp: "2026-01-01T00:00:01Z".to_owned(),
                action_type: ActionType::ToolResult,
                content: "tool_result: big_tool".to_owned(),
                tool_name: Some("big_tool".to_owned()),
                tool_arguments: None,
                tool_result: Some(long_result.clone()),
                tokens: None,
            }],
            outcome: None,
            tags: vec![],
        };

        let compressed = compress(&traj, CompressionLevel::Light);
        assert_eq!(compressed.turns.len(), 1);

        let turn = &compressed.turns[0];
        // The content should be truncated: prefix + 512 chars of result + "..."
        assert!(turn.content.len() < long_result.len());
        assert!(turn.content.contains("..."));
    }

    #[test]
    fn light_keeps_user_and_assistant_messages() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Light);

        let user_turns: Vec<_> = compressed
            .turns
            .iter()
            .filter(|t| t.role == CompressedRole::User)
            .collect();
        assert_eq!(user_turns.len(), 2);
        assert_eq!(user_turns[0].content, "What files are in /tmp?");
        assert_eq!(user_turns[1].content, "Thanks!");

        let assistant_turns: Vec<_> = compressed
            .turns
            .iter()
            .filter(|t| t.role == CompressedRole::Assistant)
            .collect();
        assert_eq!(assistant_turns.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Medium compression tests
    // -----------------------------------------------------------------------

    #[test]
    fn medium_collapses_tool_chains_into_summaries() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);

        // Expected turns: User, ActionSummary (collapsed tools), Assistant, User, Assistant
        // = 5 turns.
        assert_eq!(compressed.turns.len(), 5);

        assert_eq!(compressed.turns[0].role, CompressedRole::User);
        assert_eq!(compressed.turns[0].content, "What files are in /tmp?");

        assert_eq!(compressed.turns[1].role, CompressedRole::ActionSummary);
        assert!(compressed.turns[1].content.starts_with("Used "));
        assert!(compressed.turns[1].content.contains("shell_exec"));
        assert!(compressed.turns[1].content.contains("read_file"));
        assert_eq!(compressed.turns[1].tools_used.len(), 2);
        assert!(compressed.turns[1].tools_used.contains(&"shell_exec".to_owned()));
        assert!(compressed.turns[1].tools_used.contains(&"read_file".to_owned()));

        assert_eq!(compressed.turns[2].role, CompressedRole::Assistant);
        assert_eq!(compressed.turns[3].role, CompressedRole::User);
        assert_eq!(compressed.turns[4].role, CompressedRole::Assistant);
    }

    #[test]
    fn medium_drops_system_messages() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);

        for turn in &compressed.turns {
            assert!(
                !turn.content.contains("Memory consolidation nudge"),
                "system message should be dropped in medium compression"
            );
        }
    }

    #[test]
    fn medium_action_summary_contains_brief_args() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);

        let action = &compressed.turns[1];
        // Should contain the tool arguments (they're short enough not to be truncated).
        assert!(action.content.contains(r#"{"command":"ls /tmp"}"#));
        assert!(action.content.contains(r#"{"path":"/tmp/foo.txt"}"#));
    }

    #[test]
    fn medium_truncates_long_args() {
        let long_args = format!(r#"{{"query":"{}"}}"#, "a".repeat(200));
        let traj = Trajectory {
            session_id: "sess-long-args".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "hash".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![
                TrajectoryStep {
                    step_index: 0,
                    timestamp: "2026-01-01T00:00:01Z".to_owned(),
                    action_type: ActionType::ToolCall,
                    content: "tool_call: search".to_owned(),
                    tool_name: Some("search".to_owned()),
                    tool_arguments: Some(long_args.clone()),
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 1,
                    timestamp: "2026-01-01T00:00:02Z".to_owned(),
                    action_type: ActionType::ToolResult,
                    content: "tool_result: search".to_owned(),
                    tool_name: Some("search".to_owned()),
                    tool_arguments: None,
                    tool_result: Some("results".to_owned()),
                    tokens: None,
                },
            ],
            outcome: None,
            tags: vec![],
        };

        let compressed = compress(&traj, CompressionLevel::Medium);
        assert_eq!(compressed.turns.len(), 1);
        let action = &compressed.turns[0];
        assert!(action.content.contains("..."), "long args should be truncated");
        // The truncated args should be shorter than the original.
        assert!(action.content.len() < long_args.len());
    }

    // -----------------------------------------------------------------------
    // Heavy compression tests
    // -----------------------------------------------------------------------

    #[test]
    fn heavy_produces_only_user_assistant_pairs() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Heavy);

        // Two user messages, each followed by the last assistant message in
        // its segment.
        assert_eq!(compressed.turns.len(), 4);

        assert_eq!(compressed.turns[0].role, CompressedRole::User);
        assert_eq!(compressed.turns[0].content, "What files are in /tmp?");

        assert_eq!(compressed.turns[1].role, CompressedRole::Assistant);
        assert!(compressed.turns[1].content.contains("3 files"));

        assert_eq!(compressed.turns[2].role, CompressedRole::User);
        assert_eq!(compressed.turns[2].content, "Thanks!");

        assert_eq!(compressed.turns[3].role, CompressedRole::Assistant);
        assert_eq!(compressed.turns[3].content, "You're welcome!");
    }

    #[test]
    fn heavy_drops_tool_calls_and_system_messages() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Heavy);

        for turn in &compressed.turns {
            assert_ne!(
                turn.role,
                CompressedRole::ActionSummary,
                "heavy compression should have no action summary turns"
            );
            assert!(
                !turn.content.contains("tool_call"),
                "heavy compression should not contain tool call references"
            );
            assert!(
                !turn.content.contains("Memory consolidation"),
                "heavy compression should not contain system messages"
            );
        }
    }

    #[test]
    fn heavy_keeps_last_assistant_before_next_user() {
        // Build a trajectory with multiple assistant messages between user messages.
        let traj = Trajectory {
            session_id: "sess-multi".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "hash".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![
                step(0, ActionType::UserMessage, "Hello"),
                step(1, ActionType::AssistantMessage, "First thought..."),
                step(2, ActionType::AssistantMessage, "Actually, the answer is 42."),
                step(3, ActionType::UserMessage, "Are you sure?"),
                step(4, ActionType::AssistantMessage, "Yes, I'm sure."),
            ],
            outcome: None,
            tags: vec![],
        };

        let compressed = compress(&traj, CompressionLevel::Heavy);
        assert_eq!(compressed.turns.len(), 4);

        // First pair: user -> LAST assistant before "Are you sure?"
        assert_eq!(compressed.turns[0].content, "Hello");
        assert_eq!(compressed.turns[1].content, "Actually, the answer is 42.");

        // Second pair
        assert_eq!(compressed.turns[2].content, "Are you sure?");
        assert_eq!(compressed.turns[3].content, "Yes, I'm sure.");
    }

    #[test]
    fn heavy_user_with_no_following_assistant() {
        let traj = Trajectory {
            session_id: "sess-no-reply".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "hash".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![
                step(0, ActionType::UserMessage, "Hello"),
                step(1, ActionType::ToolCall, "tool_call: search"),
                // No assistant message follows
            ],
            outcome: Some(TrajectoryOutcome::Abandoned),
            tags: vec![],
        };

        let compressed = compress(&traj, CompressionLevel::Heavy);
        // Only the user message, no assistant to pair with.
        assert_eq!(compressed.turns.len(), 1);
        assert_eq!(compressed.turns[0].role, CompressedRole::User);
    }

    // -----------------------------------------------------------------------
    // Empty trajectory tests
    // -----------------------------------------------------------------------

    #[test]
    fn compress_empty_trajectory_light() {
        let traj = empty_trajectory();
        let compressed = compress(&traj, CompressionLevel::Light);

        assert!(compressed.turns.is_empty());
        assert_eq!(compressed.session_id, "sess-empty");
        assert!(compressed.outcome.is_none());
    }

    #[test]
    fn compress_empty_trajectory_medium() {
        let traj = empty_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);
        assert!(compressed.turns.is_empty());
    }

    #[test]
    fn compress_empty_trajectory_heavy() {
        let traj = empty_trajectory();
        let compressed = compress(&traj, CompressionLevel::Heavy);
        assert!(compressed.turns.is_empty());
    }

    // -----------------------------------------------------------------------
    // Simple trajectory (no tool calls) tests
    // -----------------------------------------------------------------------

    #[test]
    fn simple_trajectory_light() {
        let traj = simple_trajectory();
        let compressed = compress(&traj, CompressionLevel::Light);

        assert_eq!(compressed.turns.len(), 2);
        assert_eq!(compressed.turns[0].role, CompressedRole::User);
        assert_eq!(compressed.turns[0].content, "What is 2+2?");
        assert_eq!(compressed.turns[1].role, CompressedRole::Assistant);
        assert_eq!(compressed.turns[1].content, "2+2 equals 4.");
    }

    #[test]
    fn simple_trajectory_medium() {
        let traj = simple_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);

        // No tool calls to collapse, so same as light for user/assistant.
        assert_eq!(compressed.turns.len(), 2);
        assert_eq!(compressed.turns[0].role, CompressedRole::User);
        assert_eq!(compressed.turns[1].role, CompressedRole::Assistant);
    }

    #[test]
    fn simple_trajectory_heavy() {
        let traj = simple_trajectory();
        let compressed = compress(&traj, CompressionLevel::Heavy);

        assert_eq!(compressed.turns.len(), 2);
        assert_eq!(compressed.turns[0].role, CompressedRole::User);
        assert_eq!(compressed.turns[1].role, CompressedRole::Assistant);
        assert_eq!(compressed.turns[1].content, "2+2 equals 4.");
    }

    // -----------------------------------------------------------------------
    // ShareGPT export tests
    // -----------------------------------------------------------------------

    #[test]
    fn sharegpt_maps_roles_correctly() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);
        let entries = to_sharegpt(&compressed);

        assert_eq!(entries.len(), compressed.turns.len());

        // First turn is User -> "human"
        assert_eq!(entries[0]["from"], "human");
        // Second turn is ActionSummary -> "thought"
        assert_eq!(entries[1]["from"], "thought");
        // Third turn is Assistant -> "gpt"
        assert_eq!(entries[2]["from"], "gpt");
        // Fourth turn is User -> "human"
        assert_eq!(entries[3]["from"], "human");
        // Fifth turn is Assistant -> "gpt"
        assert_eq!(entries[4]["from"], "gpt");
    }

    #[test]
    fn sharegpt_contains_content() {
        let traj = simple_trajectory();
        let compressed = compress(&traj, CompressionLevel::Heavy);
        let entries = to_sharegpt(&compressed);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["value"], "What is 2+2?");
        assert_eq!(entries[1]["value"], "2+2 equals 4.");
    }

    #[test]
    fn sharegpt_empty_trajectory() {
        let traj = empty_trajectory();
        let compressed = compress(&traj, CompressionLevel::Light);
        let entries = to_sharegpt(&compressed);
        assert!(entries.is_empty());
    }

    #[test]
    fn sharegpt_serializes_to_valid_json() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);
        let entries = to_sharegpt(&compressed);

        let json = serde_json::to_string_pretty(&entries).expect("should serialize");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&json).expect("should parse back");
        assert_eq!(parsed.len(), entries.len());
    }

    // -----------------------------------------------------------------------
    // ChatML export tests
    // -----------------------------------------------------------------------

    #[test]
    fn chatml_format_basic() {
        let traj = simple_trajectory();
        let compressed = compress(&traj, CompressionLevel::Heavy);
        let chatml = to_chatml(&compressed);

        let expected = "\
<|im_start|>user\nWhat is 2+2?<|im_end|>\n\
<|im_start|>assistant\n2+2 equals 4.<|im_end|>\n";
        assert_eq!(chatml, expected);
    }

    #[test]
    fn chatml_action_summary_uses_assistant_role() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);
        let chatml = to_chatml(&compressed);

        // ActionSummary should be emitted as "assistant" role in ChatML.
        assert!(chatml.contains("<|im_start|>assistant\nUsed "));
    }

    #[test]
    fn chatml_empty_trajectory() {
        let traj = empty_trajectory();
        let compressed = compress(&traj, CompressionLevel::Light);
        let chatml = to_chatml(&compressed);
        assert!(chatml.is_empty());
    }

    #[test]
    fn chatml_contains_all_turns() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);
        let chatml = to_chatml(&compressed);

        let start_count = chatml.matches("<|im_start|>").count();
        let end_count = chatml.matches("<|im_end|>").count();
        assert_eq!(start_count, compressed.turns.len());
        assert_eq!(end_count, compressed.turns.len());
    }

    // -----------------------------------------------------------------------
    // CompressedTrajectory serialization round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn compressed_trajectory_serialization_round_trip() {
        let traj = sample_trajectory();
        let compressed = compress(&traj, CompressionLevel::Medium);

        let json = serde_json::to_string_pretty(&compressed).expect("should serialize");
        let parsed: CompressedTrajectory =
            serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(parsed.session_id, compressed.session_id);
        assert_eq!(parsed.model, compressed.model);
        assert_eq!(parsed.turns.len(), compressed.turns.len());
        assert_eq!(parsed.outcome, compressed.outcome);
        assert_eq!(parsed.tags, compressed.tags);
    }

    // -----------------------------------------------------------------------
    // Edge case: trajectory starting with tool calls (no user message first)
    // -----------------------------------------------------------------------

    #[test]
    fn medium_handles_orphan_tool_calls() {
        // Trajectory that starts with ToolCall without a preceding UserMessage.
        let traj = Trajectory {
            session_id: "sess-orphan".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "hash".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![
                TrajectoryStep {
                    step_index: 0,
                    timestamp: "2026-01-01T00:00:01Z".to_owned(),
                    action_type: ActionType::ToolCall,
                    content: "tool_call: init".to_owned(),
                    tool_name: Some("init".to_owned()),
                    tool_arguments: Some("{}".to_owned()),
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 1,
                    timestamp: "2026-01-01T00:00:02Z".to_owned(),
                    action_type: ActionType::ToolResult,
                    content: "tool_result: init".to_owned(),
                    tool_name: Some("init".to_owned()),
                    tool_arguments: None,
                    tool_result: Some("initialized".to_owned()),
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 2,
                    timestamp: "2026-01-01T00:00:03Z".to_owned(),
                    action_type: ActionType::AssistantMessage,
                    content: "System initialized.".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: None,
                },
            ],
            outcome: None,
            tags: vec![],
        };

        let compressed = compress(&traj, CompressionLevel::Medium);
        assert_eq!(compressed.turns.len(), 2); // ActionSummary + Assistant
        assert_eq!(compressed.turns[0].role, CompressedRole::ActionSummary);
        assert_eq!(compressed.turns[1].role, CompressedRole::Assistant);
    }

    #[test]
    fn heavy_skips_leading_non_user_steps() {
        let traj = Trajectory {
            session_id: "sess-leading".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "hash".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![
                step(0, ActionType::SystemMessage, "System init"),
                step(1, ActionType::AssistantMessage, "I'm ready."),
                step(2, ActionType::UserMessage, "Hi"),
                step(3, ActionType::AssistantMessage, "Hello!"),
            ],
            outcome: None,
            tags: vec![],
        };

        let compressed = compress(&traj, CompressionLevel::Heavy);
        assert_eq!(compressed.turns.len(), 2);
        assert_eq!(compressed.turns[0].content, "Hi");
        assert_eq!(compressed.turns[1].content, "Hello!");
    }

    // -----------------------------------------------------------------------
    // Edge case: tools_used deduplication in medium
    // -----------------------------------------------------------------------

    #[test]
    fn medium_deduplicates_tool_names() {
        let traj = Trajectory {
            session_id: "sess-dedup".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "hash".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![
                TrajectoryStep {
                    step_index: 0,
                    timestamp: "2026-01-01T00:00:01Z".to_owned(),
                    action_type: ActionType::ToolCall,
                    content: "tool_call: grep".to_owned(),
                    tool_name: Some("grep".to_owned()),
                    tool_arguments: Some(r#"{"pattern":"foo"}"#.to_owned()),
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 1,
                    timestamp: "2026-01-01T00:00:02Z".to_owned(),
                    action_type: ActionType::ToolResult,
                    content: "tool_result: grep".to_owned(),
                    tool_name: Some("grep".to_owned()),
                    tool_arguments: None,
                    tool_result: Some("match1".to_owned()),
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 2,
                    timestamp: "2026-01-01T00:00:03Z".to_owned(),
                    action_type: ActionType::ToolCall,
                    content: "tool_call: grep".to_owned(),
                    tool_name: Some("grep".to_owned()),
                    tool_arguments: Some(r#"{"pattern":"bar"}"#.to_owned()),
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 3,
                    timestamp: "2026-01-01T00:00:04Z".to_owned(),
                    action_type: ActionType::ToolResult,
                    content: "tool_result: grep".to_owned(),
                    tool_name: Some("grep".to_owned()),
                    tool_arguments: None,
                    tool_result: Some("match2".to_owned()),
                    tokens: None,
                },
            ],
            outcome: None,
            tags: vec![],
        };

        let compressed = compress(&traj, CompressionLevel::Medium);
        assert_eq!(compressed.turns.len(), 1);
        let action = &compressed.turns[0];
        // "grep" should appear only once in tools_used despite two calls.
        assert_eq!(action.tools_used, vec!["grep"]);
        // But both calls should appear in the summary content.
        assert_eq!(action.content.matches("grep(").count(), 2);
    }

    // -----------------------------------------------------------------------
    // Helper
    // -----------------------------------------------------------------------

    fn step(index: usize, action_type: ActionType, content: &str) -> TrajectoryStep {
        TrajectoryStep {
            step_index: index,
            timestamp: format!("2026-01-01T00:00:{:02}Z", index),
            action_type,
            content: content.to_owned(),
            tool_name: None,
            tool_arguments: None,
            tool_result: None,
            tokens: None,
        }
    }
}
