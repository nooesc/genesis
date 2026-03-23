use std::collections::BTreeMap;
use std::process::Command;

use crate::{truncate_output_bytes, ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that runs a command on a remote host via SSH.
pub struct SshExecTool;

impl ToolHandler for SshExecTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let host = call
            .arguments
            .get("host")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "host",
            })?;

        let command = call
            .arguments
            .get("command")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "command",
            })?;

        let user = call.arguments.get("user");
        let port = call.arguments.get("port");
        let identity_file = call.arguments.get("identity_file");

        let mut cmd = Command::new("ssh");

        // Disable strict host key checking for non-interactive use and set
        // a connect timeout so the tool doesn't hang indefinitely.
        cmd.arg("-o").arg("StrictHostKeyChecking=accept-new");
        cmd.arg("-o").arg("ConnectTimeout=10");

        if let Some(p) = port {
            cmd.arg("-p").arg(p);
        }
        if let Some(key) = identity_file {
            cmd.arg("-i").arg(key);
        }

        let destination = match user {
            Some(u) => format!("{u}@{host}"),
            None => host.clone(),
        };
        cmd.arg(&destination).arg(command);

        let output = cmd.output().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to run ssh: {e}"),
        })?;

        let stdout = truncate_output_bytes(&output.stdout);
        let stderr = truncate_output_bytes(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let content = super::combine_command_output(&stdout, &stderr, exit_code);

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("host".to_owned(), destination),
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
    fn ssh_exec_requires_host_argument() {
        let tool = SshExecTool;
        let call = ToolCall {
            name: "ssh_exec".to_owned(),
            arguments: BTreeMap::from([("command".to_owned(), "ls".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "host",
                ..
            }
        ));
    }

    #[test]
    fn ssh_exec_requires_command_argument() {
        let tool = SshExecTool;
        let call = ToolCall {
            name: "ssh_exec".to_owned(),
            arguments: BTreeMap::from([("host".to_owned(), "example.com".to_owned())]),
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
    fn ssh_exec_requires_both_arguments() {
        let tool = SshExecTool;
        let call = ToolCall {
            name: "ssh_exec".to_owned(),
            arguments: BTreeMap::new(),
        };
        // Host is checked first
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "host",
                ..
            }
        ));
    }

    #[test]
    fn ssh_exec_to_unreachable_host_fails_gracefully() {
        let tool = SshExecTool;
        let call = ToolCall {
            name: "ssh_exec".to_owned(),
            arguments: BTreeMap::from([
                ("host".to_owned(), "192.0.2.1".to_owned()), // TEST-NET, unreachable
                ("command".to_owned(), "echo hello".to_owned()),
                ("port".to_owned(), "22222".to_owned()),
            ]),
        };
        // SSH to an unreachable host should either time out or fail without panic.
        let result = tool.run(&call, &ctx());
        match result {
            Ok(output) => {
                // SSH ran but couldn't connect
                assert!(
                    output.metadata.get("exit_code").unwrap() != "0",
                    "should have non-zero exit code"
                );
            }
            Err(ToolError::ExecutionFailed { .. }) => {
                // SSH binary issue - also acceptable
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn ssh_exec_metadata_includes_user_prefix() {
        // We can only test the metadata format since we cannot really SSH.
        // A connection to a bogus host will include user@host in metadata.
        let tool = SshExecTool;
        let call = ToolCall {
            name: "ssh_exec".to_owned(),
            arguments: BTreeMap::from([
                ("host".to_owned(), "192.0.2.1".to_owned()),
                ("command".to_owned(), "whoami".to_owned()),
                ("user".to_owned(), "admin".to_owned()),
                ("port".to_owned(), "22222".to_owned()),
            ]),
        };
        // The result will fail (unreachable) but we can check the metadata host field
        let result = tool.run(&call, &ctx());
        match result {
            Ok(output) => {
                assert_eq!(output.metadata.get("host").unwrap(), "admin@192.0.2.1");
            }
            Err(ToolError::ExecutionFailed { .. }) => {
                // SSH binary missing - skip metadata check
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
}
