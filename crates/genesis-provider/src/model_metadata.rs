//! Well-known model metadata for context limits and capability detection.

use std::collections::HashMap;

/// Metadata describing a model's capabilities and limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMetadata {
    pub model: &'static str,
    pub context_length: usize,
    pub max_output_tokens: Option<usize>,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_thinking: bool,
}

/// Returns metadata for well-known models, keyed by model name.
pub fn known_metadata() -> HashMap<&'static str, ModelMetadata> {
    let models = vec![
        // OpenAI
        ModelMetadata {
            model: "gpt-4.1",
            context_length: 1_048_576,
            max_output_tokens: Some(32_768),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: false,
        },
        ModelMetadata {
            model: "gpt-4.1-mini",
            context_length: 1_048_576,
            max_output_tokens: Some(32_768),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: false,
        },
        ModelMetadata {
            model: "gpt-4.1-nano",
            context_length: 1_048_576,
            max_output_tokens: Some(32_768),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: false,
        },
        ModelMetadata {
            model: "o4-mini",
            context_length: 200_000,
            max_output_tokens: Some(100_000),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        // Anthropic
        ModelMetadata {
            model: "claude-sonnet-4-20250514",
            context_length: 200_000,
            max_output_tokens: Some(64_000),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "claude-opus-4-20250514",
            context_length: 200_000,
            max_output_tokens: Some(32_000),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "claude-opus-4-6",
            context_length: 200_000,
            max_output_tokens: Some(32_000),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "claude-sonnet-4-6",
            context_length: 200_000,
            max_output_tokens: Some(64_000),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "claude-haiku-4-5",
            context_length: 200_000,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "claude-haiku-3.5-20241022",
            context_length: 200_000,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: false,
        },
        // Google
        ModelMetadata {
            model: "gemini-2.5-pro",
            context_length: 1_048_576,
            max_output_tokens: Some(65_536),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "gemini-2.5-flash",
            context_length: 1_048_576,
            max_output_tokens: Some(65_536),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "gemini-2.0-flash",
            context_length: 1_048_576,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: false,
        },
        // DeepSeek
        ModelMetadata {
            model: "deepseek-r1",
            context_length: 128_000,
            max_output_tokens: Some(8_192),
            supports_tools: false,
            supports_vision: false,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "deepseek-v3",
            context_length: 128_000,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
        },
        // Meta
        ModelMetadata {
            model: "llama-3.1-70b",
            context_length: 131_072,
            max_output_tokens: Some(4_096),
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
        },
        ModelMetadata {
            model: "llama-3.1-405b",
            context_length: 131_072,
            max_output_tokens: Some(4_096),
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
        },
        ModelMetadata {
            model: "llama-4-maverick",
            context_length: 1_048_576,
            max_output_tokens: Some(32_768),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: true,
        },
        ModelMetadata {
            model: "llama-4-scout",
            context_length: 524_288,
            max_output_tokens: Some(32_768),
            supports_tools: true,
            supports_vision: true,
            supports_thinking: false,
        },
        // Mistral
        ModelMetadata {
            model: "mistral-large",
            context_length: 128_000,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
        },
        // Qwen
        ModelMetadata {
            model: "qwen-2.5-72b",
            context_length: 131_072,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
        },
        ModelMetadata {
            model: "qwen-3-235b",
            context_length: 131_072,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_vision: false,
            supports_thinking: true,
        },
        // Moonshot / Kimi
        ModelMetadata {
            model: "moonshotai/kimi-k2",
            context_length: 131_072,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_vision: false,
            supports_thinking: true,
        },
    ];

    models.into_iter().map(|m| (m.model, m)).collect()
}

/// Look up metadata for a specific model name.
/// Falls back to a fuzzy match if the exact name isn't found
/// (e.g. "gpt-4.1" matches "gpt-4.1" even when passed as "gpt-4.1-2025xxxx").
pub fn lookup(model: &str) -> Option<ModelMetadata> {
    let db = known_metadata();

    // Exact match first
    if let Some(m) = db.get(model) {
        return Some(m.clone());
    }

    // Try prefix match (for versioned model names like "claude-sonnet-4-20250514-v2")
    for (name, meta) in &db {
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
        assert!(meta.supports_tools);
        assert!(meta.supports_vision);
    }

    #[test]
    fn lookup_finds_prefix_match() {
        let meta = lookup("claude-sonnet-4-20250514-v2").expect("should match by prefix");
        assert_eq!(meta.model, "claude-sonnet-4-20250514");
        assert!(meta.supports_thinking);
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("completely-unknown-model").is_none());
    }

    #[test]
    fn all_models_have_positive_context_length() {
        for (_, meta) in known_metadata() {
            assert!(meta.context_length > 0, "{} has zero context length", meta.model);
        }
    }

    #[test]
    fn thinking_models_are_flagged_correctly() {
        let db = known_metadata();
        assert!(db["deepseek-r1"].supports_thinking);
        assert!(db["o4-mini"].supports_thinking);
        assert!(db["gemini-2.5-pro"].supports_thinking);
        assert!(!db["gpt-4.1"].supports_thinking);
        assert!(!db["llama-3.1-70b"].supports_thinking);
    }

    #[test]
    fn vision_models_are_flagged_correctly() {
        let db = known_metadata();
        assert!(db["gpt-4.1"].supports_vision);
        assert!(db["claude-sonnet-4-20250514"].supports_vision);
        assert!(!db["deepseek-r1"].supports_vision);
        assert!(!db["llama-3.1-70b"].supports_vision);
    }
}
