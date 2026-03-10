//! Evaluation and benchmarking framework for agent performance testing.
//!
//! Define evaluation suites as collections of test cases with expected
//! outcomes, run the agent against them, and collect performance metrics.
//!
//! ## Concepts
//!
//! - **EvalSuite**: A named collection of test cases (e.g. "math", "coding", "reasoning")
//! - **EvalCase**: A single test with prompt, expected output criteria, and metadata
//! - **EvalResult**: Outcome of running a case (pass/fail, latency, token usage, score)
//! - **EvalReport**: Aggregated results for a suite run

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A collection of evaluation test cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSuite {
    /// Suite name (e.g. "math_reasoning", "code_generation").
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Version for tracking suite evolution.
    #[serde(default = "default_version")]
    pub version: String,
    /// Tags for categorization.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The test cases.
    pub cases: Vec<EvalCase>,
}

fn default_version() -> String {
    "1.0".to_owned()
}

/// A single evaluation test case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    /// Unique ID within the suite.
    pub id: String,
    /// The prompt to send to the agent.
    pub prompt: String,
    /// Optional system prompt override for this case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// How to evaluate the agent's response.
    pub criteria: EvalCriteria,
    /// Difficulty level (1-5).
    #[serde(default = "default_difficulty")]
    pub difficulty: u8,
    /// Tags for categorization.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Maximum allowed execution time in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

fn default_difficulty() -> u8 {
    3
}

/// Criteria for evaluating an agent's response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCriteria {
    /// The response must contain all of these strings (case-insensitive).
    #[serde(default)]
    pub must_contain: Vec<String>,
    /// The response must NOT contain any of these strings.
    #[serde(default)]
    pub must_not_contain: Vec<String>,
    /// Expected exact answer (for deterministic evaluations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_match: Option<String>,
    /// Regex pattern the response must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex_match: Option<String>,
    /// Minimum expected tool calls.
    #[serde(default)]
    pub min_tool_calls: usize,
    /// Maximum allowed tool calls (0 = unlimited).
    #[serde(default)]
    pub max_tool_calls: usize,
    /// Maximum allowed turns (0 = unlimited).
    #[serde(default)]
    pub max_turns: usize,
    /// Custom scoring function name (for external evaluators).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorer: Option<String>,
}

/// Result of evaluating a single test case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    /// Case ID from the suite.
    pub case_id: String,
    /// Whether the case passed all criteria.
    pub passed: bool,
    /// Score from 0.0 to 1.0.
    pub score: f64,
    /// The agent's response text.
    pub response: String,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Input tokens used.
    pub input_tokens: u32,
    /// Output tokens used.
    pub output_tokens: u32,
    /// Number of turns the agent took.
    pub turns_used: usize,
    /// Number of tool calls made.
    pub tool_calls: usize,
    /// Individual criteria check results.
    pub checks: Vec<CriteriaCheck>,
    /// Any error that occurred during execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of checking a single criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriteriaCheck {
    pub criterion: String,
    pub passed: bool,
    pub detail: String,
}

/// Aggregated report for a complete suite run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    /// Suite name.
    pub suite_name: String,
    /// Suite version.
    pub suite_version: String,
    /// Model used for this run.
    pub model: String,
    /// ISO 8601 timestamp of when the run started.
    pub started_at: String,
    /// ISO 8601 timestamp of when the run completed.
    pub completed_at: String,
    /// Total execution duration in milliseconds.
    pub total_duration_ms: u64,
    /// Number of cases that passed.
    pub passed: usize,
    /// Number of cases that failed.
    pub failed: usize,
    /// Number of cases that errored.
    pub errored: usize,
    /// Overall pass rate (0.0 to 1.0).
    pub pass_rate: f64,
    /// Average score across all cases.
    pub avg_score: f64,
    /// Total input tokens across all cases.
    pub total_input_tokens: u32,
    /// Total output tokens across all cases.
    pub total_output_tokens: u32,
    /// Per-case results.
    pub results: Vec<EvalResult>,
    /// Per-tag pass rates.
    pub tag_results: HashMap<String, TagResult>,
    /// Per-difficulty pass rates.
    pub difficulty_results: HashMap<u8, TagResult>,
}

/// Aggregated results for a tag or difficulty level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagResult {
    pub total: usize,
    pub passed: usize,
    pub pass_rate: f64,
    pub avg_score: f64,
}

/// Evaluate an agent response against criteria, returning individual checks.
pub fn evaluate_response(
    response: &str,
    criteria: &EvalCriteria,
    turns_used: usize,
    tool_calls: usize,
) -> (bool, f64, Vec<CriteriaCheck>) {
    let mut checks = Vec::new();
    let mut all_passed = true;
    let lower_response = response.to_lowercase();

    // Check must_contain
    for required in &criteria.must_contain {
        let found = lower_response.contains(&required.to_lowercase());
        if !found {
            all_passed = false;
        }
        checks.push(CriteriaCheck {
            criterion: format!("must_contain: {required}"),
            passed: found,
            detail: if found {
                "found".to_owned()
            } else {
                "not found in response".to_owned()
            },
        });
    }

    // Check must_not_contain
    for forbidden in &criteria.must_not_contain {
        let found = lower_response.contains(&forbidden.to_lowercase());
        if found {
            all_passed = false;
        }
        checks.push(CriteriaCheck {
            criterion: format!("must_not_contain: {forbidden}"),
            passed: !found,
            detail: if found {
                "found in response (should not be present)".to_owned()
            } else {
                "correctly absent".to_owned()
            },
        });
    }

    // Check exact_match
    if let Some(ref expected) = criteria.exact_match {
        let matches = response.trim() == expected.trim();
        if !matches {
            all_passed = false;
        }
        checks.push(CriteriaCheck {
            criterion: "exact_match".to_owned(),
            passed: matches,
            detail: if matches {
                "exact match".to_owned()
            } else {
                format!(
                    "expected '{}', got '{}'",
                    truncate(expected, 50),
                    truncate(response, 50)
                )
            },
        });
    }

    // Check regex_match
    if let Some(ref pattern) = criteria.regex_match {
        match regex::Regex::new(pattern) {
            Ok(re) => {
                let matches = re.is_match(response);
                if !matches {
                    all_passed = false;
                }
                checks.push(CriteriaCheck {
                    criterion: format!("regex_match: {pattern}"),
                    passed: matches,
                    detail: if matches {
                        "pattern matched".to_owned()
                    } else {
                        "pattern not found".to_owned()
                    },
                });
            }
            Err(e) => {
                all_passed = false;
                checks.push(CriteriaCheck {
                    criterion: format!("regex_match: {pattern}"),
                    passed: false,
                    detail: format!("invalid regex: {e}"),
                });
            }
        }
    }

    // Check min_tool_calls
    if criteria.min_tool_calls > 0 {
        let met = tool_calls >= criteria.min_tool_calls;
        if !met {
            all_passed = false;
        }
        checks.push(CriteriaCheck {
            criterion: format!("min_tool_calls: {}", criteria.min_tool_calls),
            passed: met,
            detail: format!("{tool_calls} tool calls made"),
        });
    }

    // Check max_tool_calls
    if criteria.max_tool_calls > 0 {
        let within = tool_calls <= criteria.max_tool_calls;
        if !within {
            all_passed = false;
        }
        checks.push(CriteriaCheck {
            criterion: format!("max_tool_calls: {}", criteria.max_tool_calls),
            passed: within,
            detail: format!("{tool_calls} tool calls made"),
        });
    }

    // Check max_turns
    if criteria.max_turns > 0 {
        let within = turns_used <= criteria.max_turns;
        if !within {
            all_passed = false;
        }
        checks.push(CriteriaCheck {
            criterion: format!("max_turns: {}", criteria.max_turns),
            passed: within,
            detail: format!("{turns_used} turns used"),
        });
    }

    // Compute score
    let total_checks = checks.len();
    let passed_checks = checks.iter().filter(|c| c.passed).count();
    let score = if total_checks > 0 {
        passed_checks as f64 / total_checks as f64
    } else if all_passed {
        1.0
    } else {
        0.0
    };

    (all_passed, score, checks)
}

/// Build an aggregated report from individual results.
pub fn build_report(
    suite: &EvalSuite,
    model: &str,
    started_at: &str,
    completed_at: &str,
    total_duration_ms: u64,
    results: Vec<EvalResult>,
) -> EvalReport {
    let passed = results.iter().filter(|r| r.passed).count();
    let errored = results.iter().filter(|r| r.error.is_some()).count();
    let failed = results.len() - passed - errored;

    let pass_rate = if results.is_empty() {
        0.0
    } else {
        passed as f64 / results.len() as f64
    };

    let avg_score = if results.is_empty() {
        0.0
    } else {
        results.iter().map(|r| r.score).sum::<f64>() / results.len() as f64
    };

    let total_input_tokens: u32 = results.iter().map(|r| r.input_tokens).sum();
    let total_output_tokens: u32 = results.iter().map(|r| r.output_tokens).sum();

    // Per-tag aggregation
    let mut tag_results: HashMap<String, (usize, usize, f64)> = HashMap::new();
    for (i, result) in results.iter().enumerate() {
        if let Some(case) = suite.cases.get(i) {
            for tag in &case.tags {
                let entry = tag_results.entry(tag.clone()).or_insert((0, 0, 0.0));
                entry.0 += 1;
                if result.passed {
                    entry.1 += 1;
                }
                entry.2 += result.score;
            }
        }
    }
    let tag_results: HashMap<String, TagResult> = tag_results
        .into_iter()
        .map(|(tag, (total, pass, score_sum))| {
            (
                tag,
                TagResult {
                    total,
                    passed: pass,
                    pass_rate: if total > 0 { pass as f64 / total as f64 } else { 0.0 },
                    avg_score: if total > 0 { score_sum / total as f64 } else { 0.0 },
                },
            )
        })
        .collect();

    // Per-difficulty aggregation
    let mut diff_results: HashMap<u8, (usize, usize, f64)> = HashMap::new();
    for (i, result) in results.iter().enumerate() {
        if let Some(case) = suite.cases.get(i) {
            let entry = diff_results
                .entry(case.difficulty)
                .or_insert((0, 0, 0.0));
            entry.0 += 1;
            if result.passed {
                entry.1 += 1;
            }
            entry.2 += result.score;
        }
    }
    let difficulty_results: HashMap<u8, TagResult> = diff_results
        .into_iter()
        .map(|(diff, (total, pass, score_sum))| {
            (
                diff,
                TagResult {
                    total,
                    passed: pass,
                    pass_rate: if total > 0 { pass as f64 / total as f64 } else { 0.0 },
                    avg_score: if total > 0 { score_sum / total as f64 } else { 0.0 },
                },
            )
        })
        .collect();

    EvalReport {
        suite_name: suite.name.clone(),
        suite_version: suite.version.clone(),
        model: model.to_owned(),
        started_at: started_at.to_owned(),
        completed_at: completed_at.to_owned(),
        total_duration_ms,
        passed,
        failed,
        errored,
        pass_rate,
        avg_score,
        total_input_tokens,
        total_output_tokens,
        results,
        tag_results,
        difficulty_results,
    }
}

/// Parse an evaluation suite from YAML.
pub fn parse_suite(yaml: &str) -> Result<EvalSuite, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

/// Validate an evaluation suite for common issues.
pub fn validate_suite(suite: &EvalSuite) -> Vec<String> {
    let mut issues = Vec::new();

    if suite.name.is_empty() {
        issues.push("Suite has no name".into());
    }

    if suite.cases.is_empty() {
        issues.push("Suite has no test cases".into());
    }

    let mut seen_ids = std::collections::HashSet::new();
    for (i, case) in suite.cases.iter().enumerate() {
        if case.id.is_empty() {
            issues.push(format!("Case {} has an empty ID", i + 1));
        }
        if !seen_ids.insert(&case.id) {
            issues.push(format!("Duplicate case ID: '{}'", case.id));
        }
        if case.prompt.is_empty() {
            issues.push(format!("Case '{}' has an empty prompt", case.id));
        }
        if case.criteria.must_contain.is_empty()
            && case.criteria.must_not_contain.is_empty()
            && case.criteria.exact_match.is_none()
            && case.criteria.regex_match.is_none()
            && case.criteria.min_tool_calls == 0
            && case.criteria.max_tool_calls == 0
            && case.criteria.max_turns == 0
            && case.criteria.scorer.is_none()
        {
            issues.push(format!("Case '{}' has no evaluation criteria", case.id));
        }
        if case.difficulty > 5 {
            issues.push(format!(
                "Case '{}' has difficulty {} (max is 5)",
                case.id, case.difficulty
            ));
        }
    }

    issues
}

fn truncate(s: &str, max: usize) -> String {
    crate::compress::truncate(s, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_criteria() -> EvalCriteria {
        EvalCriteria {
            must_contain: vec!["hello".into(), "world".into()],
            must_not_contain: vec!["error".into()],
            exact_match: None,
            regex_match: None,
            min_tool_calls: 0,
            max_tool_calls: 0,
            max_turns: 0,
            scorer: None,
        }
    }

    #[test]
    fn evaluate_passing_response() {
        let criteria = sample_criteria();
        let (passed, score, checks) =
            evaluate_response("Hello World! This is great.", &criteria, 1, 0);
        assert!(passed);
        assert_eq!(score, 1.0);
        assert!(checks.iter().all(|c| c.passed));
    }

    #[test]
    fn evaluate_missing_required_content() {
        let criteria = sample_criteria();
        let (passed, score, checks) =
            evaluate_response("Hello there!", &criteria, 1, 0);
        assert!(!passed);
        assert!(score < 1.0);
        let world_check = checks.iter().find(|c| c.criterion.contains("world")).unwrap();
        assert!(!world_check.passed);
    }

    #[test]
    fn evaluate_forbidden_content() {
        let criteria = sample_criteria();
        let (passed, _score, checks) =
            evaluate_response("Hello World! But there was an error.", &criteria, 1, 0);
        assert!(!passed);
        let error_check = checks.iter().find(|c| c.criterion.contains("error")).unwrap();
        assert!(!error_check.passed);
    }

    #[test]
    fn evaluate_exact_match() {
        let criteria = EvalCriteria {
            must_contain: vec![],
            must_not_contain: vec![],
            exact_match: Some("42".into()),
            regex_match: None,
            min_tool_calls: 0,
            max_tool_calls: 0,
            max_turns: 0,
            scorer: None,
        };
        let (passed, score, _) = evaluate_response("42", &criteria, 1, 0);
        assert!(passed);
        assert_eq!(score, 1.0);

        let (passed, _, _) = evaluate_response("43", &criteria, 1, 0);
        assert!(!passed);
    }

    #[test]
    fn evaluate_regex_match() {
        let criteria = EvalCriteria {
            must_contain: vec![],
            must_not_contain: vec![],
            exact_match: None,
            regex_match: Some(r"\d{3}-\d{4}".into()),
            min_tool_calls: 0,
            max_tool_calls: 0,
            max_turns: 0,
            scorer: None,
        };
        let (passed, _, _) = evaluate_response("Call 555-1234 now", &criteria, 1, 0);
        assert!(passed);

        let (passed, _, _) = evaluate_response("No phone number here", &criteria, 1, 0);
        assert!(!passed);
    }

    #[test]
    fn evaluate_tool_call_limits() {
        let criteria = EvalCriteria {
            must_contain: vec![],
            must_not_contain: vec![],
            exact_match: None,
            regex_match: None,
            min_tool_calls: 2,
            max_tool_calls: 5,
            max_turns: 0,
            scorer: None,
        };
        // Within bounds
        let (passed, _, _) = evaluate_response("done", &criteria, 1, 3);
        assert!(passed);

        // Too few
        let (passed, _, checks) = evaluate_response("done", &criteria, 1, 1);
        assert!(!passed);
        assert!(checks.iter().any(|c| c.criterion.contains("min_tool_calls") && !c.passed));

        // Too many
        let (passed, _, checks) = evaluate_response("done", &criteria, 1, 6);
        assert!(!passed);
        assert!(checks.iter().any(|c| c.criterion.contains("max_tool_calls") && !c.passed));
    }

    #[test]
    fn evaluate_max_turns() {
        let criteria = EvalCriteria {
            must_contain: vec![],
            must_not_contain: vec![],
            exact_match: None,
            regex_match: None,
            min_tool_calls: 0,
            max_tool_calls: 0,
            max_turns: 3,
            scorer: None,
        };
        let (passed, _, _) = evaluate_response("done", &criteria, 2, 0);
        assert!(passed);

        let (passed, _, _) = evaluate_response("done", &criteria, 5, 0);
        assert!(!passed);
    }

    #[test]
    fn parse_suite_from_yaml() {
        let yaml = r#"
name: basic_math
description: Simple arithmetic tests
version: "1.0"
tags: [math, basic]
cases:
  - id: add_simple
    prompt: "What is 2 + 2?"
    difficulty: 1
    tags: [addition]
    criteria:
      must_contain: ["4"]
  - id: multiply
    prompt: "What is 7 * 8?"
    difficulty: 2
    tags: [multiplication]
    criteria:
      must_contain: ["56"]
      max_turns: 1
"#;
        let suite = parse_suite(yaml).unwrap();
        assert_eq!(suite.name, "basic_math");
        assert_eq!(suite.cases.len(), 2);
        assert_eq!(suite.cases[0].id, "add_simple");
        assert_eq!(suite.cases[0].difficulty, 1);
        assert_eq!(suite.cases[1].criteria.must_contain, vec!["56"]);
        assert_eq!(suite.cases[1].criteria.max_turns, 1);
    }

    #[test]
    fn validate_catches_empty_suite() {
        let suite = EvalSuite {
            name: "empty".into(),
            description: "".into(),
            version: "1.0".into(),
            tags: vec![],
            cases: vec![],
        };
        let issues = validate_suite(&suite);
        assert!(issues.iter().any(|i| i.contains("no test cases")));
    }

    #[test]
    fn validate_catches_duplicate_ids() {
        let suite = EvalSuite {
            name: "dups".into(),
            description: "".into(),
            version: "1.0".into(),
            tags: vec![],
            cases: vec![
                EvalCase {
                    id: "same".into(),
                    prompt: "q1".into(),
                    system_prompt: None,
                    criteria: EvalCriteria {
                        must_contain: vec!["a".into()],
                        ..default_criteria()
                    },
                    difficulty: 1,
                    tags: vec![],
                    timeout_secs: None,
                },
                EvalCase {
                    id: "same".into(),
                    prompt: "q2".into(),
                    system_prompt: None,
                    criteria: EvalCriteria {
                        must_contain: vec!["b".into()],
                        ..default_criteria()
                    },
                    difficulty: 1,
                    tags: vec![],
                    timeout_secs: None,
                },
            ],
        };
        let issues = validate_suite(&suite);
        assert!(issues.iter().any(|i| i.contains("Duplicate")));
    }

    #[test]
    fn validate_catches_no_criteria() {
        let suite = EvalSuite {
            name: "no_criteria".into(),
            description: "".into(),
            version: "1.0".into(),
            tags: vec![],
            cases: vec![EvalCase {
                id: "empty_criteria".into(),
                prompt: "test".into(),
                system_prompt: None,
                criteria: default_criteria(),
                difficulty: 1,
                tags: vec![],
                timeout_secs: None,
            }],
        };
        let issues = validate_suite(&suite);
        assert!(issues.iter().any(|i| i.contains("no evaluation criteria")));
    }

    #[test]
    fn validate_passes_valid_suite() {
        let yaml = r#"
name: valid_suite
cases:
  - id: test1
    prompt: "What is 1+1?"
    criteria:
      must_contain: ["2"]
"#;
        let suite = parse_suite(yaml).unwrap();
        let issues = validate_suite(&suite);
        assert!(issues.is_empty(), "Expected no issues but got: {:?}", issues);
    }

    #[test]
    fn build_report_aggregates_correctly() {
        let suite = EvalSuite {
            name: "test_suite".into(),
            description: "".into(),
            version: "1.0".into(),
            tags: vec![],
            cases: vec![
                EvalCase {
                    id: "a".into(),
                    prompt: "q1".into(),
                    system_prompt: None,
                    criteria: default_criteria(),
                    difficulty: 1,
                    tags: vec!["math".into()],
                    timeout_secs: None,
                },
                EvalCase {
                    id: "b".into(),
                    prompt: "q2".into(),
                    system_prompt: None,
                    criteria: default_criteria(),
                    difficulty: 2,
                    tags: vec!["math".into()],
                    timeout_secs: None,
                },
                EvalCase {
                    id: "c".into(),
                    prompt: "q3".into(),
                    system_prompt: None,
                    criteria: default_criteria(),
                    difficulty: 1,
                    tags: vec!["logic".into()],
                    timeout_secs: None,
                },
            ],
        };

        let results = vec![
            EvalResult {
                case_id: "a".into(),
                passed: true,
                score: 1.0,
                response: "r1".into(),
                duration_ms: 100,
                input_tokens: 50,
                output_tokens: 30,
                turns_used: 1,
                tool_calls: 0,
                checks: vec![],
                error: None,
            },
            EvalResult {
                case_id: "b".into(),
                passed: false,
                score: 0.5,
                response: "r2".into(),
                duration_ms: 200,
                input_tokens: 60,
                output_tokens: 40,
                turns_used: 2,
                tool_calls: 1,
                checks: vec![],
                error: None,
            },
            EvalResult {
                case_id: "c".into(),
                passed: true,
                score: 1.0,
                response: "r3".into(),
                duration_ms: 150,
                input_tokens: 55,
                output_tokens: 35,
                turns_used: 1,
                tool_calls: 0,
                checks: vec![],
                error: None,
            },
        ];

        let report = build_report(
            &suite,
            "test-model",
            "2025-01-01T00:00:00Z",
            "2025-01-01T00:01:00Z",
            450,
            results,
        );

        assert_eq!(report.passed, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errored, 0);
        assert!((report.pass_rate - 2.0 / 3.0).abs() < 0.01);
        assert!((report.avg_score - (1.0 + 0.5 + 1.0) / 3.0).abs() < 0.01);
        assert_eq!(report.total_input_tokens, 165);
        assert_eq!(report.total_output_tokens, 105);

        // Tag results
        let math = &report.tag_results["math"];
        assert_eq!(math.total, 2);
        assert_eq!(math.passed, 1);

        let logic = &report.tag_results["logic"];
        assert_eq!(logic.total, 1);
        assert_eq!(logic.passed, 1);

        // Difficulty results
        let d1 = &report.difficulty_results[&1];
        assert_eq!(d1.total, 2);
        assert_eq!(d1.passed, 2);

        let d2 = &report.difficulty_results[&2];
        assert_eq!(d2.total, 1);
        assert_eq!(d2.passed, 0);
    }

    #[test]
    fn report_serializes_to_json() {
        let report = EvalReport {
            suite_name: "test".into(),
            suite_version: "1.0".into(),
            model: "gpt-4".into(),
            started_at: "2025-01-01T00:00:00Z".into(),
            completed_at: "2025-01-01T00:01:00Z".into(),
            total_duration_ms: 1000,
            passed: 5,
            failed: 1,
            errored: 0,
            pass_rate: 0.833,
            avg_score: 0.9,
            total_input_tokens: 500,
            total_output_tokens: 300,
            results: vec![],
            tag_results: HashMap::new(),
            difficulty_results: HashMap::new(),
        };

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["suite_name"], "test");
        assert_eq!(json["passed"], 5);
        assert_eq!(json["failed"], 1);
    }

    fn default_criteria() -> EvalCriteria {
        EvalCriteria {
            must_contain: vec![],
            must_not_contain: vec![],
            exact_match: None,
            regex_match: None,
            min_tool_calls: 0,
            max_tool_calls: 0,
            max_turns: 0,
            scorer: None,
        }
    }
}
