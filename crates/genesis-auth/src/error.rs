use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("not logged in: run `genesis login` to authenticate")]
    NotLoggedIn,

    #[error("device code request failed: {message}")]
    DeviceCodeRequest { message: String },

    #[error("login timed out after {minutes} minutes")]
    LoginTimeout { minutes: u32 },

    #[error("login cancelled")]
    LoginCancelled,

    #[error("token exchange failed: {message}")]
    TokenExchange { message: String },

    #[error("token refresh failed: {message}")]
    TokenRefresh { message: String },

    #[error("auth store I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("auth store JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),
}
