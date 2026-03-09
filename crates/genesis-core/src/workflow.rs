//! Simple workflow engine for multi-step agent pipelines.
//!
//! A workflow is a sequence of named steps, each with a prompt template and
//! optional configuration. Steps execute sequentially, with each step's output
//! available to subsequent steps via `{{step_name}}` template variables.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A workflow definition containing ordered steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<WorkflowStep>,
}

/// A single step in a workflow pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    /// Unique name for this step (used as template variable).
    pub name: String,
    /// Prompt template. Use `{{input}}` for the initial input and
    /// `{{step_name}}` for previous step outputs.
    pub prompt: String,
    /// Optional model override for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional max turns for this step (default: use agent default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<usize>,
    /// If true, this step's output is used as the final workflow result
    /// even if more steps follow (early exit on success).
    #[serde(default)]
    pub terminal: bool,
}

/// Result of executing a single workflow step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_name: String,
    pub output: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Result of executing a complete workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub workflow_name: String,
    pub step_results: Vec<StepResult>,
    pub final_output: String,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

impl WorkflowResult {
    /// Number of steps that completed successfully.
    pub fn steps_completed(&self) -> usize {
        self.step_results.len()
    }
}

/// Render a prompt template by substituting variables.
///
/// Supported variables:
/// - `{{input}}` — the initial workflow input
/// - `{{step_name}}` — output from a previous step
pub fn render_prompt(
    template: &str,
    input: &str,
    step_outputs: &HashMap<String, String>,
) -> String {
    let mut result = template.replace("{{input}}", input);
    for (name, output) in step_outputs {
        result = result.replace(&format!("{{{{{name}}}}}"), output);
    }
    result
}

/// Parse a workflow definition from YAML.
pub fn parse_workflow(yaml: &str) -> Result<WorkflowDefinition, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

/// Validate a workflow definition for common issues.
pub fn validate_workflow(workflow: &WorkflowDefinition) -> Vec<String> {
    let mut issues = Vec::new();

    if workflow.steps.is_empty() {
        issues.push("Workflow has no steps".into());
    }

    let mut seen_names = std::collections::HashSet::new();
    for (i, step) in workflow.steps.iter().enumerate() {
        if step.name.is_empty() {
            issues.push(format!("Step {} has an empty name", i + 1));
        }
        if !seen_names.insert(&step.name) {
            issues.push(format!("Duplicate step name: '{}'", step.name));
        }
        if step.prompt.is_empty() {
            issues.push(format!("Step '{}' has an empty prompt", step.name));
        }

        // Check for references to future steps
        for (j, other) in workflow.steps.iter().enumerate() {
            if j > i {
                let var = format!("{{{{{}}}}}", other.name);
                if step.prompt.contains(&var) {
                    issues.push(format!(
                        "Step '{}' references future step '{}' (only backward references allowed)",
                        step.name, other.name
                    ));
                }
            }
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_prompt_substitutes_input() {
        let result = render_prompt("Analyze: {{input}}", "hello world", &HashMap::new());
        assert_eq!(result, "Analyze: hello world");
    }

    #[test]
    fn render_prompt_substitutes_step_outputs() {
        let mut outputs = HashMap::new();
        outputs.insert("research".into(), "Found 3 results".into());
        let result = render_prompt(
            "Based on research: {{research}}\nNow summarize for: {{input}}",
            "the user",
            &outputs,
        );
        assert_eq!(result, "Based on research: Found 3 results\nNow summarize for: the user");
    }

    #[test]
    fn render_prompt_leaves_unknown_variables() {
        let result = render_prompt("Hello {{unknown}}", "test", &HashMap::new());
        assert_eq!(result, "Hello {{unknown}}");
    }

    #[test]
    fn parse_workflow_from_yaml() {
        let yaml = r#"
name: research_pipeline
description: Research and summarize a topic
steps:
  - name: research
    prompt: "Research the following topic: {{input}}"
  - name: summarize
    prompt: "Summarize these findings: {{research}}"
    terminal: true
"#;
        let workflow = parse_workflow(yaml).unwrap();
        assert_eq!(workflow.name, "research_pipeline");
        assert_eq!(workflow.steps.len(), 2);
        assert_eq!(workflow.steps[0].name, "research");
        assert_eq!(workflow.steps[1].name, "summarize");
        assert!(workflow.steps[1].terminal);
    }

    #[test]
    fn validate_catches_empty_steps() {
        let workflow = WorkflowDefinition {
            name: "empty".into(),
            description: "".into(),
            steps: vec![],
        };
        let issues = validate_workflow(&workflow);
        assert!(issues.iter().any(|i| i.contains("no steps")));
    }

    #[test]
    fn validate_catches_duplicate_names() {
        let workflow = WorkflowDefinition {
            name: "dups".into(),
            description: "".into(),
            steps: vec![
                WorkflowStep {
                    name: "step1".into(),
                    prompt: "do thing".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
                WorkflowStep {
                    name: "step1".into(),
                    prompt: "do other".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
            ],
        };
        let issues = validate_workflow(&workflow);
        assert!(issues.iter().any(|i| i.contains("Duplicate")));
    }

    #[test]
    fn validate_catches_forward_references() {
        let workflow = WorkflowDefinition {
            name: "forward_ref".into(),
            description: "".into(),
            steps: vec![
                WorkflowStep {
                    name: "first".into(),
                    prompt: "Use result: {{second}}".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
                WorkflowStep {
                    name: "second".into(),
                    prompt: "do thing".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
            ],
        };
        let issues = validate_workflow(&workflow);
        assert!(issues.iter().any(|i| i.contains("future step")));
    }

    #[test]
    fn validate_passes_valid_workflow() {
        let workflow = WorkflowDefinition {
            name: "valid".into(),
            description: "A valid workflow".into(),
            steps: vec![
                WorkflowStep {
                    name: "research".into(),
                    prompt: "Research: {{input}}".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
                WorkflowStep {
                    name: "summarize".into(),
                    prompt: "Summarize: {{research}}".into(),
                    model: None,
                    max_turns: None,
                    terminal: true,
                },
            ],
        };
        let issues = validate_workflow(&workflow);
        assert!(issues.is_empty(), "Expected no issues but got: {:?}", issues);
    }

    #[test]
    fn workflow_result_serializes() {
        let result = WorkflowResult {
            workflow_name: "test".into(),
            step_results: vec![
                StepResult {
                    step_name: "a".into(),
                    output: "result a".into(),
                    input_tokens: 100,
                    output_tokens: 50,
                },
            ],
            final_output: "result a".into(),
            total_input_tokens: 100,
            total_output_tokens: 50,
        };
        assert_eq!(result.steps_completed(), 1);
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["workflow_name"], "test");
    }
}
