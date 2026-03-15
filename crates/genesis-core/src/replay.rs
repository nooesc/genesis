use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::trajectory::{ReplayEvent, Trajectory, TrajectoryOutcome};

const MISSING_TOOL_NAME: &str = "<missing>";
const TRUNCATED_MARKER: &str = "... (truncated)";

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("failed to read trajectory file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse trajectory JSON {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ReplayEventCounts {
    pub user: usize,
    pub assistant: usize,
    pub tool_call: usize,
    pub tool_result: usize,
    pub system: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolUsageSummary {
    pub name: String,
    pub call_count: usize,
    pub result_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayWarningCode {
    MissingToolName,
    MissingToolArguments,
    MissingToolResult,
    TruncatedContent,
    MissingCompletionTimestamp,
    MissingOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayWarning {
    pub code: ReplayWarningCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub session_id: String,
    pub model: String,
    pub system_prompt_hash: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TrajectoryOutcome>,
    pub tags: Vec<String>,
    pub total_events: usize,
    pub event_counts: ReplayEventCounts,
    pub tool_usage: Vec<ToolUsageSummary>,
    pub warnings: Vec<ReplayWarning>,
}

pub fn load_raw_trajectory(path: impl AsRef<Path>) -> Result<Trajectory, ReplayError> {
    let path = path.as_ref().to_path_buf();
    let raw = fs::read_to_string(&path).map_err(|source| ReplayError::Read {
        path: path.clone(),
        source,
    })?;

    serde_json::from_str(&raw).map_err(|source| ReplayError::Parse { path, source })
}

pub fn build_replay_report(trajectory: &Trajectory) -> ReplayReport {
    build_replay_report_with_source(trajectory, None)
}

pub fn load_and_report(path: impl AsRef<Path>) -> Result<ReplayReport, ReplayError> {
    let path = path.as_ref().to_path_buf();
    let trajectory = load_raw_trajectory(&path)?;
    Ok(build_replay_report_with_source(&trajectory, Some(&path)))
}

fn build_replay_report_with_source(
    trajectory: &Trajectory,
    source_path: Option<&Path>,
) -> ReplayReport {
    let replay_events = trajectory.replay_events();
    let mut event_counts = ReplayEventCounts::default();
    let mut tool_usage = BTreeMap::<String, ToolUsageSummary>::new();
    let mut warnings = Vec::new();

    for event in replay_events {
        match event {
            ReplayEvent::User {
                metadata: _,
                content,
            } => {
                event_counts.user += 1;
                push_truncation_warning(&mut warnings, content.as_str(), "content", None);
            }
            ReplayEvent::Assistant {
                metadata: _,
                content,
            } => {
                event_counts.assistant += 1;
                push_truncation_warning(&mut warnings, content.as_str(), "content", None);
            }
            ReplayEvent::System {
                metadata: _,
                content,
            } => {
                event_counts.system += 1;
                push_truncation_warning(&mut warnings, content.as_str(), "content", None);
            }
            ReplayEvent::ToolCall {
                metadata,
                tool_name,
                arguments,
                content,
            } => {
                event_counts.tool_call += 1;
                push_truncation_warning(
                    &mut warnings,
                    content.as_str(),
                    "content",
                    Some(metadata.step_index),
                );

                let normalized_name = normalize_optional(tool_name.as_deref());
                if normalized_name == MISSING_TOOL_NAME {
                    warnings.push(ReplayWarning {
                        code: ReplayWarningCode::MissingToolName,
                        message: format!(
                            "tool call at step {} is missing a tool name",
                            metadata.step_index
                        ),
                        step_index: Some(metadata.step_index),
                    });
                }

                let usage = tool_usage
                    .entry(normalized_name.to_owned())
                    .or_insert_with(|| ToolUsageSummary {
                        name: normalized_name.to_owned(),
                        call_count: 0,
                        result_count: 0,
                    });
                usage.call_count += 1;

                match arguments.as_deref().map(str::trim) {
                    Some(value) if !value.is_empty() => {
                        push_truncation_warning(
                            &mut warnings,
                            value,
                            "tool_arguments",
                            Some(metadata.step_index),
                        );
                    }
                    _ => warnings.push(ReplayWarning {
                        code: ReplayWarningCode::MissingToolArguments,
                        message: format!(
                            "tool call '{}' at step {} is missing arguments",
                            normalized_name, metadata.step_index
                        ),
                        step_index: Some(metadata.step_index),
                    }),
                }
            }
            ReplayEvent::ToolResult {
                metadata,
                tool_name,
                result,
                content,
            } => {
                event_counts.tool_result += 1;
                push_truncation_warning(
                    &mut warnings,
                    content.as_str(),
                    "content",
                    Some(metadata.step_index),
                );

                let normalized_name = normalize_optional(tool_name.as_deref());
                if normalized_name == MISSING_TOOL_NAME {
                    warnings.push(ReplayWarning {
                        code: ReplayWarningCode::MissingToolName,
                        message: format!(
                            "tool result at step {} is missing a tool name",
                            metadata.step_index
                        ),
                        step_index: Some(metadata.step_index),
                    });
                }

                let usage = tool_usage
                    .entry(normalized_name.to_owned())
                    .or_insert_with(|| ToolUsageSummary {
                        name: normalized_name.to_owned(),
                        call_count: 0,
                        result_count: 0,
                    });
                usage.result_count += 1;

                match result.as_deref().map(str::trim) {
                    Some(value) if !value.is_empty() => {
                        push_truncation_warning(
                            &mut warnings,
                            value,
                            "tool_result",
                            Some(metadata.step_index),
                        );
                    }
                    _ => warnings.push(ReplayWarning {
                        code: ReplayWarningCode::MissingToolResult,
                        message: format!(
                            "tool result '{}' at step {} is missing payload data",
                            normalized_name, metadata.step_index
                        ),
                        step_index: Some(metadata.step_index),
                    }),
                }
            }
        }
    }

    if trajectory.completed_at.is_none() {
        warnings.push(ReplayWarning {
            code: ReplayWarningCode::MissingCompletionTimestamp,
            message: "trajectory does not include completed_at".to_owned(),
            step_index: None,
        });
    }

    if trajectory.outcome.is_none() {
        warnings.push(ReplayWarning {
            code: ReplayWarningCode::MissingOutcome,
            message: "trajectory does not include an outcome".to_owned(),
            step_index: None,
        });
    }

    let total_events = event_counts.user
        + event_counts.assistant
        + event_counts.tool_call
        + event_counts.tool_result
        + event_counts.system;

    ReplayReport {
        source_path: source_path.map(|path| path.display().to_string()),
        session_id: trajectory.session_id.clone(),
        model: trajectory.model.clone(),
        system_prompt_hash: trajectory.system_prompt_hash.clone(),
        started_at: trajectory.started_at.clone(),
        completed_at: trajectory.completed_at.clone(),
        outcome: trajectory.outcome.clone(),
        tags: trajectory.tags.clone(),
        total_events,
        event_counts,
        tool_usage: tool_usage.into_values().collect(),
        warnings,
    }
}

fn normalize_optional(value: Option<&str>) -> &str {
    match value.map(str::trim) {
        Some(value) if !value.is_empty() => value,
        _ => MISSING_TOOL_NAME,
    }
}

fn push_truncation_warning(
    warnings: &mut Vec<ReplayWarning>,
    value: &str,
    field_name: &str,
    step_index: Option<usize>,
) {
    if value.contains(TRUNCATED_MARKER) {
        let location = match step_index {
            Some(step_index) => format!("step {}", step_index),
            None => "trajectory".to_owned(),
        };
        warnings.push(ReplayWarning {
            code: ReplayWarningCode::TruncatedContent,
            message: format!("{field_name} at {location} appears truncated"),
            step_index,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::trajectory::{ActionType, TrajectoryStep};

    use super::*;

    #[test]
    fn build_replay_report_summarizes_events_and_tools() {
        let trajectory = Trajectory {
            session_id: "session-1".to_owned(),
            model: "gpt-test".to_owned(),
            system_prompt_hash: "hash-1".to_owned(),
            started_at: "2026-03-08T10:00:00Z".to_owned(),
            completed_at: Some("2026-03-08T10:01:00Z".to_owned()),
            steps: vec![
                TrajectoryStep {
                    step_index: 2,
                    timestamp: "2026-03-08T10:00:02Z".to_owned(),
                    action_type: ActionType::ToolCall,
                    content: "tool_call: shell".to_owned(),
                    tool_name: Some("shell".to_owned()),
                    tool_arguments: Some("{\"cmd\":\"pwd\"}".to_owned()),
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 1,
                    timestamp: "2026-03-08T10:00:01Z".to_owned(),
                    action_type: ActionType::AssistantMessage,
                    content: "running a tool".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 3,
                    timestamp: "2026-03-08T10:00:03Z".to_owned(),
                    action_type: ActionType::ToolResult,
                    content: "tool_result: shell".to_owned(),
                    tool_name: Some("shell".to_owned()),
                    tool_arguments: None,
                    tool_result: Some("/tmp".to_owned()),
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 0,
                    timestamp: "2026-03-08T10:00:00Z".to_owned(),
                    action_type: ActionType::UserMessage,
                    content: "print working directory".to_owned(),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: None,
                },
            ],
            outcome: Some(TrajectoryOutcome::Success),
            tags: vec!["offline_eval".to_owned(), "smoke".to_owned()],
        };

        let report = build_replay_report(&trajectory);

        assert_eq!(report.session_id, "session-1");
        assert_eq!(report.model, "gpt-test");
        assert_eq!(report.total_events, 4);
        assert_eq!(
            report.event_counts,
            ReplayEventCounts {
                user: 1,
                assistant: 1,
                tool_call: 1,
                tool_result: 1,
                system: 0,
            }
        );
        assert_eq!(
            report.tool_usage,
            vec![ToolUsageSummary {
                name: "shell".to_owned(),
                call_count: 1,
                result_count: 1,
            }]
        );
        assert_eq!(report.outcome, Some(TrajectoryOutcome::Success));
        assert_eq!(report.tags, vec!["offline_eval", "smoke"]);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn build_replay_report_emits_replayability_warnings() {
        let trajectory = Trajectory {
            session_id: "session-2".to_owned(),
            model: "gpt-test".to_owned(),
            system_prompt_hash: "hash-2".to_owned(),
            started_at: "2026-03-08T11:00:00Z".to_owned(),
            completed_at: None,
            steps: vec![
                TrajectoryStep {
                    step_index: 0,
                    timestamp: "2026-03-08T11:00:00Z".to_owned(),
                    action_type: ActionType::ToolCall,
                    content: format!("tool_call{}", TRUNCATED_MARKER),
                    tool_name: None,
                    tool_arguments: None,
                    tool_result: None,
                    tokens: None,
                },
                TrajectoryStep {
                    step_index: 1,
                    timestamp: "2026-03-08T11:00:01Z".to_owned(),
                    action_type: ActionType::ToolResult,
                    content: "tool_result".to_owned(),
                    tool_name: Some("".to_owned()),
                    tool_arguments: None,
                    tool_result: Some(format!("payload{}", TRUNCATED_MARKER)),
                    tokens: None,
                },
            ],
            outcome: None,
            tags: vec![],
        };

        let report = build_replay_report(&trajectory);

        assert_eq!(report.tool_usage.len(), 1);
        assert_eq!(report.tool_usage[0].name, MISSING_TOOL_NAME);
        assert_eq!(report.tool_usage[0].call_count, 1);
        assert_eq!(report.tool_usage[0].result_count, 1);

        let warning_codes = report
            .warnings
            .iter()
            .map(|warning| warning.code.clone())
            .collect::<Vec<_>>();

        assert!(warning_codes.contains(&ReplayWarningCode::MissingToolName));
        assert!(warning_codes.contains(&ReplayWarningCode::MissingToolArguments));
        assert!(warning_codes.contains(&ReplayWarningCode::TruncatedContent));
        assert!(warning_codes.contains(&ReplayWarningCode::MissingCompletionTimestamp));
        assert!(warning_codes.contains(&ReplayWarningCode::MissingOutcome));
        assert!(!warning_codes.contains(&ReplayWarningCode::MissingToolResult));
    }

    #[test]
    fn load_raw_trajectory_and_report_from_disk() {
        let trajectory = Trajectory {
            session_id: "session-3".to_owned(),
            model: "gpt-test".to_owned(),
            system_prompt_hash: "hash-3".to_owned(),
            started_at: "2026-03-08T12:00:00Z".to_owned(),
            completed_at: Some("2026-03-08T12:00:30Z".to_owned()),
            steps: vec![TrajectoryStep {
                step_index: 0,
                timestamp: "2026-03-08T12:00:00Z".to_owned(),
                action_type: ActionType::SystemMessage,
                content: "boot".to_owned(),
                tool_name: None,
                tool_arguments: None,
                tool_result: None,
                tokens: None,
            }],
            outcome: Some(TrajectoryOutcome::Abandoned),
            tags: vec!["fixture".to_owned()],
        };

        let path = unique_temp_path("trajectory");
        let serialized = serde_json::to_string(&trajectory).expect("serialize trajectory");
        fs::write(&path, serialized).expect("write trajectory fixture");

        let loaded = load_raw_trajectory(&path).expect("load trajectory");
        let report = load_and_report(&path).expect("load report");

        assert_eq!(loaded.session_id, "session-3");
        assert_eq!(report.source_path, Some(path.display().to_string()));
        assert_eq!(report.event_counts.system, 1);
        assert_eq!(report.outcome, Some(TrajectoryOutcome::Abandoned));

        let _ = fs::remove_file(path);
    }

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{nanos}.json"))
    }
}
