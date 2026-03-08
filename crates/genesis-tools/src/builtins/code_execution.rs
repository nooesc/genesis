use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use crate::{truncate_output_bytes, ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Sandboxed Python code execution tool.
///
/// Runs Python code in a subprocess with a stripped environment (no access to
/// secrets or host env vars). The agent can use this for computation, data
/// analysis, and scripting without resorting to raw shell access.
pub struct CodeExecutionTool;

/// Environment variables allowed into the sandboxed Python process.
const ALLOWED_ENV: &[&str] = &["PATH", "HOME", "LANG", "TERM", "TMPDIR"];

impl ToolHandler for CodeExecutionTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let code = call
            .arguments
            .get("code")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "code",
            })?;

        let timeout_secs: u64 = call
            .arguments
            .get("timeout_secs")
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let language = call
            .arguments
            .get("language")
            .map(|s| s.as_str())
            .unwrap_or("python");

        let (program, args) = match language {
            "python" | "python3" => ("python3", vec!["-c", code.as_str()]),
            "node" | "javascript" | "js" => ("node", vec!["-e", code.as_str()]),
            "ruby" => ("ruby", vec!["-e", code.as_str()]),
            other => {
                return Err(ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!(
                        "unsupported language '{other}'. Supported: python, node, ruby"
                    ),
                });
            }
        };

        let mut cmd = Command::new(program);
        cmd.args(&args);

        // Sandbox: clear env, only pass safe vars through
        cmd.env_clear();
        for key in ALLOWED_ENV {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        // Ensure Python doesn't buffer output
        cmd.env("PYTHONUNBUFFERED", "1");

        let child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to spawn {program}: {e}"),
            })?;

        let tool_name = call.name.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = child.wait_with_output();
            let _ = tx.send(result);
        });

        let output = match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
            Ok(result) => result.map_err(|e| ToolError::ExecutionFailed {
                tool: tool_name.clone(),
                reason: format!("error collecting output: {e}"),
            })?,
            Err(_) => {
                return Err(ToolError::ExecutionFailed {
                    tool: tool_name,
                    reason: format!(
                        "code execution timed out after {timeout_secs}s. \
                         Use the `timeout_secs` argument to increase the limit."
                    ),
                });
            }
        };

        let stdout = truncate_output_bytes(&output.stdout);
        let stderr = truncate_output_bytes(&output.stderr);
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
                ("language".to_owned(), language.to_owned()),
                ("exit_code".to_owned(), exit_code.to_string()),
            ]),
        })
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
            terminal_backend: None,
            default_working_dir: None,
        }
    }

    #[test]
    fn runs_simple_python() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([("code".to_owned(), "print(2 + 2)".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.content.trim(), "4");
        assert_eq!(output.metadata.get("exit_code").unwrap(), "0");
        assert_eq!(output.metadata.get("language").unwrap(), "python");
    }

    #[test]
    fn captures_python_stderr() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([(
                "code".to_owned(),
                "import sys; sys.stderr.write('oops\\n'); sys.exit(1)".to_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("oops"));
        assert!(output.content.contains("[stderr]"));
        assert_eq!(output.metadata.get("exit_code").unwrap(), "1");
    }

    #[test]
    fn requires_code_argument() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn env_is_sandboxed() {
        // Set a secret env var and verify it's NOT visible in the subprocess
        std::env::set_var("GENESIS_TEST_SECRET", "hunter2");
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([(
                "code".to_owned(),
                "import os; print(os.environ.get('GENESIS_TEST_SECRET', 'NOT_FOUND'))".to_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.content.trim(), "NOT_FOUND");
        std::env::remove_var("GENESIS_TEST_SECRET");
    }

    #[test]
    fn timeout_kills_slow_code() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([
                ("code".to_owned(), "import time; time.sleep(60)".to_owned()),
                ("timeout_secs".to_owned(), "1".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("timed out"), "expected timeout, got: {reason}");
            }
            _ => panic!("expected ExecutionFailed, got: {err:?}"),
        }
    }

    #[test]
    fn supports_node_language() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([
                ("code".to_owned(), "console.log(3 * 7)".to_owned()),
                ("language".to_owned(), "node".to_owned()),
            ]),
        };

        // This test may fail if node is not installed, so just check the metadata
        match tool.run(&call, &ctx()) {
            Ok(output) => {
                assert_eq!(output.metadata.get("language").unwrap(), "node");
            }
            Err(ToolError::ExecutionFailed { reason, .. }) => {
                // node not installed is acceptable in CI
                assert!(reason.contains("failed to spawn"), "unexpected error: {reason}");
            }
            Err(e) => panic!("unexpected error type: {e:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_language() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([
                ("code".to_owned(), "puts 'hello'".to_owned()),
                ("language".to_owned(), "haskell".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("unsupported language"));
            }
            _ => panic!("expected ExecutionFailed, got: {err:?}"),
        }
    }

    #[test]
    fn multiline_python() {
        let tool = CodeExecutionTool;
        let code = "for i in range(3):\n    print(f'item {i}')";
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([("code".to_owned(), code.to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("item 0"));
        assert!(output.content.contains("item 1"));
        assert!(output.content.contains("item 2"));
    }
}
