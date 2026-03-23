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
        let mut inner = self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("circuit breaker state lock poisoned, recovering");
            poisoned.into_inner()
        });
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
        let mut inner = self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("circuit breaker state lock poisoned, recovering");
            poisoned.into_inner()
        });
        if inner.state == CircuitState::HalfOpen {
            tracing::info!("circuit breaker closing after successful probe");
        }
        inner.consecutive_failures = 0;
        inner.state = CircuitState::Closed;
        inner.opened_at = None;
    }

    /// Record a failed request. Increments failure count and may open circuit.
    pub fn record_failure(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("circuit breaker state lock poisoned, recovering");
            poisoned.into_inner()
        });
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
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::warn!("circuit breaker state lock poisoned, recovering");
                poisoned.into_inner()
            })
            .state
    }

    /// Number of consecutive failures since the last success.
    pub fn consecutive_failures(&self) -> u32 {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::warn!("circuit breaker state lock poisoned, recovering");
                poisoned.into_inner()
            })
            .consecutive_failures
    }

    /// Total number of times the circuit has opened (lifetime counter).
    pub fn open_count(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::warn!("circuit breaker state lock poisoned, recovering");
                poisoned.into_inner()
            })
            .open_count
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

    // ── 1. Initial state ──────────────────────────────────────────────

    #[test]
    fn starts_closed() {
        let cb = CircuitBreaker::with_defaults();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn starts_with_zero_failures_and_zero_open_count() {
        let cb = CircuitBreaker::with_defaults();
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.open_count(), 0);
    }

    #[test]
    fn default_trait_creates_same_as_with_defaults() {
        let cb: CircuitBreaker = CircuitBreaker::default();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.open_count(), 0);
        assert!(cb.allow_request());
    }

    #[test]
    fn custom_thresholds_start_closed() {
        let cb = CircuitBreaker::new(10, Duration::from_secs(120));
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.open_count(), 0);
    }

    // ── 2. Failure threshold ──────────────────────────────────────────

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
    fn stays_closed_below_threshold() {
        let cb = CircuitBreaker::new(5, Duration::from_secs(30));
        for _ in 0..4 {
            cb.record_failure();
        }
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 4);
    }

    #[test]
    fn threshold_of_one_opens_on_first_failure() {
        let cb = CircuitBreaker::new(1, Duration::from_secs(30));
        assert_eq!(cb.state(), CircuitState::Closed);
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert_eq!(cb.open_count(), 1);
    }

    #[test]
    fn consecutive_failures_tracks_count_accurately() {
        let cb = CircuitBreaker::new(10, Duration::from_secs(30));
        for i in 1..=7 {
            cb.record_failure();
            assert_eq!(cb.consecutive_failures(), i);
        }
    }

    #[test]
    fn failures_beyond_threshold_stay_open_and_keep_counting() {
        let cb = CircuitBreaker::new(2, Duration::from_secs(300));
        cb.record_failure();
        cb.record_failure(); // → Open
        assert_eq!(cb.state(), CircuitState::Open);

        // Additional failures while Open still increment the counter.
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.consecutive_failures(), 4);
        assert_eq!(cb.state(), CircuitState::Open);
        // open_count should not increment for failures while already Open.
        assert_eq!(cb.open_count(), 1);
    }

    // ── 3. Open state blocks ──────────────────────────────────────────

    #[test]
    fn rejects_when_open() {
        let cb = CircuitBreaker::new(2, Duration::from_secs(300));
        cb.record_failure();
        cb.record_failure(); // → Open

        assert!(!cb.allow_request()); // Should reject
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn rejects_multiple_times_while_open() {
        let cb = CircuitBreaker::new(1, Duration::from_secs(300));
        cb.record_failure(); // → Open

        for _ in 0..10 {
            assert!(!cb.allow_request());
            assert_eq!(cb.state(), CircuitState::Open);
        }
    }

    // ── 4. Cooldown transition to HalfOpen ────────────────────────────

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
    fn does_not_transition_before_cooldown_expires() {
        let cb = CircuitBreaker::new(1, Duration::from_secs(60));
        cb.record_failure(); // → Open

        // Cooldown is 60 seconds — calling immediately should still block.
        assert!(!cb.allow_request());
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn half_open_still_allows_requests() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(5));
        cb.record_failure(); // → Open

        std::thread::sleep(Duration::from_millis(10));
        assert!(cb.allow_request()); // → HalfOpen
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // A second allow_request in HalfOpen should also return true.
        assert!(cb.allow_request());
    }

    // ── 5. HalfOpen success → Closed ──────────────────────────────────

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
    fn closed_after_half_open_success_allows_requests_normally() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(5));
        cb.record_failure();
        cb.record_failure(); // → Open

        std::thread::sleep(Duration::from_millis(10));
        assert!(cb.allow_request()); // → HalfOpen
        cb.record_success(); // → Closed

        // Should behave like a fresh circuit breaker now.
        assert!(cb.allow_request());
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 0);
    }

    #[test]
    fn open_count_preserved_after_half_open_recovery() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(5));
        cb.record_failure(); // → Open, open_count = 1

        std::thread::sleep(Duration::from_millis(10));
        cb.allow_request(); // → HalfOpen
        cb.record_success(); // → Closed

        // open_count is a lifetime counter — should still be 1.
        assert_eq!(cb.open_count(), 1);
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    // ── 6. HalfOpen failure → Open ────────────────────────────────────

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
    fn half_open_failure_resets_cooldown() {
        // Use a longer cooldown (500ms) so the immediate check after
        // record_failure() can't race past the cooldown window even on
        // a slow CI runner.
        let cb = CircuitBreaker::new(1, Duration::from_millis(500));
        cb.record_failure(); // → Open (open_count = 1)

        std::thread::sleep(Duration::from_millis(600));
        cb.allow_request(); // → HalfOpen
        cb.record_failure(); // → Open again (open_count = 2), fresh cooldown

        // Immediately after reopening, cooldown has NOT expired.
        assert!(!cb.allow_request());
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for the new cooldown.
        std::thread::sleep(Duration::from_millis(600));
        assert!(cb.allow_request()); // → HalfOpen again
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    // ── 7. Success resets failure count ───────────────────────────────

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
    fn success_in_closed_state_keeps_closed() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 0);
    }

    #[test]
    fn interleaved_successes_prevent_opening() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(30));

        // 2 failures, then success, repeated — should never open.
        for _ in 0..10 {
            cb.record_failure();
            cb.record_failure();
            cb.record_success();
        }
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.open_count(), 0);
    }

    // ── 8. Accessor methods ──────────────────────────────────────────

    #[test]
    fn state_accessor_returns_correct_values() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(5));

        // Closed
        assert_eq!(cb.state(), CircuitState::Closed);

        // Open
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // HalfOpen
        std::thread::sleep(Duration::from_millis(10));
        cb.allow_request();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Back to Closed
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn consecutive_failures_accessor_tracks_correctly() {
        let cb = CircuitBreaker::new(10, Duration::from_secs(30));

        assert_eq!(cb.consecutive_failures(), 0);
        cb.record_failure();
        assert_eq!(cb.consecutive_failures(), 1);
        cb.record_failure();
        assert_eq!(cb.consecutive_failures(), 2);
        cb.record_success();
        assert_eq!(cb.consecutive_failures(), 0);
    }

    #[test]
    fn open_count_is_lifetime_counter() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(5));

        assert_eq!(cb.open_count(), 0);

        // First trip.
        cb.record_failure(); // → Open
        assert_eq!(cb.open_count(), 1);

        // Recover.
        std::thread::sleep(Duration::from_millis(10));
        cb.allow_request(); // → HalfOpen
        cb.record_success(); // → Closed
        assert_eq!(cb.open_count(), 1);

        // Second trip.
        cb.record_failure(); // → Open
        assert_eq!(cb.open_count(), 2);

        // Recover again.
        std::thread::sleep(Duration::from_millis(10));
        cb.allow_request(); // → HalfOpen
        cb.record_success(); // → Closed
        assert_eq!(cb.open_count(), 2);

        // Third trip.
        cb.record_failure(); // → Open
        assert_eq!(cb.open_count(), 3);
    }

    // ── Display impl ─────────────────────────────────────────────────

    #[test]
    fn display_impl() {
        assert_eq!(CircuitState::Closed.to_string(), "closed");
        assert_eq!(CircuitState::Open.to_string(), "open");
        assert_eq!(CircuitState::HalfOpen.to_string(), "half-open");
    }

    // ── Full lifecycle ───────────────────────────────────────────────

    #[test]
    fn full_lifecycle_closed_open_half_open_closed() {
        let cb = CircuitBreaker::new(3, Duration::from_millis(5));

        // Phase 1: Closed — requests allowed, failures accumulate.
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
        cb.record_failure();
        cb.record_failure();
        cb.record_failure(); // → Open
        assert_eq!(cb.state(), CircuitState::Open);
        assert_eq!(cb.consecutive_failures(), 3);
        assert_eq!(cb.open_count(), 1);

        // Phase 2: Open — requests blocked.
        assert!(!cb.allow_request());

        // Phase 3: Cooldown expires → HalfOpen.
        std::thread::sleep(Duration::from_millis(10));
        assert!(cb.allow_request());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Phase 4: Probe succeeds → Closed.
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.open_count(), 1);
        assert!(cb.allow_request());
    }

    #[test]
    fn multiple_open_close_cycles() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(5));

        for cycle in 1..=5u64 {
            // Trip the breaker.
            cb.record_failure();
            assert_eq!(cb.state(), CircuitState::Open);
            assert_eq!(cb.open_count(), cycle);

            // Wait, probe, recover.
            std::thread::sleep(Duration::from_millis(10));
            assert!(cb.allow_request()); // → HalfOpen
            cb.record_success(); // → Closed
            assert_eq!(cb.state(), CircuitState::Closed);
        }
        assert_eq!(cb.open_count(), 5);
    }

    // ── CircuitState derive traits ───────────────────────────────────

    #[test]
    fn circuit_state_debug_format() {
        // Verify Debug derive works.
        let debug_str = format!("{:?}", CircuitState::Closed);
        assert_eq!(debug_str, "Closed");
        assert_eq!(format!("{:?}", CircuitState::Open), "Open");
        assert_eq!(format!("{:?}", CircuitState::HalfOpen), "HalfOpen");
    }

    #[test]
    fn circuit_state_clone_and_copy() {
        let state = CircuitState::Open;
        let cloned = state;
        let copied = state; // Copy
        assert_eq!(state, cloned);
        assert_eq!(state, copied);
    }

    #[test]
    fn circuit_state_equality() {
        assert_eq!(CircuitState::Closed, CircuitState::Closed);
        assert_ne!(CircuitState::Closed, CircuitState::Open);
        assert_ne!(CircuitState::Open, CircuitState::HalfOpen);
        assert_ne!(CircuitState::Closed, CircuitState::HalfOpen);
    }
}
