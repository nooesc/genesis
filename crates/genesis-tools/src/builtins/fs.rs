use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_READ_BYTES: usize = 128 * 1024;

pub struct ReadFileTool;

impl ToolHandler for ReadFileTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let content = fs::read_to_string(path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read `{path}`: {e}"),
        })?;

        let content = if content.len() > MAX_READ_BYTES {
            let mut truncated = content[..MAX_READ_BYTES].to_string();
            truncated.push_str("\n... (file truncated)");
            truncated
        } else {
            content
        };

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
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
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

        // Create parent directories if needed.
        if let Some(parent) = Path::new(path).parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to create directories for `{path}`: {e}"),
                })?;
            }
        }

        fs::write(path, content).map_err(|e| ToolError::ExecutionFailed {
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
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let entries = fs::read_dir(path).map_err(|e| ToolError::ExecutionFailed {
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
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
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
}
