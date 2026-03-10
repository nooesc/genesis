//! Shared HTTP client builder for tool modules.
//!
//! Provides a consistent fallback strategy: if the full-featured client
//! fails to build (TLS backend init), a minimal client with the timeout
//! preserved is used instead.

use std::time::Duration;

/// Build a blocking HTTP client with a fallback that preserves the timeout.
///
/// `configure` receives a builder with the timeout already set and can add
/// user-agent, redirect policy, etc.  If the configured builder fails, a
/// minimal builder with only the timeout is tried.
pub fn build_blocking_client(
    timeout: Duration,
    configure: impl FnOnce(
        reqwest::blocking::ClientBuilder,
    ) -> reqwest::blocking::ClientBuilder,
) -> reqwest::blocking::Client {
    let builder = reqwest::blocking::Client::builder().timeout(timeout);
    configure(builder).build().unwrap_or_else(|e| {
        eprintln!("warning: HTTP client build failed ({e}), using minimal fallback");
        reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .expect("minimal HTTP client build must succeed")
    })
}
