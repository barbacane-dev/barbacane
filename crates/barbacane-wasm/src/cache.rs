//! Response cache with TTL support.
//!
//! This module provides an in-memory response cache for the cache middleware.
//! Entries are keyed by (path, method, vary_headers) and include TTL expiration.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A cached response entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheEntry {
    /// The cached response status code.
    pub status: u16,
    /// The cached response headers.
    pub headers: HashMap<String, String>,
    /// The cached response body.
    pub body: Option<String>,
    /// Cache metadata for debugging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<CacheMetadata>,
}

/// Cache metadata for debugging and headers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheMetadata {
    /// When the entry was created (Unix timestamp).
    pub created_at: u64,
    /// When the entry expires (Unix timestamp).
    pub expires_at: u64,
    /// Remaining TTL in seconds.
    pub ttl_remaining: u64,
}

/// Internal cache entry with expiration.
struct InternalEntry {
    entry: CacheEntry,
    expires_at: Instant,
    created_at: Instant,
    _ttl_secs: u64,
}

/// Result of a cache lookup.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheResult {
    /// Whether there was a cache hit.
    pub hit: bool,
    /// The cached entry (only set on hit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<CacheEntry>,
}

/// Thread-safe response cache.
#[derive(Clone)]
pub struct ResponseCache {
    /// Cached entries by key.
    entries: Arc<RwLock<HashMap<String, InternalEntry>>>,
    /// Cleanup interval.
    cleanup_interval: Duration,
    /// Last cleanup time.
    last_cleanup: Arc<RwLock<Instant>>,
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseCache {
    /// Create a new response cache.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            cleanup_interval: Duration::from_secs(60),
            last_cleanup: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Get a cache entry by key.
    pub fn get(&self, key: &str) -> CacheResult {
        self.maybe_cleanup();

        let entries = self.entries.read();
        let now = Instant::now();

        if let Some(internal) = entries.get(key) {
            if internal.expires_at > now {
                // Cache hit
                let ttl_remaining = internal.expires_at.saturating_duration_since(now).as_secs();
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let mut entry = internal.entry.clone();
                entry.metadata = Some(CacheMetadata {
                    created_at: now_unix - internal.created_at.elapsed().as_secs(),
                    expires_at: now_unix + ttl_remaining,
                    ttl_remaining,
                });

                return CacheResult {
                    hit: true,
                    entry: Some(entry),
                };
            }
        }

        // Cache miss
        CacheResult {
            hit: false,
            entry: None,
        }
    }

    /// Set a cache entry with TTL.
    pub fn set(&self, key: &str, entry: CacheEntry, ttl_secs: u64) {
        let now = Instant::now();
        let expires_at = now + Duration::from_secs(ttl_secs);

        let internal = InternalEntry {
            entry,
            expires_at,
            created_at: now,
            _ttl_secs: ttl_secs,
        };

        let mut entries = self.entries.write();
        entries.insert(key.to_string(), internal);
    }

    /// Invalidate a cache entry.
    pub fn invalidate(&self, key: &str) {
        let mut entries = self.entries.write();
        entries.remove(key);
    }

    /// Clear all cache entries.
    pub fn clear(&self) {
        let mut entries = self.entries.write();
        entries.clear();
    }

    /// Periodically clean up expired entries.
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

                if let Some(mut entries) = self.entries.try_write() {
                    entries.retain(|_, v| v.expires_at > now);
                }
            }
        }
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        let entries = self.entries.read();
        let now = Instant::now();
        let valid_count = entries.values().filter(|e| e.expires_at > now).count();

        CacheStats {
            total_entries: entries.len(),
            valid_entries: valid_count,
        }
    }
}

/// Cache statistics.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Total number of entries (including expired).
    pub total_entries: usize,
    /// Number of non-expired entries.
    pub valid_entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_miss() {
        let cache = ResponseCache::new();
        let result = cache.get("test-key");
        assert!(!result.hit);
        assert!(result.entry.is_none());
    }

    #[test]
    fn test_cache_hit() {
        let cache = ResponseCache::new();

        let entry = CacheEntry {
            status: 200,
            headers: HashMap::new(),
            body: Some("test body".to_string()),
            metadata: None,
        };

        cache.set("test-key", entry, 60);

        let result = cache.get("test-key");
        assert!(result.hit);
        assert!(result.entry.is_some());
        let cached = result.entry.unwrap();
        assert_eq!(cached.status, 200);
        assert_eq!(cached.body, Some("test body".to_string()));
    }

    #[test]
    fn test_cache_invalidate() {
        let cache = ResponseCache::new();

        let entry = CacheEntry {
            status: 200,
            headers: HashMap::new(),
            body: None,
            metadata: None,
        };

        cache.set("test-key", entry, 60);
        assert!(cache.get("test-key").hit);

        cache.invalidate("test-key");
        assert!(!cache.get("test-key").hit);
    }

    #[test]
    fn test_cache_stats() {
        let cache = ResponseCache::new();

        let entry = CacheEntry {
            status: 200,
            headers: HashMap::new(),
            body: None,
            metadata: None,
        };

        cache.set("key1", entry.clone(), 60);
        cache.set("key2", entry.clone(), 60);
        cache.set("key3", entry, 60);

        let stats = cache.stats();
        assert_eq!(stats.total_entries, 3);
        assert_eq!(stats.valid_entries, 3);
    }
}
