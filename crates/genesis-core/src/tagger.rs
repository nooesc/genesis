//! Automatic trajectory tagging for training data enrichment.
//!
//! Analyzes trajectory content and applies semantic tags based on
//! tool usage patterns, conversation characteristics, and outcomes.

use crate::trajectory::{ActionType, Trajectory, TrajectoryOutcome};
use std::collections::HashSet;

/// Analyze a trajectory and return suggested tags.
pub fn auto_tag(trajectory: &Trajectory) -> Vec<String> {
    let mut tags = HashSet::new();

    tag_by_tools(trajectory, &mut tags);
    tag_by_outcome(trajectory, &mut tags);
    tag_by_depth(trajectory, &mut tags);
    tag_by_content(trajectory, &mut tags);
    tag_by_model(trajectory, &mut tags);

    let mut sorted: Vec<String> = tags.into_iter().collect();
    sorted.sort();
    sorted
}

fn tag_by_tools(trajectory: &Trajectory, tags: &mut HashSet<String>) {
    let tools: HashSet<&str> = trajectory
        .steps
        .iter()
        .filter(|s| s.action_type == ActionType::ToolCall)
        .filter_map(|s| s.tool_name.as_deref())
        .collect();

    if tools.is_empty() {
        tags.insert("no-tools".to_owned());
        return;
    }

    // File operations
    if tools
        .iter()
        .any(|t| matches!(*t, "read_file" | "write_file" | "patch" | "list_dir" | "list_tree"))
    {
        tags.insert("file-ops".to_owned());
    }

    // Shell usage
    if tools.contains("shell_exec") {
        tags.insert("shell".to_owned());
    }

    // Git operations
    if tools
        .iter()
        .any(|t| t.starts_with("git_"))
    {
        tags.insert("git".to_owned());
    }

    // Web/research
    if tools
        .iter()
        .any(|t| matches!(*t, "web_search" | "web_request" | "browse"))
    {
        tags.insert("web-research".to_owned());
    }

    // Code search
    if tools
        .iter()
        .any(|t| matches!(*t, "glob_search" | "search_files"))
    {
        tags.insert("code-search".to_owned());
    }

    // Memory operations
    if tools
        .iter()
        .any(|t| matches!(*t, "memory_store" | "memory_recall"))
    {
        tags.insert("memory".to_owned());
    }

    // Creative/multimodal
    if tools
        .iter()
        .any(|t| matches!(*t, "image_generation" | "text_to_speech" | "vision" | "transcribe"))
    {
        tags.insert("multimodal".to_owned());
    }

    // Docker/SSH (remote ops)
    if tools
        .iter()
        .any(|t| matches!(*t, "docker_exec" | "ssh_exec"))
    {
        tags.insert("remote-ops".to_owned());
    }

    // Home automation
    if tools
        .iter()
        .any(|t| t.starts_with("ha_"))
    {
        tags.insert("home-automation".to_owned());
    }

    // Agent coordination
    if tools
        .iter()
        .any(|t| matches!(*t, "spawn_subagent" | "mixture_of_agents" | "reason_with_model"))
    {
        tags.insert("multi-agent".to_owned());
    }

    // Skills management
    if tools
        .iter()
        .any(|t| t.starts_with("skill_"))
    {
        tags.insert("skills".to_owned());
    }

    // Code execution
    if tools.contains("code_execution") {
        tags.insert("code-execution".to_owned());
    }

    // Tool diversity indicator
    match tools.len() {
        1 => tags.insert("single-tool".to_owned()),
        2..=3 => tags.insert("few-tools".to_owned()),
        4..=6 => tags.insert("multi-tool".to_owned()),
        _ => tags.insert("tool-heavy".to_owned()),
    };
}

fn tag_by_outcome(trajectory: &Trajectory, tags: &mut HashSet<String>) {
    match &trajectory.outcome {
        Some(TrajectoryOutcome::Success) => {
            tags.insert("success".to_owned());
        }
        Some(TrajectoryOutcome::Failure { .. }) => {
            tags.insert("failure".to_owned());
        }
        Some(TrajectoryOutcome::Abandoned) => {
            tags.insert("abandoned".to_owned());
        }
        None => {
            tags.insert("no-outcome".to_owned());
        }
    }
}

fn tag_by_depth(trajectory: &Trajectory, tags: &mut HashSet<String>) {
    let user_turns = trajectory
        .steps
        .iter()
        .filter(|s| s.action_type == ActionType::UserMessage)
        .count();

    let total_steps = trajectory.steps.len();

    match user_turns {
        0 => {}
        1 => {
            tags.insert("single-turn".to_owned());
        }
        2..=3 => {
            tags.insert("short-conversation".to_owned());
        }
        4..=8 => {
            tags.insert("medium-conversation".to_owned());
        }
        _ => {
            tags.insert("long-conversation".to_owned());
        }
    }

    if total_steps > 50 {
        tags.insert("complex-trajectory".to_owned());
    }
}

fn tag_by_content(trajectory: &Trajectory, tags: &mut HashSet<String>) {
    let all_content: String = trajectory
        .steps
        .iter()
        .map(|s| s.content.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    let lower = all_content.to_lowercase();

    // Coding indicators
    if lower.contains("function") || lower.contains("class ") || lower.contains("def ")
        || lower.contains("impl ") || lower.contains("struct ") || lower.contains("fn ")
    {
        tags.insert("coding".to_owned());
    }

    // Bug fix indicators
    if lower.contains("fix") || lower.contains("bug") || lower.contains("error")
        || lower.contains("issue")
    {
        tags.insert("debugging".to_owned());
    }

    // Testing indicators
    if lower.contains("test") || lower.contains("assert") || lower.contains("expect") {
        tags.insert("testing".to_owned());
    }

    // Refactoring indicators
    if lower.contains("refactor") || lower.contains("rename") || lower.contains("move")
        || lower.contains("extract")
    {
        tags.insert("refactoring".to_owned());
    }

    // Check for tool errors in results
    let error_count = trajectory
        .steps
        .iter()
        .filter(|s| {
            s.tool_result
                .as_ref()
                .map_or(false, |r| r.starts_with("Error:") || r.starts_with("error:"))
        })
        .count();

    if error_count > 3 {
        tags.insert("error-prone".to_owned());
    }
}

fn tag_by_model(trajectory: &Trajectory, tags: &mut HashSet<String>) {
    let model = &trajectory.model;
    if model.contains("gpt-4") || model.contains("gpt4") {
        tags.insert("model:gpt-4".to_owned());
    } else if model.contains("gpt-3") {
        tags.insert("model:gpt-3".to_owned());
    } else if model.contains("claude") {
        tags.insert("model:claude".to_owned());
    } else if model.contains("gemini") {
        tags.insert("model:gemini".to_owned());
    } else if model.contains("llama") || model.contains("hermes") {
        tags.insert("model:open-source".to_owned());
    } else if !model.is_empty() {
        tags.insert(format!("model:{model}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::TrajectoryStep;

    fn make_step(action: ActionType, content: &str) -> TrajectoryStep {
        TrajectoryStep {
            step_index: 0,
            timestamp: "2025-01-01T00:00:00Z".to_owned(),
            action_type: action,
            content: content.to_owned(),
            tool_name: None,
            tool_arguments: None,
            tool_result: None,
            tokens: None,
        }
    }

    fn make_tool_call(name: &str) -> TrajectoryStep {
        TrajectoryStep {
            step_index: 0,
            timestamp: "2025-01-01T00:00:00Z".to_owned(),
            action_type: ActionType::ToolCall,
            content: "".to_owned(),
            tool_name: Some(name.to_owned()),
            tool_arguments: Some("{}".to_owned()),
            tool_result: None,
            tokens: None,
        }
    }

    fn base_trajectory() -> Trajectory {
        Trajectory {
            session_id: "test".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "abc".to_owned(),
            started_at: "2025-01-01T00:00:00Z".to_owned(),
            completed_at: Some("2025-01-01T00:01:00Z".to_owned()),
            steps: vec![
                make_step(ActionType::UserMessage, "List files"),
                make_tool_call("shell_exec"),
                make_step(ActionType::AssistantMessage, "Here are the files"),
            ],
            outcome: Some(TrajectoryOutcome::Success),
            tags: vec![],
        }
    }

    #[test]
    fn tags_successful_shell_trajectory() {
        let tags = auto_tag(&base_trajectory());
        assert!(tags.contains(&"success".to_owned()));
        assert!(tags.contains(&"shell".to_owned()));
        assert!(tags.contains(&"single-tool".to_owned()));
        assert!(tags.contains(&"single-turn".to_owned()));
        assert!(tags.contains(&"model:gpt-4".to_owned()));
    }

    #[test]
    fn tags_no_tools() {
        let mut t = base_trajectory();
        t.steps = vec![
            make_step(ActionType::UserMessage, "Hello"),
            make_step(ActionType::AssistantMessage, "Hi"),
        ];
        let tags = auto_tag(&t);
        assert!(tags.contains(&"no-tools".to_owned()));
    }

    #[test]
    fn tags_git_operations() {
        let mut t = base_trajectory();
        t.steps.push(make_tool_call("git_status"));
        t.steps.push(make_tool_call("git_commit"));
        let tags = auto_tag(&t);
        assert!(tags.contains(&"git".to_owned()));
        assert!(tags.contains(&"few-tools".to_owned()));
    }

    #[test]
    fn tags_web_research() {
        let mut t = base_trajectory();
        t.steps.push(make_tool_call("web_search"));
        let tags = auto_tag(&t);
        assert!(tags.contains(&"web-research".to_owned()));
    }

    #[test]
    fn tags_multimodal() {
        let mut t = base_trajectory();
        t.steps.push(make_tool_call("image_generation"));
        let tags = auto_tag(&t);
        assert!(tags.contains(&"multimodal".to_owned()));
    }

    #[test]
    fn tags_failure() {
        let mut t = base_trajectory();
        t.outcome = Some(TrajectoryOutcome::Failure {
            reason: "test".to_owned(),
        });
        let tags = auto_tag(&t);
        assert!(tags.contains(&"failure".to_owned()));
        assert!(!tags.contains(&"success".to_owned()));
    }

    #[test]
    fn tags_multi_turn() {
        let mut t = base_trajectory();
        for _ in 0..5 {
            t.steps.push(make_step(ActionType::UserMessage, "more"));
            t.steps.push(make_step(ActionType::AssistantMessage, "ok"));
        }
        let tags = auto_tag(&t);
        assert!(tags.contains(&"medium-conversation".to_owned()));
    }

    #[test]
    fn tags_coding_content() {
        let mut t = base_trajectory();
        t.steps.push(make_step(
            ActionType::AssistantMessage,
            "impl Foo { fn bar() {} }",
        ));
        let tags = auto_tag(&t);
        assert!(tags.contains(&"coding".to_owned()));
    }

    #[test]
    fn tags_debugging_content() {
        let mut t = base_trajectory();
        t.steps[0] = make_step(ActionType::UserMessage, "Fix this bug in the login function");
        let tags = auto_tag(&t);
        assert!(tags.contains(&"debugging".to_owned()));
    }

    #[test]
    fn tags_home_automation() {
        let mut t = base_trajectory();
        t.steps.push(make_tool_call("ha_get_state"));
        let tags = auto_tag(&t);
        assert!(tags.contains(&"home-automation".to_owned()));
    }

    #[test]
    fn tags_claude_model() {
        let mut t = base_trajectory();
        t.model = "claude-sonnet-4-6".to_owned();
        let tags = auto_tag(&t);
        assert!(tags.contains(&"model:claude".to_owned()));
        assert!(!tags.contains(&"model:gpt-4".to_owned()));
    }

    #[test]
    fn auto_tags_are_sorted() {
        let tags = auto_tag(&base_trajectory());
        let mut sorted = tags.clone();
        sorted.sort();
        assert_eq!(tags, sorted);
    }
}
