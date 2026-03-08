use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Targeted find-and-replace tool.
///
/// Instead of rewriting an entire file (which wastes tokens and risks data loss),
/// this tool applies a precise text replacement within a file. The old text must
/// exist exactly once in the file to avoid ambiguity.
pub struct PatchTool;

impl ToolHandler for PatchTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let old_text = call
            .arguments
            .get("old_text")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "old_text",
            })?;

        let new_text = call
            .arguments
            .get("new_text")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "new_text",
            })?;

        let file_path = Path::new(path);
        let content = fs::read_to_string(file_path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read `{path}`: {e}"),
        })?;

        // Count occurrences to ensure unambiguous replacement.
        let count = content.matches(old_text.as_str()).count();
        if count == 0 {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "old_text not found in `{path}`. Make sure the text matches exactly, \
                     including whitespace and indentation."
                ),
            });
        }

        let replace_all = call
            .arguments
            .get("replace_all")
            .map(|v| v == "true")
            .unwrap_or(false);

        if count > 1 && !replace_all {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "old_text found {count} times in `{path}`. Provide more surrounding \
                     context to make the match unique, or set replace_all to \"true\" to \
                     replace every occurrence."
                ),
            });
        }

        let updated = content.replace(old_text.as_str(), new_text.as_str());
        fs::write(file_path, &updated).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to write `{path}`: {e}"),
        })?;

        let replacements = if replace_all { count } else { 1 };
        Ok(ToolOutput {
            content: format!(
                "patched `{path}`: {replacements} replacement(s) applied ({} bytes → {} bytes)",
                content.len(),
                updated.len()
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("path".to_owned(), path.clone()),
                ("replacements".to_owned(), replacements.to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
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

    #[test]
    fn patch_replaces_unique_text() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world\ngoodbye world\n").unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "hello world".to_owned()),
                ("new_text".to_owned(), "hi world".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("1 replacement(s)"));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "hi world\ngoodbye world\n"
        );
    }

    #[test]
    fn patch_errors_on_missing_text() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world\n").unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "not in file".to_owned()),
                ("new_text".to_owned(), "replacement".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("not found"));
            }
            _ => panic!("expected ExecutionFailed"),
        }
    }

    #[test]
    fn patch_errors_on_ambiguous_match() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello\nhello\nhello\n").unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "hello".to_owned()),
                ("new_text".to_owned(), "hi".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("3 times"));
            }
            _ => panic!("expected ExecutionFailed"),
        }
    }

    #[test]
    fn patch_replace_all_replaces_every_occurrence() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello\nhello\nhello\n").unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "hello".to_owned()),
                ("new_text".to_owned(), "hi".to_owned()),
                ("replace_all".to_owned(), "true".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("3 replacement(s)"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "hi\nhi\nhi\n");
    }

    #[test]
    fn patch_errors_on_missing_file() {
        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), "/nonexistent/file.txt".to_owned()),
                ("old_text".to_owned(), "hello".to_owned()),
                ("new_text".to_owned(), "hi".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn patch_preserves_indentation() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("code.rs");
        fs::write(
            &file,
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        )
        .unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "    let x = 1;".to_owned()),
                (
                    "new_text".to_owned(),
                    "    let x = 42;\n    let z = 100;".to_owned(),
                ),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("1 replacement(s)"));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "fn main() {\n    let x = 42;\n    let z = 100;\n    let y = 2;\n}\n"
        );
    }
}
