use std::collections::BTreeMap;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Tool that allows the agent to ask structured clarifying questions.
///
/// When the agent needs more information before proceeding, it can use this
/// tool instead of guessing. The question is returned as output with metadata
/// flagging it as a clarification request, which the agent loop can use to
/// pause and wait for user input.
pub struct ClarifyTool;

impl ToolHandler for ClarifyTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let question = call
            .arguments
            .get("question")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "question",
            })?;

        if question.trim().is_empty() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: "question cannot be empty".to_owned(),
            });
        }

        // Build structured output with optional choices
        let mut content = format!("[Clarification needed]\n{question}");

        if let Some(choices) = call.arguments.get("choices") {
            // Parse comma-separated choices
            let options: Vec<&str> = choices.split(',').map(|s| s.trim()).collect();
            if !options.is_empty() {
                content.push_str("\n\nOptions:");
                for (i, option) in options.iter().enumerate() {
                    content.push_str(&format!("\n  {}. {option}", i + 1));
                }
            }
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("requires_input".to_owned(), "true".to_owned()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx()
    }

    #[test]
    fn clarify_returns_question() {
        let tool = ClarifyTool;
        let call = ToolCall {
            name: "clarify".to_owned(),
            arguments: BTreeMap::from([(
                "question".to_owned(),
                "What database should I use?".to_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("What database should I use?"));
        assert!(output.content.contains("[Clarification needed]"));
        assert_eq!(
            output.metadata.get("requires_input").unwrap(),
            "true"
        );
    }

    #[test]
    fn clarify_with_choices() {
        let tool = ClarifyTool;
        let call = ToolCall {
            name: "clarify".to_owned(),
            arguments: BTreeMap::from([
                ("question".to_owned(), "Pick a framework:".to_owned()),
                ("choices".to_owned(), "React, Vue, Svelte".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("Pick a framework:"));
        assert!(output.content.contains("1. React"));
        assert!(output.content.contains("2. Vue"));
        assert!(output.content.contains("3. Svelte"));
    }

    #[test]
    fn clarify_requires_question() {
        let tool = ClarifyTool;
        let call = ToolCall {
            name: "clarify".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn clarify_rejects_empty_question() {
        let tool = ClarifyTool;
        let call = ToolCall {
            name: "clarify".to_owned(),
            arguments: BTreeMap::from([("question".to_owned(), "   ".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }
}
