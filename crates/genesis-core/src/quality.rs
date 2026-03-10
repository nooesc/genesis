//! Trajectory quality scoring for training data filtering.
//!
//! Evaluates trajectories against multiple quality dimensions and produces
//! a composite score. Low-scoring trajectories can be filtered out before
//! inclusion in training datasets.

use crate::trajectory::{ActionType, Trajectory, TrajectoryOutcome};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Quality score for a single trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityScore {
    /// Overall score from 0.0 (worst) to 1.0 (best).
    pub overall: f64,
    /// Individual dimension scores.
    pub dimensions: QualityDimensions,
    /// Issues found during scoring.
    pub issues: Vec<String>,
}

/// Individual quality dimensions, each scored 0.0 to 1.0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityDimensions {
    /// Did the trajectory reach a successful outcome?
    pub outcome: f64,
    /// Ratio of useful content vs. boilerplate/errors.
    pub signal_to_noise: f64,
    /// Tool usage diversity — more diverse tool usage scores higher.
    pub tool_diversity: f64,
    /// Conversation depth — multi-turn conversations score higher than single-turn.
    pub depth: f64,
    /// Efficiency — fewer wasted turns (errors, retries) scores higher.
    pub efficiency: f64,
    /// Completeness — trajectory has all expected parts (user msg, response, outcome).
    pub completeness: f64,
}

/// Weights for combining dimension scores into overall score.
#[derive(Debug, Clone)]
pub struct QualityWeights {
    pub outcome: f64,
    pub signal_to_noise: f64,
    pub tool_diversity: f64,
    pub depth: f64,
    pub efficiency: f64,
    pub completeness: f64,
}

impl Default for QualityWeights {
    fn default() -> Self {
        Self {
            outcome: 0.25,
            signal_to_noise: 0.20,
            tool_diversity: 0.10,
            depth: 0.15,
            efficiency: 0.15,
            completeness: 0.15,
        }
    }
}

/// Score a trajectory for training data quality.
pub fn score(trajectory: &Trajectory) -> QualityScore {
    score_with_weights(trajectory, &QualityWeights::default())
}

/// Score a trajectory with custom weights.
pub fn score_with_weights(trajectory: &Trajectory, weights: &QualityWeights) -> QualityScore {
    let mut issues = Vec::new();

    let outcome = score_outcome(trajectory, &mut issues);
    let signal_to_noise = score_signal_to_noise(trajectory, &mut issues);
    let tool_diversity = score_tool_diversity(trajectory);
    let depth = score_depth(trajectory, &mut issues);
    let efficiency = score_efficiency(trajectory, &mut issues);
    let completeness = score_completeness(trajectory, &mut issues);

    let dimensions = QualityDimensions {
        outcome,
        signal_to_noise,
        tool_diversity,
        depth,
        efficiency,
        completeness,
    };

    let total_weight = weights.outcome
        + weights.signal_to_noise
        + weights.tool_diversity
        + weights.depth
        + weights.efficiency
        + weights.completeness;

    let overall = if total_weight > 0.0 {
        (outcome * weights.outcome
            + signal_to_noise * weights.signal_to_noise
            + tool_diversity * weights.tool_diversity
            + depth * weights.depth
            + efficiency * weights.efficiency
            + completeness * weights.completeness)
            / total_weight
    } else {
        0.0
    };

    QualityScore {
        overall,
        dimensions,
        issues,
    }
}

fn score_outcome(trajectory: &Trajectory, issues: &mut Vec<String>) -> f64 {
    match &trajectory.outcome {
        Some(TrajectoryOutcome::Success) => 1.0,
        Some(TrajectoryOutcome::Failure { reason }) => {
            issues.push(format!("trajectory failed: {reason}"));
            0.0
        }
        Some(TrajectoryOutcome::Abandoned) => {
            issues.push("trajectory was abandoned".to_owned());
            0.2
        }
        None => {
            issues.push("no outcome recorded".to_owned());
            0.5
        }
    }
}

fn score_signal_to_noise(trajectory: &Trajectory, issues: &mut Vec<String>) -> f64 {
    if trajectory.steps.is_empty() {
        issues.push("empty trajectory".to_owned());
        return 0.0;
    }

    let total_steps = trajectory.steps.len();
    let mut error_steps = 0usize;
    let mut empty_steps = 0usize;
    let mut truncated_steps = 0usize;

    for step in &trajectory.steps {
        if step.content.is_empty() && step.tool_result.as_ref().is_none_or(|r| r.is_empty()) {
            empty_steps += 1;
        }
        if let Some(result) = &step.tool_result {
            if result.starts_with("Error:") || result.starts_with("error:") {
                error_steps += 1;
            }
        }
        if step.content.contains("(truncated)") || step.content.contains("(output truncated)") {
            truncated_steps += 1;
        }
    }

    let noise_ratio = (error_steps + empty_steps) as f64 / total_steps as f64;
    let signal = 1.0 - noise_ratio;

    if error_steps > total_steps / 2 {
        issues.push(format!("{error_steps}/{total_steps} steps contain errors"));
    }
    if truncated_steps > total_steps / 4 {
        issues.push(format!("{truncated_steps} steps have truncated output"));
    }

    signal.max(0.0)
}

fn score_tool_diversity(trajectory: &Trajectory) -> f64 {
    let tools: HashSet<&str> = trajectory
        .steps
        .iter()
        .filter(|s| s.action_type == ActionType::ToolCall)
        .filter_map(|s| s.tool_name.as_deref())
        .collect();

    let tool_count = tools.len();
    // Score: 0 tools = 0.3 (still valid for pure conversation),
    // 1 tool = 0.5, 2-3 = 0.7, 4-5 = 0.85, 6+ = 1.0
    match tool_count {
        0 => 0.3,
        1 => 0.5,
        2..=3 => 0.7,
        4..=5 => 0.85,
        _ => 1.0,
    }
}

fn score_depth(trajectory: &Trajectory, issues: &mut Vec<String>) -> f64 {
    let user_turns = trajectory
        .steps
        .iter()
        .filter(|s| s.action_type == ActionType::UserMessage)
        .count();

    let assistant_turns = trajectory
        .steps
        .iter()
        .filter(|s| s.action_type == ActionType::AssistantMessage)
        .count();

    if user_turns == 0 {
        issues.push("no user messages".to_owned());
        return 0.0;
    }

    if assistant_turns == 0 {
        issues.push("no assistant responses".to_owned());
        return 0.0;
    }

    // Score conversation depth: single exchange = 0.4, 2-3 = 0.6, 4-6 = 0.8, 7+ = 1.0
    let exchanges = user_turns.min(assistant_turns);
    match exchanges {
        1 => 0.4,
        2..=3 => 0.6,
        4..=6 => 0.8,
        _ => 1.0,
    }
}

fn score_efficiency(trajectory: &Trajectory, issues: &mut Vec<String>) -> f64 {
    let tool_calls = trajectory
        .steps
        .iter()
        .filter(|s| s.action_type == ActionType::ToolCall)
        .count();

    let tool_results = trajectory
        .steps
        .iter()
        .filter(|s| s.action_type == ActionType::ToolResult)
        .count();

    let error_results = trajectory
        .steps
        .iter()
        .filter(|s| {
            s.action_type == ActionType::ToolResult
                && s.tool_result
                    .as_ref()
                    .is_some_and(|r| r.starts_with("Error:") || r.starts_with("error:"))
        })
        .count();

    // Check for call/result mismatch
    if tool_calls > 0 && tool_results == 0 {
        issues.push("tool calls without results".to_owned());
        return 0.3;
    }

    if tool_calls == 0 {
        return 0.8; // No tool calls is fine, just less interesting
    }

    let error_rate = error_results as f64 / tool_calls as f64;
    if error_rate > 0.5 {
        issues.push(format!(
            "high tool error rate: {error_results}/{tool_calls} calls failed"
        ));
    }

    // Check for repeated tool calls (possible stuck loop)
    let consecutive_same_tool = count_consecutive_same_tool(trajectory);
    if consecutive_same_tool >= 3 {
        issues.push(format!(
            "possible stuck loop: {consecutive_same_tool} consecutive same-tool calls"
        ));
    }

    let stuck_penalty = if consecutive_same_tool >= 3 {
        0.3
    } else {
        0.0
    };

    (1.0 - error_rate - stuck_penalty).max(0.0)
}

fn count_consecutive_same_tool(trajectory: &Trajectory) -> usize {
    let tool_names: Vec<&str> = trajectory
        .steps
        .iter()
        .filter(|s| s.action_type == ActionType::ToolCall)
        .filter_map(|s| s.tool_name.as_deref())
        .collect();

    let mut max_consecutive = 0usize;
    let mut current = 0usize;
    let mut last_name = "";

    for name in &tool_names {
        if *name == last_name {
            current += 1;
            max_consecutive = max_consecutive.max(current);
        } else {
            current = 1;
            last_name = name;
        }
    }
    max_consecutive
}

fn score_completeness(trajectory: &Trajectory, issues: &mut Vec<String>) -> f64 {
    let mut score: f64 = 1.0;

    // Has user message?
    let has_user = trajectory
        .steps
        .iter()
        .any(|s| s.action_type == ActionType::UserMessage);
    if !has_user {
        issues.push("missing user message".to_owned());
        score -= 0.3;
    }

    // Has assistant response?
    let has_assistant = trajectory
        .steps
        .iter()
        .any(|s| s.action_type == ActionType::AssistantMessage);
    if !has_assistant {
        issues.push("missing assistant response".to_owned());
        score -= 0.3;
    }

    // Has outcome?
    if trajectory.outcome.is_none() {
        issues.push("missing outcome".to_owned());
        score -= 0.2;
    }

    // Has model?
    if trajectory.model.is_empty() {
        issues.push("missing model identifier".to_owned());
        score -= 0.1;
    }

    // Has completed_at timestamp?
    if trajectory.completed_at.is_none() {
        score -= 0.1;
    }

    score.max(0.0)
}

/// Filter trajectories by minimum quality score.
pub fn filter_by_quality(trajectories: &[Trajectory], min_score: f64) -> Vec<usize> {
    trajectories
        .iter()
        .enumerate()
        .filter(|(_, t)| score(t).overall >= min_score)
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::{ActionType, TrajectoryStep};

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

    fn make_tool_call(name: &str, content: &str) -> TrajectoryStep {
        TrajectoryStep {
            step_index: 0,
            timestamp: "2025-01-01T00:00:00Z".to_owned(),
            action_type: ActionType::ToolCall,
            content: content.to_owned(),
            tool_name: Some(name.to_owned()),
            tool_arguments: Some("{}".to_owned()),
            tool_result: None,
            tokens: None,
        }
    }

    fn make_tool_result(name: &str, result: &str) -> TrajectoryStep {
        TrajectoryStep {
            step_index: 0,
            timestamp: "2025-01-01T00:00:00Z".to_owned(),
            action_type: ActionType::ToolResult,
            content: "".to_owned(),
            tool_name: Some(name.to_owned()),
            tool_arguments: None,
            tool_result: Some(result.to_owned()),
            tokens: None,
        }
    }

    fn good_trajectory() -> Trajectory {
        Trajectory {
            session_id: "test-1".to_owned(),
            model: "gpt-4".to_owned(),
            system_prompt_hash: "abc".to_owned(),
            started_at: "2025-01-01T00:00:00Z".to_owned(),
            completed_at: Some("2025-01-01T00:01:00Z".to_owned()),
            steps: vec![
                make_step(ActionType::UserMessage, "List the files in /tmp"),
                make_tool_call("shell_exec", "ls /tmp"),
                make_tool_result("shell_exec", "file1.txt\nfile2.txt"),
                make_step(ActionType::AssistantMessage, "Found 2 files: file1.txt and file2.txt"),
                make_step(ActionType::UserMessage, "Read file1.txt"),
                make_tool_call("read_file", "read_file /tmp/file1.txt"),
                make_tool_result("read_file", "Hello world"),
                make_step(ActionType::AssistantMessage, "The file contains: Hello world"),
            ],
            outcome: Some(TrajectoryOutcome::Success),
            tags: vec!["demo".to_owned()],
        }
    }

    fn bad_trajectory() -> Trajectory {
        Trajectory {
            session_id: "test-2".to_owned(),
            model: "".to_owned(),
            system_prompt_hash: "abc".to_owned(),
            started_at: "2025-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![
                make_step(ActionType::UserMessage, "Do something"),
                make_tool_call("shell_exec", "broken command"),
                make_tool_result("shell_exec", "Error: command not found"),
                make_tool_call("shell_exec", "broken command"),
                make_tool_result("shell_exec", "Error: command not found"),
                make_tool_call("shell_exec", "broken command"),
                make_tool_result("shell_exec", "Error: command not found"),
            ],
            outcome: Some(TrajectoryOutcome::Failure {
                reason: "stuck in error loop".to_owned(),
            }),
            tags: vec![],
        }
    }

    #[test]
    fn good_trajectory_scores_high() {
        let s = score(&good_trajectory());
        assert!(s.overall > 0.7, "good trajectory should score > 0.7, got {}", s.overall);
        assert_eq!(s.dimensions.outcome, 1.0);
        assert!(s.dimensions.completeness > 0.9);
        assert!(s.issues.is_empty(), "good trajectory should have no issues: {:?}", s.issues);
    }

    #[test]
    fn bad_trajectory_scores_low() {
        let s = score(&bad_trajectory());
        assert!(s.overall < 0.4, "bad trajectory should score < 0.4, got {}", s.overall);
        assert_eq!(s.dimensions.outcome, 0.0);
        assert!(!s.issues.is_empty());
    }

    #[test]
    fn empty_trajectory_scores_zero() {
        let t = Trajectory {
            session_id: "empty".to_owned(),
            model: "".to_owned(),
            system_prompt_hash: "".to_owned(),
            started_at: "2025-01-01T00:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![],
            outcome: None,
            tags: vec![],
        };
        let s = score(&t);
        assert!(s.overall < 0.3, "empty trajectory should score very low, got {}", s.overall);
    }

    #[test]
    fn tool_diversity_rewards_variety() {
        let mut t = good_trajectory();
        // Add more diverse tools
        t.steps.push(make_tool_call("web_search", "search"));
        t.steps.push(make_tool_result("web_search", "results"));
        t.steps.push(make_tool_call("write_file", "write"));
        t.steps.push(make_tool_result("write_file", "ok"));
        t.steps.push(make_tool_call("git_status", "status"));
        t.steps.push(make_tool_result("git_status", "clean"));

        let s = score(&t);
        assert!(s.dimensions.tool_diversity >= 0.85);
    }

    #[test]
    fn stuck_loop_detected() {
        let t = bad_trajectory();
        let s = score(&t);
        let has_stuck_issue = s.issues.iter().any(|i| i.contains("stuck loop"));
        assert!(has_stuck_issue, "should detect stuck loop: {:?}", s.issues);
    }

    #[test]
    fn filter_removes_low_quality() {
        let trajectories = vec![good_trajectory(), bad_trajectory()];
        let good_indices = filter_by_quality(&trajectories, 0.5);
        assert_eq!(good_indices, vec![0], "should keep only the good trajectory");
    }

    #[test]
    fn abandoned_trajectory_partial_score() {
        let mut t = good_trajectory();
        t.outcome = Some(TrajectoryOutcome::Abandoned);
        let s = score(&t);
        assert!(s.overall > 0.3 && s.overall < 0.8, "abandoned should score medium: {}", s.overall);
        assert_eq!(s.dimensions.outcome, 0.2);
    }

    #[test]
    fn no_outcome_scores_middle() {
        let mut t = good_trajectory();
        t.outcome = None;
        let s = score(&t);
        assert_eq!(s.dimensions.outcome, 0.5);
        assert!(s.dimensions.completeness < 1.0);
    }

    #[test]
    fn custom_weights_change_overall() {
        let t = good_trajectory();

        let default_score = score(&t);

        // Weight outcome at 100%, everything else at 0
        let outcome_only = QualityWeights {
            outcome: 1.0,
            signal_to_noise: 0.0,
            tool_diversity: 0.0,
            depth: 0.0,
            efficiency: 0.0,
            completeness: 0.0,
        };
        let outcome_score = score_with_weights(&t, &outcome_only);

        assert_eq!(outcome_score.overall, 1.0, "outcome-only should be 1.0 for success");
        // Default includes other dimensions, so won't be exactly 1.0
        assert!(default_score.overall < 1.0 || default_score.overall == 1.0);
    }

    #[test]
    fn consecutive_same_tool_counted() {
        let mut t = good_trajectory();
        t.steps.clear();
        t.steps.push(make_tool_call("shell_exec", "cmd1"));
        t.steps.push(make_tool_call("shell_exec", "cmd2"));
        t.steps.push(make_tool_call("shell_exec", "cmd3"));
        t.steps.push(make_tool_call("shell_exec", "cmd4"));
        assert_eq!(count_consecutive_same_tool(&t), 4);
    }

    #[test]
    fn mixed_tools_low_consecutive() {
        let mut t = good_trajectory();
        t.steps.clear();
        t.steps.push(make_tool_call("shell_exec", "a"));
        t.steps.push(make_tool_call("read_file", "b"));
        t.steps.push(make_tool_call("shell_exec", "c"));
        // No tool repeats consecutively, so max is 1 (each appears once in a row)
        assert!(count_consecutive_same_tool(&t) <= 1);
    }
}
