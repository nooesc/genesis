use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_RESULTS: usize = 50;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

pub struct SearchFilesTool;

impl ToolHandler for SearchFilesTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = call
            .arguments
            .get("pattern")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "pattern",
            })?;

        let path = call
            .arguments
            .get("path")
            .map(|p| p.as_str())
            .unwrap_or(".");

        if !Path::new(path).exists() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("path does not exist: {path}"),
            });
        }

        // Use grep -rn for recursive search with line numbers
        let output = Command::new("grep")
            .args(["-rn", "--include=*", "-m", &MAX_RESULTS.to_string(), pattern, path])
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to run grep: {e}"),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let content = if stdout.is_empty() {
            format!("no matches found for pattern `{pattern}` in {path}")
        } else if stdout.len() > MAX_OUTPUT_BYTES {
            let mut truncated = stdout[..MAX_OUTPUT_BYTES].to_string();
            truncated.push_str("\n... (output truncated)");
            truncated
        } else {
            stdout.into_owned()
        };

        let match_count = content.lines().count();

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("match_count".to_owned(), match_count.to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: false,
        }
    }

    #[test]
    fn search_files_finds_matching_content() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("hello.txt"), "hello world\ngoodbye world\n").unwrap();
        fs::write(dir.path().join("other.txt"), "nothing here\n").unwrap();

        let tool = SearchFilesTool;
        let call = ToolCall {
            name: "search_files".to_owned(),
            arguments: BTreeMap::from([
                ("pattern".to_owned(), "hello".to_owned()),
                ("path".to_owned(), dir.path().display().to_string()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("hello world"));
        assert!(!output.content.contains("nothing here"));
    }

    #[test]
    fn search_files_reports_no_matches() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("file.txt"), "nothing relevant\n").unwrap();

        let tool = SearchFilesTool;
        let call = ToolCall {
            name: "search_files".to_owned(),
            arguments: BTreeMap::from([
                ("pattern".to_owned(), "nonexistent_pattern_xyz".to_owned()),
                ("path".to_owned(), dir.path().display().to_string()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("no matches found"));
    }

    #[test]
    fn search_files_requires_pattern() {
        let tool = SearchFilesTool;
        let call = ToolCall {
            name: "search_files".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn search_files_errors_on_missing_path() {
        let tool = SearchFilesTool;
        let call = ToolCall {
            name: "search_files".to_owned(),
            arguments: BTreeMap::from([
                ("pattern".to_owned(), "test".to_owned()),
                ("path".to_owned(), "/nonexistent/path/abc123".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }
}
