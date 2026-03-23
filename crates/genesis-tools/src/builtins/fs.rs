use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_READ_BYTES: usize = 128 * 1024;

/// Path components that indicate sensitive directories or files.
/// Access to these is blocked regardless of working directory settings.
const SENSITIVE_PATHS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws/credentials",
    ".aws/config",
    ".config/gcloud",
];

/// Absolute sensitive paths that are blocked unconditionally.
const SENSITIVE_ABSOLUTE: &[&str] = &["/etc/shadow", "/etc/passwd", "/etc/sudoers"];

/// Check whether the given path touches a known sensitive location.
fn is_sensitive_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();

    // Block known absolute sensitive files.
    for &sensitive in SENSITIVE_ABSOLUTE {
        if path_str == sensitive || path_str.starts_with(&format!("{sensitive}/")) {
            return true;
        }
    }

    // Block paths that contain sensitive directory/file components.
    for component in path.components() {
        let comp = component.as_os_str().to_string_lossy();
        for &sensitive in SENSITIVE_PATHS {
            if sensitive.contains('/') {
                // Multi-component sensitive path (e.g. ".aws/credentials"):
                // check if the full path string contains it.
                if path_str.contains(sensitive) {
                    return true;
                }
            } else if comp == sensitive {
                return true;
            }
        }
    }

    false
}

/// Canonicalize and validate a user-supplied path.
///
/// 1. Resolves the path to an absolute canonical form (following symlinks).
///    For paths that don't exist yet, the parent is canonicalized and the
///    file name is appended.
/// 2. If `context.default_working_dir` is set, verifies the resolved path
///    falls within that directory tree.
/// 3. Blocks access to known sensitive paths (`.ssh/`, `.gnupg/`, etc.).
fn validate_path(
    raw: &str,
    tool_name: &str,
    context: &ToolContext,
) -> Result<PathBuf, ToolError> {
    let path = Path::new(raw);

    // Check the raw (user-supplied) path first so we catch traversal patterns
    // like "/etc/shadow" even on systems where /etc symlinks elsewhere.
    if is_sensitive_path(path) {
        return Err(ToolError::InvalidInput {
            tool: tool_name.to_owned(),
            reason: format!("access to `{raw}` is blocked (sensitive path)"),
        });
    }

    // Canonicalize: resolve symlinks and relative components.
    let canonical = if path.exists() {
        path.canonicalize().map_err(|e| ToolError::InvalidInput {
            tool: tool_name.to_owned(),
            reason: format!("cannot resolve path `{raw}`: {e}"),
        })?
    } else {
        // For paths that don't exist yet, canonicalize the parent and append
        // the file name so we can still validate the location.
        let parent = path.parent().unwrap_or(Path::new("."));
        let parent_canonical =
            parent.canonicalize().map_err(|e| ToolError::InvalidInput {
                tool: tool_name.to_owned(),
                reason: format!("parent directory does not exist for `{raw}`: {e}"),
            })?;
        parent_canonical.join(path.file_name().unwrap_or_default())
    };

    // Also check the canonical path against the sensitive list in case a
    // symlink resolved into a sensitive location.
    if is_sensitive_path(&canonical) {
        return Err(ToolError::InvalidInput {
            tool: tool_name.to_owned(),
            reason: format!(
                "access to `{}` is blocked (sensitive path)",
                canonical.display()
            ),
        });
    }

    // Working-directory containment check.
    if let Some(ref root) = context.default_working_dir {
        let root_path = Path::new(root);
        let root_canonical = root_path
            .canonicalize()
            .unwrap_or_else(|_| root_path.to_path_buf());
        if !canonical.starts_with(&root_canonical) {
            return Err(ToolError::InvalidInput {
                tool: tool_name.to_owned(),
                reason: format!(
                    "path `{}` is outside working directory `{}`",
                    canonical.display(),
                    root_canonical.display()
                ),
            });
        }
    }

    Ok(canonical)
}

pub struct ReadFileTool;

impl ToolHandler for ReadFileTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_arg = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let path = validate_path(path_arg, &call.name, context)?;

        let content =
            fs::read_to_string(&path).map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to read `{}`: {e}", path.display()),
            })?;

        let content = crate::truncate_at(&content, MAX_READ_BYTES, "\n... (file truncated)");

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("path".to_owned(), path.display().to_string()),
            ]),
        })
    }
}

pub struct WriteFileTool;

impl ToolHandler for WriteFileTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_arg = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let content = call
            .arguments
            .get("content")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "content",
            })?;

        // Create parent directories if needed (before validation so
        // canonicalization of the parent succeeds for new nested paths).
        let raw_path = Path::new(path_arg);
        if let Some(parent) = raw_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to create directories for `{path_arg}`: {e}"),
                })?;
            }
        }

        let path = validate_path(path_arg, &call.name, context)?;

        fs::write(&path, content).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to write `{}`: {e}", path.display()),
        })?;

        let display_path = path.display().to_string();
        Ok(ToolOutput {
            content: format!("wrote {} bytes to {display_path}", content.len()),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("path".to_owned(), display_path),
                ("bytes_written".to_owned(), content.len().to_string()),
            ]),
        })
    }
}

pub struct ListDirTool;

impl ToolHandler for ListDirTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_arg = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let path = validate_path(path_arg, &call.name, context)?;

        let entries = fs::read_dir(&path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to list `{}`: {e}", path.display()),
        })?;

        let mut lines = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to read directory entry: {e}"),
            })?;

            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type().map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to read file type for `{name}`: {e}"),
            })?;

            let suffix = if file_type.is_dir() { "/" } else { "" };
            lines.push(format!("{name}{suffix}"));
        }

        lines.sort();

        let content = if lines.is_empty() {
            "(empty directory)".to_owned()
        } else {
            lines.join("\n")
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("path".to_owned(), path.display().to_string()),
                ("entry_count".to_owned(), lines.len().to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use std::fs;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx_destructive()
    }

    fn ctx_with_working_dir(dir: &Path) -> ToolContext {
        ToolContext {
            default_working_dir: Some(dir.to_string_lossy().into_owned()),
            ..crate::test_utils::test_ctx_destructive()
        }
    }

    // ---- ReadFileTool ----

    #[test]
    fn read_file_returns_contents() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        fs::write(&file_path, "hello world").unwrap();

        let tool = ReadFileTool;
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                file_path.to_string_lossy().into_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.content, "hello world");
    }

    #[test]
    fn read_file_errors_on_missing_file() {
        let tool = ReadFileTool;
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::from([("path".to_owned(), "/nonexistent/file.txt".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidInput { .. } | ToolError::ExecutionFailed { .. })
        );
    }

    // ---- WriteFileTool ----

    #[test]
    fn write_file_creates_and_writes() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("output.txt");

        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file_path.to_string_lossy().into_owned()),
                ("content".to_owned(), "written content".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("15 bytes"));
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "written content");
    }

    #[test]
    fn write_file_creates_parent_directories() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("nested").join("deep").join("file.txt");

        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file_path.to_string_lossy().into_owned()),
                ("content".to_owned(), "nested content".to_owned()),
            ]),
        };

        tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "nested content");
    }

    // ---- ListDirTool ----

    #[test]
    fn list_dir_lists_entries() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();

        let tool = ListDirTool;
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                dir.path().to_string_lossy().into_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("a.txt"));
        assert!(output.content.contains("b.txt"));
        assert!(output.content.contains("subdir/"));
    }

    #[test]
    fn list_dir_errors_on_missing_path() {
        let tool = ListDirTool;
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::from([("path".to_owned(), "/nonexistent/dir".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidInput { .. } | ToolError::ExecutionFailed { .. })
        );
    }

    // ---- Path validation ----

    #[test]
    fn validate_path_blocks_ssh_directory() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let ssh_path = format!("{home}/.ssh/id_rsa");
        // .ssh may or may not exist; either way the sensitive check should fire
        // if the parent exists.
        let path = Path::new(&ssh_path);
        if path.parent().map_or(false, |p| p.exists()) {
            let result = validate_path(&ssh_path, "test_tool", &ctx());
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, ToolError::InvalidInput { .. }));
            assert!(err.to_string().contains("sensitive path"));
        }
    }

    #[test]
    fn validate_path_blocks_etc_shadow() {
        // /etc exists on unix so canonicalization will succeed for the parent.
        #[cfg(unix)]
        {
            let result = validate_path("/etc/shadow", "test_tool", &ctx());
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, ToolError::InvalidInput { .. }));
            assert!(err.to_string().contains("sensitive path"));
        }
    }

    #[test]
    fn validate_path_blocks_etc_passwd() {
        #[cfg(unix)]
        {
            let result = validate_path("/etc/passwd", "test_tool", &ctx());
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, ToolError::InvalidInput { .. }));
            assert!(err.to_string().contains("sensitive path"));
        }
    }

    #[test]
    fn validate_path_blocks_aws_credentials() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let aws_path = format!("{home}/.aws/credentials");
        let path = Path::new(&aws_path);
        if path.parent().map_or(false, |p| p.exists()) {
            let result = validate_path(&aws_path, "test_tool", &ctx());
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, ToolError::InvalidInput { .. }));
            assert!(err.to_string().contains("sensitive path"));
        }
    }

    #[test]
    fn validate_path_blocks_gnupg() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let gnupg_path = format!("{home}/.gnupg/secring.gpg");
        let path = Path::new(&gnupg_path);
        if path.parent().map_or(false, |p| p.exists()) {
            let result = validate_path(&gnupg_path, "test_tool", &ctx());
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, ToolError::InvalidInput { .. }));
            assert!(err.to_string().contains("sensitive path"));
        }
    }

    #[test]
    fn validate_path_allows_normal_path() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("normal.txt");
        fs::write(&file, "").unwrap();

        let result = validate_path(
            &file.to_string_lossy(),
            "test_tool",
            &ctx(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn validate_path_enforces_working_dir_containment() {
        let root = tempdir().unwrap();
        let inside = root.path().join("inside.txt");
        fs::write(&inside, "").unwrap();

        // Path inside working dir should succeed.
        let ctx = ctx_with_working_dir(root.path());
        let result = validate_path(
            &inside.to_string_lossy(),
            "test_tool",
            &ctx,
        );
        assert!(result.is_ok());

        // Path outside working dir should fail.
        let other = tempdir().unwrap();
        let outside = other.path().join("outside.txt");
        fs::write(&outside, "").unwrap();

        let result = validate_path(
            &outside.to_string_lossy(),
            "test_tool",
            &ctx,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
        assert!(err.to_string().contains("outside working directory"));
    }

    #[test]
    fn validate_path_catches_traversal_attack() {
        let root = tempdir().unwrap();
        let sub = root.path().join("sub");
        fs::create_dir(&sub).unwrap();
        // Create a file at the root level (outside sub/).
        let escape_target = root.path().join("secret.txt");
        fs::write(&escape_target, "secret").unwrap();

        let ctx = ctx_with_working_dir(&sub);

        // Attempt path traversal via ../
        let traversal = format!("{}/../secret.txt", sub.display());
        let result = validate_path(&traversal, "test_tool", &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
        assert!(err.to_string().contains("outside working directory"));
    }

    #[test]
    fn read_file_blocked_outside_working_dir() {
        let root = tempdir().unwrap();
        let other = tempdir().unwrap();
        let outside_file = other.path().join("secret.txt");
        fs::write(&outside_file, "secret data").unwrap();

        let ctx = ctx_with_working_dir(root.path());
        let tool = ReadFileTool;
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                outside_file.to_string_lossy().into_owned(),
            )]),
        };

        let err = tool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
    }

    #[test]
    fn write_file_blocked_outside_working_dir() {
        let root = tempdir().unwrap();
        let other = tempdir().unwrap();
        let outside_file = other.path().join("evil.txt");

        let ctx = ctx_with_working_dir(root.path());
        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), outside_file.to_string_lossy().into_owned()),
                ("content".to_owned(), "malicious".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
    }

    #[test]
    fn list_dir_blocked_outside_working_dir() {
        let root = tempdir().unwrap();
        let other = tempdir().unwrap();

        let ctx = ctx_with_working_dir(root.path());
        let tool = ListDirTool;
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                other.path().to_string_lossy().into_owned(),
            )]),
        };

        let err = tool.run(&call, &ctx).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
    }

    #[test]
    fn is_sensitive_path_unit_tests() {
        assert!(is_sensitive_path(Path::new("/home/user/.ssh/id_rsa")));
        assert!(is_sensitive_path(Path::new("/home/user/.gnupg/secring.gpg")));
        assert!(is_sensitive_path(Path::new("/home/user/.aws/credentials")));
        assert!(is_sensitive_path(Path::new("/home/user/.aws/config")));
        assert!(is_sensitive_path(Path::new("/home/user/.config/gcloud/key.json")));
        assert!(is_sensitive_path(Path::new("/etc/shadow")));
        assert!(is_sensitive_path(Path::new("/etc/passwd")));
        assert!(is_sensitive_path(Path::new("/etc/sudoers")));

        // Normal paths should not be blocked.
        assert!(!is_sensitive_path(Path::new("/home/user/project/main.rs")));
        assert!(!is_sensitive_path(Path::new("/tmp/test.txt")));
        assert!(!is_sensitive_path(Path::new("/var/log/app.log")));
    }

    #[test]
    fn read_file_requires_path() {
        let tool = ReadFileTool;
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "path",
                ..
            }
        ));
    }

    #[test]
    fn write_file_requires_path() {
        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([("content".to_owned(), "hello".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "path",
                ..
            }
        ));
    }

    #[test]
    fn write_file_requires_content() {
        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([("path".to_owned(), "/tmp/test.txt".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "content",
                ..
            }
        ));
    }

    #[test]
    fn list_dir_requires_path() {
        let tool = ListDirTool;
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "path",
                ..
            }
        ));
    }

    #[test]
    fn list_dir_empty_directory() {
        let dir = tempdir().unwrap();

        let tool = ListDirTool;
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                dir.path().to_string_lossy().into_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.content, "(empty directory)");
    }

    #[test]
    fn read_file_metadata_includes_path() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("meta.txt");
        fs::write(&file_path, "content").unwrap();

        let tool = ReadFileTool;
        let path_str = file_path.to_string_lossy().into_owned();
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::from([("path".to_owned(), path_str.clone())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.metadata.get("path").unwrap(), &path_str);
        assert_eq!(output.metadata.get("tool").unwrap(), "read_file");
    }

    #[test]
    fn write_file_metadata_includes_bytes_written() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("bytes.txt");

        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file_path.to_string_lossy().into_owned()),
                ("content".to_owned(), "twelve chars".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.metadata.get("bytes_written").unwrap(), "12");
    }

    #[test]
    fn write_file_overwrites_existing() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("overwrite.txt");
        fs::write(&file_path, "original").unwrap();

        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file_path.to_string_lossy().into_owned()),
                ("content".to_owned(), "replaced".to_owned()),
            ]),
        };

        tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "replaced");
    }

    #[test]
    fn list_dir_sorts_entries() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("zebra.txt"), "").unwrap();
        fs::write(dir.path().join("apple.txt"), "").unwrap();
        fs::write(dir.path().join("mango.txt"), "").unwrap();

        let tool = ListDirTool;
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                dir.path().to_string_lossy().into_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        let lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(lines[0], "apple.txt");
        assert_eq!(lines[1], "mango.txt");
        assert_eq!(lines[2], "zebra.txt");
    }

    #[test]
    fn list_dir_metadata_includes_entry_count() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();

        let tool = ListDirTool;
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                dir.path().to_string_lossy().into_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.metadata.get("entry_count").unwrap(), "2");
    }
}
