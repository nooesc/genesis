//! Per-model pricing data for cost estimation.
//!
//! Prices are stored as USD per million tokens. When an exact model match
//! isn't found, prefix matching is used (e.g., "gpt-4.1" matches
//! "gpt-4.1-mini" and "gpt-4.1-nano").

/// Pricing for a single model (USD per million tokens).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

/// Estimated cost for a request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostEstimate {
    pub input_cost: f64,
    pub output_cost: f64,
    pub total_cost: f64,
}

impl std::fmt::Display for CostEstimate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.total_cost < 0.01 {
            write!(f, "${:.4}", self.total_cost)
        } else {
            write!(f, "${:.2}", self.total_cost)
        }
    }
}

/// Known model pricing table (USD per million tokens).
/// Prices as of early 2026. Models not in the table return None.
const PRICING: &[(&str, ModelPricing)] = &[
    // OpenAI GPT-4.1 family
    (
        "gpt-4.1",
        ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        },
    ),
    (
        "gpt-4.1-mini",
        ModelPricing {
            input_per_million: 0.40,
            output_per_million: 1.60,
        },
    ),
    (
        "gpt-4.1-nano",
        ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.40,
        },
    ),
    // OpenAI GPT-4o family
    (
        "gpt-4o",
        ModelPricing {
            input_per_million: 2.50,
            output_per_million: 10.0,
        },
    ),
    (
        "gpt-4o-mini",
        ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
        },
    ),
    // OpenAI o-series reasoning
    (
        "o3",
        ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        },
    ),
    (
        "o3-mini",
        ModelPricing {
            input_per_million: 1.10,
            output_per_million: 4.40,
        },
    ),
    (
        "o4-mini",
        ModelPricing {
            input_per_million: 1.10,
            output_per_million: 4.40,
        },
    ),
    // Anthropic Claude
    (
        "claude-opus-4",
        ModelPricing {
            input_per_million: 15.0,
            output_per_million: 75.0,
        },
    ),
    (
        "claude-sonnet-4",
        ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
    ),
    (
        "claude-haiku-4",
        ModelPricing {
            input_per_million: 0.80,
            output_per_million: 4.0,
        },
    ),
    (
        "claude-3.5-sonnet",
        ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
    ),
    (
        "claude-3.5-haiku",
        ModelPricing {
            input_per_million: 0.80,
            output_per_million: 4.0,
        },
    ),
    // Google Gemini
    (
        "gemini-2.5-pro",
        ModelPricing {
            input_per_million: 1.25,
            output_per_million: 10.0,
        },
    ),
    (
        "gemini-2.5-flash",
        ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
        },
    ),
    (
        "gemini-2.0-flash",
        ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.40,
        },
    ),
    // DeepSeek
    (
        "deepseek-chat",
        ModelPricing {
            input_per_million: 0.27,
            output_per_million: 1.10,
        },
    ),
    (
        "deepseek-reasoner",
        ModelPricing {
            input_per_million: 0.55,
            output_per_million: 2.19,
        },
    ),
    (
        "deepseek-r1",
        ModelPricing {
            input_per_million: 0.55,
            output_per_million: 2.19,
        },
    ),
    (
        "deepseek-v3",
        ModelPricing {
            input_per_million: 0.27,
            output_per_million: 1.10,
        },
    ),
    // Mistral
    (
        "mistral-large",
        ModelPricing {
            input_per_million: 2.0,
            output_per_million: 6.0,
        },
    ),
    (
        "mistral-small",
        ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.30,
        },
    ),
    (
        "codestral",
        ModelPricing {
            input_per_million: 0.30,
            output_per_million: 0.90,
        },
    ),
    // Meta Llama (via API providers)
    (
        "llama-4-maverick",
        ModelPricing {
            input_per_million: 0.20,
            output_per_million: 0.60,
        },
    ),
    (
        "llama-4-scout",
        ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.40,
        },
    ),
    (
        "llama-3.3-70b",
        ModelPricing {
            input_per_million: 0.18,
            output_per_million: 0.36,
        },
    ),
    // Qwen (via API providers)
    (
        "qwen-2.5-72b",
        ModelPricing {
            input_per_million: 0.35,
            output_per_million: 0.70,
        },
    ),
    (
        "qwen-2.5-coder-32b",
        ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.30,
        },
    ),
    (
        "qwen-3-235b",
        ModelPricing {
            input_per_million: 0.50,
            output_per_million: 1.00,
        },
    ),
    (
        "qwen-3-30b",
        ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.30,
        },
    ),
    // xAI Grok
    (
        "grok-3",
        ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
    ),
    (
        "grok-3-mini",
        ModelPricing {
            input_per_million: 0.30,
            output_per_million: 0.50,
        },
    ),
];

/// Look up pricing for a model by name.
///
/// Tries exact match first, then prefix match (longest prefix wins).
/// Strips common provider prefixes like `openai/`, `anthropic/`.
pub fn lookup_pricing(model: &str) -> Option<ModelPricing> {
    // Strip provider prefix (e.g., "openai/gpt-4.1-mini" -> "gpt-4.1-mini")
    let model_name = model.split('/').next_back().unwrap_or(model);

    // Exact match
    if let Some(entry) = PRICING.iter().find(|(name, _)| *name == model_name) {
        return Some(entry.1);
    }

    // Prefix match (longest prefix wins)
    PRICING
        .iter()
        .filter(|(name, _)| model_name.starts_with(name))
        .max_by_key(|(name, _)| name.len())
        .map(|(_, pricing)| *pricing)
}

/// Estimate cost for a given number of input/output tokens.
pub fn estimate_cost(model: &str, input_tokens: u32, output_tokens: u32) -> Option<CostEstimate> {
    let pricing = lookup_pricing(model)?;
    let input_cost = (input_tokens as f64 / 1_000_000.0) * pricing.input_per_million;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * pricing.output_per_million;
    Some(CostEstimate {
        input_cost,
        output_cost,
        total_cost: input_cost + output_cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_model_match() {
        let pricing = lookup_pricing("gpt-4.1-mini").unwrap();
        assert_eq!(pricing.input_per_million, 0.40);
        assert_eq!(pricing.output_per_million, 1.60);
    }

    #[test]
    fn prefix_match_for_versioned_model() {
        // "gpt-4.1-mini-2025-04-14" should match "gpt-4.1-mini"
        let pricing = lookup_pricing("gpt-4.1-mini-2025-04-14").unwrap();
        assert_eq!(pricing.input_per_million, 0.40);
    }

    #[test]
    fn strips_provider_prefix() {
        let pricing = lookup_pricing("openai/gpt-4.1-mini").unwrap();
        assert_eq!(pricing.input_per_million, 0.40);
    }

    #[test]
    fn anthropic_prefix_stripped() {
        let pricing = lookup_pricing("anthropic/claude-sonnet-4").unwrap();
        assert_eq!(pricing.input_per_million, 3.0);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(lookup_pricing("unknown-model-xyz").is_none());
    }

    #[test]
    fn estimate_cost_basic() {
        let cost = estimate_cost("gpt-4.1-mini", 1_000_000, 500_000).unwrap();
        assert!((cost.input_cost - 0.40).abs() < 1e-10);
        assert!((cost.output_cost - 0.80).abs() < 1e-10);
        assert!((cost.total_cost - 1.20).abs() < 1e-10);
    }

    #[test]
    fn estimate_cost_small_usage() {
        let cost = estimate_cost("gpt-4.1-mini", 100, 50).unwrap();
        assert!(cost.total_cost < 0.001);
    }

    #[test]
    fn estimate_cost_unknown_model() {
        assert!(estimate_cost("unknown-model", 100, 50).is_none());
    }

    #[test]
    fn cost_estimate_display_small() {
        let cost = CostEstimate {
            input_cost: 0.001,
            output_cost: 0.002,
            total_cost: 0.003,
        };
        assert_eq!(format!("{cost}"), "$0.0030");
    }

    #[test]
    fn cost_estimate_display_large() {
        let cost = CostEstimate {
            input_cost: 0.50,
            output_cost: 0.75,
            total_cost: 1.25,
        };
        assert_eq!(format!("{cost}"), "$1.25");
    }

    #[test]
    fn claude_opus_pricing() {
        let pricing = lookup_pricing("claude-opus-4").unwrap();
        assert_eq!(pricing.input_per_million, 15.0);
        assert_eq!(pricing.output_per_million, 75.0);
    }

    #[test]
    fn gemini_pricing() {
        let pricing = lookup_pricing("gemini-2.5-flash").unwrap();
        assert_eq!(pricing.input_per_million, 0.15);
    }

    #[test]
    fn longest_prefix_wins() {
        // "gpt-4.1-mini" should match "gpt-4.1-mini" not "gpt-4.1"
        let pricing = lookup_pricing("gpt-4.1-mini").unwrap();
        assert_eq!(pricing.input_per_million, 0.40); // mini pricing, not base
    }

    #[test]
    fn deepseek_pricing() {
        let pricing = lookup_pricing("deepseek-chat").unwrap();
        assert_eq!(pricing.input_per_million, 0.27);
    }

    #[test]
    fn mistral_pricing() {
        let pricing = lookup_pricing("mistral-large-latest").unwrap();
        assert_eq!(pricing.input_per_million, 2.0);
    }

    #[test]
    fn llama_pricing() {
        let pricing = lookup_pricing("llama-4-maverick").unwrap();
        assert_eq!(pricing.input_per_million, 0.20);
    }

    #[test]
    fn grok_pricing() {
        let pricing = lookup_pricing("grok-3-mini").unwrap();
        assert_eq!(pricing.input_per_million, 0.30);
    }

    #[test]
    fn openrouter_prefix_stripped() {
        let pricing = lookup_pricing("deepseek/deepseek-chat").unwrap();
        assert_eq!(pricing.input_per_million, 0.27);
    }

    #[test]
    fn lookup_returns_pricing_for_exact_model_match() {
        // Verify several exact-match models return their known pricing
        let cases: &[(&str, f64, f64)] = &[
            ("gpt-4.1", 2.0, 8.0),
            ("gpt-4.1-nano", 0.10, 0.40),
            ("gpt-4o", 2.50, 10.0),
            ("gpt-4o-mini", 0.15, 0.60),
            ("o3", 2.0, 8.0),
            ("o3-mini", 1.10, 4.40),
            ("o4-mini", 1.10, 4.40),
            ("claude-opus-4", 15.0, 75.0),
            ("claude-sonnet-4", 3.0, 15.0),
            ("claude-haiku-4", 0.80, 4.0),
            ("claude-3.5-sonnet", 3.0, 15.0),
            ("claude-3.5-haiku", 0.80, 4.0),
            ("gemini-2.5-pro", 1.25, 10.0),
            ("gemini-2.0-flash", 0.10, 0.40),
            ("deepseek-reasoner", 0.55, 2.19),
            ("deepseek-r1", 0.55, 2.19),
            ("deepseek-v3", 0.27, 1.10),
            ("codestral", 0.30, 0.90),
            ("mistral-small", 0.10, 0.30),
            ("llama-4-scout", 0.15, 0.40),
            ("llama-3.3-70b", 0.18, 0.36),
            ("qwen-2.5-72b", 0.35, 0.70),
            ("qwen-2.5-coder-32b", 0.15, 0.30),
            ("qwen-3-235b", 0.50, 1.00),
            ("qwen-3-30b", 0.15, 0.30),
            ("grok-3", 3.0, 15.0),
        ];
        for &(model, expected_input, expected_output) in cases {
            let pricing =
                lookup_pricing(model).unwrap_or_else(|| panic!("expected pricing for {model}"));
            assert_eq!(
                pricing.input_per_million, expected_input,
                "input pricing mismatch for {model}"
            );
            assert_eq!(
                pricing.output_per_million, expected_output,
                "output pricing mismatch for {model}"
            );
        }
    }

    #[test]
    fn prefix_matching_returns_pricing_for_versioned_variants() {
        // Model names with date suffixes or extra qualifiers should
        // match the longest matching prefix in the pricing table.
        let cases: &[(&str, f64)] = &[
            ("gpt-4.1-2025-04-14", 2.0),         // matches "gpt-4.1"
            ("gpt-4o-2025-03-01", 2.50),         // matches "gpt-4o"
            ("claude-opus-4-20260301", 15.0),    // matches "claude-opus-4"
            ("gemini-2.5-pro-preview", 1.25),    // matches "gemini-2.5-pro"
            ("deepseek-chat-0324", 0.27),        // matches "deepseek-chat"
            ("grok-3-mini-fast", 0.30),          // matches "grok-3-mini"
            ("llama-4-maverick-instruct", 0.20), // matches "llama-4-maverick"
        ];
        for &(model, expected_input) in cases {
            let pricing = lookup_pricing(model)
                .unwrap_or_else(|| panic!("expected prefix match for {model}"));
            assert_eq!(
                pricing.input_per_million, expected_input,
                "prefix match mismatch for {model}"
            );
        }
    }

    #[test]
    fn unknown_model_returns_none_for_various_inputs() {
        let unknown_models = &[
            "totally-unknown-model",
            "gpt-5",
            "claude-5-opus",
            "",
            "openai/nonexistent-model",
            "some-provider/some-model",
        ];
        for model in unknown_models {
            assert!(
                lookup_pricing(model).is_none(),
                "expected None for unknown model {model:?}"
            );
        }
    }

    #[test]
    fn estimate_cost_returns_correct_values_for_known_tokens() {
        // 1M input tokens + 1M output tokens at claude-opus-4 pricing
        let cost = estimate_cost("claude-opus-4", 1_000_000, 1_000_000).unwrap();
        assert!((cost.input_cost - 15.0).abs() < 1e-10);
        assert!((cost.output_cost - 75.0).abs() < 1e-10);
        assert!((cost.total_cost - 90.0).abs() < 1e-10);
    }

    #[test]
    fn estimate_cost_with_zero_tokens_returns_zero() {
        let cost = estimate_cost("gpt-4.1-mini", 0, 0).unwrap();
        assert_eq!(cost.input_cost, 0.0);
        assert_eq!(cost.output_cost, 0.0);
        assert_eq!(cost.total_cost, 0.0);
    }

    #[test]
    fn estimate_cost_with_zero_input_tokens_only_charges_output() {
        let cost = estimate_cost("gpt-4.1-mini", 0, 1_000_000).unwrap();
        assert_eq!(cost.input_cost, 0.0);
        assert!((cost.output_cost - 1.60).abs() < 1e-10);
        assert!((cost.total_cost - 1.60).abs() < 1e-10);
    }

    #[test]
    fn estimate_cost_with_zero_output_tokens_only_charges_input() {
        let cost = estimate_cost("gpt-4.1-mini", 1_000_000, 0).unwrap();
        assert!((cost.input_cost - 0.40).abs() < 1e-10);
        assert_eq!(cost.output_cost, 0.0);
        assert!((cost.total_cost - 0.40).abs() < 1e-10);
    }

    #[test]
    fn all_models_in_table_have_positive_pricing() {
        for &(name, pricing) in PRICING {
            assert!(
                pricing.input_per_million > 0.0,
                "model {name} has non-positive input pricing: {}",
                pricing.input_per_million
            );
            assert!(
                pricing.output_per_million > 0.0,
                "model {name} has non-positive output pricing: {}",
                pricing.output_per_million
            );
        }
    }

    #[test]
    fn prefix_match_prefers_longest_prefix() {
        // "gpt-4.1-mini" is both prefixed by "gpt-4.1" and exact-matched
        // by "gpt-4.1-mini". The longer/exact match should win.
        let pricing = lookup_pricing("gpt-4.1-mini-something").unwrap();
        // Should match "gpt-4.1-mini" (len 12) not "gpt-4.1" (len 7)
        assert_eq!(pricing.input_per_million, 0.40);
        assert_eq!(pricing.output_per_million, 1.60);
    }

    #[test]
    fn provider_prefix_with_multiple_slashes() {
        // "some/nested/gpt-4.1-mini" — only last segment matters
        let pricing = lookup_pricing("provider/sub/gpt-4.1-mini").unwrap();
        assert_eq!(pricing.input_per_million, 0.40);
    }

    #[test]
    fn cost_estimate_display_boundary_at_one_cent() {
        // Exactly 0.01 should use 2-decimal format
        let cost = CostEstimate {
            input_cost: 0.005,
            output_cost: 0.005,
            total_cost: 0.01,
        };
        assert_eq!(format!("{cost}"), "$0.01");

        // Just below 0.01 should use 4-decimal format
        let cost = CostEstimate {
            input_cost: 0.004,
            output_cost: 0.005,
            total_cost: 0.009,
        };
        assert_eq!(format!("{cost}"), "$0.0090");
    }
}
