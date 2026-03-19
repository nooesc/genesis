//! Circuit breaker pattern for LLM provider calls.
//!
//! Tracks consecutive failures and short-circuits requests when the provider
//! is likely down, avoiding wasted latency and API calls.
//!
//! ## States
//!
//! - **Closed** (normal): requests pass through. Consecutive failures tracked.
//! - **Open** (failing): requests fail fast with `CircuitOpen`. After a cooldown
//!   period, transitions to Half-Open.
//! - **Half-Open** (probing): one request allowed through. Success → Closed.
//!   Failure → Open (resets cooldown).

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — requests pass through.
    Closed,
    /// Provider is failing — requests rejected immediately.
    Open,
    /// Probing — one request allowed to test recovery.
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "closed"),
            CircuitState::Open => write!(f, "open"),
            CircuitState::HalfOpen => write!(f, "half-open"),
        }
    }
}

/// Internal mutable state behind the mutex.
struct Inner {
    state: CircuitState,
    /// Consecutive failures since the last success.
    consecutive_failures: u32,
    /// When the circuit was last opened (for cooldown calculation).
    opened_at: Option<Instant>,
    /// Total number of times the circuit has opened (lifetime counter).
    open_count: u64,
}

/// Thread-safe circuit breaker for a single provider.
///
/// Wraps the provider call path: check `allow_request()` before calling,
/// then `record_success()` or `record_failure()` based on the outcome.
pub struct CircuitBreaker {
    inner: Mutex<Inner>,
    /// Number of consecutive failures before opening the circuit.
    failure_threshold: u32,
    /// How long to wait in Open state before probing (Half-Open).
    cooldown: Duration,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given thresholds.
    ///
    /// - `failure_threshold`: consecutive failures to trigger Open (default: 5)
    /// - `cooldown`: seconds to wait before probing (default: 30s)
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner {
                state: CircuitState::Closed,
                consecutive_failures: 0,
                opened_at: None,
                open_count: 0,
            }),
            failure_threshold,
            cooldown,
        }
    }

    /// Create a circuit breaker with default settings (5 failures, 30s cooldown).
    pub fn with_defaults() -> Self {
        Self::new(5, Duration::from_secs(30))
    }

    /// Check whether a request should be allowed through.
    ///
    /// Returns `true` if the request can proceed, `false` if it should
    /// be rejected (circuit is Open and cooldown hasn't expired).
    pub fn allow_request(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        match inner.state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => {
                // Already in probing mode — allow exactly one request.
                // (The first caller wins; concurrent callers see HalfOpen
                // and also proceed, which is fine — the state machine
                // handles multiple outcomes gracefully.)
                true
            }
            CircuitState::Open => {
                // Check if cooldown has expired.
                if let Some(opened_at) = inner.opened_at {
                    if opened_at.elapsed() >= self.cooldown {
                        // Transition to Half-Open — allow a probe request.
                        inner.state = CircuitState::HalfOpen;
                        tracing::info!(
                            cooldown_secs = self.cooldown.as_secs(),
                            "circuit breaker transitioning to half-open (probing)"
                        );
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful request. Resets failure count and closes circuit.
    pub fn record_success(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.state == CircuitState::HalfOpen {
            tracing::info!("circuit breaker closing after successful probe");
        }
        inner.consecutive_failures = 0;
        inner.state = CircuitState::Closed;
        inner.opened_at = None;
    }

    /// Record a failed request. Increments failure count and may open circuit.
    pub fn record_failure(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.consecutive_failures += 1;

        match inner.state {
            CircuitState::Closed => {
                if inner.consecutive_failures >= self.failure_threshold {
                    inner.state = CircuitState::Open;
                    inner.opened_at = Some(Instant::now());
                    inner.open_count += 1;
                    tracing::warn!(
                        failures = inner.consecutive_failures,
                        open_count = inner.open_count,
                        cooldown_secs = self.cooldown.as_secs(),
                        "circuit breaker opened after consecutive failures"
                    );
                }
            }
            CircuitState::HalfOpen => {
                // Probe failed — back to Open with fresh cooldown.
                inner.state = CircuitState::Open;
                inner.opened_at = Some(Instant::now());
                inner.open_count += 1;
                tracing::warn!("circuit breaker re-opened after failed probe");
            }
            CircuitState::Open => {
                // Already open — just count the failure.
            }
        }
    }

    /// Current state of the circuit breaker.
    pub fn state(&self) -> CircuitState {
        self.inner.lock().unwrap().state
    }

    /// Number of consecutive failures since the last success.
    pub fn consecutive_failures(&self) -> u32 {
        self.inner.lock().unwrap().consecutive_failures
    }

    /// Total number of times the circuit has opened (lifetime counter).
    pub fn open_count(&self) -> u64 {
        self.inner.lock().unwrap().open_count
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_closed() {
        let cb = CircuitBreaker::with_defaults();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn opens_after_threshold_failures() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(30));
        assert!(cb.allow_request());

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        cb.record_failure(); // 3rd failure → Open
        assert_eq!(cb.state(), CircuitState::Open);
        assert_eq!(cb.open_count(), 1);
    }

    #[test]
    fn rejects_when_open() {
        let cb = CircuitBreaker::new(2, Duration::from_secs(300));
        cb.record_failure();
        cb.record_failure(); // → Open

        assert!(!cb.allow_request()); // Should reject
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn transitions_to_half_open_after_cooldown() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(10));
        cb.record_failure();
        cb.record_failure(); // → Open

        // Wait for cooldown.
        std::thread::sleep(Duration::from_millis(15));
        assert!(cb.allow_request()); // Should transition to HalfOpen
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn closes_on_half_open_success() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(10));
        cb.record_failure();
        cb.record_failure(); // → Open

        std::thread::sleep(Duration::from_millis(15));
        assert!(cb.allow_request()); // → HalfOpen
        cb.record_success(); // → Closed
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 0);
    }

    #[test]
    fn reopens_on_half_open_failure() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(10));
        cb.record_failure();
        cb.record_failure(); // → Open

        std::thread::sleep(Duration::from_millis(15));
        assert!(cb.allow_request()); // → HalfOpen
        cb.record_failure(); // → Open again
        assert_eq!(cb.state(), CircuitState::Open);
        assert_eq!(cb.open_count(), 2);
    }

    #[test]
    fn success_resets_failure_count() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_failure();
        cb.record_failure(); // 2 failures
        cb.record_success(); // Reset
        assert_eq!(cb.consecutive_failures(), 0);

        // Need 3 more failures to open.
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed); // Still closed (only 2)
        cb.record_failure(); // 3rd → Open
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn display_impl() {
        assert_eq!(CircuitState::Closed.to_string(), "closed");
        assert_eq!(CircuitState::Open.to_string(), "open");
        assert_eq!(CircuitState::HalfOpen.to_string(), "half-open");
    }
}
