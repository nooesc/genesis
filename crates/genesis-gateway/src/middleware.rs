//! Auth middleware, rate limiting, request logging, and CORS configuration.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

use axum::extract::State;
use axum::http::{header, Request};
use axum::middleware::Next;
use axum::response::Response;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

use std::sync::Arc;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Rate limiter
// ---------------------------------------------------------------------------

/// Simple in-memory sliding-window rate limiter keyed by IP address.
///
/// Each entry stores `(request_count, window_start_secs)`.  When a new
/// request arrives and the current timestamp is still within the same
/// 60-second window, the count increments.  Otherwise the window resets.
/// Stale entries (older than 2 minutes) are purged on every check to
/// prevent unbounded memory growth.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    /// Max requests per 60-second window.  Stored here so the middleware
    /// doesn't need to re-read `AppState`.
    max_rpm: u32,
    /// Map from IP -> (count, window_start_epoch_secs).
    entries: Mutex<HashMap<IpAddr, (u32, u64)>>,
    /// Epoch second of the last purge, used to amortize cleanup.
    last_purge: std::sync::atomic::AtomicU64,
}

/// How often (in seconds) to purge stale rate-limit entries.
const PURGE_INTERVAL_SECS: u64 = genesis_config::defaults::timeouts::RATE_PURGE_INTERVAL_SECS;

/// Duration of each rate-limit sliding window, in seconds.
const RATE_WINDOW_SECS: u64 = genesis_config::defaults::timeouts::RATE_WINDOW_SECS;

impl RateLimiter {
    pub fn new(max_rpm: u32) -> Self {
        Self {
            max_rpm,
            entries: Mutex::new(HashMap::new()),
            last_purge: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut map = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // Recover from poisoned mutex — another thread panicked while
                // holding the lock. Clear the stale state and continue.
                let mut guard = poisoned.into_inner();
                guard.clear();
                guard
            }
        };

        // Amortized purge: only scan & remove stale entries periodically
        let prev = self.last_purge.load(std::sync::atomic::Ordering::Relaxed);
        if now.saturating_sub(prev) >= PURGE_INTERVAL_SECS {
            map.retain(|_, (_, window_start)| {
                now.saturating_sub(*window_start) < PURGE_INTERVAL_SECS
            });
            self.last_purge
                .store(now, std::sync::atomic::Ordering::Relaxed);
        }

        let entry = map.entry(ip).or_insert((0, now));
        if now.saturating_sub(entry.1) >= RATE_WINDOW_SECS {
            // New window
            *entry = (1, now);
            true
        } else if entry.0 < self.max_rpm {
            entry.0 += 1;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Middleware handlers
// ---------------------------------------------------------------------------

/// Middleware that logs every request with method, path, status, and duration.
pub(crate) async fn request_logging_middleware(
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let start = std::time::Instant::now();

    let response = next.run(request).await;

    let duration = start.elapsed();
    let status = response.status().as_u16();

    // Skip logging health checks to reduce noise
    if path != "/health" {
        info!(
            method = %method,
            path = %path,
            status = status,
            duration_ms = duration.as_millis() as u64,
            "request completed"
        );
    }

    response
}

pub(crate) async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, axum::http::StatusCode> {
    let expected_key = match &state.api_key {
        Some(key) => key,
        None if state.api_key_required => return Err(axum::http::StatusCode::UNAUTHORIZED),
        None => return Ok(next.run(request).await),
    };

    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let token = auth_header.and_then(|value| {
        let value = value.trim();
        let mut parts = value.splitn(2, ' ');
        let scheme = parts.next()?.trim();
        let credentials = parts.next()?.trim();
        if scheme.eq_ignore_ascii_case("bearer") {
            Some(credentials)
        } else {
            None
        }
    });

    match token {
        Some(t) if crate::verify::constant_time_eq(t.as_bytes(), expected_key.as_bytes()) => {
            Ok(next.run(request).await)
        }
        _ => Err(axum::http::StatusCode::UNAUTHORIZED),
    }
}

/// Extracts the client IP from the request.
///
/// Extracts the client IP from proxy headers when the peer socket belongs to a
/// trusted proxy; otherwise falls back to the peer socket address via
/// `ConnectInfo`.
pub(crate) fn client_ip<B>(state: &AppState, request: &Request<B>) -> Option<IpAddr> {
    let peer_ip = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());

    let peer_is_trusted_proxy = peer_ip
        .map(|ip| state.trusted_proxies.contains(&ip))
        .unwrap_or(false);

    if peer_is_trusted_proxy {
        // X-Forwarded-For: first entry is the original client
        if let Some(forwarded) = request.headers().get("x-forwarded-for") {
            if let Ok(val) = forwarded.to_str() {
                if let Some(first) = val.split(',').next() {
                    if let Ok(ip) = first.trim().parse::<IpAddr>() {
                        return Some(ip);
                    }
                }
            }
        }

        // X-Real-IP
        if let Some(real_ip) = request.headers().get("x-real-ip") {
            if let Ok(val) = real_ip.to_str() {
                if let Ok(ip) = val.trim().parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }

    // Peer address via ConnectInfo (populated by axum::serve)
    peer_ip
}

pub(crate) async fn rate_limit_middleware(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, axum::http::StatusCode> {
    let limiter = match &state.rate_limiter {
        Some(l) => l,
        None => return Ok(next.run(request).await),
    };
    let ip = client_ip(&state, &request).unwrap_or(IpAddr::from([127, 0, 0, 1]));
    if limiter.check(ip) {
        Ok(next.run(request).await)
    } else {
        Err(axum::http::StatusCode::TOO_MANY_REQUESTS)
    }
}

// ---------------------------------------------------------------------------
// CORS configuration
// ---------------------------------------------------------------------------

/// Default localhost origins allowed for development when no CORS origins are configured.
const LOCALHOST_ORIGINS: &[&str] = &[
    "http://localhost:3000",
    "http://localhost:5173",
    "http://localhost:8080",
    "http://127.0.0.1:3000",
    "http://127.0.0.1:5173",
    "http://127.0.0.1:8080",
];

fn parse_origin_values(origins: &[&str]) -> Vec<axum::http::HeaderValue> {
    origins
        .iter()
        .filter_map(|o| match o.parse() {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(origin = %o, error = %e, "skipping invalid CORS origin");
                None
            }
        })
        .collect()
}

/// Build CORS layer from gateway config.
///
/// - If `cors_origins` contains `"*"`, all origins are allowed.
/// - If `cors_origins` is non-empty, only those origins are allowed.
/// - Default (empty or no gateway config): localhost origins only.
pub(crate) fn build_cors_layer(gateway: Option<&genesis_config::GatewayConfig>) -> CorsLayer {
    use axum::http::{HeaderValue, Method};

    let origins: &[String] = gateway.map(|g| g.cors_origins.as_slice()).unwrap_or(&[]);

    let base = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    if origins.iter().any(|o| o == "*") {
        base.allow_origin(Any)
    } else if origins.is_empty() {
        base.allow_origin(parse_origin_values(LOCALHOST_ORIGINS))
    } else {
        let values: Vec<HeaderValue> = origins
            .iter()
            .filter_map(|o| match o.parse::<HeaderValue>() {
                Ok(v) => Some(v),
                Err(e) => {
                    error!(origin = %o, error = %e, "invalid CORS origin in config, skipping");
                    None
                }
            })
            .collect();
        if values.is_empty() {
            error!("all configured CORS origins are invalid, falling back to default localhost origins ({LOCALHOST_ORIGINS:?})");
            base.allow_origin(parse_origin_values(LOCALHOST_ORIGINS))
        } else {
            base.allow_origin(values)
        }
    }
}
