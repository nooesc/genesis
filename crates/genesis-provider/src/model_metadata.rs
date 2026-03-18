//! Well-known model metadata for context limits and capability detection.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Per-model capability flags.
///
/// These flags allow the agent loop and provider client to auto-adapt
/// behavior based on what a model supports, without model-specific
/// if/else chains scattered across the codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    /// Whether the model supports tool/function calling.
    pub supports_tools: bool,
    /// Whether the model can process image inputs.
    pub supports_vision: bool,
    /// Whether the model supports extended thinking / chain-of-thought.
    pub supports_thinking: bool,
    /// Whether the model supports parallel tool calls in a single response.
    /// Most models do; disable for small models like gpt-4.1-nano.
    pub supports_parallel_tools: bool,
    /// Whether the model supports OpenAI-style strict function calling.
    pub supports_strict_mode: bool,
    /// Whether the model requires thought signatures to be echoed back
    /// (Gemini thinking models).
    pub requires_thought_signatures: bool,
    /// Whether the model supports Anthropic token-efficient tool use beta.
    pub supports_token_efficient_tools: bool,
    /// Whether the model supports OpenAI predicted outputs.
    pub supports_predicted_outputs: bool,
    /// Whether the model supports Anthropic citations API.
    pub supports_citations: bool,
    /// Whether the model supports explicit context/prompt caching
    /// (Anthropic cache_control, Gemini context caching).
    pub supports_context_caching: bool,
    /// Whether the model supports batch API for async bulk processing.
    pub supports_batch_api: bool,
}

impl Default for ModelCapabilities {
    /// Conservative defaults for unknown models: only basic tool calling enabled.
    fn default() -> Self {
        Self {
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
            supports_parallel_tools: true,
            supports_strict_mode: false,
            requires_thought_signatures: false,
            supports_token_efficient_tools: false,
            supports_predicted_outputs: false,
            supports_citations: false,
            supports_context_caching: false,
            supports_batch_api: false,
        }
    }
}

/// Metadata describing a model's capabilities and limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMetadata {
    pub model: &'static str,
    pub context_length: usize,
    pub max_output_tokens: Option<usize>,
    pub capabilities: ModelCapabilities,
}

impl ModelMetadata {
    /// Convenience: whether this model supports tool calling.
    pub fn supports_tools(&self) -> bool {
        self.capabilities.supports_tools
    }

    /// Convenience: whether this model supports vision/image inputs.
    pub fn supports_vision(&self) -> bool {
        self.capabilities.supports_vision
    }

    /// Convenience: whether this model supports extended thinking.
    pub fn supports_thinking(&self) -> bool {
        self.capabilities.supports_thinking
    }
}

/// Returns metadata for well-known models, keyed by model name.
pub fn known_metadata() -> HashMap<&'static str, ModelMetadata> {
    let cap = ModelCapabilities::default;

    let models = vec![
        // ── OpenAI ─────────────────────────────────────────────────────
        ModelMetadata {
            model: "gpt-4.1",
            context_length: 1_048_576,
            max_output_tokens: Some(32_768),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_strict_mode: true,
                supports_predicted_outputs: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "gpt-4.1-mini",
            context_length: 1_048_576,
            max_output_tokens: Some(32_768),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_strict_mode: true,
                supports_predicted_outputs: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "gpt-4.1-nano",
            context_length: 1_048_576,
            max_output_tokens: Some(32_768),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_strict_mode: true,
                supports_parallel_tools: false, // nano models: disable parallel
                supports_predicted_outputs: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "o4-mini",
            context_length: 200_000,
            max_output_tokens: Some(100_000),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                supports_parallel_tools: false, // reasoning models: disable parallel
                supports_strict_mode: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        // ── Anthropic ──────────────────────────────────────────────────
        ModelMetadata {
            model: "claude-sonnet-4-20250514",
            context_length: 200_000,
            max_output_tokens: Some(64_000),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                supports_token_efficient_tools: true,
                supports_citations: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "claude-opus-4-20250514",
            context_length: 200_000,
            max_output_tokens: Some(32_000),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                supports_token_efficient_tools: true,
                supports_citations: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "claude-opus-4-6",
            context_length: 200_000,
            max_output_tokens: Some(32_000),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                supports_token_efficient_tools: true,
                supports_citations: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "claude-sonnet-4-6",
            context_length: 200_000,
            max_output_tokens: Some(64_000),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                supports_token_efficient_tools: true,
                supports_citations: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "claude-haiku-4-5",
            context_length: 200_000,
            max_output_tokens: Some(8_192),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                supports_token_efficient_tools: true,
                supports_citations: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "claude-haiku-3.5-20241022",
            context_length: 200_000,
            max_output_tokens: Some(8_192),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_citations: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        // ── Google ─────────────────────────────────────────────────────
        ModelMetadata {
            model: "gemini-2.5-pro",
            context_length: 1_048_576,
            max_output_tokens: Some(65_536),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                requires_thought_signatures: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "gemini-2.5-flash",
            context_length: 1_048_576,
            max_output_tokens: Some(65_536),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                requires_thought_signatures: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "gemini-2.0-flash",
            context_length: 1_048_576,
            max_output_tokens: Some(8_192),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_context_caching: true,
                supports_batch_api: true,
                ..cap()
            },
        },
        // ── DeepSeek ───────────────────────────────────────────────────
        ModelMetadata {
            model: "deepseek-r1",
            context_length: 128_000,
            max_output_tokens: Some(8_192),
            capabilities: ModelCapabilities {
                supports_tools: false,
                supports_thinking: true,
                supports_parallel_tools: false,
                ..cap()
            },
        },
        ModelMetadata {
            model: "deepseek-v3",
            context_length: 128_000,
            max_output_tokens: Some(8_192),
            capabilities: cap(),
        },
        // ── Meta ───────────────────────────────────────────────────────
        ModelMetadata {
            model: "llama-3.1-70b",
            context_length: 131_072,
            max_output_tokens: Some(4_096),
            capabilities: cap(),
        },
        ModelMetadata {
            model: "llama-3.1-405b",
            context_length: 131_072,
            max_output_tokens: Some(4_096),
            capabilities: cap(),
        },
        ModelMetadata {
            model: "llama-4-maverick",
            context_length: 1_048_576,
            max_output_tokens: Some(32_768),
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_thinking: true,
                ..cap()
            },
        },
        ModelMetadata {
            model: "llama-4-scout",
            context_length: 524_288,
            max_output_tokens: Some(32_768),
            capabilities: ModelCapabilities {
                supports_vision: true,
                ..cap()
            },
        },
        // ── Mistral ────────────────────────────────────────────────────
        ModelMetadata {
            model: "mistral-large",
            context_length: 128_000,
            max_output_tokens: Some(8_192),
            capabilities: cap(),
        },
        // ── Qwen ───────────────────────────────────────────────────────
        ModelMetadata {
            model: "qwen-2.5-72b",
            context_length: 131_072,
            max_output_tokens: Some(8_192),
            capabilities: cap(),
        },
        ModelMetadata {
            model: "qwen-3-235b",
            context_length: 131_072,
            max_output_tokens: Some(8_192),
            capabilities: ModelCapabilities {
                supports_thinking: true,
                ..cap()
            },
        },
        // ── Moonshot / Kimi ────────────────────────────────────────────
        ModelMetadata {
            model: "moonshotai/kimi-k2",
            context_length: 131_072,
            max_output_tokens: Some(8_192),
            capabilities: ModelCapabilities {
                supports_thinking: true,
                ..cap()
            },
        },
    ];

    models.into_iter().map(|m| (m.model, m)).collect()
}

/// Lazily-initialized metadata table built once on first access.
static KNOWN_METADATA: LazyLock<HashMap<&'static str, ModelMetadata>> =
    LazyLock::new(known_metadata);

/// Look up metadata for a specific model name.
/// Falls back to a fuzzy match if the exact name isn't found
/// (e.g. "gpt-4.1" matches "gpt-4.1" even when passed as "gpt-4.1-2025xxxx").
pub fn lookup(model: &str) -> Option<ModelMetadata> {
    // Exact match first
    if let Some(m) = KNOWN_METADATA.get(model) {
        return Some(m.clone());
    }

    // Try prefix match (for versioned model names like "claude-sonnet-4-20250514-v2")
    for (name, meta) in KNOWN_METADATA.iter() {
        if model.starts_with(name) {
            return Some(meta.clone());
        }
    }

    None
}

/// Returns the default context length for unknown models.
pub const DEFAULT_CONTEXT_LENGTH: usize = 128_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_metadata_includes_major_models() {
        let db = known_metadata();
        assert!(db.contains_key("gpt-4.1"));
        assert!(db.contains_key("claude-sonnet-4-20250514"));
        assert!(db.contains_key("gemini-2.5-pro"));
        assert!(db.contains_key("deepseek-r1"));
        assert!(db.contains_key("llama-3.1-70b"));
        assert!(db.contains_key("qwen-2.5-72b"));
    }

    #[test]
    fn known_metadata_has_correct_context_lengths() {
        let db = known_metadata();
        assert_eq!(db["gpt-4.1"].context_length, 1_048_576);
        assert_eq!(db["claude-sonnet-4-20250514"].context_length, 200_000);
        assert_eq!(db["deepseek-r1"].context_length, 128_000);
    }

    #[test]
    fn lookup_finds_exact_match() {
        let meta = lookup("gpt-4.1").expect("should find gpt-4.1");
        assert_eq!(meta.context_length, 1_048_576);
        assert!(meta.supports_tools());
        assert!(meta.supports_vision());
    }

    #[test]
    fn lookup_finds_prefix_match() {
        let meta = lookup("claude-sonnet-4-20250514-v2").expect("should match by prefix");
        assert_eq!(meta.model, "claude-sonnet-4-20250514");
        assert!(meta.supports_thinking());
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("completely-unknown-model").is_none());
    }

    #[test]
    fn all_models_have_positive_context_length() {
        for (_, meta) in known_metadata() {
            assert!(
                meta.context_length > 0,
                "{} has zero context length",
                meta.model
            );
        }
    }

    #[test]
    fn thinking_models_are_flagged_correctly() {
        let db = known_metadata();
        assert!(db["deepseek-r1"].supports_thinking());
        assert!(db["o4-mini"].supports_thinking());
        assert!(db["gemini-2.5-pro"].supports_thinking());
        assert!(!db["gpt-4.1"].supports_thinking());
        assert!(!db["llama-3.1-70b"].supports_thinking());
    }

    #[test]
    fn vision_models_are_flagged_correctly() {
        let db = known_metadata();
        assert!(db["gpt-4.1"].supports_vision());
        assert!(db["claude-sonnet-4-20250514"].supports_vision());
        assert!(!db["deepseek-r1"].supports_vision());
        assert!(!db["llama-3.1-70b"].supports_vision());
    }

    // ── New capability tests ────────────────────────────────────────────

    #[test]
    fn claude_models_support_anthropic_features() {
        let db = known_metadata();
        let claude = &db["claude-sonnet-4-20250514"];
        assert!(claude.capabilities.supports_token_efficient_tools);
        assert!(claude.capabilities.supports_citations);
        assert!(claude.capabilities.supports_context_caching);
    }

    #[test]
    fn openai_models_support_strict_mode() {
        let db = known_metadata();
        assert!(db["gpt-4.1"].capabilities.supports_strict_mode);
        assert!(db["gpt-4.1-mini"].capabilities.supports_strict_mode);
        assert!(db["o4-mini"].capabilities.supports_strict_mode);
    }

    #[test]
    fn nano_and_reasoning_models_disable_parallel_tools() {
        let db = known_metadata();
        assert!(!db["gpt-4.1-nano"].capabilities.supports_parallel_tools);
        assert!(!db["o4-mini"].capabilities.supports_parallel_tools);
        assert!(!db["deepseek-r1"].capabilities.supports_parallel_tools);
    }

    #[test]
    fn gemini_thinking_models_require_thought_signatures() {
        let db = known_metadata();
        assert!(db["gemini-2.5-pro"].capabilities.requires_thought_signatures);
        assert!(db["gemini-2.5-flash"].capabilities.requires_thought_signatures);
        assert!(!db["gemini-2.0-flash"].capabilities.requires_thought_signatures);
    }

    #[test]
    fn batch_api_flagged_for_supported_providers() {
        let db = known_metadata();
        // OpenAI
        assert!(db["gpt-4.1"].capabilities.supports_batch_api);
        assert!(db["o4-mini"].capabilities.supports_batch_api);
        // Anthropic (Message Batches API)
        assert!(db["claude-sonnet-4-20250514"].capabilities.supports_batch_api);
        assert!(db["claude-haiku-4-5"].capabilities.supports_batch_api);
        // Google (Batch API)
        assert!(db["gemini-2.5-pro"].capabilities.supports_batch_api);
        assert!(db["gemini-2.0-flash"].capabilities.supports_batch_api);
    }

    #[test]
    fn openai_models_support_predicted_outputs() {
        let db = known_metadata();
        assert!(db["gpt-4.1"].capabilities.supports_predicted_outputs);
        assert!(db["gpt-4.1-mini"].capabilities.supports_predicted_outputs);
        assert!(db["gpt-4.1-nano"].capabilities.supports_predicted_outputs);
    }

    #[test]
    fn deepseek_r1_disables_parallel_tools() {
        let db = known_metadata();
        assert!(!db["deepseek-r1"].capabilities.supports_parallel_tools);
        assert!(!db["deepseek-r1"].capabilities.supports_tools);
    }

    #[test]
    fn default_capabilities_are_conservative() {
        let caps = ModelCapabilities::default();
        assert!(caps.supports_tools);
        assert!(caps.supports_parallel_tools);
        assert!(!caps.supports_vision);
        assert!(!caps.supports_thinking);
        assert!(!caps.supports_strict_mode);
        assert!(!caps.requires_thought_signatures);
        assert!(!caps.supports_token_efficient_tools);
        assert!(!caps.supports_predicted_outputs);
        assert!(!caps.supports_citations);
        assert!(!caps.supports_context_caching);
        assert!(!caps.supports_batch_api);
    }

    #[test]
    fn convenience_methods_delegate_to_capabilities() {
        let meta = lookup("gpt-4.1").unwrap();
        assert_eq!(meta.supports_tools(), meta.capabilities.supports_tools);
        assert_eq!(meta.supports_vision(), meta.capabilities.supports_vision);
        assert_eq!(meta.supports_thinking(), meta.capabilities.supports_thinking);
    }

    #[test]
    fn no_model_has_parallel_tools_without_tools() {
        for (name, meta) in known_metadata() {
            if !meta.capabilities.supports_tools {
                assert!(
                    !meta.capabilities.supports_parallel_tools,
                    "{name} has supports_parallel_tools=true but supports_tools=false"
                );
            }
        }
    }

    #[test]
    fn claude_haiku_35_supports_citations() {
        let db = known_metadata();
        assert!(db["claude-haiku-3.5-20241022"].capabilities.supports_citations);
    }
}
