use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API returned error status {status}: {body}")]
    ApiError { status: u16, body: String },
    #[error("API response missing choices array or choices was empty")]
    EmptyChoices,
    #[error("missing API key: set {env_var} or configure provider.api_key_env")]
    MissingApiKey { env_var: String },
    #[error("failed to serialize request: {0}")]
    Serialize(#[from] serde_json::Error),
}
