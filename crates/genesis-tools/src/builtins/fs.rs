use std::collections::BTreeMap;
use std::fs;

use crate::builtins::path_security::validate_path;
use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_READ_BYTES: usize = 128 * 1024;

pub struct ReadFileTool;

impl ToolHandler for ReadFileTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let canonical = validate_path(path, &call.name, context)?;

        let content =
            fs::read_to_string(&canonical).map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to read `{path}`: {e}"),
            })?;

        let content = crate::truncate_at(&content, MAX_READ_BYTES, "\n... (file truncated)");

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("path".to_owned(), path.clone()),
            ]),
        })
    }
}

pub struct WriteFileTool;

impl ToolHandler for WriteFileTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = call
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

        // Validate path *before* creating directories — we must not create
        // directories in locations we shouldn't be writing to.
        let canonical = validate_path(path, &call.name, context)?;

        // Create parent directories if needed.
        if let Some(parent) = canonical.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to create directories for `{path}`: {e}"),
                })?;
            }
        }

        fs::write(&canonical, content).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to write `{path}`: {e}"),
        })?;

        Ok(ToolOutput {
            content: format!("wrote {} bytes to {path}", content.len()),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("path".to_owned(), path.clone()),
                ("bytes_written".to_owned(), content.len().to_string()),
            ]),
        })
    }
}

pub struct ListDirTool;

impl ToolHandler for ListDirTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let canonical = validate_path(path, &call.name, context)?;

        let entries = fs::read_dir(&canonical).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to list `{path}`: {e}"),
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
                ("path".to_owned(), path.clone()),
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

    fn ctx_with_working_dir(dir: &str) -> ToolContext {
        ToolContext {
            default_working_dir: Some(dir.to_owned()),
            ..ctx()
        }
    }

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
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

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
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    // --- Path validation integration tests ---

    #[test]
    fn read_file_blocks_sensitive_path() {
        let tool = ReadFileTool;
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::from([("path".to_owned(), format!("{home}/.ssh/id_rsa"))]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sensitive path"), "error: {msg}");
    }

    #[test]
    fn write_file_blocks_sensitive_path() {
        let tool = WriteFileTool;
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), format!("{home}/.ssh/authorized_keys")),
                ("content".to_owned(), "evil key".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sensitive path"), "error: {msg}");
    }

    #[test]
    fn list_dir_blocks_sensitive_path() {
        let tool = ListDirTool;
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::from([("path".to_owned(), format!("{home}/.ssh"))]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sensitive path"), "error: {msg}");
    }

    #[test]
    fn read_file_blocks_path_outside_working_dir() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let file = outside.path().join("secret.txt");
        fs::write(&file, "secret").unwrap();

        let ctx = ctx_with_working_dir(&dir.path().to_string_lossy());
        let tool = ReadFileTool;
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                file.to_string_lossy().into_owned(),
            )]),
        };

        let err = tool.run(&call, &ctx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("outside the allowed working directory"), "error: {msg}");
    }

    #[test]
    fn write_file_allowed_in_working_dir() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("ok.txt");

        let ctx = ctx_with_working_dir(&dir.path().to_string_lossy());
        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file_path.to_string_lossy().into_owned()),
                ("content".to_owned(), "allowed".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx).expect("should succeed");
        assert!(output.content.contains("7 bytes"));
    }

    #[test]
    fn read_file_blocks_traversal_escape() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("project");
        fs::create_dir(&sub).unwrap();

        let ctx = ctx_with_working_dir(&sub.to_string_lossy());
        let tool = ReadFileTool;
        // Attempt path traversal via ..
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::from([(
                "path".to_owned(),
                format!("{}/../../../etc/hosts", sub.display()),
            )]),
        };

        let err = tool.run(&call, &ctx).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("outside the allowed working directory"),
            "error: {msg}"
        );
    }
}
