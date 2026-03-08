use std::collections::BTreeMap;
use std::process::Command;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Tool that runs a command inside a Docker container via `docker exec`.
pub struct DockerExecTool;

impl ToolHandler for DockerExecTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let container = call
            .arguments
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

        let stdout = truncate_output(&output.stdout);
        let stderr = truncate_output(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let mut content = String::new();
        if !stdout.is_empty() {
            content.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("[stderr]\n");
            content.push_str(&stderr);
        }
        if content.is_empty() {
            content = format!("(no output, exit code {exit_code})");
        }

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

fn truncate_output(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() > MAX_OUTPUT_BYTES {
        let mut truncated = s[..MAX_OUTPUT_BYTES].to_string();
        truncated.push_str("\n... (output truncated)");
        truncated
    } else {
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
            terminal_backend: None,
        }
    }

    #[test]
    fn docker_exec_requires_container_argument() {
        let tool = DockerExecTool;
        let call = ToolCall {
            name: "docker_exec".to_owned(),
            arguments: BTreeMap::from([("command".to_owned(), "ls".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { argument: "container", .. }));
    }

    #[test]
    fn docker_exec_requires_command_argument() {
        let tool = DockerExecTool;
        let call = ToolCall {
            name: "docker_exec".to_owned(),
            arguments: BTreeMap::from([("container".to_owned(), "my-app".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { argument: "command", .. }));
    }
}
