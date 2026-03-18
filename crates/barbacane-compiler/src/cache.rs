//! Local file cache for downloaded remote plugins.
//!
//! Cache layout:
//! ```text
//! ~/.barbacane/cache/plugins/<sha256-of-url>/
//!   plugin.wasm
//!   plugin.toml        (optional)
//!   metadata.json
//! ```

use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::error::CompileError;

/// Cached plugin data.
pub struct CachedPlugin {
    /// WASM binary content.
    pub wasm_bytes: Vec<u8>,
    /// Raw plugin.toml content (if available).
    pub plugin_toml: Option<String>,
}

/// Plugin download cache backed by the local filesystem.
pub struct PluginCache {
    cache_dir: PathBuf,
}

impl PluginCache {
    /// Create a new cache rooted at `~/.barbacane/cache/plugins/`.
    pub fn new() -> Result<Self, CompileError> {
        let home = home::home_dir().ok_or_else(|| {
            CompileError::PluginResolution("could not determine home directory".to_string())
        })?;
        let cache_dir = home.join(".barbacane").join("cache").join("plugins");
        std::fs::create_dir_all(&cache_dir).map_err(|e| {
            CompileError::PluginResolution(format!(
                "failed to create plugin cache directory {}: {e}",
                cache_dir.display()
            ))
        })?;
        Ok(Self { cache_dir })
    }

    /// Create a cache at a custom path (for testing).
    #[cfg(test)]
    pub fn with_dir(cache_dir: PathBuf) -> Result<Self, CompileError> {
        std::fs::create_dir_all(&cache_dir).map_err(|e| {
            CompileError::PluginResolution(format!(
                "failed to create plugin cache directory {}: {e}",
                cache_dir.display()
            ))
        })?;
        Ok(Self { cache_dir })
    }

    /// Compute the cache key (SHA-256 of the URL).
    fn cache_key(url: &str) -> String {
        hex::encode(Sha256::digest(url.as_bytes()))
    }

    /// Directory for a specific cached plugin.
    fn entry_dir(&self, url: &str) -> PathBuf {
        self.cache_dir.join(Self::cache_key(url))
    }

    /// Look up a cached plugin.
    ///
    /// If `expected_sha256` is provided, validates the cached WASM against it.
    /// Returns `None` on cache miss or checksum mismatch.
    pub fn get(&self, url: &str, expected_sha256: Option<&str>) -> Option<CachedPlugin> {
        let dir = self.entry_dir(url);
        let wasm_path = dir.join("plugin.wasm");
        let wasm_bytes = std::fs::read(&wasm_path).ok()?;

        // Validate checksum if provided
        if let Some(expected) = expected_sha256 {
            let actual = hex::encode(Sha256::digest(&wasm_bytes));
            if actual != expected {
                tracing::warn!(
                    url,
                    expected,
                    actual,
                    "cached plugin checksum mismatch, will re-download"
                );
                return None;
            }
        }

        let plugin_toml = std::fs::read_to_string(dir.join("plugin.toml")).ok();

        Some(CachedPlugin {
            wasm_bytes,
            plugin_toml,
        })
    }

    /// Store a downloaded plugin in the cache.
    pub fn put(
        &self,
        url: &str,
        wasm_bytes: &[u8],
        plugin_toml: Option<&str>,
    ) -> Result<(), CompileError> {
        let dir = self.entry_dir(url);
        std::fs::create_dir_all(&dir).map_err(|e| {
            CompileError::PluginResolution(format!(
                "failed to create cache entry {}: {e}",
                dir.display()
            ))
        })?;

        std::fs::write(dir.join("plugin.wasm"), wasm_bytes).map_err(|e| {
            CompileError::PluginResolution(format!("failed to write cached plugin wasm: {e}"))
        })?;

        if let Some(toml_content) = plugin_toml {
            std::fs::write(dir.join("plugin.toml"), toml_content).map_err(|e| {
                CompileError::PluginResolution(format!("failed to write cached plugin.toml: {e}"))
            })?;
        }

        // Write metadata
        let metadata = serde_json::json!({
            "url": url,
            "sha256": hex::encode(Sha256::digest(wasm_bytes)),
            "downloaded_at": chrono::Utc::now().to_rfc3339(),
        });
        std::fs::write(
            dir.join("metadata.json"),
            serde_json::to_string_pretty(&metadata)
                .expect("metadata JSON serialization is infallible"),
        )
        .map_err(|e| {
            CompileError::PluginResolution(format!("failed to write cache metadata: {e}"))
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_cache() -> (TempDir, PluginCache) {
        let temp = TempDir::new().expect("tempdir");
        let cache = PluginCache::with_dir(temp.path().join("cache")).expect("cache");
        (temp, cache)
    }

    #[test]
    fn cache_miss_returns_none() {
        let (_temp, cache) = test_cache();
        assert!(cache.get("https://example.com/plugin.wasm", None).is_none());
    }

    #[test]
    fn cache_put_and_get() {
        let (_temp, cache) = test_cache();
        let url = "https://example.com/plugin.wasm";
        let wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        let toml_content = "[plugin]\nname = \"test\"\nversion = \"1.0.0\"\ntype = \"middleware\"";

        cache.put(url, &wasm, Some(toml_content)).expect("put");

        let cached = cache.get(url, None).expect("cache hit");
        assert_eq!(cached.wasm_bytes, wasm);
        assert_eq!(cached.plugin_toml.as_deref(), Some(toml_content));
    }

    #[test]
    fn cache_sha256_mismatch_invalidates() {
        let (_temp, cache) = test_cache();
        let url = "https://example.com/plugin.wasm";
        let wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

        cache.put(url, &wasm, None).expect("put");

        // Wrong checksum should miss
        assert!(cache.get(url, Some("deadbeef")).is_none());

        // Correct checksum should hit
        let correct = hex::encode(sha2::Sha256::digest(&wasm));
        assert!(cache.get(url, Some(&correct)).is_some());
    }

    #[test]
    fn cache_without_plugin_toml() {
        let (_temp, cache) = test_cache();
        let url = "https://example.com/plugin.wasm";
        let wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

        cache.put(url, &wasm, None).expect("put");

        let cached = cache.get(url, None).expect("cache hit");
        assert_eq!(cached.wasm_bytes, wasm);
        assert!(cached.plugin_toml.is_none());
    }
}
