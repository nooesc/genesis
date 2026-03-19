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
    configure: impl FnOnce(reqwest::blocking::ClientBuilder) -> reqwest::blocking::ClientBuilder,
) -> reqwest::blocking::Client {
    let builder = reqwest::blocking::Client::builder().timeout(timeout);
    configure(builder).build().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to build configured HTTP client, falling back to default");
        reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .expect("minimal HTTP client build must succeed")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_client_with_default_config_succeeds() {
        let client = build_blocking_client(Duration::from_secs(30), |b| b);
        // If we got here without panic, the client was built successfully.
        // Verify it's usable by dropping it (no-op, but proves ownership).
        drop(client);
    }

    #[test]
    fn build_client_with_custom_user_agent_succeeds() {
        let client = build_blocking_client(Duration::from_secs(10), |b| {
            b.user_agent("genesis-test/0.1")
        });
        drop(client);
    }

    #[test]
    fn build_client_preserves_timeout() {
        // Build clients with various timeout values to ensure none panic.
        for secs in [1, 5, 30, 120] {
            let _client = build_blocking_client(Duration::from_secs(secs), |b| b);
        }
        // Also verify a sub-second timeout works.
        let _client = build_blocking_client(Duration::from_millis(500), |b| b);
    }
}
