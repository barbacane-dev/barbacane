//! Circuit breaker implementation for upstream resilience.
//!
//! Implements a simple circuit breaker pattern:
//! - **Closed**: Normal operation, requests flow through
//! - **Open**: Failures exceeded threshold, requests fail fast (503)
//! - **Half-Open**: After reset timeout, allow one request through to test

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation.
    Closed,
    /// Failing fast.
    Open,
    /// Testing with single request.
    HalfOpen,
}

/// Circuit breaker configuration.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening the circuit.
    pub failure_threshold: u32,
    /// Time window for counting failures.
    pub failure_window: Duration,
    /// Time to wait before transitioning to half-open.
    pub reset_timeout: Duration,
    /// Number of successes in half-open to close the circuit.
    pub success_threshold: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            failure_window: Duration::from_secs(60),
            reset_timeout: Duration::from_secs(30),
            success_threshold: 1,
        }
    }
}

/// Circuit breaker for a single upstream.
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    /// Current failure count within the window.
    failure_count: AtomicU32,
    /// Timestamp of first failure in current window (millis since epoch).
    window_start: AtomicU64,
    /// Timestamp when circuit opened (millis since epoch).
    opened_at: AtomicU64,
    /// Success count in half-open state.
    half_open_successes: AtomicU32,
    /// Lock for state transitions.
    state_lock: Mutex<CircuitState>,
    /// Reference time for relative calculations.
    epoch: Instant,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            failure_count: AtomicU32::new(0),
            window_start: AtomicU64::new(0),
            opened_at: AtomicU64::new(0),
            half_open_successes: AtomicU32::new(0),
            state_lock: Mutex::new(CircuitState::Closed),
            epoch: Instant::now(),
        }
    }

    /// Get the current circuit state.
    pub fn state(&self) -> CircuitState {
        let mut state = self.state_lock.lock();
        self.evaluate_state(&mut state);
        *state
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        let mut state = self.state_lock.lock();
        self.evaluate_state(&mut state);

        match *state {
            CircuitState::Closed => {
                // Reset failure count on success
                self.failure_count.store(0, Ordering::SeqCst);
                self.window_start.store(0, Ordering::SeqCst);
            }
            CircuitState::HalfOpen => {
                // Count successes to close the circuit
                let successes = self.half_open_successes.fetch_add(1, Ordering::SeqCst) + 1;
                if successes >= self.config.success_threshold {
                    *state = CircuitState::Closed;
                    self.failure_count.store(0, Ordering::SeqCst);
                    self.window_start.store(0, Ordering::SeqCst);
                    self.half_open_successes.store(0, Ordering::SeqCst);
                    tracing::info!("circuit breaker closed after successful recovery");
                }
            }
            CircuitState::Open => {
                // Shouldn't happen - no requests in open state
            }
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        let mut state = self.state_lock.lock();
        self.evaluate_state(&mut state);

        let now = self.now_millis();

        match *state {
            CircuitState::Closed => {
                let window_start = self.window_start.load(Ordering::SeqCst);
                let window_ms = self.config.failure_window.as_millis() as u64;

                // Check if we're in a new window
                if window_start == 0 || now > window_start + window_ms {
                    // Start new window
                    self.window_start.store(now, Ordering::SeqCst);
                    self.failure_count.store(1, Ordering::SeqCst);

                    // Check if threshold is 1 (immediate open)
                    if self.config.failure_threshold == 1 {
                        *state = CircuitState::Open;
                        self.opened_at.store(now, Ordering::SeqCst);
                        tracing::warn!(
                            failures = 1,
                            threshold = self.config.failure_threshold,
                            "circuit breaker opened"
                        );
                    }
                } else {
                    // Increment in current window
                    let count = self.failure_count.fetch_add(1, Ordering::SeqCst) + 1;

                    if count >= self.config.failure_threshold {
                        *state = CircuitState::Open;
                        self.opened_at.store(now, Ordering::SeqCst);
                        tracing::warn!(
                            failures = count,
                            threshold = self.config.failure_threshold,
                            "circuit breaker opened"
                        );
                    }
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in half-open reopens the circuit
                *state = CircuitState::Open;
                self.opened_at.store(now, Ordering::SeqCst);
                self.half_open_successes.store(0, Ordering::SeqCst);
                tracing::warn!("circuit breaker reopened after half-open failure");
            }
            CircuitState::Open => {
                // Already open, nothing to do
            }
        }
    }

    /// Evaluate and potentially transition state.
    fn evaluate_state(&self, state: &mut CircuitState) {
        if *state == CircuitState::Open {
            let now = self.now_millis();
            let opened_at = self.opened_at.load(Ordering::SeqCst);
            let reset_ms = self.config.reset_timeout.as_millis() as u64;

            if now > opened_at + reset_ms {
                *state = CircuitState::HalfOpen;
                self.half_open_successes.store(0, Ordering::SeqCst);
                tracing::info!("circuit breaker transitioned to half-open");
            }
        }
    }

    /// Get current time in milliseconds since epoch.
    /// Returns at least 1 to avoid confusion with uninitialized state.
    fn now_millis(&self) -> u64 {
        let elapsed = self.epoch.elapsed().as_millis() as u64;
        // Return at least 1 to distinguish from uninitialized (0)
        elapsed.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_initial_state_closed() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_opens_after_threshold() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            failure_window: Duration::from_secs(60),
            reset_timeout: Duration::from_secs(30),
            success_threshold: 1,
        };
        let cb = CircuitBreaker::new(config);

        // Record three failures in a row
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();

        // Circuit should be open after reaching threshold
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_success_resets_count() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            failure_window: Duration::from_secs(60),
            reset_timeout: Duration::from_secs(30),
            success_threshold: 1,
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        cb.record_failure();
        cb.record_success(); // Resets count

        cb.record_failure();
        cb.record_failure();
        // Should still be closed (only 2 failures since reset)
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_after_timeout() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            failure_window: Duration::from_secs(60),
            reset_timeout: Duration::from_millis(50),
            success_threshold: 1,
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        // With threshold=1, should be open immediately
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for reset timeout
        sleep(Duration::from_millis(100));
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_closes_after_half_open_success() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            failure_window: Duration::from_secs(60),
            reset_timeout: Duration::from_millis(20),
            success_threshold: 1,
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for reset timeout
        sleep(Duration::from_millis(50));
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_reopens_on_half_open_failure() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            failure_window: Duration::from_secs(60),
            reset_timeout: Duration::from_millis(20),
            success_threshold: 1,
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        // Wait for reset timeout
        sleep(Duration::from_millis(50));
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }
}
