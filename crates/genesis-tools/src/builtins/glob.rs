use std::collections::BTreeMap;
use std::path::Path;

use glob::glob as glob_match;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput, NOISE_DIRS};

const DEFAULT_LIMIT: usize = 100;

pub struct GlobSearchTool;

impl ToolHandler for GlobSearchTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = call
            .arguments
            .get("pattern")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "pattern",
            })?;

        let base_path = call
            .arguments
            .get("path")
            .map(|p| p.as_str())
            .unwrap_or(".");

        let type_filter = call
            .arguments
            .get("type")
            .map(|t| t.as_str())
            .unwrap_or("any");

        let limit = call
            .arguments
            .get("limit")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_LIMIT);

        // Validate type filter.
        if !matches!(type_filter, "file" | "dir" | "any") {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "invalid type filter `{type_filter}`: expected \"file\", \"dir\", or \"any\""
                ),
            });
        }

        // Validate base path exists.
        let base = Path::new(base_path);
        if !base.exists() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("path does not exist: {base_path}"),
            });
        }

        // Build the full glob pattern. If the pattern already starts with the
        // base path (or is absolute), use it as-is. Otherwise prepend the base.
        let full_pattern = if pattern.starts_with(base_path)
            || Path::new(pattern).is_absolute()
            || base_path == "."
        {
            if base_path == "." && !pattern.starts_with("./") && !Path::new(pattern).is_absolute() {
                format!("./{pattern}")
            } else {
                pattern.to_string()
            }
        } else {
            let base_str = base_path.trim_end_matches('/');
            format!("{base_str}/{pattern}")
        };

        let paths = glob_match(&full_pattern).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("invalid glob pattern `{full_pattern}`: {e}"),
        })?;

        let mut results: Vec<String> = Vec::new();

        for entry in paths {
            let path = match entry {
                Ok(p) => p,
                Err(_) => continue, // skip permission errors, etc.
            };

            // Skip hidden files and noise directories (only check the
            // portion of the path relative to the base directory so that
            // absolute path prefixes like `/var/.tmp…` don't trigger
            // false positives).
            if should_skip(&path, base) {
                continue;
            }

            // Apply type filter.
            match type_filter {
                "file" if !path.is_file() => continue,
                "dir" if !path.is_dir() => continue,
                _ => {}
            }

            results.push(path.display().to_string());

            if results.len() >= limit {
                break;
            }
        }

        results.sort();

        // Enforce limit again after sorting (sorting may have re-ordered
        // items that were collected up to the limit during iteration, but this
        // is mostly a safeguard).
        results.truncate(limit);

        let match_count = results.len();
        let content = if results.is_empty() {
            format!("no files matched pattern `{pattern}` in {base_path}")
        } else {
            results.join("\n")
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("match_count".to_owned(), match_count.to_string()),
                ("pattern".to_owned(), full_pattern),
                ("base_path".to_owned(), base_path.to_owned()),
            ]),
        })
    }
}

/// Returns `true` if the *relative* portion of `path` (after stripping the
/// `base` prefix) contains a hidden component (starts with `.`) or a
/// well-known noise directory.  This avoids false positives when the base
/// directory itself lives under a dot-prefixed path (e.g. `/var/.tmp…`).
fn should_skip(path: &Path, base: &Path) -> bool {
    let relative = path.strip_prefix(base).unwrap_or(path);
    for component in relative.components() {
        if let std::path::Component::Normal(seg) = component {
            let s = seg.to_string_lossy();
            if s.starts_with('.') {
                return true;
            }
            if NOISE_DIRS.contains(&s.as_ref()) {
                return true;
            }
        }
    }
    false
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
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
        }
    }

    fn make_call(args: Vec<(&str, &str)>) -> ToolCall {
        ToolCall {
            name: "glob_search".to_owned(),
            arguments: args
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn basic_glob_matching() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("foo.txt"), "").unwrap();
        fs::write(dir.path().join("bar.txt"), "").unwrap();
        fs::write(dir.path().join("baz.rs"), "").unwrap();

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "*.txt"),
            ("path", &dir.path().display().to_string()),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("foo.txt"));
        assert!(output.content.contains("bar.txt"));
        assert!(!output.content.contains("baz.rs"));
        assert_eq!(output.metadata.get("match_count").unwrap(), "2");
    }

    #[test]
    fn recursive_glob() {
        let dir = tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("root.txt"), "").unwrap();
        fs::write(sub.join("nested.txt"), "").unwrap();
        fs::write(sub.join("other.rs"), "").unwrap();

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "**/*.txt"),
            ("path", &dir.path().display().to_string()),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("root.txt"));
        assert!(output.content.contains("nested.txt"));
        assert!(!output.content.contains("other.rs"));
    }

    #[test]
    fn type_filter_file_only() {
        let dir = tempdir().expect("tempdir");
        let sub = dir.path().join("subdir");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("file.txt"), "").unwrap();

        let tool = GlobSearchTool;

        // Filter to files only - use a pattern that could match both files and dirs.
        let call = make_call(vec![
            ("pattern", "**/*"),
            ("path", &dir.path().display().to_string()),
            ("type", "file"),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("file.txt"));
        // The directory "subdir" should not appear as a result.
        let lines: Vec<&str> = output.content.lines().collect();
        for line in &lines {
            assert!(
                !line.ends_with("subdir"),
                "directory should be excluded with type=file"
            );
        }
    }

    #[test]
    fn type_filter_dir_only() {
        let dir = tempdir().expect("tempdir");
        let sub = dir.path().join("mydir");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("file.txt"), "").unwrap();

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "**/*"),
            ("path", &dir.path().display().to_string()),
            ("type", "dir"),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("mydir"));
        assert!(!output.content.contains("file.txt"));
    }

    #[test]
    fn limit_enforcement() {
        let dir = tempdir().expect("tempdir");
        for i in 0..20 {
            fs::write(dir.path().join(format!("file_{i:02}.txt")), "").unwrap();
        }

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "*.txt"),
            ("path", &dir.path().display().to_string()),
            ("limit", "5"),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        let count: usize = output.metadata.get("match_count").unwrap().parse().unwrap();
        assert!(count <= 5, "should respect limit of 5, got {count}");
        assert_eq!(output.content.lines().count(), 5);
    }

    #[test]
    fn no_matches() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("file.rs"), "").unwrap();

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "*.py"),
            ("path", &dir.path().display().to_string()),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("no files matched"));
        assert_eq!(output.metadata.get("match_count").unwrap(), "0");
    }

    #[test]
    fn invalid_pattern_error() {
        let dir = tempdir().expect("tempdir");

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "[invalid"),
            ("path", &dir.path().display().to_string()),
        ]);

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { .. }),
            "expected ExecutionFailed, got {err:?}"
        );
    }

    #[test]
    fn missing_pattern_error() {
        let tool = GlobSearchTool;
        let call = make_call(vec![]);

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn nonexistent_path_error() {
        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "*.txt"),
            ("path", "/nonexistent/path/abc123"),
        ]);

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn skips_hidden_files() {
        let dir = tempdir().expect("tempdir");
        let hidden = dir.path().join(".hidden");
        fs::create_dir_all(&hidden).unwrap();
        fs::write(hidden.join("secret.txt"), "").unwrap();
        fs::write(dir.path().join("visible.txt"), "").unwrap();

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "**/*.txt"),
            ("path", &dir.path().display().to_string()),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("visible.txt"));
        assert!(!output.content.contains("secret.txt"));
    }

    #[test]
    fn skips_noise_directories() {
        let dir = tempdir().expect("tempdir");
        let nm = dir.path().join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("dep.js"), "").unwrap();
        fs::write(dir.path().join("app.js"), "").unwrap();

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "**/*.js"),
            ("path", &dir.path().display().to_string()),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("app.js"));
        assert!(!output.content.contains("dep.js"));
    }

    #[test]
    fn results_sorted_alphabetically() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("charlie.txt"), "").unwrap();
        fs::write(dir.path().join("alpha.txt"), "").unwrap();
        fs::write(dir.path().join("bravo.txt"), "").unwrap();

        let tool = GlobSearchTool;
        let call = make_call(vec![
            ("pattern", "*.txt"),
            ("path", &dir.path().display().to_string()),
        ]);

        let output = tool.run(&call, &ctx()).expect("should succeed");
        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("alpha.txt"));
        assert!(lines[1].contains("bravo.txt"));
        assert!(lines[2].contains("charlie.txt"));
    }
}
