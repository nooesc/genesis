//! Adaptive model routing — cost-aware task classification.
//!
//! Routes simple tasks to cheap/fast models and complex tasks to capable
//! models. Phase 1 uses rule-based heuristics; Phase 2 (future) will use
//! a trained classifier.
//!
//! **37-85% cost reduction** while retaining 95%+ accuracy per RouteLLM
//! (ICLR 2025) and AdaptEvolve.

use genesis_config::RoutingConfig;
use tracing::{debug, info};

/// Task complexity tier — determines which model to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskComplexity {
    /// Simple tasks: greetings, short questions, simple lookups.
    Simple,
    /// Moderate tasks: code edits, file operations, multi-step work.
    Moderate,
    /// Complex tasks: architecture, multi-file refactors, deep reasoning.
    Complex,
}

impl std::fmt::Display for TaskComplexity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskComplexity::Simple => write!(f, "simple"),
            TaskComplexity::Moderate => write!(f, "moderate"),
            TaskComplexity::Complex => write!(f, "complex"),
        }
    }
}

/// Routes tasks to appropriate model tiers based on complexity.
pub struct ModelRouter {
    config: RoutingConfig,
    /// Current escalation level. When a tool call fails, we escalate
    /// to a higher tier for the remainder of the turn.
    current_escalation: Option<TaskComplexity>,
    /// Last classified complexity (before escalation).
    last_classification: Option<TaskComplexity>,
}

impl ModelRouter {
    pub fn new(config: RoutingConfig) -> Self {
        Self {
            config,
            current_escalation: None,
            last_classification: None,
        }
    }

    /// Select the model to use for the next LLM request.
    ///
    /// If escalation is active (previous tool call failed), uses the
    /// escalated tier. Otherwise classifies the task and selects.
    pub fn select_model(&mut self, user_message: &str, message_count: usize) -> (&str, TaskComplexity) {
        if !self.config.enabled {
            return (self.default_model(), self.default_complexity());
        }

        // If escalated, use the escalated tier.
        if let Some(escalated) = self.current_escalation {
            let model = self.model_for_complexity(escalated);
            debug!(
                complexity = %escalated,
                model,
                "using escalated model tier"
            );
            return (model, escalated);
        }

        let complexity = classify_task(user_message, message_count);
        self.last_classification = Some(complexity);
        let model = self.model_for_complexity(complexity);

        info!(
            complexity = %complexity,
            model,
            message_len = user_message.len(),
            "routed task to model tier"
        );

        (model, complexity)
    }

    /// Escalate to the next higher tier after a failure.
    pub fn escalate(&mut self) {
        let current = self
            .current_escalation
            .or(self.last_classification)
            .unwrap_or(self.default_complexity());
        let next = match current {
            TaskComplexity::Simple => TaskComplexity::Moderate,
            TaskComplexity::Moderate => TaskComplexity::Complex,
            TaskComplexity::Complex => TaskComplexity::Complex,
        };
        info!(
            from = %self.current_escalation.map(|c| c.to_string()).unwrap_or_else(|| "none".to_owned()),
            to = %next,
            "escalating model tier after failure"
        );
        self.current_escalation = Some(next);
    }

    /// Reset escalation (e.g., at the start of a new user turn).
    pub fn reset_escalation(&mut self) {
        self.current_escalation = None;
        self.last_classification = None;
    }

    fn model_for_complexity(&self, complexity: TaskComplexity) -> &str {
        match complexity {
            TaskComplexity::Simple => &self.config.cheap,
            TaskComplexity::Moderate => &self.config.mid,
            TaskComplexity::Complex => &self.config.top,
        }
    }

    fn default_model(&self) -> &str {
        match self.config.default_tier.as_str() {
            "cheap" | "simple" => &self.config.cheap,
            "top" | "complex" => &self.config.top,
            _ => &self.config.mid,
        }
    }

    fn default_complexity(&self) -> TaskComplexity {
        match self.config.default_tier.as_str() {
            "cheap" | "simple" => TaskComplexity::Simple,
            "top" | "complex" => TaskComplexity::Complex,
            _ => TaskComplexity::Moderate,
        }
    }
}

/// Heuristic task classifier.
///
/// Analyzes the user message to determine complexity tier based on:
/// - Message length
/// - Presence of code-related keywords
/// - Complexity signals (architecture, refactor, etc.)
/// - Conversation depth (message count)
pub fn classify_task(message: &str, message_count: usize) -> TaskComplexity {
    let msg = message.to_lowercase();
    let len = message.len();

    // Complex signals — multi-step, architecture, reasoning
    let complex_keywords = [
        "refactor",
        "architect",
        "redesign",
        "migrate",
        "implement",
        "build a",
        "create a",
        "debug",
        "investigate",
        "analyze",
        "optimize",
        "performance",
        "security",
        "review",
        "compare",
        "explain in detail",
        "step by step",
        "multiple files",
        "across the codebase",
        "comprehensive",
    ];

    // Code editing signals
    let code_keywords = [
        "function",
        "class",
        "method",
        "variable",
        "import",
        "module",
        "error",
        "bug",
        "fix",
        "test",
        "add",
        "change",
        "modify",
        "update",
        "delete",
        "remove",
        "file",
        "code",
        "api",
        "endpoint",
    ];

    // Simple signals — greetings, short questions
    let simple_keywords = [
        "hello",
        "hi",
        "hey",
        "thanks",
        "thank you",
        "what is",
        "what's",
        "how are",
        "yes",
        "no",
        "ok",
        "okay",
        "sure",
        "got it",
    ];

    let complex_score: usize = complex_keywords
        .iter()
        .filter(|kw| msg.contains(*kw))
        .count();
    let code_score: usize = code_keywords
        .iter()
        .filter(|kw| msg.contains(*kw))
        .count();
    let simple_score: usize = simple_keywords
        .iter()
        .filter(|kw| msg.contains(*kw))
        .count();

    // Long messages with multiple complex keywords → Complex
    if complex_score >= 2 || (len > 500 && complex_score >= 1) {
        return TaskComplexity::Complex;
    }

    // Deep conversations (many messages) tend to be complex
    if message_count > 10 && code_score >= 1 {
        return TaskComplexity::Complex;
    }

    // Code-related work → Moderate
    if code_score >= 2 || (len > 200 && code_score >= 1) {
        return TaskComplexity::Moderate;
    }

    // Short simple messages → Simple
    if len < 50 && simple_score >= 1 {
        return TaskComplexity::Simple;
    }

    // Short messages without complex signals → Simple
    if len < 100 && complex_score == 0 && code_score == 0 {
        return TaskComplexity::Simple;
    }

    // Default → Moderate
    TaskComplexity::Moderate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> RoutingConfig {
        RoutingConfig {
            enabled: true,
            cheap: "gpt-4.1-mini".to_owned(),
            mid: "gpt-4.1".to_owned(),
            top: "o3".to_owned(),
            default_tier: "mid".to_owned(),
        }
    }

    #[test]
    fn simple_greeting_routes_to_cheap() {
        let complexity = classify_task("Hello, how are you?", 0);
        assert_eq!(complexity, TaskComplexity::Simple);
    }

    #[test]
    fn short_question_routes_to_simple() {
        let complexity = classify_task("What time is it?", 0);
        assert_eq!(complexity, TaskComplexity::Simple);
    }

    #[test]
    fn code_edit_routes_to_moderate() {
        let complexity = classify_task(
            "Add a new function to handle user authentication",
            2,
        );
        assert_eq!(complexity, TaskComplexity::Moderate);
    }

    #[test]
    fn complex_refactor_routes_to_complex() {
        let complexity = classify_task(
            "Refactor the authentication module to use a new architecture pattern with better security",
            5,
        );
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn long_detailed_request_routes_to_complex() {
        let long_msg = "I need you to investigate the performance issue in our API. \
            Look at multiple files across the codebase, analyze the database queries, \
            and optimize the slow endpoints. Please provide a comprehensive review.";
        let complexity = classify_task(long_msg, 3);
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn model_router_selects_correct_tier() {
        let mut router = ModelRouter::new(test_config());
        let (model, _) = router.select_model("Hello!", 0);
        assert_eq!(model, "gpt-4.1-mini");

        let (model, _) = router.select_model("Fix the bug in the test function", 2);
        assert_eq!(model, "gpt-4.1");

        let (model, _) = router.select_model(
            "Refactor the entire authentication architecture for better security",
            5,
        );
        assert_eq!(model, "o3");
    }

    #[test]
    fn escalation_upgrades_tier() {
        let mut router = ModelRouter::new(test_config());

        // Start with simple task.
        let (model, _) = router.select_model("Hello!", 0);
        assert_eq!(model, "gpt-4.1-mini");

        // Escalate on failure.
        router.escalate();
        let (model, complexity) = router.select_model("Hello!", 0);
        assert_eq!(model, "gpt-4.1"); // Upgraded from cheap
        assert_eq!(complexity, TaskComplexity::Moderate);

        // Escalate again.
        router.escalate();
        let (model, complexity) = router.select_model("Hello!", 0);
        assert_eq!(model, "o3"); // Upgraded to top
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn reset_clears_escalation() {
        let mut router = ModelRouter::new(test_config());
        router.escalate();
        router.escalate();
        let (model, _) = router.select_model("Hello!", 0);
        assert_eq!(model, "o3");

        router.reset_escalation();
        let (model, _) = router.select_model("Hello!", 0);
        assert_eq!(model, "gpt-4.1-mini"); // Back to cheap for simple task
    }

    #[test]
    fn disabled_routing_uses_default_tier() {
        let mut config = test_config();
        config.enabled = false;
        let mut router = ModelRouter::new(config);
        let (model, _) = router.select_model(
            "Refactor the entire architecture",
            10,
        );
        assert_eq!(model, "gpt-4.1"); // Uses default (mid), ignoring classification
    }

    #[test]
    fn deep_conversation_escalates_complexity() {
        // Same message, but in a deep conversation
        let complexity = classify_task("Fix the error in the code", 15);
        assert_eq!(complexity, TaskComplexity::Complex);

        let complexity = classify_task("Fix the error in the code", 2);
        assert_eq!(complexity, TaskComplexity::Moderate);
    }
}
