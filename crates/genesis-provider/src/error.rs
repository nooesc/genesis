use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider response body exceeded {max_bytes} bytes")]
    ResponseTooLarge { max_bytes: usize },
    #[error("provider response body was not valid UTF-8: {message}")]
    ResponseBodyNotUtf8 { message: String },
    #[error("API returned error status {status}: {body}")]
    ApiError { status: u16, body: String },
    #[error("API response missing choices array or choices was empty")]
    EmptyChoices,
    #[error("missing API key: set {env_var} or configure provider.api_key_env")]
    MissingApiKey { env_var: String },
    #[error("failed to serialize request: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("stream decode error: {0}")]
    StreamDecode(String),
    #[error("all {count} providers failed (primary + fallbacks)")]
    AllProvidersFailed { count: usize },
}
