use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput, NOISE_DIRS};

/// Maximum number of entries before the output is truncated.
const MAX_ENTRIES: usize = 2000;

pub struct ListTreeTool;

impl ToolHandler for ListTreeTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_arg = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let validated_path =
            crate::sandbox::validate_tool_path(path_arg, &call.name, &context.path_validator)?;

        let path = &validated_path.to_string_lossy().into_owned();

        let max_depth: usize = call
            .arguments
            .get("max_depth")
            .map(|v| v.parse::<usize>())
            .transpose()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("invalid max_depth: {e}"),
            })?
            .unwrap_or(3);

        let show_hidden = call
            .arguments
            .get("show_hidden")
            .map(|v| v == "true")
            .unwrap_or(false);

        let pattern = call.arguments.get("pattern").cloned();

        let root = Path::new(path);
        if !root.is_dir() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("`{path}` is not a directory or does not exist"),
            });
        }

        let mut lines: Vec<String> = Vec::new();
        let mut dir_count: usize = 0;
        let mut file_count: usize = 0;
        let mut truncated = false;

        // Push the root directory name.
        lines.push(path.to_string());

        walk_dir(
            root,
            "",
            max_depth,
            0,
            show_hidden,
            pattern.as_deref(),
            &mut lines,
            &mut dir_count,
            &mut file_count,
            &mut truncated,
        );

        let mut content = lines.join("\n");

        if truncated {
            content.push_str("\n... (output truncated)");
        }

        content.push_str(&format!("\n\n{dir_count} directories, {file_count} files"));

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("path".to_owned(), path.clone()),
                ("directories".to_owned(), dir_count.to_string()),
                ("files".to_owned(), file_count.to_string()),
            ]),
        })
    }
}

/// Returns true if `name` matches the given suffix pattern.
///
/// Supports simple glob-like patterns of the form `*.ext`. If the pattern
/// starts with `*.`, we match the suffix after the `*`. Otherwise we fall back
/// to a plain suffix match.
fn matches_pattern(name: &str, pattern: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        name.ends_with(suffix)
    } else {
        name.ends_with(pattern)
    }
}

/// Recursively walks `dir` and appends tree-formatted lines.
#[allow(clippy::too_many_arguments)]
fn walk_dir(
    dir: &Path,
    prefix: &str,
    max_depth: usize,
    current_depth: usize,
    show_hidden: bool,
    pattern: Option<&str>,
    lines: &mut Vec<String>,
    dir_count: &mut usize,
    file_count: &mut usize,
    truncated: &mut bool,
) {
    if current_depth >= max_depth {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Collect and sort: directories first, then files, alphabetically within
    // each group.
    let mut dirs: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();

        // Skip hidden entries unless requested.
        if !show_hidden && name.starts_with('.') {
            continue;
        }

        let path = entry.path();
        if path.is_dir() {
            // Always skip noise directories.
            if NOISE_DIRS.contains(&name.as_str()) {
                continue;
            }
            dirs.push((name, path));
        } else {
            files.push((name, path));
        }
    }

    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // If a pattern filter is active, only include matching files.
    // Directories are kept for now -- we recurse eagerly and prune empty ones.
    if let Some(pat) = pattern {
        files.retain(|(name, _)| matches_pattern(name, pat));
    }

    let total = dirs.len() + files.len();
    let mut index = 0;

    for (name, path) in dirs.iter().chain(files.iter()) {
        if *truncated {
            return;
        }

        index += 1;
        let is_last_entry = index == total;
        let connector = if is_last_entry {
            "\u{2514}\u{2500}\u{2500}"
        } else {
            "\u{251c}\u{2500}\u{2500}"
        };

        let is_dir = path.is_dir();
        let display_name = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };

        if is_dir {
            // When pattern-filtering, recurse eagerly and only emit the
            // directory line if the subtree produced output. This avoids
            // a separate `subtree_has_match` pre-scan (double traversal).
            let child_prefix = if is_last_entry {
                format!("{prefix}    ")
            } else {
                format!("{prefix}\u{2502}   ")
            };

            if pattern.is_some() {
                let before = lines.len();
                let dir_before = *dir_count;
                let file_before = *file_count;
                walk_dir(
                    path,
                    &child_prefix,
                    max_depth,
                    current_depth + 1,
                    show_hidden,
                    pattern,
                    lines,
                    dir_count,
                    file_count,
                    truncated,
                );
                // Only emit the directory line if the subtree had content.
                if lines.len() > before {
                    lines.insert(before, format!("{prefix}{connector} {display_name}"));
                    *dir_count = dir_before + 1 + (*dir_count - dir_before);
                } else {
                    // Subtree was empty -- skip this directory entirely.
                    *dir_count = dir_before;
                    *file_count = file_before;
                }
            } else {
                lines.push(format!("{prefix}{connector} {display_name}"));
                *dir_count += 1;
                if lines.len() >= MAX_ENTRIES {
                    *truncated = true;
                    return;
                }
                walk_dir(
                    path,
                    &child_prefix,
                    max_depth,
                    current_depth + 1,
                    show_hidden,
                    pattern,
                    lines,
                    dir_count,
                    file_count,
                    truncated,
                );
            }
        } else {
            lines.push(format!("{prefix}{connector} {display_name}"));
            *file_count += 1;
        }

        if lines.len() >= MAX_ENTRIES {
            *truncated = true;
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use std::fs;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx()
    }

    fn make_call(args: Vec<(&str, &str)>) -> ToolCall {
        ToolCall {
            name: "list_tree".to_owned(),
            arguments: args
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn basic_tree_output() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src").join("main.rs"), "").unwrap();
        fs::write(root.join("src").join("lib.rs"), "").unwrap();
        fs::write(root.join("Cargo.toml"), "").unwrap();

        let tool = ListTreeTool;
        let call = make_call(vec![("path", &root.to_string_lossy())]);
        let output = tool.run(&call, &ctx()).expect("should succeed");

        // Should contain directory and file names.
        assert!(output.content.contains("src/"));
        assert!(output.content.contains("main.rs"));
        assert!(output.content.contains("lib.rs"));
        assert!(output.content.contains("Cargo.toml"));

        // Directories should appear before files.
        let src_pos = output.content.find("src/").unwrap();
        let cargo_pos = output.content.find("Cargo.toml").unwrap();
        assert!(
            src_pos < cargo_pos,
            "directories should be listed before files"
        );

        // Summary line.
        assert!(output.content.contains("1 directories, 3 files"));
    }

    #[test]
    fn max_depth_limits_recursion() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create a 3-level deep structure: a/ -> b/ -> c/ -> deep.txt
        // Also place a file at level 1: a/top.txt
        fs::create_dir_all(root.join("a").join("b").join("c")).unwrap();
        fs::write(root.join("a").join("b").join("c").join("deep.txt"), "").unwrap();
        fs::write(root.join("a").join("top.txt"), "").unwrap();

        let tool = ListTreeTool;

        // max_depth=2: depth 0 lists root children (a/), depth 1 lists a/'s
        // children (b/, top.txt). b/'s contents are at depth 2 so NOT shown.
        let call = make_call(vec![("path", &root.to_string_lossy()), ("max_depth", "2")]);
        let output = tool.run(&call, &ctx()).expect("should succeed");

        assert!(output.content.contains("a/"));
        assert!(output.content.contains("b/"));
        assert!(output.content.contains("top.txt"));
        assert!(
            !output.content.contains("c/"),
            "depth 2+ directory should not appear"
        );
        assert!(
            !output.content.contains("deep.txt"),
            "depth 3 file should not appear"
        );

        // max_depth=1: only root-level children visible.
        let call = make_call(vec![("path", &root.to_string_lossy()), ("max_depth", "1")]);
        let output = tool.run(&call, &ctx()).expect("should succeed");

        assert!(output.content.contains("a/"));
        assert!(!output.content.contains("b/"), "depth 1+ should not appear");
    }

    #[test]
    fn show_hidden_toggle() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join(".hidden"), "").unwrap();
        fs::write(root.join("visible"), "").unwrap();

        let tool = ListTreeTool;

        // Hidden files should be excluded by default.
        let call = make_call(vec![("path", &root.to_string_lossy())]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(!output.content.contains(".hidden"));
        assert!(output.content.contains("visible"));

        // With show_hidden=true, hidden files should appear.
        let call = make_call(vec![
            ("path", &root.to_string_lossy()),
            ("show_hidden", "true"),
        ]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains(".hidden"));
        assert!(output.content.contains("visible"));
    }

    #[test]
    fn pattern_filtering() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src").join("main.rs"), "").unwrap();
        fs::write(root.join("src").join("utils.py"), "").unwrap();
        fs::write(root.join("readme.md"), "").unwrap();

        let tool = ListTreeTool;
        let call = make_call(vec![("path", &root.to_string_lossy()), ("pattern", "*.rs")]);
        let output = tool.run(&call, &ctx()).expect("should succeed");

        assert!(output.content.contains("main.rs"));
        // The parent directory should still be shown because it has a matching file.
        assert!(output.content.contains("src/"));
        // Non-matching files should be filtered out.
        assert!(!output.content.contains("utils.py"));
        assert!(!output.content.contains("readme.md"));
    }

    #[test]
    fn empty_directory() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let tool = ListTreeTool;
        let call = make_call(vec![("path", &root.to_string_lossy())]);
        let output = tool.run(&call, &ctx()).expect("should succeed");

        assert!(output.content.contains("0 directories, 0 files"));
    }

    #[test]
    fn error_on_nonexistent_path() {
        let tool = ListTreeTool;
        let call = make_call(vec![("path", "/nonexistent/path/that/does/not/exist")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn noise_directories_are_skipped() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::create_dir(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules").join("pkg.js"), "").unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src").join("app.rs"), "").unwrap();

        let tool = ListTreeTool;
        let call = make_call(vec![("path", &root.to_string_lossy())]);
        let output = tool.run(&call, &ctx()).expect("should succeed");

        assert!(!output.content.contains("node_modules"));
        assert!(output.content.contains("src/"));
        assert!(output.content.contains("app.rs"));
    }

    #[test]
    fn tree_connectors_are_correct() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("aaa"), "").unwrap();
        fs::write(root.join("zzz"), "").unwrap();

        let tool = ListTreeTool;
        let call = make_call(vec![("path", &root.to_string_lossy())]);
        let output = tool.run(&call, &ctx()).expect("should succeed");

        // First entry should use the branch connector.
        assert!(output.content.contains("\u{251c}\u{2500}\u{2500} aaa"));
        // Last entry should use the end connector.
        assert!(output.content.contains("\u{2514}\u{2500}\u{2500} zzz"));
    }
}
