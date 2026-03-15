use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use crate::{truncate_output, ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_RESULTS_STR: &str = "100";

static RG_AVAILABLE: OnceLock<bool> = OnceLock::new();

fn rg_available() -> bool {
    *RG_AVAILABLE.get_or_init(|| which_exists("rg"))
}

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

        let case_insensitive = call
            .arguments
            .get("case_insensitive")
            .map(|v| v == "true")
            .unwrap_or(false);

        let file_type = call.arguments.get("file_type").map(|s| s.as_str());
        let glob_filter = call.arguments.get("glob").map(|s| s.as_str());
        let context_lines = call
            .arguments
            .get("context_lines")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);

        // Prefer ripgrep (rg), fall back to grep.
        let use_rg = rg_available();

        let output = if use_rg {
            run_ripgrep(
                pattern,
                path,
                case_insensitive,
                file_type,
                glob_filter,
                context_lines,
            )
        } else {
            run_grep(pattern, path, case_insensitive)
        }
        .map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("search failed: {e}"),
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let content = if stdout.is_empty() {
            format!("no matches found for pattern `{pattern}` in {path}")
        } else {
            truncate_output(&stdout)
        };

        let match_count = content
            .lines()
            .filter(|l| !l.starts_with("--")) // skip context separator lines
            .filter(|l| !l.is_empty())
            .count();

        let tool_name = if use_rg { "rg" } else { "grep" };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("match_count".to_owned(), match_count.to_string()),
                ("search_engine".to_owned(), tool_name.to_owned()),
            ]),
        })
    }
}

fn run_ripgrep(
    pattern: &str,
    path: &str,
    case_insensitive: bool,
    file_type: Option<&str>,
    glob_filter: Option<&str>,
    context_lines: usize,
) -> Result<std::process::Output, String> {
    let mut cmd = Command::new("rg");
    cmd.args(["--line-number", "--no-heading", "--color", "never"]);
    cmd.args(["--max-count", MAX_RESULTS_STR]);

    if case_insensitive {
        cmd.arg("--ignore-case");
    }

    if let Some(ft) = file_type {
        cmd.args(["--type", ft]);
    }

    if let Some(g) = glob_filter {
        cmd.args(["--glob", g]);
    }

    if context_lines > 0 {
        cmd.args(["-C", &context_lines.to_string()]);
    }

    cmd.arg(pattern);
    cmd.arg(path);

    cmd.output().map_err(|e| e.to_string())
}

fn run_grep(
    pattern: &str,
    path: &str,
    case_insensitive: bool,
) -> Result<std::process::Output, String> {
    let mut cmd = Command::new("grep");
    cmd.args(["-rn", "--include=*"]);
    cmd.args(["-m", MAX_RESULTS_STR]);

    if case_insensitive {
        cmd.arg("-i");
    }

    cmd.arg(pattern);
    cmd.arg(path);

    cmd.output().map_err(|e| e.to_string())
}

fn which_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_OUTPUT_BYTES;
    use std::fs;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
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

    #[test]
    fn search_files_case_insensitive() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("test.txt"), "Hello World\n").unwrap();

        let tool = SearchFilesTool;
        let call = ToolCall {
            name: "search_files".to_owned(),
            arguments: BTreeMap::from([
                ("pattern".to_owned(), "hello".to_owned()),
                ("path".to_owned(), dir.path().display().to_string()),
                ("case_insensitive".to_owned(), "true".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("Hello World"));
    }

    #[test]
    fn which_exists_finds_common_tools() {
        // grep should exist on all unix systems
        assert!(which_exists("grep"));
        assert!(!which_exists("nonexistent_tool_abc123"));
    }

    #[test]
    fn truncate_output_preserves_short_text() {
        let short = "hello world\n";
        assert_eq!(truncate_output(short), short);
    }

    #[test]
    fn truncate_output_cuts_at_newline() {
        let long = "a\n".repeat(100_000);
        let result = truncate_output(&long);
        assert!(result.len() <= MAX_OUTPUT_BYTES + 50); // allow for suffix
        assert!(result.ends_with("... (output truncated)"));
    }

    #[test]
    fn reports_search_engine_in_metadata() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("test.txt"), "hello\n").unwrap();

        let tool = SearchFilesTool;
        let call = ToolCall {
            name: "search_files".to_owned(),
            arguments: BTreeMap::from([
                ("pattern".to_owned(), "hello".to_owned()),
                ("path".to_owned(), dir.path().display().to_string()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        let engine = output.metadata.get("search_engine").unwrap();
        assert!(engine == "rg" || engine == "grep");
    }
}
