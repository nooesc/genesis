//! Shared application state for all request handlers.

use std::net::IpAddr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, OnceLock};

use axum::http::StatusCode;

use crate::webhooks;

/// Prometheus-style histogram with fixed bucket boundaries.
pub(crate) struct HistogramBuckets {
    /// Bucket boundaries in milliseconds.
    boundaries: &'static [u64],
    /// Count of observations in each bucket (cumulative).
    counts: Vec<u64>,
    /// Total count of all observations.
    total_count: u64,
    /// Sum of all observed values (for computing mean).
    total_sum: f64,
}

pub(crate) const DURATION_BUCKETS: &[u64] = &[50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000];

impl HistogramBuckets {
    pub(crate) fn new(boundaries: &'static [u64]) -> Self {
        Self {
            boundaries,
            counts: vec![0; boundaries.len()],
            total_count: 0,
            total_sum: 0.0,
        }
    }

    pub(crate) fn observe(&mut self, value_ms: u64) {
        self.total_count += 1;
        self.total_sum += value_ms as f64;
        for (i, &boundary) in self.boundaries.iter().enumerate() {
            if value_ms <= boundary {
                self.counts[i] += 1;
            }
        }
    }

    pub(crate) fn format_prometheus(&self, name: &str, help: &str) -> String {
        use std::fmt::Write;
        let mut out = format!("# HELP {name} {help}\n# TYPE {name} histogram\n");
        for (i, &boundary) in self.boundaries.iter().enumerate() {
            let _ = writeln!(out, "{name}_bucket{{le=\"{boundary}\"}} {}", self.counts[i]);
        }
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {}", self.total_count);
        let _ = writeln!(out, "{name}_sum {}", self.total_sum);
        let _ = writeln!(out, "{name}_count {}", self.total_count);
        out
    }
}

/// Shared application state for all request handlers.
pub struct AppState {
    pub loaded: genesis_config::LoadedConfig,
    /// Optional API key for gateway authentication.
    /// When set, protected routes require `Authorization: Bearer <key>`.
    /// If absent and `api_key_required` is true, protected routes are rejected.
    pub api_key: Option<String>,
    /// Whether protected routes must require an API key.
    pub api_key_required: bool,
    /// Shared MCP manager for external tool servers (connected at startup).
    pub mcp: Option<std::sync::Arc<genesis_mcp::McpManager>>,
    /// Shared HTTP client for outbound platform API calls (connection pooling).
    pub http_client: reqwest::Client,
    /// Optional per-IP rate limiter.
    pub(crate) rate_limiter: Option<crate::middleware::RateLimiter>,
    /// Trusted reverse proxy IPs allowed to supply forwarded headers.
    pub trusted_proxies: Vec<IpAddr>,
    /// Webhook event dispatcher for external notifications.
    pub webhooks: webhooks::WebhookDispatcher,
    /// Timestamp when the gateway started (for uptime reporting).
    pub started_at: std::time::Instant,
    // --- Metrics counters ---
    /// Total chat requests processed (including stream and batch).
    pub requests_total: AtomicU64,
    /// Total errors returned across all endpoints.
    pub errors_total: AtomicU64,
    /// Total input tokens processed.
    pub input_tokens_total: AtomicU64,
    /// Total output tokens generated.
    pub output_tokens_total: AtomicU64,
    /// Total streaming requests.
    pub stream_requests_total: AtomicU64,
    /// Request duration histogram buckets (in ms): [50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000, +Inf]
    pub(crate) request_duration_histogram: Mutex<HistogramBuckets>,
    /// Agent message bus for inter-agent communication.
    pub agent_bus: genesis_core::agent_bus::AgentBus,
    /// Process-local plugin runtime overrides supplied by the embedding host.
    pub plugin_runtime_overrides: genesis_core::execution::PluginRuntimeOverrides,
    /// Shared embedding provider cached on first use to avoid rebuilding per request.
    pub(crate) embedding_provider_cache: OnceLock<Arc<genesis_core::embedding::EmbeddingProvider>>,
    /// Serializes first-time provider initialization on toolchains without `OnceLock::get_or_try_init`.
    pub(crate) embedding_provider_init: Mutex<()>,
}

pub(crate) fn get_or_try_init_arc<T, E, F>(
    cache: &OnceLock<Arc<T>>,
    init_lock: &Mutex<()>,
    init: F,
) -> Result<Arc<T>, E>
where
    F: FnOnce() -> Result<T, E>,
{
    if let Some(value) = cache.get() {
        return Ok(Arc::clone(value));
    }

    let _guard = init_lock
        .lock()
        .expect("embedding provider init lock poisoned");
    if let Some(value) = cache.get() {
        return Ok(Arc::clone(value));
    }

    let value = Arc::new(init()?);
    let _ = cache.set(Arc::clone(&value));
    Ok(value)
}

impl AppState {
    pub fn new(
        loaded: genesis_config::LoadedConfig,
        api_key: Option<String>,
        api_key_required: bool,
        mcp: Option<std::sync::Arc<genesis_mcp::McpManager>>,
        rate_limit_rpm: Option<u32>,
        trusted_proxies: Vec<IpAddr>,
        plugin_runtime_overrides: genesis_core::execution::PluginRuntimeOverrides,
    ) -> Self {
        let webhook_configs = loaded
            .config
            .gateway
            .as_ref()
            .map(|g| g.webhooks.clone())
            .unwrap_or_default();
        let bus_db_path = loaded.config.storage.database_path.clone();
        Self {
            api_key,
            api_key_required,
            mcp,
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(
                    genesis_config::defaults::timeouts::GATEWAY_HTTP_CLIENT_SECS,
                ))
                .user_agent("genesis-gateway/0.1")
                .build()
                .unwrap_or_default(),
            rate_limiter: rate_limit_rpm.map(crate::middleware::RateLimiter::new),
            trusted_proxies,
            loaded,
            webhooks: webhooks::WebhookDispatcher::new(webhook_configs),
            started_at: std::time::Instant::now(),
            requests_total: AtomicU64::new(0),
            errors_total: AtomicU64::new(0),
            input_tokens_total: AtomicU64::new(0),
            output_tokens_total: AtomicU64::new(0),
            stream_requests_total: AtomicU64::new(0),
            request_duration_histogram: Mutex::new(HistogramBuckets::new(DURATION_BUCKETS)),
            agent_bus: genesis_core::agent_bus::AgentBus::with_persistence(&bus_db_path),
            plugin_runtime_overrides,
            embedding_provider_cache: OnceLock::new(),
            embedding_provider_init: Mutex::new(()),
        }
    }

    pub fn session_service(&self) -> genesis_core::execution::SessionExecutionService<'_> {
        let mut service = genesis_core::execution::SessionExecutionService::new(&self.loaded);
        if let Some(mcp) = &self.mcp {
            service.set_mcp(std::sync::Arc::clone(mcp));
        }
        service.set_plugin_runtime_overrides(self.plugin_runtime_overrides);
        service
    }

    pub(crate) fn embedding_provider(
        &self,
    ) -> Result<Option<Arc<genesis_core::embedding::EmbeddingProvider>>, (StatusCode, String)> {
        let Some(config) = self.loaded.config.embedding.as_ref() else {
            return Ok(None);
        };

        get_or_try_init_arc(
            &self.embedding_provider_cache,
            &self.embedding_provider_init,
            || crate::routes::memories::build_embedding_provider(config),
        )
        .map(Some)
    }
}
