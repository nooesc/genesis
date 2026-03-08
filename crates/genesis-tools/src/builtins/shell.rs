use std::collections::BTreeMap;
use std::process::Command;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

pub struct ShellExecTool;

impl ToolHandler for ShellExecTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = call
            .arguments
            .get("command")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "command",
            })?;

        let working_dir = call.arguments.get("working_dir");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        let output = cmd.output().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to spawn shell: {e}"),
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
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
        }
    }

    #[test]
    fn shell_exec_runs_simple_command() {
        let tool = ShellExecTool;
        let call = ToolCall {
            name: "shell_exec".to_owned(),
            arguments: BTreeMap::from([("command".to_owned(), "echo hello".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.content.trim(), "hello");
        assert_eq!(output.metadata.get("exit_code").unwrap(), "0");
    }

    #[test]
    fn shell_exec_captures_stderr() {
        let tool = ShellExecTool;
        let call = ToolCall {
            name: "shell_exec".to_owned(),
            arguments: BTreeMap::from([(
                "command".to_owned(),
                "echo oops >&2; exit 1".to_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("oops"));
        assert!(output.content.contains("[stderr]"));
        assert_eq!(output.metadata.get("exit_code").unwrap(), "1");
    }

    #[test]
    fn shell_exec_requires_command_argument() {
        let tool = ShellExecTool;
        let call = ToolCall {
            name: "shell_exec".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn shell_exec_respects_working_dir() {
        let tool = ShellExecTool;
        let call = ToolCall {
            name: "shell_exec".to_owned(),
            arguments: BTreeMap::from([
                ("command".to_owned(), "pwd".to_owned()),
                ("working_dir".to_owned(), "/tmp".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        // macOS resolves /tmp to /private/tmp
        assert!(
            output.content.contains("/tmp"),
            "output should contain /tmp: {}",
            output.content
        );
    }
}
