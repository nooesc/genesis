use std::collections::BTreeMap;

const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Resolved provider endpoint ready to use for API calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// The backend identifier (e.g., "openai", "anthropic", "openrouter").
    /// Used for backend-specific optimizations like prompt caching.
    pub backend: String,
}

/// Resolve provider credentials from config values and environment.
///
/// Resolution order:
/// 1. Explicit `base_url` from config (custom endpoint: vLLM, Ollama, etc.)
/// 2. Backend name maps to known base URLs (openrouter, openai, etc.)
/// 3. API key comes from the env var named in `api_key_env`, or falls back
///    to standard env vars per backend.
pub fn resolve(
    backend: &str,
    model: &str,
    base_url: Option<&str>,
    api_key_env: Option<&str>,
    env: &BTreeMap<String, String>,
) -> ResolvedProvider {
    let resolved_base_url = base_url
        .map(str::to_owned)
        .unwrap_or_else(|| default_base_url(backend).to_owned());

    let api_key = resolve_api_key(backend, api_key_env, env);

    ResolvedProvider {
        base_url: resolved_base_url,
        api_key,
        model: model.to_owned(),
        backend: backend.trim().to_ascii_lowercase(),
    }
}

fn default_base_url(backend: &str) -> &str {
    match backend.trim().to_ascii_lowercase().as_str() {
        "openrouter" => OPENROUTER_BASE_URL,
        "openai" => OPENAI_BASE_URL,
        "anthropic" => ANTHROPIC_BASE_URL,
        _ => OPENAI_BASE_URL,
    }
}

fn resolve_api_key(
    backend: &str,
    api_key_env: Option<&str>,
    env: &BTreeMap<String, String>,
) -> String {
    // Try explicit env var name first
    if let Some(env_name) = api_key_env {
        if let Some(key) = env.get(env_name) {
            return key.clone();
        }
    }

    // Fall back to standard env vars per backend
    let fallback_vars = match backend.trim().to_ascii_lowercase().as_str() {
        "openrouter" => &["OPENROUTER_API_KEY"][..],
        "openai" => &["OPENAI_API_KEY"][..],
        "anthropic" => &["ANTHROPIC_API_KEY"][..],
        "gemini" | "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"][..],
        _ => &["OPENAI_API_KEY"][..],
    };

    for var in fallback_vars {
        if let Some(key) = env.get(*var) {
            return key.clone();
        }
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn resolves_openrouter_with_explicit_key() {
        let env = BTreeMap::from([("OPENROUTER_API_KEY".to_owned(), "sk-or-123".to_owned())]);

        let resolved = resolve("openrouter", "nous/hermes", None, None, &env);

        assert_eq!(resolved.base_url, OPENROUTER_BASE_URL);
        assert_eq!(resolved.api_key, "sk-or-123");
        assert_eq!(resolved.model, "nous/hermes");
    }

    #[test]
    fn resolves_custom_base_url_over_backend_default() {
        let env = BTreeMap::from([("OPENAI_API_KEY".to_owned(), "sk-custom".to_owned())]);

        let resolved = resolve(
            "openai",
            "llama-3",
            Some("http://localhost:8000/v1"),
            None,
            &env,
        );

        assert_eq!(resolved.base_url, "http://localhost:8000/v1");
    }

    #[test]
    fn prefers_explicit_api_key_env_over_fallback() {
        let env = BTreeMap::from([
            ("MY_CUSTOM_KEY".to_owned(), "custom-key".to_owned()),
            ("OPENAI_API_KEY".to_owned(), "fallback-key".to_owned()),
        ]);

        let resolved = resolve("openai", "gpt-4", None, Some("MY_CUSTOM_KEY"), &env);

        assert_eq!(resolved.api_key, "custom-key");
    }

    #[test]
    fn returns_empty_key_when_nothing_found() {
        let env = BTreeMap::new();

        let resolved = resolve("openai", "gpt-4", None, None, &env);

        assert_eq!(resolved.api_key, "");
    }

    #[test]
    fn defaults_unknown_backend_to_openai_url() {
        let env = BTreeMap::new();

        let resolved = resolve("vllm", "llama-3", None, None, &env);

        assert_eq!(resolved.base_url, OPENAI_BASE_URL);
    }

    #[test]
    fn resolves_anthropic_with_correct_url_and_key() {
        let env = BTreeMap::from([("ANTHROPIC_API_KEY".to_owned(), "sk-ant-123".to_owned())]);

        let resolved = resolve("anthropic", "claude-sonnet-4-20250514", None, None, &env);

        assert_eq!(resolved.base_url, ANTHROPIC_BASE_URL);
        assert_eq!(resolved.api_key, "sk-ant-123");
        assert_eq!(resolved.model, "claude-sonnet-4-20250514");
        assert_eq!(resolved.backend, "anthropic");
    }
}
