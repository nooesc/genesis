//! Find tools — discovery tool for searching the tool registry.
//!
//! Allows the LLM to discover available tools by searching for names
//! or descriptions that match a query. Returns tool summaries (name +
//! description) without full JSON schemas, so the model can decide
//! which tools to use without the overhead of all schemas in every turn.
//!
//! **85-96% input token reduction** when combined with a core tool set
//! per Speakeasy and Anthropic Advanced Tool Use guidelines.

use std::collections::BTreeMap;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that searches the registry for available tools by name or description.
pub struct FindToolsTool;

impl ToolHandler for FindToolsTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = call
            .arguments
            .get("query")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "query",
            })?;

        if query.trim().is_empty() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: "query cannot be empty".to_owned(),
            });
        }

        // Note: The actual search is performed by the ToolRuntime's
        // async execution path which has access to the registry.
        // This handler returns a placeholder — the runtime intercepts
        // find_tools calls and performs the search directly.
        //
        // If this handler is reached directly (e.g. in tests), return
        // a message indicating the tool needs runtime support.
        Ok(ToolOutput {
            content: format!(
                "find_tools requires runtime support. Query: {query}\n\
                 This tool searches the tool registry for tools matching your query."
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("query".to_owned(), query.clone()),
                ("__find_tools".to_owned(), "true".to_owned()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx_destructive()
    }

    #[test]
    fn find_tools_requires_query() {
        let tool = FindToolsTool;
        let call = ToolCall {
            name: "find_tools".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "query",
                ..
            }
        ));
    }

    #[test]
    fn find_tools_rejects_empty_query() {
        let tool = FindToolsTool;
        let call = ToolCall {
            name: "find_tools".to_owned(),
            arguments: BTreeMap::from([("query".to_owned(), "  ".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn find_tools_returns_placeholder() {
        let tool = FindToolsTool;
        let call = ToolCall {
            name: "find_tools".to_owned(),
            arguments: BTreeMap::from([("query".to_owned(), "shell".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("shell"));
        assert_eq!(output.metadata.get("__find_tools").map(String::as_str), Some("true"));
    }
}
