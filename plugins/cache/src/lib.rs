//! Response caching middleware plugin for Barbacane API gateway.
//!
//! Caches responses based on TTL configuration and vary headers.
//! Uses the host's response cache via host_cache_get/set.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Cache middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct Cache {
    /// Time-to-live for cached responses in seconds.
    #[serde(default = "default_ttl")]
    ttl: u32,

    /// Headers that differentiate cache entries.
    #[serde(default)]
    vary: Vec<String>,

    /// HTTP methods to cache.
    #[serde(default = "default_methods")]
    methods: Vec<String>,

    /// Status codes that are cacheable.
    #[serde(default = "default_cacheable_status")]
    cacheable_status: Vec<u16>,

    /// Cache key for the current request (set during on_request).
    #[serde(skip)]
    current_cache_key: Option<String>,

    /// Whether the current request is cacheable.
    #[serde(skip)]
    is_cacheable: bool,
}

fn default_ttl() -> u32 {
    300 // 5 minutes
}

fn default_methods() -> Vec<String> {
    vec!["GET".to_string(), "HEAD".to_string()]
}

fn default_cacheable_status() -> Vec<u16> {
    vec![200, 301, 404]
}

/// Result from host_cache_get.
#[derive(Debug, Deserialize)]
struct CacheResult {
    hit: bool,
    entry: Option<CacheEntry>,
}

/// A cached response entry.
#[derive(Debug, Deserialize, serde::Serialize)]
struct CacheEntry {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Option<String>,
}

impl Cache {
    /// Handle incoming request - check cache for hit.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        // Check if this request method is cacheable
        if !self.methods.iter().any(|m| m.eq_ignore_ascii_case(&req.method)) {
            self.is_cacheable = false;
            return Action::Continue(req);
        }

        // Build cache key: method + path + vary header values
        let cache_key = self.build_cache_key(&req);

        // Store for on_response
        self.current_cache_key = Some(cache_key.clone());
        self.is_cacheable = true;

        // Check cache
        match self.check_cache(&cache_key) {
            Some(result) if result.hit => {
                // Cache hit - return cached response
                if let Some(entry) = result.entry {
                    log_message(3, &format!("cache hit: {}", cache_key));

                    // Build response with cache headers
                    let mut headers = entry.headers;
                    headers.insert("x-cache".to_string(), "HIT".to_string());

                    return Action::ShortCircuit(Response {
                        status: entry.status,
                        headers,
                        body: entry.body,
                    });
                }
            }
            _ => {
                // Cache miss - continue to dispatcher
                log_message(3, &format!("cache miss: {}", cache_key));
            }
        }

        Action::Continue(req)
    }

    /// Handle response - cache it if cacheable.
    pub fn on_response(&mut self, resp: Response) -> Response {
        // Check if this request was cacheable and we have a cache key
        if !self.is_cacheable {
            return resp;
        }

        let cache_key = match &self.current_cache_key {
            Some(k) => k.clone(),
            None => return resp,
        };

        // Check if status code is cacheable
        if !self.cacheable_status.contains(&resp.status) {
            return resp;
        }

        // Check Cache-Control header for no-store/private
        if let Some(cc) = resp.headers.get("cache-control") {
            let cc_lower = cc.to_lowercase();
            if cc_lower.contains("no-store") || cc_lower.contains("private") {
                return resp;
            }
        }

        // Store in cache
        let entry = CacheEntry {
            status: resp.status,
            headers: resp.headers.clone(),
            body: resp.body.clone(),
        };

        if self.store_in_cache(&cache_key, &entry) {
            log_message(3, &format!("cached response: {}", cache_key));
        }

        // Add cache header to response
        let mut modified_resp = resp;
        modified_resp.headers.insert("x-cache".to_string(), "MISS".to_string());
        modified_resp
    }

    /// Build cache key from request.
    fn build_cache_key(&self, req: &Request) -> String {
        let mut key = format!("{}:{}", req.method, req.path);

        // Add query string if present
        if let Some(query) = &req.query {
            key.push('?');
            key.push_str(query);
        }

        // Add vary header values
        for header_name in &self.vary {
            let lower_name = header_name.to_lowercase();
            if let Some(value) = req.headers.get(&lower_name) {
                key.push_str(&format!("|{}={}", lower_name, value));
            }
        }

        key
    }

    /// Check cache for a key.
    fn check_cache(&self, key: &str) -> Option<CacheResult> {
        let result_len = call_cache_get(key);
        if result_len < 0 {
            return None;
        }

        let mut buf = vec![0u8; result_len as usize];
        let read_len = call_cache_read_result(&mut buf);
        if read_len <= 0 {
            return None;
        }

        serde_json::from_slice(&buf[..read_len as usize]).ok()
    }

    /// Store a response in the cache.
    fn store_in_cache(&self, key: &str, entry: &CacheEntry) -> bool {
        let entry_json = match serde_json::to_string(entry) {
            Ok(j) => j,
            Err(_) => return false,
        };

        call_cache_set(key, &entry_json, self.ttl) == 0
    }
}

/// Call host_cache_get with a string key.
fn call_cache_get(key: &str) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_cache_get(key_ptr: i32, key_len: i32) -> i32;
    }
    unsafe {
        host_cache_get(key.as_ptr() as i32, key.len() as i32)
    }
}

/// Call host_cache_set.
fn call_cache_set(key: &str, entry_json: &str, ttl_secs: u32) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_cache_set(
            key_ptr: i32,
            key_len: i32,
            entry_ptr: i32,
            entry_len: i32,
            ttl_secs: u32,
        ) -> i32;
    }
    unsafe {
        host_cache_set(
            key.as_ptr() as i32,
            key.len() as i32,
            entry_json.as_ptr() as i32,
            entry_json.len() as i32,
            ttl_secs,
        )
    }
}

/// Read cache result into buffer.
fn call_cache_read_result(buf: &mut [u8]) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_cache_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }
    unsafe {
        host_cache_read_result(buf.as_mut_ptr() as i32, buf.len() as i32)
    }
}

/// Log a message via host_log.
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}
