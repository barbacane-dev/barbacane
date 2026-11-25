//! Response cache with TTL support.
//!
//! This module provides an in-memory response cache for the cache middleware.
//! Entries are keyed by (path, method, vary_headers) and include TTL expiration.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Upper bound on an entry's TTL. The TTL is plugin-controlled, so it is clamped
/// before being fed into `Instant + Duration` arithmetic to avoid an
/// overflow panic on an absurd value (1 year is well beyond any sane cache TTL).
const MAX_TTL_SECS: u64 = 366 * 24 * 60 * 60;

/// Upper bound on the number of cached entries. A plugin can write arbitrary
/// keys, so the table is capped to bound host memory; inserts past the cap evict
/// expired entries first, then the entries closest to expiry.
const MAX_CACHE_ENTRIES: usize = 10_000;

/// Upper bound on the bytes the cache holds: keys, headers and bodies. Inserts
/// past it evict as the entry cap does.
const MAX_CACHE_BYTES: usize = 256 * 1024 * 1024;

/// The largest entry the cache stores, in bytes; a larger one is not stored.
const MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;

/// A cached response entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheEntry {
    /// The cached response status code.
    pub status: u16,
    /// The cached response header fields, each with its values.
    pub headers: barbacane_plugin_sdk::types::ResponseHeaders,
    /// The cached response body (binary-safe via base64 in JSON).
    #[serde(with = "barbacane_plugin_sdk::types::base64_body")]
    pub body: Option<Vec<u8>>,
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
    /// What the entry counts against [`MAX_CACHE_BYTES`].
    size: usize,
}

/// The bytes an entry holds: its key, header names and values, and body.
fn entry_size(key: &str, entry: &CacheEntry) -> usize {
    let headers: usize = entry
        .headers
        .iter()
        .map(|(k, values)| k.len() + values.iter().map(String::len).sum::<usize>())
        .sum();
    key.len() + headers + entry.body.as_ref().map_or(0, Vec::len)
}

/// How much the cache holds.
#[derive(Debug, Clone, Copy)]
struct Limits {
    entries: usize,
    bytes: usize,
    entry_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: MAX_CACHE_ENTRIES,
            bytes: MAX_CACHE_BYTES,
            entry_bytes: MAX_ENTRY_BYTES,
        }
    }
}

/// The entries with the bytes they hold.
#[derive(Default)]
struct Entries {
    map: HashMap<String, InternalEntry>,
    bytes: usize,
}

impl Entries {
    fn remove(&mut self, key: &str) {
        if let Some(old) = self.map.remove(key) {
            self.bytes -= old.size;
        }
    }

    fn remove_expired(&mut self, now: Instant) {
        self.map.retain(|_, v| v.expires_at > now);
        self.bytes = self.map.values().map(|v| v.size).sum();
    }

    /// Evict until an entry of `size` bytes fits: expired entries first, then
    /// those closest to expiry.
    fn make_room(&mut self, size: usize, now: Instant, limits: Limits) {
        let full =
            |e: &Self| e.map.len() >= limits.entries || e.bytes.saturating_add(size) > limits.bytes;
        if !full(self) {
            return;
        }
        self.remove_expired(now);
        if !full(self) {
            return;
        }
        let mut by_expiry: Vec<(Instant, String)> = self
            .map
            .iter()
            .map(|(k, v)| (v.expires_at, k.clone()))
            .collect();
        by_expiry.sort_unstable();
        for (_, key) in by_expiry {
            if !full(self) {
                break;
            }
            self.remove(&key);
        }
    }
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
    entries: Arc<RwLock<Entries>>,
    limits: Limits,
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
            entries: Arc::new(RwLock::new(Entries::default())),
            limits: Limits::default(),
            cleanup_interval: Duration::from_secs(60),
            last_cleanup: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Get a cache entry by key.
    pub fn get(&self, key: &str) -> CacheResult {
        self.maybe_cleanup();

        let entries = self.entries.read();
        let now = Instant::now();

        if let Some(internal) = entries.map.get(key) {
            if internal.expires_at > now {
                // Cache hit
                let ttl_remaining = internal.expires_at.saturating_duration_since(now).as_secs();
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let mut entry = internal.entry.clone();
                entry.metadata = Some(CacheMetadata {
                    created_at: now_unix.saturating_sub(internal.created_at.elapsed().as_secs()),
                    expires_at: now_unix.saturating_add(ttl_remaining),
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
        // Clamp the plugin-controlled TTL and use checked arithmetic so a huge
        // value can't overflow-panic `Instant + Duration`.
        let ttl_secs = ttl_secs.min(MAX_TTL_SECS);
        let expires_at = now
            .checked_add(Duration::from_secs(ttl_secs))
            .unwrap_or(now);

        let size = entry_size(key, &entry);
        if size > self.limits.entry_bytes {
            return;
        }
        let internal = InternalEntry {
            entry,
            expires_at,
            created_at: now,
            _ttl_secs: ttl_secs,
            size,
        };

        let mut entries = self.entries.write();
        entries.remove(key);
        entries.make_room(size, now, self.limits);
        entries.bytes += size;
        entries.map.insert(key.to_string(), internal);
    }

    /// Invalidate a cache entry.
    pub fn invalidate(&self, key: &str) {
        self.entries.write().remove(key);
    }

    /// Clear all cache entries.
    pub fn clear(&self) {
        *self.entries.write() = Entries::default();
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
                    entries.remove_expired(now);
                }
            }
        }
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        let entries = self.entries.read();
        let now = Instant::now();
        let valid_count = entries.map.values().filter(|e| e.expires_at > now).count();

        CacheStats {
            total_entries: entries.map.len(),
            valid_entries: valid_count,
            bytes: entries.bytes,
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
    /// Bytes the entries hold.
    pub bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(bytes: usize) -> CacheEntry {
        CacheEntry {
            status: 200,
            headers: Default::default(),
            body: Some(vec![b'x'; bytes]),
            metadata: None,
        }
    }

    #[test]
    fn the_cache_holds_a_bounded_number_of_bytes() {
        let cache = ResponseCache {
            limits: Limits {
                entries: 100,
                bytes: 1_000,
                entry_bytes: 400,
            },
            ..ResponseCache::new()
        };

        cache.set("too-big", body(401), 60);
        assert!(!cache.get("too-big").hit, "an entry over the entry limit");

        cache.set("a", body(300), 10);
        cache.set("b", body(300), 30);
        cache.set("c", body(300), 20);
        cache.set("d", body(300), 40);
        assert!(cache.stats().bytes <= 1_000, "{:?}", cache.stats());
        assert!(
            !cache.get("a").hit,
            "the entry closest to expiry is evicted"
        );
        for key in ["b", "c", "d"] {
            assert!(cache.get(key).hit, "{key}");
        }

        cache.set("b", body(10), 30);
        // Each entry counts its one-byte key with its body.
        assert_eq!(cache.stats().bytes, 301 + 301 + 11);
        cache.invalidate("c");
        assert_eq!(cache.stats().bytes, 301 + 11);
    }

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
            headers: Default::default(),
            body: Some(b"test body".to_vec()),
            metadata: None,
        };

        cache.set("test-key", entry, 60);

        let result = cache.get("test-key");
        assert!(result.hit);
        assert!(result.entry.is_some());
        let cached = result.entry.unwrap();
        assert_eq!(cached.status, 200);
        assert_eq!(cached.body, Some(b"test body".to_vec()));
    }

    #[test]
    fn cache_entry_count_is_bounded() {
        let cache = ResponseCache::new();
        let entry = CacheEntry {
            status: 200,
            headers: Default::default(),
            body: None,
            metadata: None,
        };
        // Write more distinct keys than the cap, all with a live TTL.
        for i in 0..(MAX_CACHE_ENTRIES + 100) {
            cache.set(&format!("key-{i}"), entry.clone(), 3600);
        }
        assert!(
            cache.stats().total_entries <= MAX_CACHE_ENTRIES,
            "cache grew past the cap: {}",
            cache.stats().total_entries
        );
    }

    #[test]
    fn test_cache_invalidate() {
        let cache = ResponseCache::new();

        let entry = CacheEntry {
            status: 200,
            headers: Default::default(),
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
            headers: Default::default(),
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

    #[test]
    fn test_cache_binary_body_roundtrip() {
        let cache = ResponseCache::new();

        let binary_body = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // PNG header
        let entry = CacheEntry {
            status: 200,
            headers: std::collections::BTreeMap::from([(
                "content-type".to_string(),
                vec!["image/png".to_string()],
            )]),
            body: Some(binary_body.clone()),
            metadata: None,
        };

        cache.set("binary-key", entry, 60);
        let cached = cache.get("binary-key").entry.unwrap();
        assert_eq!(cached.body, Some(binary_body));
    }

    #[test]
    fn test_cache_entry_json_roundtrip() {
        // CacheEntry uses base64_body for JSON — verify roundtrip
        let entry = CacheEntry {
            status: 200,
            headers: Default::default(),
            body: Some(vec![0x00, 0xFF, 0x80]),
            metadata: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: CacheEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, entry.body);
    }

    #[test]
    fn test_cache_entry_json_none_body() {
        let entry = CacheEntry {
            status: 204,
            headers: Default::default(),
            body: None,
            metadata: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: CacheEntry = serde_json::from_str(&json).unwrap();
        assert!(decoded.body.is_none());
    }
}
