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
        if !self
            .methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&req.method))
        {
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
        modified_resp
            .headers
            .insert("x-cache".to_string(), "MISS".to_string());
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
#[cfg(target_arch = "wasm32")]
fn call_cache_get(key: &str) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_cache_get(key_ptr: i32, key_len: i32) -> i32;
    }
    unsafe { host_cache_get(key.as_ptr() as i32, key.len() as i32) }
}

/// Call host_cache_set.
#[cfg(target_arch = "wasm32")]
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
#[cfg(target_arch = "wasm32")]
fn call_cache_read_result(buf: &mut [u8]) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_cache_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }
    unsafe { host_cache_read_result(buf.as_mut_ptr() as i32, buf.len() as i32) }
}

/// Log a message via host_log.
#[cfg(target_arch = "wasm32")]
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod mock_host {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
        static LAST_RESULT: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }

    pub fn cache_get(key: &str) -> i32 {
        CACHE.with(|c| {
            if let Some(entry) = c.borrow().get(key) {
                let result = serde_json::json!({
                    "hit": true,
                    "entry": serde_json::from_str::<serde_json::Value>(entry).unwrap()
                });
                let bytes = serde_json::to_vec(&result).unwrap();
                let len = bytes.len() as i32;
                LAST_RESULT.with(|r| *r.borrow_mut() = Some(bytes));
                len
            } else {
                let result = serde_json::json!({ "hit": false });
                let bytes = serde_json::to_vec(&result).unwrap();
                let len = bytes.len() as i32;
                LAST_RESULT.with(|r| *r.borrow_mut() = Some(bytes));
                len
            }
        })
    }

    pub fn cache_read_result(buf: &mut [u8]) -> i32 {
        LAST_RESULT.with(|r| {
            if let Some(data) = r.borrow().as_ref() {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                len as i32
            } else {
                -1
            }
        })
    }

    pub fn cache_set(key: &str, entry_json: &str, _ttl_secs: u32) -> i32 {
        CACHE.with(|c| {
            c.borrow_mut()
                .insert(key.to_string(), entry_json.to_string())
        });
        0
    }

    #[cfg(test)]
    pub fn reset() {
        CACHE.with(|c| c.borrow_mut().clear());
        LAST_RESULT.with(|r| *r.borrow_mut() = None);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn call_cache_get(key: &str) -> i32 {
    mock_host::cache_get(key)
}

#[cfg(not(target_arch = "wasm32"))]
fn call_cache_read_result(buf: &mut [u8]) -> i32 {
    mock_host::cache_read_result(buf)
}

#[cfg(not(target_arch = "wasm32"))]
fn call_cache_set(key: &str, entry_json: &str, ttl_secs: u32) -> i32 {
    mock_host::cache_set(key, entry_json, ttl_secs)
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(_level: i32, _msg: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        #[cfg(not(target_arch = "wasm32"))]
        mock_host::reset();
    }

    fn create_test_request(method: &str, path: &str) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    #[test]
    fn test_build_cache_key_basic() {
        setup();
        let cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: None,
            is_cacheable: false,
        };

        let req = create_test_request("GET", "/api/users");
        let key = cache.build_cache_key(&req);
        assert_eq!(key, "GET:/api/users");
    }

    #[test]
    fn test_build_cache_key_with_query() {
        setup();
        let cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: None,
            is_cacheable: false,
        };

        let mut req = create_test_request("GET", "/api/users");
        req.query = Some("page=1&limit=10".to_string());
        let key = cache.build_cache_key(&req);
        assert_eq!(key, "GET:/api/users?page=1&limit=10");
    }

    #[test]
    fn test_build_cache_key_with_vary_headers() {
        setup();
        let cache = Cache {
            ttl: 300,
            vary: vec!["accept".to_string(), "accept-language".to_string()],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: None,
            is_cacheable: false,
        };

        let mut req = create_test_request("GET", "/api/users");
        req.headers
            .insert("accept".to_string(), "application/json".to_string());
        req.headers
            .insert("accept-language".to_string(), "en-US".to_string());

        let key = cache.build_cache_key(&req);
        assert_eq!(
            key,
            "GET:/api/users|accept=application/json|accept-language=en-US"
        );
    }

    #[test]
    fn test_config_defaults() {
        setup();
        let json = r#"{}"#;
        let cache: Cache = serde_json::from_str(json).unwrap();

        assert_eq!(cache.ttl, 300);
        assert_eq!(cache.methods, vec!["GET", "HEAD"]);
        assert_eq!(cache.cacheable_status, vec![200, 301, 404]);
        assert_eq!(cache.vary.len(), 0);
    }

    #[test]
    fn test_on_request_non_cacheable_method() {
        setup();
        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: None,
            is_cacheable: false,
        };

        let req = create_test_request("POST", "/api/users");
        let action = cache.on_request(req.clone());

        assert!(!cache.is_cacheable);
        assert!(cache.current_cache_key.is_none());

        match action {
            Action::Continue(returned_req) => {
                assert_eq!(returned_req.method, "POST");
                assert_eq!(returned_req.path, "/api/users");
            }
            _ => panic!("Expected Action::Continue"),
        }
    }

    #[test]
    fn test_on_request_cacheable_miss() {
        setup();
        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: None,
            is_cacheable: false,
        };

        let req = create_test_request("GET", "/api/users");
        let action = cache.on_request(req.clone());

        assert!(cache.is_cacheable);
        assert_eq!(cache.current_cache_key, Some("GET:/api/users".to_string()));

        match action {
            Action::Continue(returned_req) => {
                assert_eq!(returned_req.method, "GET");
                assert_eq!(returned_req.path, "/api/users");
            }
            _ => panic!("Expected Action::Continue for cache miss"),
        }
    }

    #[test]
    fn test_on_request_cache_hit() {
        setup();

        // Pre-populate cache
        let entry = CacheEntry {
            status: 200,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".to_string(), "application/json".to_string());
                h
            },
            body: Some(r#"{"users":[]}"#.to_string()),
        };
        let entry_json = serde_json::to_string(&entry).unwrap();
        call_cache_set("GET:/api/users", &entry_json, 300);

        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: None,
            is_cacheable: false,
        };

        let req = create_test_request("GET", "/api/users");
        let action = cache.on_request(req);

        match action {
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 200);
                assert_eq!(resp.headers.get("x-cache"), Some(&"HIT".to_string()));
                assert_eq!(
                    resp.headers.get("content-type"),
                    Some(&"application/json".to_string())
                );
                assert_eq!(resp.body, Some(r#"{"users":[]}"#.to_string()));
            }
            _ => panic!("Expected Action::ShortCircuit for cache hit"),
        }
    }

    #[test]
    fn test_on_response_cacheable() {
        setup();
        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: Some("GET:/api/users".to_string()),
            is_cacheable: true,
        };

        let resp = Response {
            status: 200,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".to_string(), "application/json".to_string());
                h
            },
            body: Some(r#"{"users":[]}"#.to_string()),
        };

        let result = cache.on_response(resp);

        assert_eq!(result.status, 200);
        assert_eq!(result.headers.get("x-cache"), Some(&"MISS".to_string()));

        // Verify it was stored in cache
        let result_len = call_cache_get("GET:/api/users");
        assert!(result_len > 0);
    }

    #[test]
    fn test_on_response_non_cacheable_status() {
        setup();
        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: Some("GET:/api/users".to_string()),
            is_cacheable: true,
        };

        let resp = Response {
            status: 500,
            headers: BTreeMap::new(),
            body: Some("Internal Server Error".to_string()),
        };

        let result = cache.on_response(resp);

        assert_eq!(result.status, 500);
        assert!(!result.headers.contains_key("x-cache"));

        // Verify it was NOT stored in cache
        let result_len = call_cache_get("GET:/api/users");
        let mut buf = vec![0u8; result_len as usize];
        call_cache_read_result(&mut buf);
        let cache_result: CacheResult = serde_json::from_slice(&buf).unwrap();
        assert!(!cache_result.hit);
    }

    #[test]
    fn test_on_response_cache_control_no_store() {
        setup();
        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: Some("GET:/api/users".to_string()),
            is_cacheable: true,
        };

        let resp = Response {
            status: 200,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("cache-control".to_string(), "no-store".to_string());
                h
            },
            body: Some(r#"{"users":[]}"#.to_string()),
        };

        let result = cache.on_response(resp);

        assert_eq!(result.status, 200);
        assert!(!result.headers.contains_key("x-cache"));

        // Verify it was NOT stored in cache
        let result_len = call_cache_get("GET:/api/users");
        let mut buf = vec![0u8; result_len as usize];
        call_cache_read_result(&mut buf);
        let cache_result: CacheResult = serde_json::from_slice(&buf).unwrap();
        assert!(!cache_result.hit);
    }

    #[test]
    fn test_on_response_cache_control_private() {
        setup();
        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: Some("GET:/api/users".to_string()),
            is_cacheable: true,
        };

        let resp = Response {
            status: 200,
            headers: {
                let mut h = BTreeMap::new();
                h.insert(
                    "cache-control".to_string(),
                    "private, max-age=3600".to_string(),
                );
                h
            },
            body: Some(r#"{"users":[]}"#.to_string()),
        };

        let result = cache.on_response(resp);

        assert_eq!(result.status, 200);
        assert!(!result.headers.contains_key("x-cache"));
    }

    #[test]
    fn test_on_response_not_cacheable_request() {
        setup();
        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: Some("POST:/api/users".to_string()),
            is_cacheable: false,
        };

        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(r#"{"created":true}"#.to_string()),
        };

        let result = cache.on_response(resp);

        assert_eq!(result.status, 200);
        assert!(!result.headers.contains_key("x-cache"));

        // Verify it was NOT stored in cache
        let result_len = call_cache_get("POST:/api/users");
        let mut buf = vec![0u8; result_len as usize];
        call_cache_read_result(&mut buf);
        let cache_result: CacheResult = serde_json::from_slice(&buf).unwrap();
        assert!(!cache_result.hit);
    }

    #[test]
    fn test_response_passthrough_behavior() {
        setup();
        let mut cache = Cache {
            ttl: 300,
            vary: vec![],
            methods: default_methods(),
            cacheable_status: default_cacheable_status(),
            current_cache_key: None,
            is_cacheable: true,
        };

        let resp = Response {
            status: 200,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".to_string(), "application/json".to_string());
                h.insert("x-custom".to_string(), "value".to_string());
                h
            },
            body: Some(r#"{"data":"test"}"#.to_string()),
        };

        let result = cache.on_response(resp.clone());

        // Should pass through without x-cache header when no cache key
        assert_eq!(result.status, resp.status);
        assert_eq!(result.body, resp.body);
        assert_eq!(
            result.headers.get("content-type"),
            Some(&"application/json".to_string())
        );
        assert_eq!(result.headers.get("x-custom"), Some(&"value".to_string()));
        assert!(!result.headers.contains_key("x-cache"));
    }

    #[test]
    fn test_cacheable_status_codes() {
        setup();

        let status_tests = vec![
            (200, true),
            (301, true),
            (404, true),
            (201, false),
            (400, false),
            (500, false),
        ];

        for (status, should_cache) in status_tests {
            mock_host::reset();

            let mut cache = Cache {
                ttl: 300,
                vary: vec![],
                methods: default_methods(),
                cacheable_status: default_cacheable_status(),
                current_cache_key: Some(format!("GET:/api/test/{}", status)),
                is_cacheable: true,
            };

            let resp = Response {
                status,
                headers: BTreeMap::new(),
                body: Some("test".to_string()),
            };

            let result = cache.on_response(resp);

            if should_cache {
                assert_eq!(result.headers.get("x-cache"), Some(&"MISS".to_string()));
            } else {
                assert!(!result.headers.contains_key("x-cache"));
            }
        }
    }
}
