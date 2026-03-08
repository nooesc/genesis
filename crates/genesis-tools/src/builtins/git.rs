use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output.to_owned();
    }

    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }

    if let Some(last_nl) = output[..end].rfind('\n') {
        let mut truncated = output[..=last_nl].to_string();
        truncated.push_str("... (output truncated)");
        truncated
    } else {
        let mut truncated = output[..end].to_string();
        truncated.push_str("\n... (output truncated)");
        truncated
    }
}

fn require_arg<'a>(call: &'a ToolCall, name: &'static str) -> Result<&'a str, ToolError> {
    call.arguments
        .get(name)
        .map(|s| s.as_str())
        .ok_or_else(|| ToolError::MissingArgument {
            tool: call.name.clone(),
            argument: name,
        })
}

fn opt_arg<'a>(call: &'a ToolCall, name: &str) -> Option<&'a str> {
    call.arguments.get(name).map(|s| s.as_str())
}

fn resolve_path(call: &ToolCall) -> &str {
    opt_arg(call, "path").unwrap_or(".")
}

fn validate_path(call: &ToolCall, path: &str) -> Result<(), ToolError> {
    if !Path::new(path).exists() {
        return Err(ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("path does not exist: {path}"),
        });
    }
    Ok(())
}

fn run_git(
    call: &ToolCall,
    path: &str,
    args: &[&str],
) -> Result<std::process::Output, ToolError> {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to execute git: {e}"),
        })
}

fn format_git_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&stderr);
    }
    result
}

fn check_destructive(call: &ToolCall, context: &ToolContext) -> Result<(), ToolError> {
    if !context.allow_destructive_tools {
        return Err(ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: "destructive tools are disabled in the current runtime".to_owned(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GitStatusTool
// ---------------------------------------------------------------------------

pub struct GitStatusTool;

impl ToolHandler for GitStatusTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = resolve_path(call);
        validate_path(call, path)?;

        let output = run_git(call, path, &["status", "--porcelain=v2", "--branch"])?;

        let combined = format_git_output(&output);
        let content = if combined.is_empty() {
            "working tree clean".to_owned()
        } else {
            truncate_output(&combined)
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("exit_code".to_owned(), output.status.code().unwrap_or(-1).to_string()),
            ]),
        })
    }
}

// ---------------------------------------------------------------------------
// GitDiffTool
// ---------------------------------------------------------------------------

pub struct GitDiffTool;

impl ToolHandler for GitDiffTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = resolve_path(call);
        validate_path(call, path)?;

        let staged = opt_arg(call, "staged")
            .map(|v| v == "true")
            .unwrap_or(false);
        let name_only = opt_arg(call, "name_only")
            .map(|v| v == "true")
            .unwrap_or(false);
        let commit_range = opt_arg(call, "commit_range");
        let file_paths = opt_arg(call, "file_paths");

        let mut args: Vec<&str> = vec!["diff"];

        if staged {
            args.push("--staged");
        }
        if name_only {
            args.push("--name-only");
        }

        if let Some(range) = commit_range {
            args.push(range);
        }

        // Owned string needed for the separator; keep it alive across the call.
        let separator = "--".to_owned();
        let file_list: Vec<&str>;
        if let Some(paths) = file_paths {
            args.push(&separator);
            file_list = paths.split_whitespace().collect();
            args.extend(&file_list);
        }

        let output = run_git(call, path, &args)?;

        let combined = format_git_output(&output);
        let content = if combined.is_empty() {
            "no diff output".to_owned()
        } else {
            truncate_output(&combined)
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("exit_code".to_owned(), output.status.code().unwrap_or(-1).to_string()),
            ]),
        })
    }
}

// ---------------------------------------------------------------------------
// GitLogTool
// ---------------------------------------------------------------------------

pub struct GitLogTool;

impl ToolHandler for GitLogTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = resolve_path(call);
        validate_path(call, path)?;

        let max_count = opt_arg(call, "max_count")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(20);
        let author = opt_arg(call, "author");
        let since = opt_arg(call, "since");
        let until = opt_arg(call, "until");
        let file_path = opt_arg(call, "file_path");

        let max_count_str = max_count.to_string();
        let mut args: Vec<&str> = vec!["log", "--oneline", "--no-decorate", "-n", &max_count_str];

        let author_flag;
        if let Some(a) = author {
            author_flag = format!("--author={a}");
            args.push(&author_flag);
        }

        let since_flag;
        if let Some(s) = since {
            since_flag = format!("--since={s}");
            args.push(&since_flag);
        }

        let until_flag;
        if let Some(u) = until {
            until_flag = format!("--until={u}");
            args.push(&until_flag);
        }

        let separator = "--".to_owned();
        if let Some(fp) = file_path {
            args.push(&separator);
            args.push(fp);
        }

        let output = run_git(call, path, &args)?;

        let combined = format_git_output(&output);
        let content = if combined.is_empty() {
            "no commits found".to_owned()
        } else {
            truncate_output(&combined)
        };

        let commit_count = content
            .lines()
            .filter(|l| !l.is_empty() && !l.contains("(output truncated)"))
            .count();

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("commit_count".to_owned(), commit_count.to_string()),
                ("exit_code".to_owned(), output.status.code().unwrap_or(-1).to_string()),
            ]),
        })
    }
}

// ---------------------------------------------------------------------------
// GitCommitTool
// ---------------------------------------------------------------------------

pub struct GitCommitTool;

impl ToolHandler for GitCommitTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        check_destructive(call, _context)?;

        let path = resolve_path(call);
        validate_path(call, path)?;

        let message = require_arg(call, "message")?;

        let all = opt_arg(call, "all")
            .map(|v| v == "true")
            .unwrap_or(false);

        let mut args: Vec<&str> = vec!["commit"];
        if all {
            args.push("--all");
        }
        args.push("-m");
        args.push(message);

        let output = run_git(call, path, &args)?;

        let combined = format_git_output(&output);

        if !output.status.success() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: truncate_output(&combined),
            });
        }

        Ok(ToolOutput {
            content: truncate_output(&combined),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("exit_code".to_owned(), output.status.code().unwrap_or(-1).to_string()),
            ]),
        })
    }
}

// ---------------------------------------------------------------------------
// GitBranchTool
// ---------------------------------------------------------------------------

pub struct GitBranchTool;

impl ToolHandler for GitBranchTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = resolve_path(call);
        validate_path(call, path)?;

        let action = opt_arg(call, "action").unwrap_or("list");
        let branch_name = opt_arg(call, "name");

        match action {
            "list" => {
                let output = run_git(call, path, &["branch", "--list", "-a"])?;
                let combined = format_git_output(&output);
                let content = if combined.is_empty() {
                    "no branches found".to_owned()
                } else {
                    truncate_output(&combined)
                };
                Ok(ToolOutput {
                    content,
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("action".to_owned(), "list".to_owned()),
                        ("exit_code".to_owned(), output.status.code().unwrap_or(-1).to_string()),
                    ]),
                })
            }
            "create" => {
                let name = branch_name.ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "name",
                })?;
                let output = run_git(call, path, &["branch", name])?;
                let combined = format_git_output(&output);
                if !output.status.success() {
                    return Err(ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: truncate_output(&combined),
                    });
                }
                let content = if combined.is_empty() {
                    format!("branch '{name}' created")
                } else {
                    truncate_output(&combined)
                };
                Ok(ToolOutput {
                    content,
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("action".to_owned(), "create".to_owned()),
                        ("branch".to_owned(), name.to_owned()),
                        ("exit_code".to_owned(), output.status.code().unwrap_or(-1).to_string()),
                    ]),
                })
            }
            "switch" => {
                let name = branch_name.ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "name",
                })?;
                let output = run_git(call, path, &["checkout", name])?;
                let combined = format_git_output(&output);
                if !output.status.success() {
                    return Err(ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: truncate_output(&combined),
                    });
                }
                let content = if combined.is_empty() {
                    format!("switched to branch '{name}'")
                } else {
                    truncate_output(&combined)
                };
                Ok(ToolOutput {
                    content,
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("action".to_owned(), "switch".to_owned()),
                        ("branch".to_owned(), name.to_owned()),
                        ("exit_code".to_owned(), output.status.code().unwrap_or(-1).to_string()),
                    ]),
                })
            }
            "delete" => {
                check_destructive(call, _context)?;

                let name = branch_name.ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "name",
                })?;
                let output = run_git(call, path, &["branch", "-d", name])?;
                let combined = format_git_output(&output);
                if !output.status.success() {
                    return Err(ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: truncate_output(&combined),
                    });
                }
                let content = if combined.is_empty() {
                    format!("branch '{name}' deleted")
                } else {
                    truncate_output(&combined)
                };
                Ok(ToolOutput {
                    content,
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("action".to_owned(), "delete".to_owned()),
                        ("branch".to_owned(), name.to_owned()),
                        ("exit_code".to_owned(), output.status.code().unwrap_or(-1).to_string()),
                    ]),
                })
            }
            other => Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "unknown action '{other}'; expected one of: list, create, switch, delete"
                ),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
            terminal_backend: None,
        }
    }

    fn ctx_no_destructive() -> ToolContext {
        ToolContext {
            allow_destructive_tools: false,
            ..ctx()
        }
    }

    /// Initialise a temporary git repo with one commit so that tools have
    /// something to operate on.
    fn init_repo() -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        let p = dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(p)
            .output()
            .expect("git init");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(p)
            .output()
            .expect("git config email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(p)
            .output()
            .expect("git config name");

        fs::write(p.join("README.md"), "# Hello\n").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(p)
            .output()
            .expect("git add");

        Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(p)
            .output()
            .expect("git commit");

        dir
    }

    fn make_call(name: &str, args: Vec<(&str, &str)>) -> ToolCall {
        ToolCall {
            name: name.to_owned(),
            arguments: args
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    // -----------------------------------------------------------------------
    // GitStatusTool
    // -----------------------------------------------------------------------

    #[test]
    fn status_clean_repo() {
        let dir = init_repo();
        let tool = GitStatusTool;
        let call = make_call(
            "git_status",
            vec![("path", &dir.path().display().to_string())],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        // Branch header is always present, so content should not be the empty fallback.
        assert!(
            output.content.contains("branch") || output.content.contains("working tree clean"),
        );
    }

    #[test]
    fn status_dirty_repo() {
        let dir = init_repo();
        fs::write(dir.path().join("new_file.txt"), "untracked\n").unwrap();

        let tool = GitStatusTool;
        let call = make_call(
            "git_status",
            vec![("path", &dir.path().display().to_string())],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("new_file.txt"));
    }

    #[test]
    fn status_bad_path() {
        let tool = GitStatusTool;
        let call = make_call("git_status", vec![("path", "/nonexistent/path/xyz")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn status_defaults_to_cwd() {
        let tool = GitStatusTool;
        let call = make_call("git_status", vec![]);
        // Should not panic; may succeed or fail depending on cwd.
        let _ = tool.run(&call, &ctx());
    }

    // -----------------------------------------------------------------------
    // GitDiffTool
    // -----------------------------------------------------------------------

    #[test]
    fn diff_no_changes() {
        let dir = init_repo();
        let tool = GitDiffTool;
        let call = make_call(
            "git_diff",
            vec![("path", &dir.path().display().to_string())],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("no diff output"));
    }

    #[test]
    fn diff_unstaged_changes() {
        let dir = init_repo();
        fs::write(dir.path().join("README.md"), "# Changed\n").unwrap();

        let tool = GitDiffTool;
        let call = make_call(
            "git_diff",
            vec![("path", &dir.path().display().to_string())],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("Changed"));
    }

    #[test]
    fn diff_staged_changes() {
        let dir = init_repo();
        fs::write(dir.path().join("README.md"), "# Staged change\n").unwrap();

        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("git add");

        let tool = GitDiffTool;
        let call = make_call(
            "git_diff",
            vec![
                ("path", &dir.path().display().to_string()),
                ("staged", "true"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("Staged change"));
    }

    #[test]
    fn diff_name_only() {
        let dir = init_repo();
        fs::write(dir.path().join("README.md"), "# Name only\n").unwrap();

        let tool = GitDiffTool;
        let call = make_call(
            "git_diff",
            vec![
                ("path", &dir.path().display().to_string()),
                ("name_only", "true"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("README.md"));
        // Should NOT contain diff hunks when name-only is set.
        assert!(!output.content.contains("@@"));
    }

    #[test]
    fn diff_specific_file() {
        let dir = init_repo();
        fs::write(dir.path().join("README.md"), "# File filter\n").unwrap();
        fs::write(dir.path().join("other.txt"), "other change\n").unwrap();
        Command::new("git")
            .args(["add", "other.txt"])
            .current_dir(dir.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "add other"])
            .current_dir(dir.path())
            .output()
            .expect("git commit");
        fs::write(dir.path().join("other.txt"), "modified other\n").unwrap();

        let tool = GitDiffTool;
        let call = make_call(
            "git_diff",
            vec![
                ("path", &dir.path().display().to_string()),
                ("file_paths", "README.md"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("README.md") || output.content.contains("File filter"));
        assert!(!output.content.contains("modified other"));
    }

    #[test]
    fn diff_bad_path() {
        let tool = GitDiffTool;
        let call = make_call("git_diff", vec![("path", "/nonexistent/abc")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    // -----------------------------------------------------------------------
    // GitLogTool
    // -----------------------------------------------------------------------

    #[test]
    fn log_shows_commits() {
        let dir = init_repo();
        let tool = GitLogTool;
        let call = make_call(
            "git_log",
            vec![("path", &dir.path().display().to_string())],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("initial commit"));
        assert_eq!(output.metadata.get("commit_count").unwrap(), "1");
    }

    #[test]
    fn log_max_count() {
        let dir = init_repo();

        // Add a second commit.
        fs::write(dir.path().join("second.txt"), "second\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "second commit"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tool = GitLogTool;
        let call = make_call(
            "git_log",
            vec![
                ("path", &dir.path().display().to_string()),
                ("max_count", "1"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("second commit"));
        assert!(!output.content.contains("initial commit"));
    }

    #[test]
    fn log_author_filter() {
        let dir = init_repo();
        let tool = GitLogTool;
        let call = make_call(
            "git_log",
            vec![
                ("path", &dir.path().display().to_string()),
                ("author", "Test"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("initial commit"));
    }

    #[test]
    fn log_no_matching_author() {
        let dir = init_repo();
        let tool = GitLogTool;
        let call = make_call(
            "git_log",
            vec![
                ("path", &dir.path().display().to_string()),
                ("author", "Nobody_that_exists_xyz"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("no commits found"));
    }

    #[test]
    fn log_file_path_filter() {
        let dir = init_repo();
        fs::write(dir.path().join("extra.txt"), "extra\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add extra"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tool = GitLogTool;
        let call = make_call(
            "git_log",
            vec![
                ("path", &dir.path().display().to_string()),
                ("file_path", "extra.txt"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("add extra"));
        assert!(!output.content.contains("initial commit"));
    }

    #[test]
    fn log_bad_path() {
        let tool = GitLogTool;
        let call = make_call("git_log", vec![("path", "/nonexistent/abc")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    // -----------------------------------------------------------------------
    // GitCommitTool
    // -----------------------------------------------------------------------

    #[test]
    fn commit_staged_changes() {
        let dir = init_repo();
        fs::write(dir.path().join("new.txt"), "new content\n").unwrap();
        Command::new("git")
            .args(["add", "new.txt"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tool = GitCommitTool;
        let call = make_call(
            "git_commit",
            vec![
                ("path", &dir.path().display().to_string()),
                ("message", "add new file"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("add new file") || output.content.contains("new.txt"));
    }

    #[test]
    fn commit_with_all_flag() {
        let dir = init_repo();
        // Modify a tracked file without staging.
        fs::write(dir.path().join("README.md"), "# Modified\n").unwrap();

        let tool = GitCommitTool;
        let call = make_call(
            "git_commit",
            vec![
                ("path", &dir.path().display().to_string()),
                ("message", "auto-stage commit"),
                ("all", "true"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(
            output.content.contains("auto-stage commit")
                || output.content.contains("README.md")
        );
    }

    #[test]
    fn commit_requires_message() {
        let tool = GitCommitTool;
        let call = make_call("git_commit", vec![]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn commit_nothing_to_commit() {
        let dir = init_repo();
        let tool = GitCommitTool;
        let call = make_call(
            "git_commit",
            vec![
                ("path", &dir.path().display().to_string()),
                ("message", "empty"),
            ],
        );
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn commit_blocked_when_destructive_disabled() {
        let dir = init_repo();
        fs::write(dir.path().join("blocked.txt"), "blocked\n").unwrap();
        Command::new("git")
            .args(["add", "blocked.txt"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tool = GitCommitTool;
        let call = make_call(
            "git_commit",
            vec![
                ("path", &dir.path().display().to_string()),
                ("message", "should fail"),
            ],
        );
        let err = tool.run(&call, &ctx_no_destructive()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    // -----------------------------------------------------------------------
    // GitBranchTool
    // -----------------------------------------------------------------------

    #[test]
    fn branch_list() {
        let dir = init_repo();
        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![
                ("path", &dir.path().display().to_string()),
                ("action", "list"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        // The default branch should appear.
        assert!(
            output.content.contains("main") || output.content.contains("master"),
            "expected branch name in output: {}",
            output.content,
        );
    }

    #[test]
    fn branch_list_is_default_action() {
        let dir = init_repo();
        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![("path", &dir.path().display().to_string())],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.metadata.get("action").unwrap(), "list");
    }

    #[test]
    fn branch_create() {
        let dir = init_repo();
        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![
                ("path", &dir.path().display().to_string()),
                ("action", "create"),
                ("name", "feature-branch"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("feature-branch"));
        assert_eq!(output.metadata.get("action").unwrap(), "create");
    }

    #[test]
    fn branch_create_requires_name() {
        let dir = init_repo();
        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![
                ("path", &dir.path().display().to_string()),
                ("action", "create"),
            ],
        );
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn branch_switch() {
        let dir = init_repo();

        // Create a branch first.
        Command::new("git")
            .args(["branch", "other-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![
                ("path", &dir.path().display().to_string()),
                ("action", "switch"),
                ("name", "other-branch"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(
            output.content.contains("other-branch")
                || output.content.contains("Switched"),
        );
        assert_eq!(output.metadata.get("action").unwrap(), "switch");
    }

    #[test]
    fn branch_switch_nonexistent() {
        let dir = init_repo();
        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![
                ("path", &dir.path().display().to_string()),
                ("action", "switch"),
                ("name", "does-not-exist"),
            ],
        );
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn branch_delete() {
        let dir = init_repo();

        // Create and then delete a branch.
        Command::new("git")
            .args(["branch", "to-delete"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![
                ("path", &dir.path().display().to_string()),
                ("action", "delete"),
                ("name", "to-delete"),
            ],
        );
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(
            output.content.contains("to-delete")
                || output.content.contains("Deleted"),
        );
        assert_eq!(output.metadata.get("action").unwrap(), "delete");
    }

    #[test]
    fn branch_delete_blocked_when_destructive_disabled() {
        let dir = init_repo();

        Command::new("git")
            .args(["branch", "protected-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![
                ("path", &dir.path().display().to_string()),
                ("action", "delete"),
                ("name", "protected-branch"),
            ],
        );
        let err = tool.run(&call, &ctx_no_destructive()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn branch_unknown_action() {
        let dir = init_repo();
        let tool = GitBranchTool;
        let call = make_call(
            "git_branch",
            vec![
                ("path", &dir.path().display().to_string()),
                ("action", "frobnicate"),
            ],
        );
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn branch_bad_path() {
        let tool = GitBranchTool;
        let call = make_call("git_branch", vec![("path", "/nonexistent/abc")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    // -----------------------------------------------------------------------
    // truncate_output
    // -----------------------------------------------------------------------

    #[test]
    fn truncate_preserves_short_text() {
        let short = "hello world\n";
        assert_eq!(truncate_output(short), short);
    }

    #[test]
    fn truncate_cuts_long_text() {
        let long = "a\n".repeat(100_000);
        let result = truncate_output(&long);
        assert!(result.len() <= MAX_OUTPUT_BYTES + 50);
        assert!(result.ends_with("... (output truncated)"));
    }
}
