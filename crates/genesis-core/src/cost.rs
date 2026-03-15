//! Per-session cost tracking and budget enforcement.
//!
//! Tracks cumulative token usage and estimated cost across agent turns,
//! with optional budget limits that halt execution when exceeded.

use genesis_provider::pricing::{estimate_cost, CostEstimate};

/// Accumulated cost for a session.
#[derive(Debug, Clone, Default)]
pub struct SessionCost {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost: f64,
    /// Per-turn breakdown.
    pub turns: Vec<TurnCost>,
    /// Optional budget limit in USD. None = unlimited.
    pub budget_limit: Option<f64>,
}

/// Cost for a single LLM turn.
#[derive(Debug, Clone)]
pub struct TurnCost {
    pub turn_number: usize,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub estimated_cost: Option<CostEstimate>,
}

/// Budget enforcement result.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetStatus {
    /// Under budget, safe to continue.
    Ok,
    /// Approaching budget limit (>80% used).
    Warning { used: f64, limit: f64 },
    /// Budget exceeded, should stop.
    Exceeded { used: f64, limit: f64 },
}

impl SessionCost {
    pub fn new(budget_limit: Option<f64>) -> Self {
        Self {
            budget_limit,
            ..Default::default()
        }
    }

    /// Record a turn's token usage and update running totals.
    pub fn record_turn(
        &mut self,
        model: &str,
        turn_number: usize,
        input_tokens: u32,
        output_tokens: u32,
    ) {
        let estimated = estimate_cost(model, input_tokens, output_tokens);

        self.total_input_tokens += input_tokens as u64;
        self.total_output_tokens += output_tokens as u64;

        if let Some(ref est) = estimated {
            self.total_cost += est.total_cost;
        }

        self.turns.push(TurnCost {
            turn_number,
            input_tokens,
            output_tokens,
            estimated_cost: estimated,
        });
    }

    /// Check current budget status.
    pub fn check_budget(&self) -> BudgetStatus {
        match self.budget_limit {
            None => BudgetStatus::Ok,
            Some(limit) if limit <= 0.0 => BudgetStatus::Ok,
            Some(limit) => {
                if self.total_cost >= limit {
                    BudgetStatus::Exceeded {
                        used: self.total_cost,
                        limit,
                    }
                } else if self.total_cost >= limit * 0.8 {
                    BudgetStatus::Warning {
                        used: self.total_cost,
                        limit,
                    }
                } else {
                    BudgetStatus::Ok
                }
            }
        }
    }

    /// Format a cost summary for display.
    pub fn summary(&self, model: &str) -> String {
        let mut lines = vec![format!(
            "Tokens: {} input, {} output ({} total)",
            self.total_input_tokens,
            self.total_output_tokens,
            self.total_input_tokens + self.total_output_tokens
        )];

        if self.total_cost > 0.0 {
            if self.total_cost < 0.01 {
                lines.push(format!("Cost: ${:.4} ({})", self.total_cost, model));
            } else {
                lines.push(format!("Cost: ${:.2} ({})", self.total_cost, model));
            }
        } else {
            lines.push(format!("Cost: unknown pricing for {model}"));
        }

        if let Some(limit) = self.budget_limit {
            let pct = if limit > 0.0 {
                (self.total_cost / limit * 100.0).min(100.0)
            } else {
                0.0
            };
            lines.push(format!(
                "Budget: ${:.2} / ${:.2} ({:.0}%)",
                self.total_cost, limit, pct
            ));
        }

        lines.push(format!("Turns: {}", self.turns.len()));

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_turn_accumulates_tokens() {
        let mut cost = SessionCost::new(None);
        cost.record_turn("gpt-4.1-mini", 1, 100, 50);
        cost.record_turn("gpt-4.1-mini", 2, 200, 100);

        assert_eq!(cost.total_input_tokens, 300);
        assert_eq!(cost.total_output_tokens, 150);
        assert_eq!(cost.turns.len(), 2);
    }

    #[test]
    fn record_turn_estimates_cost_for_known_model() {
        let mut cost = SessionCost::new(None);
        cost.record_turn("gpt-4.1-mini", 1, 1_000_000, 500_000);

        // gpt-4.1-mini: $0.40/M input, $1.60/M output
        // Expected: $0.40 + $0.80 = $1.20
        assert!((cost.total_cost - 1.20).abs() < 1e-6);
    }

    #[test]
    fn record_turn_handles_unknown_model() {
        let mut cost = SessionCost::new(None);
        cost.record_turn("unknown-model", 1, 100, 50);

        assert_eq!(cost.total_input_tokens, 100);
        assert_eq!(cost.total_output_tokens, 50);
        assert_eq!(cost.total_cost, 0.0);
        assert!(cost.turns[0].estimated_cost.is_none());
    }

    #[test]
    fn check_budget_ok_when_under_limit() {
        let mut cost = SessionCost::new(Some(1.0));
        cost.record_turn("gpt-4.1-mini", 1, 100, 50);

        assert_eq!(cost.check_budget(), BudgetStatus::Ok);
    }

    #[test]
    fn check_budget_warning_at_80_percent() {
        let mut cost = SessionCost::new(Some(1.0));
        // $0.40/M input -> need 2M input tokens for $0.80
        cost.record_turn("gpt-4.1-mini", 1, 2_000_000, 0);

        match cost.check_budget() {
            BudgetStatus::Warning { used, limit } => {
                assert!((limit - 1.0).abs() < 1e-6);
                assert!(used >= 0.8);
            }
            other => panic!("expected Warning, got: {other:?}"),
        }
    }

    #[test]
    fn check_budget_exceeded_at_limit() {
        let mut cost = SessionCost::new(Some(0.001));
        cost.record_turn("gpt-4.1-mini", 1, 1_000_000, 500_000);

        match cost.check_budget() {
            BudgetStatus::Exceeded { used, limit } => {
                assert!(used > limit);
            }
            other => panic!("expected Exceeded, got: {other:?}"),
        }
    }

    #[test]
    fn check_budget_ok_when_unlimited() {
        let mut cost = SessionCost::new(None);
        cost.record_turn("gpt-4.1-mini", 1, 10_000_000, 5_000_000);

        assert_eq!(cost.check_budget(), BudgetStatus::Ok);
    }

    #[test]
    fn summary_includes_token_counts() {
        let mut cost = SessionCost::new(None);
        cost.record_turn("gpt-4.1-mini", 1, 100, 50);

        let summary = cost.summary("gpt-4.1-mini");
        assert!(summary.contains("100 input"));
        assert!(summary.contains("50 output"));
        assert!(summary.contains("Turns: 1"));
    }

    #[test]
    fn summary_includes_budget_when_set() {
        let mut cost = SessionCost::new(Some(5.0));
        cost.record_turn("gpt-4.1-mini", 1, 100, 50);

        let summary = cost.summary("gpt-4.1-mini");
        assert!(summary.contains("Budget:"));
        assert!(summary.contains("$5.00"));
    }

    #[test]
    fn summary_shows_unknown_for_unknown_model() {
        let cost = SessionCost::new(None);
        let summary = cost.summary("unknown-model");
        assert!(summary.contains("unknown pricing"));
    }
}
