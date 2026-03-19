//! Webhook event dispatcher for Genesis gateway.
//!
//! Sends event notifications to configured webhook URLs with automatic retry
//! and exponential backoff. Failed deliveries after all retries are logged as
//! dead-letter entries for later inspection.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use genesis_config::WebhookConfig;
use reqwest::Client;
use serde::Serialize;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Event types that can be sent to webhooks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookEventType {
    MessageReceived,
    ToolCalled,
    ResponseSent,
    Error,
    SessionCreated,
    SessionReset,
}

impl WebhookEventType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::MessageReceived => "message_received",
            Self::ToolCalled => "tool_called",
            Self::ResponseSent => "response_sent",
            Self::Error => "error",
            Self::SessionCreated => "session_created",
            Self::SessionReset => "session_reset",
        }
    }
}

/// Payload sent to webhook endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub event: WebhookEventType,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub platform: Option<String>,
    pub data: serde_json::Value,
}

/// A dead-letter entry for a webhook that failed all retry attempts.
#[derive(Debug, Clone, Serialize)]
pub struct DeadLetterEntry {
    pub url: String,
    pub event_type: String,
    pub payload: String,
    pub last_error: String,
    pub attempts: u32,
    pub failed_at: String,
}

/// Metrics for webhook delivery.
#[derive(Debug, Default)]
pub struct WebhookMetrics {
    pub delivered: AtomicU64,
    pub retried: AtomicU64,
    pub failed: AtomicU64,
}

/// Maximum number of entries retained in the dead-letter queue.
const MAX_DEAD_LETTER_ENTRIES: usize = 1000;

/// Dispatcher that sends events to configured webhooks with retry and dead-letter.
#[derive(Clone)]
pub struct WebhookDispatcher {
    client: Client,
    configs: Vec<WebhookConfig>,
    dead_letters: Arc<Mutex<VecDeque<DeadLetterEntry>>>,
    metrics: Arc<WebhookMetrics>,
}

impl WebhookDispatcher {
    pub fn new(configs: Vec<WebhookConfig>) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("genesis-webhook")
            .build()
            .expect("webhook HTTP client should build");
        Self {
            client,
            configs,
            dead_letters: Arc::new(Mutex::new(VecDeque::new())),
            metrics: Arc::new(WebhookMetrics::default()),
        }
    }

    /// Check if any webhooks are configured.
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    /// Return delivery metrics (delivered, retried, failed).
    pub fn metrics(&self) -> (u64, u64, u64) {
        (
            self.metrics.delivered.load(Ordering::Relaxed),
            self.metrics.retried.load(Ordering::Relaxed),
            self.metrics.failed.load(Ordering::Relaxed),
        )
    }

    /// Return a snapshot of the dead-letter queue.
    pub async fn dead_letters(&self) -> Vec<DeadLetterEntry> {
        Vec::from(self.dead_letters.lock().await.clone())
    }

    /// Clear the dead-letter queue, returning how many entries were removed.
    pub async fn clear_dead_letters(&self) -> usize {
        let mut dl = self.dead_letters.lock().await;
        let count = dl.len();
        dl.clear();
        count
    }

    /// Dispatch an event to all matching webhooks.
    ///
    /// Spawns background tasks with retry and exponential backoff.
    pub fn dispatch(&self, payload: WebhookPayload) {
        if self.configs.is_empty() {
            return;
        }

        let event_str = payload.event.as_str();

        // Serialize once — the payload is identical for all endpoints.
        let body = match serde_json::to_string(&payload) {
            Ok(body) => body,
            Err(e) => {
                warn!(error = %e, "failed to serialize webhook payload, skipping all endpoints");
                return;
            }
        };

        for config in &self.configs {
            // Filter by event type if the webhook has a filter
            if !config.events.is_empty() && !config.events.iter().any(|e| e == event_str) {
                continue;
            }

            let client = self.client.clone();
            let url = config.url.clone();
            let secret = config.secret.clone();
            let max_retries = config.max_retries;
            let backoff_ms = config.retry_backoff_ms;
            let body = body.clone();
            let dead_letters = Arc::clone(&self.dead_letters);
            let metrics = Arc::clone(&self.metrics);
            let event_type = event_str.to_owned();

            tokio::spawn(async move {
                #[allow(unused_assignments)]
                let mut last_error = String::new();
                let mut attempt = 0u32;

                loop {
                    let mut request = client
                        .post(&url)
                        .header("Content-Type", "application/json")
                        .header("X-Genesis-Event", event_type.as_str())
                        .header("X-Genesis-Attempt", (attempt + 1).to_string());

                    // Add HMAC signature if secret is configured
                    if let Some(ref secret) = secret {
                        let signature = compute_hmac(secret, &body);
                        request = request.header("X-Genesis-Signature", signature);
                    }

                    match request.body(body.clone()).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            if attempt > 0 {
                                info!(
                                    url = url.as_str(),
                                    event = event_type.as_str(),
                                    attempt = attempt + 1,
                                    "webhook delivered after retry"
                                );
                            } else {
                                debug!(
                                    url = url.as_str(),
                                    event = event_type.as_str(),
                                    status = resp.status().as_u16(),
                                    "webhook delivered"
                                );
                            }
                            metrics.delivered.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                        Ok(resp) => {
                            last_error = format!("HTTP {}", resp.status());
                        }
                        Err(e) => {
                            last_error = e.to_string();
                        }
                    }

                    attempt += 1;
                    if attempt > max_retries {
                        break;
                    }

                    // Exponential backoff: backoff_ms * 2^(attempt-1)
                    let delay = backoff_ms * (1u64 << (attempt - 1));
                    warn!(
                        url = url.as_str(),
                        event = event_type.as_str(),
                        attempt,
                        delay_ms = delay,
                        error = last_error.as_str(),
                        "webhook delivery failed, retrying"
                    );
                    metrics.retried.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }

                // All retries exhausted — dead-letter it
                warn!(
                    url = url.as_str(),
                    event = event_type.as_str(),
                    attempts = attempt,
                    error = last_error.as_str(),
                    "webhook delivery failed permanently, adding to dead-letter queue"
                );
                metrics.failed.fetch_add(1, Ordering::Relaxed);

                let entry = DeadLetterEntry {
                    url,
                    event_type,
                    payload: body,
                    last_error,
                    attempts: attempt,
                    failed_at: chrono::Utc::now().to_rfc3339(),
                };
                let mut dl = dead_letters.lock().await;
                // Cap the dead-letter queue
                if dl.len() >= MAX_DEAD_LETTER_ENTRIES {
                    dl.pop_front();
                }
                dl.push_back(entry);
            });
        }
    }

    /// Helper to create and dispatch a standard event.
    pub fn emit(
        &self,
        event: WebhookEventType,
        session_id: Option<&str>,
        platform: Option<&str>,
        data: serde_json::Value,
    ) {
        let payload = WebhookPayload {
            event,
            timestamp: chrono::Utc::now().to_rfc3339(),
            session_id: session_id.map(str::to_owned),
            platform: platform.map(str::to_owned),
            data,
        };
        self.dispatch(payload);
    }
}

/// Compute HMAC-SHA256 signature for webhook payload verification.
fn compute_hmac(secret: &str, body: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    let result = mac.finalize();
    format!("sha256={}", hex::encode(result.into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_event_type_serializes_to_snake_case() {
        let json = serde_json::to_string(&WebhookEventType::MessageReceived).unwrap();
        assert_eq!(json, "\"message_received\"");

        let json = serde_json::to_string(&WebhookEventType::ToolCalled).unwrap();
        assert_eq!(json, "\"tool_called\"");
    }

    #[test]
    fn webhook_payload_serializes() {
        let payload = WebhookPayload {
            event: WebhookEventType::ResponseSent,
            timestamp: "2026-03-08T12:00:00Z".to_owned(),
            session_id: Some("sess-123".to_owned()),
            platform: Some("telegram".to_owned()),
            data: serde_json::json!({"response": "Hello!"}),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["event"], "response_sent");
        assert_eq!(json["session_id"], "sess-123");
        assert_eq!(json["platform"], "telegram");
        assert_eq!(json["data"]["response"], "Hello!");
    }

    #[test]
    fn dispatcher_is_empty_when_no_configs() {
        let dispatcher = WebhookDispatcher::new(vec![]);
        assert!(dispatcher.is_empty());
    }

    #[test]
    fn dispatcher_not_empty_with_configs() {
        let config = WebhookConfig {
            url: "https://example.com/webhook".to_owned(),
            secret: None,
            events: vec![],
            max_retries: 3,
            retry_backoff_ms: 1000,
        };
        let dispatcher = WebhookDispatcher::new(vec![config]);
        assert!(!dispatcher.is_empty());
    }

    #[test]
    fn hmac_signature_is_deterministic() {
        let sig1 = compute_hmac("secret123", "hello world");
        let sig2 = compute_hmac("secret123", "hello world");
        assert_eq!(sig1, sig2);
        assert!(sig1.starts_with("sha256="));
    }

    #[test]
    fn hmac_signature_changes_with_different_secret() {
        let sig1 = compute_hmac("secret1", "hello");
        let sig2 = compute_hmac("secret2", "hello");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn hmac_signature_changes_with_different_body() {
        let sig1 = compute_hmac("secret", "body1");
        let sig2 = compute_hmac("secret", "body2");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn metrics_start_at_zero() {
        let dispatcher = WebhookDispatcher::new(vec![]);
        let (delivered, retried, failed) = dispatcher.metrics();
        assert_eq!(delivered, 0);
        assert_eq!(retried, 0);
        assert_eq!(failed, 0);
    }

    #[tokio::test]
    async fn dead_letter_queue_starts_empty() {
        let dispatcher = WebhookDispatcher::new(vec![]);
        assert!(dispatcher.dead_letters().await.is_empty());
    }

    #[tokio::test]
    async fn clear_dead_letters_returns_count() {
        let dispatcher = WebhookDispatcher::new(vec![]);
        // Add a dead letter manually for testing
        {
            let mut dl = dispatcher.dead_letters.lock().await;
            dl.push_back(DeadLetterEntry {
                url: "http://example.com".into(),
                event_type: "test".into(),
                payload: "{}".into(),
                last_error: "timeout".into(),
                attempts: 3,
                failed_at: "2026-03-08T00:00:00Z".into(),
            });
        }
        let count = dispatcher.clear_dead_letters().await;
        assert_eq!(count, 1);
        assert!(dispatcher.dead_letters().await.is_empty());
    }

    #[test]
    fn dead_letter_entry_serializes() {
        let entry = DeadLetterEntry {
            url: "http://example.com".into(),
            event_type: "error".into(),
            payload: r#"{"event":"error"}"#.into(),
            last_error: "connection refused".into(),
            attempts: 4,
            failed_at: "2026-03-08T12:00:00Z".into(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["attempts"], 4);
        assert_eq!(json["last_error"], "connection refused");
    }
}
