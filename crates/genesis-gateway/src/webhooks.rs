//! Webhook event dispatcher for Genesis gateway.
//!
//! Sends event notifications to configured webhook URLs. Events are dispatched
//! asynchronously (fire-and-forget) to avoid blocking the agent loop.

use genesis_config::WebhookConfig;
use reqwest::Client;
use serde::Serialize;
use tracing::{debug, warn};

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

/// Dispatcher that sends events to configured webhooks.
#[derive(Clone)]
pub struct WebhookDispatcher {
    client: Client,
    configs: Vec<WebhookConfig>,
}

impl WebhookDispatcher {
    pub fn new(configs: Vec<WebhookConfig>) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("genesis-webhook")
            .build()
            .unwrap_or_default();
        Self { client, configs }
    }

    /// Check if any webhooks are configured.
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    /// Dispatch an event to all matching webhooks.
    ///
    /// This spawns background tasks — it does not block on delivery.
    pub fn dispatch(&self, payload: WebhookPayload) {
        if self.configs.is_empty() {
            return;
        }

        let event_str = payload.event.as_str();

        for config in &self.configs {
            // Filter by event type if the webhook has a filter
            if !config.events.is_empty()
                && !config.events.iter().any(|e| e == event_str)
            {
                continue;
            }

            let client = self.client.clone();
            let url = config.url.clone();
            let secret = config.secret.clone();
            let body = serde_json::to_string(&payload).unwrap_or_default();

            tokio::spawn(async move {
                let mut request = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("X-Genesis-Event", event_str);

                // Add HMAC signature if secret is configured
                if let Some(ref secret) = secret {
                    let signature = compute_hmac(secret, &body);
                    request = request.header("X-Genesis-Signature", signature);
                }

                match request.body(body).send().await {
                    Ok(resp) => {
                        debug!(
                            url = url.as_str(),
                            event = event_str,
                            status = resp.status().as_u16(),
                            "webhook delivered"
                        );
                    }
                    Err(e) => {
                        warn!(
                            url = url.as_str(),
                            event = event_str,
                            error = %e,
                            "webhook delivery failed"
                        );
                    }
                }
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
    use sha2::Sha256;
    use hmac::{Hmac, Mac};

    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
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
}
