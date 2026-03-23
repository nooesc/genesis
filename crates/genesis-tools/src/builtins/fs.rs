use std::collections::BTreeMap;
use std::fs;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_READ_BYTES: usize = 128 * 1024;

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

        let path =
            crate::sandbox::validate_tool_path(path_arg, &call.name, &context.path_validator)?;

        let content = fs::read_to_string(&path).map_err(|e| ToolError::ExecutionFailed {
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

        // Validate path BEFORE creating directories (sandbox escape prevention).
        let path =
            crate::sandbox::validate_tool_path(path_arg, &call.name, &context.path_validator)?;

        // Create parent directories if needed.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to create directories for `{}`: {e}", path.display()),
                })?;
            }
        }

        fs::write(&path, content).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to write `{}`: {e}", path.display()),
        })?;

        Ok(ToolOutput {
            content: format!("wrote {} bytes to {}", content.len(), path.display()),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("path".to_owned(), path.display().to_string()),
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

        let path =
            crate::sandbox::validate_tool_path(path_arg, &call.name, &context.path_validator)?;

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
    use std::path::PathBuf;

    use super::*;
    use crate::ToolContext;
    use std::fs;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx_destructive()
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

    #[test]
    fn read_file_blocks_sensitive_path() {
        use std::sync::Arc;
        let ctx = ToolContext {
            path_validator: Some(Arc::new(crate::sandbox::PathValidator::new(
                None,
                PathBuf::from("/home/user"),
            ))),
            ..crate::test_utils::test_ctx()
        };
        let tool = ReadFileTool;
        let call = ToolCall {
            name: "read_file".to_owned(),
            arguments: BTreeMap::from([("path".to_owned(), "/home/user/.ssh/id_rsa".to_owned())]),
        };
        let result = tool.run(&call, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn write_file_blocks_sensitive_path() {
        use std::sync::Arc;
        let ctx = ToolContext {
            path_validator: Some(Arc::new(crate::sandbox::PathValidator::new(
                None,
                PathBuf::from("/home/user"),
            ))),
            ..crate::test_utils::test_ctx_destructive()
        };
        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                (
                    "path".to_owned(),
                    "/home/user/.ssh/authorized_keys".to_owned(),
                ),
                ("content".to_owned(), "malicious key".to_owned()),
            ]),
        };
        let result = tool.run(&call, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn list_dir_blocks_sensitive_path() {
        use std::sync::Arc;
        let ctx = ToolContext {
            path_validator: Some(Arc::new(crate::sandbox::PathValidator::new(
                None,
                PathBuf::from("/home/user"),
            ))),
            ..crate::test_utils::test_ctx()
        };
        let tool = ListDirTool;
        let call = ToolCall {
            name: "list_dir".to_owned(),
            arguments: BTreeMap::from([("path".to_owned(), "/home/user/.ssh".to_owned())]),
        };
        let result = tool.run(&call, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn write_file_with_validator_allows_valid_path() {
        use std::sync::Arc;
        let dir = tempdir().unwrap();
        let ctx = ToolContext {
            path_validator: Some(Arc::new(crate::sandbox::PathValidator::new(
                Some(dir.path().to_path_buf()),
                PathBuf::from("/tmp/fake-home"),
            ))),
            ..crate::test_utils::test_ctx_destructive()
        };
        let file_path = dir.path().join("valid.txt");
        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file_path.to_string_lossy().into_owned()),
                ("content".to_owned(), "safe content".to_owned()),
            ]),
        };
        let result = tool.run(&call, &ctx);
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "safe content");
    }

    #[test]
    fn write_file_with_validator_blocks_outside_working_dir() {
        use std::sync::Arc;
        let dir = tempdir().unwrap();
        let ctx = ToolContext {
            path_validator: Some(Arc::new(crate::sandbox::PathValidator::new(
                Some(dir.path().to_path_buf()),
                PathBuf::from("/tmp/fake-home"),
            ))),
            ..crate::test_utils::test_ctx_destructive()
        };
        let tool = WriteFileTool;
        let call = ToolCall {
            name: "write_file".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), "/tmp/escape.txt".to_owned()),
                ("content".to_owned(), "escaped".to_owned()),
            ]),
        };
        let result = tool.run(&call, &ctx);
        assert!(result.is_err());
        // Verify the file was NOT created (create_dir_all runs after validation).
        assert!(!PathBuf::from("/tmp/escape.txt").exists());
    }
}
