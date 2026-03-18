//! Think tool — structured reasoning scratchpad for Claude models.
//!
//! A zero-cost tool that lets the model reason between tool calls without
//! the output affecting the conversation. Returns an empty result.
//!
//! **54% improvement on Tau-Bench airline domain** per Anthropic Engineering:
//! <https://www.anthropic.com/engineering/claude-think-tool>

use std::collections::BTreeMap;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that provides a structured reasoning scratchpad.
///
/// The model uses `think` to organize its thoughts between tool calls.
/// The thought content is captured in the tool call arguments but the
/// result is always empty — no tokens are wasted on a response.
pub struct ThinkTool;

impl ToolHandler for ThinkTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        // Validate that the thought parameter is present.
        let _thought =
            call.arguments
                .get("thought")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "thought",
                })?;

        // Return empty content — the value is in the tool call itself,
        // not in the result. This is zero-cost for the output budget.
        Ok(ToolOutput {
            content: String::new(),
            metadata: BTreeMap::new(),
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
    fn think_returns_empty_content() {
        let tool = ThinkTool;
        let call = ToolCall {
            name: "think".to_owned(),
            arguments: BTreeMap::from([(
                "thought".to_owned(),
                "I need to consider the file structure before editing.".to_owned(),
            )]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.is_empty(), "think tool should return empty content");
        assert!(output.metadata.is_empty());
    }

    #[test]
    fn think_requires_thought_parameter() {
        let tool = ThinkTool;
        let call = ToolCall {
            name: "think".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "thought",
                ..
            }
        ));
    }
}
