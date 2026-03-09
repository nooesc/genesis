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

/// Error from workflow execution.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("workflow validation failed: {0}")]
    Validation(String),
    #[error("step '{step}' failed: {source}")]
    StepFailed {
        step: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Execute a workflow by running each step through the provided async runner.
///
/// The `runner` closure receives `(rendered_prompt, optional_max_turns)` and
/// must return the step output text plus token counts. This keeps the executor
/// decoupled from any specific agent implementation.
///
/// # Example
///
/// ```ignore
/// let result = execute_workflow(&workflow, "user question", |prompt, max_turns| async {
///     let agent_result = agent.run_turn(&prompt).await?;
///     Ok((agent_result.response, agent_result.total_input_tokens, agent_result.total_output_tokens))
/// }).await?;
/// ```
pub async fn execute_workflow<F, Fut>(
    workflow: &WorkflowDefinition,
    input: &str,
    mut runner: F,
) -> Result<WorkflowResult, WorkflowError>
where
    F: FnMut(String, Option<usize>) -> Fut,
    Fut: std::future::Future<Output = Result<(String, u32, u32), Box<dyn std::error::Error + Send + Sync>>>,
{
    let issues = validate_workflow(workflow);
    if !issues.is_empty() {
        return Err(WorkflowError::Validation(issues.join("; ")));
    }

    let mut step_outputs: HashMap<String, String> = HashMap::new();
    let mut step_results: Vec<StepResult> = Vec::new();
    let mut total_input_tokens = 0u32;
    let mut total_output_tokens = 0u32;

    for step in &workflow.steps {
        let rendered = render_prompt(&step.prompt, input, &step_outputs);

        let (output, in_tok, out_tok) = runner(rendered, step.max_turns)
            .await
            .map_err(|e| WorkflowError::StepFailed {
                step: step.name.clone(),
                source: e,
            })?;

        total_input_tokens = total_input_tokens.saturating_add(in_tok);
        total_output_tokens = total_output_tokens.saturating_add(out_tok);

        step_outputs.insert(step.name.clone(), output.clone());
        step_results.push(StepResult {
            step_name: step.name.clone(),
            output: output.clone(),
            input_tokens: in_tok,
            output_tokens: out_tok,
        });

        if step.terminal {
            break;
        }
    }

    let final_output = step_results.last()
        .map(|r| r.output.clone())
        .unwrap_or_default();

    Ok(WorkflowResult {
        workflow_name: workflow.name.clone(),
        step_results,
        final_output,
        total_input_tokens,
        total_output_tokens,
    })
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

    #[tokio::test]
    async fn execute_workflow_runs_steps_sequentially() {
        let workflow = WorkflowDefinition {
            name: "pipeline".into(),
            description: "test pipeline".into(),
            steps: vec![
                WorkflowStep {
                    name: "step1".into(),
                    prompt: "Process: {{input}}".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
                WorkflowStep {
                    name: "step2".into(),
                    prompt: "Refine: {{step1}}".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
            ],
        };

        let result = execute_workflow(&workflow, "hello", |prompt, _max_turns| async move {
            // Simulate an agent that echoes the prompt
            Ok((format!("output_of({prompt})"), 10, 5))
        }).await.unwrap();

        assert_eq!(result.workflow_name, "pipeline");
        assert_eq!(result.step_results.len(), 2);
        assert_eq!(result.step_results[0].output, "output_of(Process: hello)");
        assert_eq!(result.step_results[1].output, "output_of(Refine: output_of(Process: hello))");
        assert_eq!(result.total_input_tokens, 20);
        assert_eq!(result.total_output_tokens, 10);
    }

    #[tokio::test]
    async fn execute_workflow_stops_at_terminal_step() {
        let workflow = WorkflowDefinition {
            name: "early_exit".into(),
            description: "".into(),
            steps: vec![
                WorkflowStep {
                    name: "a".into(),
                    prompt: "Step A: {{input}}".into(),
                    model: None,
                    max_turns: None,
                    terminal: true,
                },
                WorkflowStep {
                    name: "b".into(),
                    prompt: "Step B: {{a}}".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
            ],
        };

        let result = execute_workflow(&workflow, "test", |_prompt, _| async move {
            Ok(("done".into(), 5, 3))
        }).await.unwrap();

        assert_eq!(result.step_results.len(), 1);
        assert_eq!(result.final_output, "done");
    }

    #[tokio::test]
    async fn execute_workflow_propagates_step_error() {
        let workflow = WorkflowDefinition {
            name: "failing".into(),
            description: "".into(),
            steps: vec![
                WorkflowStep {
                    name: "fail".into(),
                    prompt: "boom".into(),
                    model: None,
                    max_turns: None,
                    terminal: false,
                },
            ],
        };

        let result = execute_workflow(&workflow, "x", |_prompt, _| async move {
            Err("agent error".into())
        }).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("fail"));
    }

    #[tokio::test]
    async fn execute_workflow_rejects_invalid() {
        let workflow = WorkflowDefinition {
            name: "empty".into(),
            description: "".into(),
            steps: vec![],
        };

        let result = execute_workflow(&workflow, "x", |_prompt, _| async move {
            Ok(("nope".into(), 0, 0))
        }).await;

        assert!(matches!(result, Err(WorkflowError::Validation(_))));
    }
}
