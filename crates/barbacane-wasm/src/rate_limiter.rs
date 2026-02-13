//! Rate limiter with sliding window algorithm.
//!
//! This module provides thread-safe rate limiting with a sliding log
//! algorithm for accurate per-window rate limiting.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Result of a rate limit check.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RateLimitResult {
    /// Whether the request is allowed.
    pub allowed: bool,
    /// Remaining requests in the current window.
    pub remaining: u32,
    /// Unix timestamp when the window resets.
    pub reset: u64,
    /// The quota limit.
    pub limit: u32,
    /// Retry-After in seconds (only set when blocked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

/// Sliding window entry for a single key.
struct SlidingWindow {
    /// Timestamps of requests within the current window.
    timestamps: Vec<Instant>,
    /// Window duration.
    window: Duration,
}

impl SlidingWindow {
    fn new(window: Duration) -> Self {
        Self {
            timestamps: Vec::new(),
            window,
        }
    }

    /// Check if a request is allowed and record it if so.
    fn check_and_record(&mut self, quota: u32) -> RateLimitResult {
        let now = Instant::now();
        let window_start = now - self.window;

        // Remove expired timestamps
        self.timestamps.retain(|&t| t > window_start);

        // Calculate reset time (end of current window from first request)
        let reset_instant = if let Some(&first) = self.timestamps.first() {
            first + self.window
        } else {
            now + self.window
        };

        // Convert to Unix timestamp
        let reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + reset_instant.saturating_duration_since(now).as_secs();

        let count = self.timestamps.len() as u32;

        if count < quota {
            // Request allowed
            self.timestamps.push(now);
            RateLimitResult {
                allowed: true,
                remaining: quota - count - 1,
                reset,
                limit: quota,
                retry_after: None,
            }
        } else {
            // Request blocked
            let retry_after = reset_instant.saturating_duration_since(now).as_secs();
            RateLimitResult {
                allowed: false,
                remaining: 0,
                reset,
                limit: quota,
                retry_after: Some(retry_after.max(1)),
            }
        }
    }
}

/// Thread-safe rate limiter using sliding window algorithm.
#[derive(Clone)]
pub struct RateLimiter {
    /// Partitioned rate limit windows: partition_key -> window
    windows: Arc<RwLock<HashMap<String, SlidingWindow>>>,
    /// Cleanup threshold: remove entries older than this
    cleanup_threshold: Duration,
    /// Last cleanup time
    last_cleanup: Arc<RwLock<Instant>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    /// Create a new rate limiter.
    pub fn new() -> Self {
        Self {
            windows: Arc::new(RwLock::new(HashMap::new())),
            cleanup_threshold: Duration::from_secs(300), // 5 minutes
            last_cleanup: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Check if a request is allowed for the given key.
    ///
    /// # Arguments
    /// * `key` - Partition key (e.g., client IP, user ID)
    /// * `quota` - Maximum requests allowed in the window
    /// * `window_secs` - Window duration in seconds
    pub fn check(&self, key: &str, quota: u32, window_secs: u64) -> RateLimitResult {
        let window_duration = Duration::from_secs(window_secs);

        // Periodic cleanup
        self.maybe_cleanup();

        // Check and update window
        let mut windows = self.windows.write();

        let window = windows
            .entry(key.to_string())
            .or_insert_with(|| SlidingWindow::new(window_duration));

        // Update window duration if changed
        window.window = window_duration;

        window.check_and_record(quota)
    }

    /// Periodically clean up old entries to prevent memory bloat.
    fn maybe_cleanup(&self) {
        let now = Instant::now();

        // Check if we need to cleanup (every cleanup_threshold duration)
        {
            let last = self.last_cleanup.read();
            if now.duration_since(*last) < self.cleanup_threshold {
                return;
            }
        }

        // Try to acquire write lock for cleanup
        if let Some(mut last) = self.last_cleanup.try_write() {
            // Double-check after acquiring lock
            if now.duration_since(*last) >= self.cleanup_threshold {
                *last = now;

                // Cleanup old windows
                if let Some(mut windows) = self.windows.try_write() {
                    let threshold = now - self.cleanup_threshold;
                    windows.retain(|_, window| {
                        // Keep if any timestamp is recent
                        window.timestamps.iter().any(|&t| t > threshold)
                    });
                }
            }
        }
    }

    /// Get statistics about the rate limiter.
    pub fn stats(&self) -> RateLimiterStats {
        let windows = self.windows.read();
        RateLimiterStats {
            active_keys: windows.len(),
        }
    }
}

/// Statistics about the rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiterStats {
    /// Number of active partition keys.
    pub active_keys: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_rate_limiting() {
        let limiter = RateLimiter::new();

        // Allow first 3 requests
        for i in 0..3 {
            let result = limiter.check("test-key", 3, 60);
            assert!(result.allowed, "request {} should be allowed", i);
            assert_eq!(result.remaining, 2 - i);
            assert_eq!(result.limit, 3);
        }

        // Block 4th request
        let result = limiter.check("test-key", 3, 60);
        assert!(!result.allowed, "request 4 should be blocked");
        assert_eq!(result.remaining, 0);
        assert!(result.retry_after.is_some());
    }

    #[test]
    fn test_different_keys() {
        let limiter = RateLimiter::new();

        // Each key has its own quota
        let result1 = limiter.check("key1", 2, 60);
        let result2 = limiter.check("key2", 2, 60);

        assert!(result1.allowed);
        assert!(result2.allowed);
        assert_eq!(result1.remaining, 1);
        assert_eq!(result2.remaining, 1);
    }

    #[test]
    fn test_stats() {
        let limiter = RateLimiter::new();

        limiter.check("key1", 10, 60);
        limiter.check("key2", 10, 60);
        limiter.check("key3", 10, 60);

        let stats = limiter.stats();
        assert_eq!(stats.active_keys, 3);
    }
}
