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

/// Upper bound on the partitions one scope (a configured rate-limit entry)
/// tracks at once. Bounds the memory a client can force by varying the
/// partition key, and keeps a flood on one entry from reaching the others.
const MAX_PARTITIONS: usize = 100_000;

/// A partition key longer than this is stored as its SHA-256, so a key's
/// length does not grow the table.
const MAX_KEY_BYTES: usize = 256;

/// The key a partition is stored under: the key itself, or the SHA-256 of a
/// key over [`MAX_KEY_BYTES`], after a NUL no header value carries.
fn stored_key(key: &str) -> String {
    if key.len() <= MAX_KEY_BYTES {
        return key.to_string();
    }
    use sha2::{Digest, Sha256};
    format!(
        "\u{0}sha256:{}",
        hex::encode(Sha256::digest(key.as_bytes()))
    )
}

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

    /// Whether a request recorded in this window still counts at `now`.
    fn is_live(&self, now: Instant) -> bool {
        self.timestamps
            .last()
            .is_some_and(|&t| now.duration_since(t) < self.window)
    }

    /// When the window's last request stops counting.
    fn expires_at(&self) -> Option<Instant> {
        self.timestamps
            .last()
            .and_then(|t| t.checked_add(self.window))
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

/// The partitions of one scope.
#[derive(Default)]
struct Table {
    /// One log per (key, window length), so calls with the same key and
    /// different windows never count against, or prune, each other.
    windows: HashMap<(String, u64), SlidingWindow>,
    /// While the table is full of live windows, the earliest time one of them
    /// can expire; a new partition is refused before then without a scan.
    full_until: Option<Instant>,
    /// Scans of a full table.
    #[cfg(test)]
    scans: usize,
}

/// Thread-safe rate limiter using sliding window algorithm.
///
/// Partitions are kept per scope, the configured entry calling it, so each
/// scope's table is bounded on its own.
#[derive(Clone)]
pub struct RateLimiter {
    tables: Arc<RwLock<HashMap<String, Table>>>,
    /// The most partitions a scope's table holds.
    max_partitions: usize,
    /// How often windows with no request left in them are removed.
    cleanup_interval: Duration,
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
        Self::with_cleanup_interval(Duration::from_secs(300))
    }

    fn with_cleanup_interval(cleanup_interval: Duration) -> Self {
        Self {
            tables: Arc::new(RwLock::new(HashMap::new())),
            max_partitions: MAX_PARTITIONS,
            cleanup_interval,
            last_cleanup: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Check if a request is allowed for the given key.
    ///
    /// # Arguments
    /// * `scope` - The configured entry the partition belongs to
    /// * `key` - Partition key (e.g., client IP, user ID)
    /// * `quota` - Maximum requests allowed in the window
    /// * `window_secs` - Window duration in seconds
    pub fn check(&self, scope: &str, key: &str, quota: u32, window_secs: u64) -> RateLimitResult {
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

        let mut tables = self.tables.write();
        let table = tables.entry(scope.to_string()).or_default();

        // Bound the scope's table: an attacker-controlled partition key (e.g. a
        // spoofable header) could otherwise create unbounded windows. When the
        // table is full, windows whose requests expired are dropped; if it is
        // still full of live ones, the new partition is refused, and refused
        // without another scan until one of them can expire.
        let window_key = (stored_key(key), window_secs);
        if table.windows.len() >= self.max_partitions && !table.windows.contains_key(&window_key) {
            let now = Instant::now();
            let still_full = table.full_until.is_some_and(|until| now < until) || {
                #[cfg(test)]
                {
                    table.scans += 1;
                }
                table.windows.retain(|_, w| w.is_live(now));
                table.full_until = (table.windows.len() >= self.max_partitions)
                    .then(|| {
                        table
                            .windows
                            .values()
                            .filter_map(SlidingWindow::expires_at)
                            .min()
                    })
                    .flatten();
                table.full_until.is_some()
            };
            if still_full {
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

        table
            .windows
            .entry(window_key)
            .or_insert_with(|| SlidingWindow::new(window_duration))
            .check_and_record(quota)
    }

    /// Periodically clean up old entries to prevent memory bloat.
    fn maybe_cleanup(&self) {
        let now = Instant::now();

        {
            let last = self.last_cleanup.read();
            if now.duration_since(*last) < self.cleanup_interval {
                return;
            }
        }

        if let Some(mut last) = self.last_cleanup.try_write() {
            if now.duration_since(*last) >= self.cleanup_interval {
                *last = now;
                if let Some(mut tables) = self.tables.try_write() {
                    for table in tables.values_mut() {
                        table.windows.retain(|_, window| window.is_live(now));
                        table.full_until = None;
                    }
                    tables.retain(|_, table| !table.windows.is_empty());
                }
            }
        }
    }

    /// Get statistics about the rate limiter.
    pub fn stats(&self) -> RateLimiterStats {
        let tables = self.tables.read();
        RateLimiterStats {
            active_keys: tables.values().map(|t| t.windows.len()).sum(),
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

    fn with_max_partitions(max_partitions: usize) -> RateLimiter {
        RateLimiter {
            max_partitions,
            ..RateLimiter::new()
        }
    }

    #[test]
    fn a_full_table_does_not_reach_another_scope() {
        let limiter = with_max_partitions(3);
        for key in ["a", "b", "c"] {
            assert!(limiter.check("flooded", key, 10, 3600).allowed);
        }
        assert!(!limiter.check("flooded", "d", 10, 3600).allowed);
        assert!(
            limiter.check("flooded", "a", 10, 3600).allowed,
            "a known partition"
        );
        assert!(limiter.check("other", "d", 10, 3600).allowed);
    }

    #[test]
    fn a_full_table_is_scanned_once_until_a_window_can_expire() {
        let limiter = with_max_partitions(2);
        assert!(limiter.check("s", "a", 10, 3600).allowed);
        assert!(limiter.check("s", "b", 10, 3600).allowed);
        for i in 0..100 {
            assert!(!limiter.check("s", &format!("new-{i}"), 10, 3600).allowed);
        }
        assert_eq!(limiter.tables.read()["s"].scans, 1);
    }

    #[test]
    fn a_full_table_takes_new_partitions_once_its_windows_expire() {
        let limiter = with_max_partitions(2);
        assert!(limiter.check("s", "a", 10, 1).allowed);
        assert!(limiter.check("s", "b", 10, 1).allowed);
        assert!(!limiter.check("s", "c", 10, 1).allowed);
        assert!(!limiter.check("s", "c", 10, 1).allowed, "still full");
        std::thread::sleep(Duration::from_millis(1100));
        assert!(limiter.check("s", "c", 10, 1).allowed);
    }

    #[test]
    fn a_long_key_is_stored_as_its_digest() {
        let long = "k".repeat(10_000);
        let other = format!("{}x", &long[..9_999]);
        assert_eq!(stored_key(&long).len(), 1 + "sha256:".len() + 64);
        assert_ne!(stored_key(&long), stored_key(&other));
        assert_eq!(stored_key("10.0.0.1"), "10.0.0.1");

        let limiter = RateLimiter::new();
        assert!(limiter.check("s", &long, 1, 60).allowed);
        assert!(!limiter.check("s", &long, 1, 60).allowed);
        assert!(limiter.check("s", &other, 1, 60).allowed);
    }

    #[test]
    fn quota_is_clamped() {
        let limiter = RateLimiter::new();
        // A huge plugin-supplied quota is clamped to MAX_QUOTA.
        let result = limiter.check("scope", "clamp-key", u32::MAX, 60);
        assert!(result.allowed);
        assert_eq!(result.limit, MAX_QUOTA);
    }

    #[test]
    fn huge_window_does_not_panic() {
        let limiter = RateLimiter::new();
        // u64::MAX seconds would overflow Instant arithmetic if not clamped.
        let result = limiter.check("scope", "big-window", 5, u64::MAX);
        assert!(result.allowed);
    }

    #[test]
    fn test_basic_rate_limiting() {
        let limiter = RateLimiter::new();

        // Allow first 3 requests
        for i in 0..3 {
            let result = limiter.check("scope", "test-key", 3, 60);
            assert!(result.allowed, "request {} should be allowed", i);
            assert_eq!(result.remaining, 2 - i);
            assert_eq!(result.limit, 3);
        }

        // Block 4th request
        let result = limiter.check("scope", "test-key", 3, 60);
        assert!(!result.allowed, "request 4 should be blocked");
        assert_eq!(result.remaining, 0);
        assert!(result.retry_after.is_some());
    }

    #[test]
    fn test_different_keys() {
        let limiter = RateLimiter::new();

        // Each key has its own quota
        let result1 = limiter.check("scope", "key1", 2, 60);
        let result2 = limiter.check("scope", "key2", 2, 60);

        assert!(result1.allowed);
        assert!(result2.allowed);
        assert_eq!(result1.remaining, 1);
        assert_eq!(result2.remaining, 1);
    }

    #[test]
    fn a_short_window_does_not_prune_a_long_one_on_the_same_key() {
        let limiter = RateLimiter::new();
        for _ in 0..2 {
            assert!(limiter.check("scope", "client", 2, 3600).allowed);
        }
        std::thread::sleep(Duration::from_millis(1100));
        assert!(limiter.check("scope", "client", 100, 1).allowed);

        assert!(!limiter.check("scope", "client", 2, 3600).allowed);
    }

    #[test]
    fn cleanup_keeps_a_window_while_its_requests_count() {
        let limiter = RateLimiter::with_cleanup_interval(Duration::from_millis(50));
        assert!(limiter.check("scope", "client", 1, 3600).allowed);
        std::thread::sleep(Duration::from_millis(100));
        limiter.check("scope", "someone-else", 1, 3600);

        assert!(!limiter.check("scope", "client", 1, 3600).allowed);
    }

    #[test]
    fn cleanup_removes_a_window_once_its_requests_expired() {
        let limiter = RateLimiter::with_cleanup_interval(Duration::from_millis(50));
        assert!(limiter.check("scope", "client", 1, 1).allowed);
        std::thread::sleep(Duration::from_millis(1100));
        limiter.check("scope", "someone-else", 1, 3600);

        assert_eq!(limiter.stats().active_keys, 1);
    }

    #[test]
    fn test_stats() {
        let limiter = RateLimiter::new();

        limiter.check("scope", "key1", 10, 60);
        limiter.check("scope", "key2", 10, 60);
        limiter.check("scope", "key3", 10, 60);

        let stats = limiter.stats();
        assert_eq!(stats.active_keys, 3);
    }
}
