use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Lightweight representation of trajectory data used by the tool.
/// This mirrors the core Trajectory struct but lives in genesis-tools to avoid
/// a circular dependency on genesis-core.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrajectoryData {
    session_id: String,
    model: String,
    system_prompt_hash: String,
    started_at: String,
    completed_at: Option<String>,
    steps: Vec<serde_json::Value>,
    outcome: Option<serde_json::Value>,
    tags: Vec<String>,
}

/// Tool that lets the agent interact with trajectory recordings on disk.
///
/// Supported actions:
/// - `export` — reads and returns a trajectory file for the current session
/// - `status` — reports whether a trajectory file exists for the current session
/// - `tag` — adds a tag to the current session's trajectory
/// - `set_outcome` — sets the outcome on the current session's trajectory
pub struct TrajectoryTool;

fn trajectories_dir(context: &ToolContext) -> PathBuf {
    Path::new(&context.data_dir).join("trajectories")
}

fn trajectory_path(context: &ToolContext) -> PathBuf {
    trajectories_dir(context).join(format!("{}.json", context.session_id))
}

fn ensure_trajectories_dir(context: &ToolContext, tool_name: &str) -> Result<(), ToolError> {
    let dir = trajectories_dir(context);
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to create trajectories directory: {e}"),
        })?;
    }
    Ok(())
}

fn load_trajectory(
    context: &ToolContext,
    tool_name: &str,
) -> Result<TrajectoryData, ToolError> {
    let path = trajectory_path(context);
    let raw = fs::read_to_string(&path).map_err(|e| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!(
            "failed to read trajectory file `{}`: {e}",
            path.display()
        ),
    })?;
    serde_json::from_str(&raw).map_err(|e| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!(
            "failed to parse trajectory file `{}`: {e}",
            path.display()
        ),
    })
}

fn save_trajectory(
    context: &ToolContext,
    tool_name: &str,
    trajectory: &TrajectoryData,
) -> Result<(), ToolError> {
    ensure_trajectories_dir(context, tool_name)?;
    let path = trajectory_path(context);
    let json = serde_json::to_string_pretty(trajectory).map_err(|e| {
        ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to serialize trajectory: {e}"),
        }
    })?;
    fs::write(&path, json).map_err(|e| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!(
            "failed to write trajectory file `{}`: {e}",
            path.display()
        ),
    })
}

impl ToolHandler for TrajectoryTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let action = call
            .arguments
            .get("action")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "action",
            })?;

        match action.as_str() {
            "export" => self.handle_export(call, context),
            "status" => self.handle_status(call, context),
            "tag" => self.handle_tag(call, context),
            "set_outcome" => self.handle_set_outcome(call, context),
            other => Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "unknown action `{other}`; valid actions: export, status, tag, set_outcome"
                ),
            }),
        }
    }
}

impl TrajectoryTool {
    fn handle_export(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let format = call
            .arguments
            .get("format")
            .map(|s| s.as_str())
            .unwrap_or("json");

        let trajectory = load_trajectory(context, &call.name)?;

        let content = match format {
            "sharegpt" => export_sharegpt(&trajectory, &call.name)?,
            "chatml" => export_chatml(&trajectory),
            "json" => {
                serde_json::to_string_pretty(&trajectory).map_err(|e| {
                    ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: format!("failed to serialize trajectory: {e}"),
                    }
                })?
            }
            other => {
                return Err(ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!(
                        "unknown format: {other}. Use 'json', 'sharegpt', or 'chatml'."
                    ),
                });
            }
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("format".to_owned(), format.to_owned()),
                ("session_id".to_owned(), context.session_id.clone()),
            ]),
        })
    }

    fn handle_status(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = trajectory_path(context);
        if path.exists() {
            let trajectory = load_trajectory(context, &call.name)?;
            let step_count = trajectory.steps.len();
            let has_outcome = trajectory.outcome.is_some();
            let is_complete = trajectory.completed_at.is_some();
            let tag_count = trajectory.tags.len();

            Ok(ToolOutput {
                content: format!(
                    "trajectory recording active for session `{}`\n\
                     steps: {step_count}\n\
                     tags: {tag_count}\n\
                     outcome set: {has_outcome}\n\
                     completed: {is_complete}\n\
                     file: {}",
                    context.session_id,
                    path.display()
                ),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), call.name.clone()),
                    ("exists".to_owned(), "true".to_owned()),
                    ("step_count".to_owned(), step_count.to_string()),
                    ("session_id".to_owned(), context.session_id.clone()),
                ]),
            })
        } else {
            Ok(ToolOutput {
                content: format!(
                    "no trajectory recording found for session `{}`\n\
                     expected path: {}",
                    context.session_id,
                    path.display()
                ),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), call.name.clone()),
                    ("exists".to_owned(), "false".to_owned()),
                    ("session_id".to_owned(), context.session_id.clone()),
                ]),
            })
        }
    }

    fn handle_tag(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let tag = call
            .arguments
            .get("tag")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "tag",
            })?;

        let mut trajectory = load_trajectory(context, &call.name)?;

        if !trajectory.tags.contains(tag) {
            trajectory.tags.push(tag.clone());
        }

        save_trajectory(context, &call.name, &trajectory)?;

        Ok(ToolOutput {
            content: format!(
                "added tag `{tag}` to trajectory for session `{}`; tags: [{}]",
                context.session_id,
                trajectory.tags.join(", ")
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("tag".to_owned(), tag.clone()),
                ("session_id".to_owned(), context.session_id.clone()),
            ]),
        })
    }

    fn handle_set_outcome(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let outcome_str = call
            .arguments
            .get("outcome")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "outcome",
            })?;

        let outcome_value = match outcome_str.as_str() {
            "success" => serde_json::json!({"type": "success"}),
            "abandoned" => serde_json::json!({"type": "abandoned"}),
            "failure" => {
                let reason = call
                    .arguments
                    .get("reason")
                    .cloned()
                    .unwrap_or_else(|| "unspecified".to_owned());
                serde_json::json!({"type": "failure", "reason": reason})
            }
            other => {
                return Err(ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!(
                        "unknown outcome `{other}`; valid outcomes: success, failure, abandoned"
                    ),
                });
            }
        };

        let mut trajectory = load_trajectory(context, &call.name)?;
        trajectory.outcome = Some(outcome_value);
        save_trajectory(context, &call.name, &trajectory)?;

        Ok(ToolOutput {
            content: format!(
                "set outcome `{outcome_str}` on trajectory for session `{}`",
                context.session_id
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("outcome".to_owned(), outcome_str.clone()),
                ("session_id".to_owned(), context.session_id.clone()),
            ]),
        })
    }
}

fn format_step_value(step: &serde_json::Value, action_type: &str) -> String {
    match action_type {
        "tool_call" => {
            let name = step
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let args = step
                .get("tool_arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            format!("{name}: {args}")
        }
        "tool_result" => {
            let name = step
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let result = step
                .get("tool_result")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("{name}: {result}")
        }
        _ => step
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
    }
}

fn export_sharegpt(trajectory: &TrajectoryData, tool_name: &str) -> Result<String, ToolError> {
    let entries: Vec<serde_json::Value> = trajectory
        .steps
        .iter()
        .filter_map(|step| {
            let action_type = step.get("action_type")?.as_str()?;
            let from = match action_type {
                "user_message" => "human",
                "assistant_message" => "gpt",
                "tool_call" => "tool_call",
                "tool_result" => "tool_result",
                "system_message" => "system",
                _ => return None,
            };

            Some(serde_json::json!({
                "from": from,
                "value": format_step_value(step, action_type),
            }))
        })
        .collect();

    serde_json::to_string_pretty(&entries).map_err(|e| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!("failed to serialize sharegpt format: {e}"),
    })
}

fn export_chatml(trajectory: &TrajectoryData) -> String {
    let mut output = String::new();

    for step in &trajectory.steps {
        let Some(action_type) = step.get("action_type").and_then(|v| v.as_str()) else {
            continue;
        };

        let role = match action_type {
            "user_message" => "user",
            "assistant_message" => "assistant",
            "tool_call" => "assistant",
            "tool_result" => "tool",
            "system_message" => "system",
            _ => continue,
        };

        output.push_str(&format!(
            "<|im_start|>{role}\n{}<|im_end|>\n",
            format_step_value(step, action_type)
        ));
    }

    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use super::{trajectory_path, TrajectoryTool};
    use crate::{ToolCall, ToolContext, ToolHandler};

    fn ctx(data_dir: &str) -> ToolContext {
        ToolContext {
            session_id: "session-42".to_owned(),
            data_dir: data_dir.to_owned(),
            ..crate::test_utils::test_ctx()
        }
    }

    /// Write a minimal valid trajectory file for the given context.
    fn write_sample_trajectory(context: &ToolContext) {
        let dir = std::path::Path::new(&context.data_dir).join("trajectories");
        std::fs::create_dir_all(&dir).expect("should create trajectories dir");
        let path = dir.join(format!("{}.json", context.session_id));
        let data = serde_json::json!({
            "session_id": context.session_id,
            "model": "gpt-4",
            "system_prompt_hash": "abc123",
            "started_at": "2026-01-01T00:00:00+00:00",
            "completed_at": null,
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-01-01T00:00:01+00:00",
                    "action_type": "user_message",
                    "content": "hello"
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-01-01T00:00:02+00:00",
                    "action_type": "assistant_message",
                    "content": "hi there"
                },
                {
                    "step_index": 2,
                    "timestamp": "2026-01-01T00:00:03+00:00",
                    "action_type": "tool_call",
                    "content": "tool_call: read_file",
                    "tool_name": "read_file",
                    "tool_arguments": "{\"path\":\"/tmp/f.txt\"}"
                },
                {
                    "step_index": 3,
                    "timestamp": "2026-01-01T00:00:04+00:00",
                    "action_type": "tool_result",
                    "content": "tool_result: read_file",
                    "tool_name": "read_file",
                    "tool_result": "file contents here"
                }
            ],
            "outcome": null,
            "tags": ["demo"]
        });
        std::fs::write(path, serde_json::to_string_pretty(&data).unwrap())
            .expect("should write trajectory");
    }

    #[test]
    fn status_reports_no_trajectory_when_file_missing() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let output = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([("action".to_owned(), "status".to_owned())]),
                },
                &context,
            )
            .expect("status should succeed");

        assert!(output.content.contains("no trajectory recording found"));
        assert_eq!(output.metadata.get("exists").unwrap(), "false");
    }

    #[test]
    fn status_reports_active_trajectory_when_file_exists() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let output = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([("action".to_owned(), "status".to_owned())]),
                },
                &context,
            )
            .expect("status should succeed");

        assert!(output.content.contains("trajectory recording active"));
        assert!(output.content.contains("steps: 4"));
        assert_eq!(output.metadata.get("exists").unwrap(), "true");
        assert_eq!(output.metadata.get("step_count").unwrap(), "4");
    }

    #[test]
    fn export_json_returns_full_trajectory() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let output = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([("action".to_owned(), "export".to_owned())]),
                },
                &context,
            )
            .expect("export should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&output.content).expect("output should be valid JSON");
        assert_eq!(parsed["session_id"], "session-42");
        assert_eq!(parsed["steps"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn export_sharegpt_format() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let output = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "export".to_owned()),
                        ("format".to_owned(), "sharegpt".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("sharegpt export should succeed");

        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&output.content).expect("should be valid JSON array");

        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0]["from"], "human");
        assert_eq!(entries[0]["value"], "hello");
        assert_eq!(entries[1]["from"], "gpt");
        assert_eq!(entries[1]["value"], "hi there");
        assert_eq!(entries[2]["from"], "tool_call");
        assert!(entries[2]["value"].as_str().unwrap().contains("read_file"));
        assert_eq!(entries[3]["from"], "tool_result");
        assert!(entries[3]["value"]
            .as_str()
            .unwrap()
            .contains("file contents here"));
    }

    #[test]
    fn export_chatml_format() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        let trajectory = serde_json::json!({
            "session_id": context.session_id,
            "model": "gpt-4",
            "system_prompt_hash": "abc123",
            "started_at": "2026-01-01T00:00:00+00:00",
            "completed_at": null,
            "steps": [
                {
                    "step_index": 0,
                    "timestamp": "2026-01-01T00:00:00+00:00",
                    "action_type": "system_message",
                    "content": "Follow the operator playbook."
                },
                {
                    "step_index": 1,
                    "timestamp": "2026-01-01T00:00:01+00:00",
                    "action_type": "user_message",
                    "content": "hello"
                },
                {
                    "step_index": 2,
                    "timestamp": "2026-01-01T00:00:02+00:00",
                    "action_type": "assistant_message",
                    "content": "hi there"
                },
                {
                    "step_index": 3,
                    "timestamp": "2026-01-01T00:00:03+00:00",
                    "action_type": "tool_call",
                    "content": "tool_call: read_file",
                    "tool_name": "read_file",
                    "tool_arguments": "{\"path\":\"/tmp/f.txt\"}"
                },
                {
                    "step_index": 4,
                    "timestamp": "2026-01-01T00:00:04+00:00",
                    "action_type": "tool_result",
                    "content": "tool_result: read_file",
                    "tool_name": "read_file",
                    "tool_result": "file contents here"
                }
            ],
            "outcome": null,
            "tags": ["demo"]
        });
        let path = std::path::Path::new(&context.data_dir)
            .join("trajectories")
            .join(format!("{}.json", context.session_id));
        std::fs::create_dir_all(path.parent().unwrap()).expect("should create trajectories dir");
        std::fs::write(path, serde_json::to_string_pretty(&trajectory).unwrap())
            .expect("should write trajectory");

        let output = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "export".to_owned()),
                        ("format".to_owned(), "chatml".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("chatml export should succeed");

        assert!(output
            .content
            .contains("<|im_start|>system\nFollow the operator playbook.<|im_end|>\n"));
        assert!(output.content.contains("<|im_start|>user\nhello<|im_end|>\n"));
        assert!(output
            .content
            .contains("<|im_start|>assistant\nhi there<|im_end|>\n"));
        assert!(output.content.contains(
            "<|im_start|>assistant\nread_file: {\"path\":\"/tmp/f.txt\"}<|im_end|>\n"
        ));
        assert!(output.content.contains(
            "<|im_start|>tool\nread_file: file contents here<|im_end|>\n"
        ));
    }

    #[test]
    fn export_fails_when_no_trajectory_file() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let err = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([("action".to_owned(), "export".to_owned())]),
                },
                &context,
            )
            .expect_err("export should fail without a file");

        match err {
            crate::ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("failed to read trajectory file"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[test]
    fn tag_adds_tag_and_persists() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let output = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "tag".to_owned()),
                        ("tag".to_owned(), "training".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("tag should succeed");

        assert!(output.content.contains("added tag `training`"));
        assert!(output.content.contains("demo"));
        assert!(output.content.contains("training"));

        // Verify persistence by re-reading.
        let raw = std::fs::read_to_string(trajectory_path(&context)).expect("should read file");
        let data: serde_json::Value = serde_json::from_str(&raw).expect("should parse");
        let tags: Vec<&str> = data["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(tags.contains(&"demo"));
        assert!(tags.contains(&"training"));
    }

    #[test]
    fn tag_deduplicates() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        // Add "demo" which already exists.
        TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "tag".to_owned()),
                        ("tag".to_owned(), "demo".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("tag should succeed");

        let raw = std::fs::read_to_string(trajectory_path(&context)).expect("should read file");
        let data: serde_json::Value = serde_json::from_str(&raw).expect("should parse");
        let tags = data["tags"].as_array().unwrap();
        // Should still only have one "demo".
        let demo_count = tags
            .iter()
            .filter(|v| v.as_str() == Some("demo"))
            .count();
        assert_eq!(demo_count, 1);
    }

    #[test]
    fn tag_requires_tag_argument() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let err = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([("action".to_owned(), "tag".to_owned())]),
                },
                &context,
            )
            .expect_err("tag without argument should fail");

        match err {
            crate::ToolError::MissingArgument { argument, .. } => {
                assert_eq!(argument, "tag");
            }
            other => panic!("expected MissingArgument, got: {other:?}"),
        }
    }

    #[test]
    fn set_outcome_success() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let output = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "set_outcome".to_owned()),
                        ("outcome".to_owned(), "success".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("set_outcome should succeed");

        assert!(output.content.contains("set outcome `success`"));

        // Verify persistence.
        let raw = std::fs::read_to_string(trajectory_path(&context)).expect("should read file");
        let data: serde_json::Value = serde_json::from_str(&raw).expect("should parse");
        assert_eq!(data["outcome"]["type"], "success");
    }

    #[test]
    fn set_outcome_failure_with_reason() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let output = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "set_outcome".to_owned()),
                        ("outcome".to_owned(), "failure".to_owned()),
                        ("reason".to_owned(), "budget exceeded".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("set_outcome failure should succeed");

        assert!(output.content.contains("set outcome `failure`"));

        let raw = std::fs::read_to_string(trajectory_path(&context)).expect("should read file");
        let data: serde_json::Value = serde_json::from_str(&raw).expect("should parse");
        assert_eq!(data["outcome"]["type"], "failure");
        assert_eq!(data["outcome"]["reason"], "budget exceeded");
    }

    #[test]
    fn set_outcome_failure_defaults_reason() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "set_outcome".to_owned()),
                        ("outcome".to_owned(), "failure".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("set_outcome failure without reason should succeed");

        let raw = std::fs::read_to_string(trajectory_path(&context)).expect("should read file");
        let data: serde_json::Value = serde_json::from_str(&raw).expect("should parse");
        assert_eq!(data["outcome"]["reason"], "unspecified");
    }

    #[test]
    fn set_outcome_abandoned() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "set_outcome".to_owned()),
                        ("outcome".to_owned(), "abandoned".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("set_outcome abandoned should succeed");

        let raw = std::fs::read_to_string(trajectory_path(&context)).expect("should read file");
        let data: serde_json::Value = serde_json::from_str(&raw).expect("should parse");
        assert_eq!(data["outcome"]["type"], "abandoned");
    }

    #[test]
    fn set_outcome_invalid_value() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let err = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "set_outcome".to_owned()),
                        ("outcome".to_owned(), "partial".to_owned()),
                    ]),
                },
                &context,
            )
            .expect_err("invalid outcome should fail");

        match err {
            crate::ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("unknown outcome `partial`"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[test]
    fn set_outcome_requires_outcome_argument() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());
        write_sample_trajectory(&context);

        let err = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([
                        ("action".to_owned(), "set_outcome".to_owned()),
                    ]),
                },
                &context,
            )
            .expect_err("set_outcome without argument should fail");

        match err {
            crate::ToolError::MissingArgument { argument, .. } => {
                assert_eq!(argument, "outcome");
            }
            other => panic!("expected MissingArgument, got: {other:?}"),
        }
    }

    #[test]
    fn unknown_action_returns_error() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let err = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::from([("action".to_owned(), "purge".to_owned())]),
                },
                &context,
            )
            .expect_err("unknown action should fail");

        match err {
            crate::ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("unknown action `purge`"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[test]
    fn missing_action_argument_returns_error() {
        let dir = tempdir().expect("tempdir");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let err = TrajectoryTool
            .run(
                &ToolCall {
                    name: "trajectory".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &context,
            )
            .expect_err("missing action should fail");

        match err {
            crate::ToolError::MissingArgument { argument, .. } => {
                assert_eq!(argument, "action");
            }
            other => panic!("expected MissingArgument, got: {other:?}"),
        }
    }

    #[test]
    fn trajectory_path_uses_session_id() {
        let context = ctx("/tmp/genesis");
        let path = trajectory_path(&context);
        assert_eq!(
            path.to_string_lossy(),
            "/tmp/genesis/trajectories/session-42.json"
        );
    }
}
