//! Rate limiter with sliding window algorithm.
//!
//! This module provides thread-safe rate limiting with a sliding log
//! algorithm for accurate per-window rate limiting.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Upper bound on a rate-limit window. The window is plugin-controlled, so it is
/// clamped before being turned into a Duration to keep `Instant` arithmetic out
/// of overflow territory (1 year is far beyond any sane window).
const MAX_WINDOW_SECS: u64 = 366 * 24 * 60 * 60;

/// Upper bound on the plugin-controlled quota. Caps how many timestamps a single
/// window can retain, bounding per-window memory.
const MAX_QUOTA: u32 = 1_000_000;

/// Upper bound on the number of distinct partition keys tracked at once. Bounds
/// the total memory an attacker can force by varying the partition key.
const MAX_PARTITIONS: usize = 100_000;

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

        // Remove expired timestamps. Compare via `duration_since` (recorded
        // timestamps are always <= now on the monotonic clock) instead of
        // `now - window`, which underflow-panics when the window exceeds the
        // process's monotonic-clock uptime.
        self.timestamps
            .retain(|&t| now.duration_since(t) < self.window);

        // Calculate reset time (end of current window from first request).
        // checked_add guards against `Instant + Duration` overflow.
        let reset_instant = self
            .timestamps
            .first()
            .copied()
            .unwrap_or(now)
            .checked_add(self.window)
            .unwrap_or(now);

        // Convert to Unix timestamp
        let reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .saturating_add(reset_instant.saturating_duration_since(now).as_secs());

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
        // Clamp the plugin-controlled window so the Duration can't drive
        // `Instant` arithmetic into overflow territory (1 year is far beyond
        // any sane rate-limit window).
        let window_secs = window_secs.min(MAX_WINDOW_SECS);
        let window_duration = Duration::from_secs(window_secs);
        // Clamp the plugin-controlled quota so a huge value can't grow the
        // per-window timestamp log without bound.
        let quota = quota.min(MAX_QUOTA);

        // Periodic cleanup
        self.maybe_cleanup();

        // Check and update window
        let mut windows = self.windows.write();

        // Bound the partition table: an attacker-controlled partition key (e.g.
        // a spoofable header) could otherwise create unbounded windows. When the
        // table is full of a fresh key, evict stale partitions first; if it is
        // still saturated with active keys, fail closed for the new key.
        if windows.len() >= MAX_PARTITIONS && !windows.contains_key(key) {
            let now = Instant::now();
            windows.retain(|_, w| {
                w.timestamps
                    .iter()
                    .any(|&t| now.duration_since(t) < self.cleanup_threshold)
            });
            if windows.len() >= MAX_PARTITIONS {
                let reset = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
                    .saturating_add(window_secs);
                return RateLimitResult {
                    allowed: false,
                    remaining: 0,
                    reset,
                    limit: quota,
                    retry_after: Some(window_secs.max(1)),
                };
            }
        }

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

                // Cleanup old windows. Compare via `duration_since` (recorded
                // timestamps are <= now) rather than `now - threshold`, which
                // underflow-panics early in the process's uptime.
                if let Some(mut windows) = self.windows.try_write() {
                    windows.retain(|_, window| {
                        // Keep if any timestamp is recent
                        window
                            .timestamps
                            .iter()
                            .any(|&t| now.duration_since(t) < self.cleanup_threshold)
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
    fn quota_is_clamped() {
        let limiter = RateLimiter::new();
        // A huge plugin-supplied quota is clamped to MAX_QUOTA.
        let result = limiter.check("clamp-key", u32::MAX, 60);
        assert!(result.allowed);
        assert_eq!(result.limit, MAX_QUOTA);
    }

    #[test]
    fn huge_window_does_not_panic() {
        let limiter = RateLimiter::new();
        // u64::MAX seconds would overflow Instant arithmetic if not clamped.
        let result = limiter.check("big-window", 5, u64::MAX);
        assert!(result.allowed);
    }

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
