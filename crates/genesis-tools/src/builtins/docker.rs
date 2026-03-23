use std::collections::BTreeMap;
use std::process::Command;

use crate::{truncate_output_bytes, ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that runs a command inside a Docker container via `docker exec`.
pub struct DockerExecTool;

impl ToolHandler for DockerExecTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let container =
            call.arguments
                .get("container")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "container",
                })?;

        let command = call
            .arguments
            .get("command")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "command",
            })?;

        let working_dir = call.arguments.get("working_dir");
        let user = call.arguments.get("user");

        let mut cmd = Command::new("docker");
        cmd.arg("exec");

        if let Some(dir) = working_dir {
            cmd.arg("-w").arg(dir);
        }
        if let Some(u) = user {
            cmd.arg("-u").arg(u);
        }

        cmd.arg(container).arg("sh").arg("-c").arg(command);

        let output = cmd.output().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to run docker exec: {e}"),
        })?;

        let stdout = truncate_output_bytes(&output.stdout);
        let stderr = truncate_output_bytes(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let content = super::combine_command_output(&stdout, &stderr, exit_code);

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("container".to_owned(), container.clone()),
                ("exit_code".to_owned(), exit_code.to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx_destructive()
    }

    #[test]
    fn docker_exec_requires_container_argument() {
        let tool = DockerExecTool;
        let call = ToolCall {
            name: "docker_exec".to_owned(),
            arguments: BTreeMap::from([("command".to_owned(), "ls".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "container",
                ..
            }
        ));
    }

    #[test]
    fn docker_exec_requires_command_argument() {
        let tool = DockerExecTool;
        let call = ToolCall {
            name: "docker_exec".to_owned(),
            arguments: BTreeMap::from([("container".to_owned(), "my-app".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "command",
                ..
            }
        ));
    }

    #[test]
    fn docker_exec_requires_both_arguments() {
        let tool = DockerExecTool;
        let call = ToolCall {
            name: "docker_exec".to_owned(),
            arguments: BTreeMap::new(),
        };
        // Container is checked first
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "container",
                ..
            }
        ));
    }

    #[test]
    fn docker_exec_with_nonexistent_container_fails() {
        let tool = DockerExecTool;
        let call = ToolCall {
            name: "docker_exec".to_owned(),
            arguments: BTreeMap::from([
                (
                    "container".to_owned(),
                    "nonexistent-container-12345".to_owned(),
                ),
                ("command".to_owned(), "echo hello".to_owned()),
            ]),
        };
        // docker exec with a nonexistent container should either fail to spawn
        // docker or return a non-zero exit code. Either way it should not panic.
        let result = tool.run(&call, &ctx());
        match result {
            Ok(output) => {
                // Docker ran but failed (exit code != 0)
                assert!(
                    output.content.contains("[stderr]")
                        || output.metadata.get("exit_code").unwrap() != "0"
                );
            }
            Err(ToolError::ExecutionFailed { .. }) => {
                // Docker binary not found - also acceptable
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
}
