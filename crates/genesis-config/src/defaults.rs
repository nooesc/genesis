//! Centralized default constants for timeouts, limits, and retry parameters.
//!
//! All magic numbers that were previously scattered across the workspace live
//! here. Crates import from this module instead of defining their own constants.
//! Many of these are also exposed as configurable fields in `GenesisConfig`.

/// Timeout defaults (in seconds unless noted otherwise).
pub mod timeouts {
    /// Shell command execution timeout (seconds).
    pub const SHELL_COMMAND_SECS: u64 = 120;

    /// Browser command timeout (seconds).
    pub const BROWSER_COMMAND_SECS: u64 = 30;

    /// Browser session inactivity timeout (seconds).
    pub const BROWSER_SESSION_SECS: u64 = 300;

    /// Image generation API timeout (seconds).
    pub const IMAGE_GENERATION_SECS: u64 = 120;

    /// Home Assistant REST API timeout (seconds).
    pub const HOME_ASSISTANT_SECS: u64 = 15;

    /// Webhook signature timestamp replay tolerance (seconds).
    /// Incoming webhooks with timestamps older than this are rejected.
    pub const WEBHOOK_TIMESTAMP_TOLERANCE_SECS: i64 = 300;

    /// Webhook HTTP client timeout for outbound dispatches (seconds).
    pub const WEBHOOK_DISPATCH_SECS: u64 = 10;

    /// Rate-limiter stale-entry purge interval (seconds).
    pub const RATE_PURGE_INTERVAL_SECS: u64 = 120;

    /// Rate-limiter sliding window duration (seconds).
    pub const RATE_WINDOW_SECS: u64 = 60;

    /// LLM provider HTTP connect timeout (seconds).
    pub const PROVIDER_CONNECT_SECS: u64 = 30;

    /// LLM provider HTTP response timeout — long for streaming completions (seconds).
    pub const PROVIDER_RESPONSE_SECS: u64 = 300;

    /// Embedding API HTTP connect timeout (seconds).
    pub const EMBEDDING_CONNECT_SECS: u64 = 30;

    /// Embedding API HTTP response timeout (seconds).
    pub const EMBEDDING_RESPONSE_SECS: u64 = 60;

    /// OAuth device code polling interval (seconds).
    pub const DEVICE_CODE_POLL_INTERVAL_SECS: u64 = 5;

    /// OAuth device code flow timeout (minutes).
    pub const DEVICE_CODE_TIMEOUT_MINS: u64 = 15;

    /// Token refresh skew — refresh this many seconds before actual expiry.
    pub const TOKEN_REFRESH_SKEW_SECS: i64 = 120;

    /// Token refresh HTTP request timeout (seconds).
    pub const TOKEN_REFRESH_TIMEOUT_SECS: u64 = 15;

    /// Device-code login flow HTTP client timeout (seconds).
    pub const DEVICE_CODE_HTTP_CLIENT_TIMEOUT_SECS: u64 = 15;

    /// Credential cache TTL (seconds).
    pub const CREDENTIAL_CACHE_TTL_SECS: u64 = 60;

    /// Code execution (PTC) command timeout (seconds).
    pub const CODE_EXEC_COMMAND_SECS: u64 = 30;

    /// Code execution UDS accept timeout (seconds).
    pub const CODE_EXEC_ACCEPT_SECS: u64 = 10;

    /// Code execution stream read timeout (seconds).
    pub const CODE_EXEC_READ_SECS: u64 = 300;

    /// OpenRouter model cache TTL (seconds).
    pub const OPENROUTER_MODEL_CACHE_TTL_SECS: u64 = 3600;

    /// Gateway shared HTTP client timeout (seconds).
    pub const GATEWAY_HTTP_CLIENT_SECS: u64 = 30;

    /// Batch API HTTP connect timeout (seconds).
    pub const BATCH_CONNECT_SECS: u64 = 30;

    /// Batch API HTTP response timeout (seconds).
    pub const BATCH_RESPONSE_SECS: u64 = 300;

    /// Batch API initial poll delay (seconds).
    pub const BATCH_POLL_INITIAL_SECS: u64 = 5;

    /// Batch API maximum poll delay (seconds).
    pub const BATCH_POLL_MAX_SECS: u64 = 60;

    /// OpenRouter model list fetch timeout (seconds).
    pub const OPENROUTER_FETCH_TIMEOUT_SECS: u64 = 15;
}

/// Agent loop defaults.
pub mod agent {
    /// Default maximum agent loop iterations per user turn.
    pub const DEFAULT_MAX_TURNS: usize = 10;

    /// Default per-session budget limit in USD.
    /// Set to `0.0` or `null` in config to disable.
    pub const DEFAULT_BUDGET_LIMIT: f64 = 5.0;
}

/// Size and count limits.
pub mod limits {
    /// Maximum tool output size in bytes (64 KiB).
    pub const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

    /// Context token compression threshold as a fraction (0.0–1.0).
    /// When prompt tokens exceed this fraction of `max_context_tokens`,
    /// the middle portion of the conversation is summarized.
    pub const CONTEXT_COMPRESSION_THRESHOLD: f64 = 0.85;

    /// Maximum non-streaming LLM response body size (4 MiB).
    pub const MAX_NON_STREAMING_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

    /// Browser page snapshot maximum character count.
    pub const BROWSER_SNAPSHOT_MAX_CHARS: usize = 8000;

    /// Home Assistant maximum entities returned per list request.
    pub const HA_MAX_RESPONSE_ENTITIES: usize = 500;
}

/// Retry and resilience defaults.
pub mod retry {
    /// Maximum LLM API retry attempts for transient errors.
    pub const MAX_RETRIES: u32 = 3;

    /// Base delay for exponential backoff (seconds).
    pub const BASE_DELAY_SECS: u64 = 1;

    /// Maximum backoff delay cap (seconds).
    pub const MAX_DELAY_SECS: u64 = 8;

    /// Circuit breaker: consecutive failures before opening.
    pub const CIRCUIT_BREAKER_FAILURE_THRESHOLD: u32 = 5;

    /// Circuit breaker: cooldown period before probing (seconds).
    pub const CIRCUIT_BREAKER_COOLDOWN_SECS: u64 = 30;

    /// Consecutive same-tool failures before injecting a stuck-loop nudge.
    pub const STUCK_LOOP_THRESHOLD: usize = 3;
}
