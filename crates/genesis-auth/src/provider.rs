/// Auth method for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    /// Provider-specific OAuth variant (e.g., OpenAI Codex)
    OAuthExternal,
    /// Standard RFC 8628 device code flow (e.g., Nous Portal) -- future
    OAuthDeviceCode,
    /// Simple API key from env var -- future
    ApiKey,
}

/// A known auth provider with its endpoints and configuration.
#[derive(Debug, Clone)]
pub struct AuthProviderConfig {
    pub id: &'static str,
    pub name: &'static str,
    pub auth_type: AuthType,
    pub inference_base_url: &'static str,
    pub client_id: Option<&'static str>,
}

pub const CODEX_INFERENCE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CODEX_DEVICE_CODE_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub const CODEX_DEVICE_POLL_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/token";
pub const CODEX_DEVICE_VERIFY_URL: &str = "https://auth.openai.com/codex/device";
pub const CODEX_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

pub const CODEX_PROVIDER_ID: &str = "openai-codex";

const PROVIDERS: &[AuthProviderConfig] = &[AuthProviderConfig {
    id: CODEX_PROVIDER_ID,
    name: "OpenAI Codex (ChatGPT)",
    auth_type: AuthType::OAuthExternal,
    inference_base_url: CODEX_INFERENCE_URL,
    client_id: Some(CODEX_OAUTH_CLIENT_ID),
}];

/// Look up a provider by ID.
pub fn lookup(id: &str) -> Option<&'static AuthProviderConfig> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// Return all registered providers.
pub fn all() -> &'static [AuthProviderConfig] {
    PROVIDERS
}

/// Return all providers that support OAuth login.
pub fn oauth_providers() -> Vec<&'static AuthProviderConfig> {
    PROVIDERS
        .iter()
        .filter(|p| matches!(p.auth_type, AuthType::OAuthExternal | AuthType::OAuthDeviceCode))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_codex_provider() {
        let p = lookup(CODEX_PROVIDER_ID).unwrap();
        assert_eq!(p.id, CODEX_PROVIDER_ID);
        assert_eq!(p.auth_type, AuthType::OAuthExternal);
        assert_eq!(p.inference_base_url, CODEX_INFERENCE_URL);
        assert_eq!(p.client_id, Some(CODEX_OAUTH_CLIENT_ID));
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("nonexistent").is_none());
    }

    #[test]
    fn all_returns_at_least_one_provider() {
        assert!(!all().is_empty());
    }

    #[test]
    fn oauth_providers_includes_codex() {
        let oauth = oauth_providers();
        assert!(oauth.iter().any(|p| p.id == CODEX_PROVIDER_ID));
    }
}
