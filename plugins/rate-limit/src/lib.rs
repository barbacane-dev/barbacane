//! Rate limiting middleware plugin for Barbacane API gateway.
//!
//! Implements rate limiting with IETF draft-ietf-httpapi-ratelimit-headers support.
//! Uses the host's sliding window rate limiter via host_rate_limit_check.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;

/// Rate limit middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct RateLimit {
    /// Maximum requests allowed in the window.
    quota: u32,

    /// Window duration in seconds.
    window: u32,

    /// Policy name for RateLimit-Policy header.
    #[serde(default = "default_policy_name")]
    policy_name: String,

    /// Partition key configuration.
    /// Options: "client_ip", "header:<name>", "context:<key>", or a static string.
    #[serde(default = "default_partition_key")]
    partition_key: String,

    /// Exact IPs of trusted reverse proxies, used when `partition_key` is
    /// "client_ip". Forwarded headers are honored only behind a trusted proxy;
    /// otherwise a client cannot rotate `X-Forwarded-For` to dodge the limit.
    #[serde(default)]
    trusted_proxies: Vec<String>,

    /// When the host rate limiter is unavailable, allow the request through
    /// (fail open) instead of rejecting it. Defaults to false (fail closed), so
    /// a limiter outage cannot silently disable rate limiting.
    #[serde(default)]
    fail_open: bool,
}

fn default_policy_name() -> String {
    "default".to_string()
}

fn default_partition_key() -> String {
    "client_ip".to_string()
}

/// Result from host_rate_limit_check.
#[derive(Debug, Deserialize)]
struct RateLimitResult {
    allowed: bool,
    remaining: u32,
    reset: u64,
    limit: u32,
    retry_after: Option<u64>,
}

impl RateLimit {
    /// Handle incoming request - check rate limit.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        // Extract the partition key
        let key = self.extract_partition_key(&req);

        // Call the host rate limiter
        let result = match self.check_rate_limit(&key) {
            Some(r) => r,
            None => {
                // Rate limiter unavailable. Fail closed by default so an outage
                // cannot silently disable rate limiting; operators may opt into
                // fail-open.
                if self.fail_open {
                    log_message(
                        1,
                        "rate limiter unavailable, failing open (allowing request)",
                    );
                    return Action::Continue(req);
                }
                log_message(
                    3,
                    "rate limiter unavailable, failing closed (rejecting request)",
                );
                return Action::ShortCircuit(self.unavailable_response());
            }
        };

        // Build rate limit headers
        let policy_header = format!("{};q={};w={}", self.policy_name, self.quota, self.window);

        if result.allowed {
            // Request allowed - add headers and continue
            let mut modified_req = req;
            modified_req
                .headers
                .insert("x-ratelimit-policy".to_string(), policy_header);
            modified_req.headers.insert(
                "x-ratelimit-remaining".to_string(),
                result.remaining.to_string(),
            );
            modified_req
                .headers
                .insert("x-ratelimit-reset".to_string(), result.reset.to_string());
            modified_req
                .headers
                .insert("x-ratelimit-limit".to_string(), result.limit.to_string());
            Action::Continue(modified_req)
        } else {
            // Request blocked - return 429
            Action::ShortCircuit(self.too_many_requests_response(&result, &policy_header))
        }
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Extract the partition key from the request.
    fn extract_partition_key(&self, req: &Request) -> String {
        if self.partition_key == "client_ip" {
            // Trusted-proxy-aware client IP: forwarded headers are honored only
            // behind a configured trusted proxy, so the partition can't be
            // spoofed to get a fresh bucket per request.
            resolve_client_ip(req, &self.trusted_proxies)
        } else if let Some(header_name) = self.partition_key.strip_prefix("header:") {
            // Use specified header value
            req.headers
                .get(header_name)
                .or_else(|| req.headers.get(&header_name.to_lowercase()))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string())
        } else if let Some(context_key) = self.partition_key.strip_prefix("context:") {
            // Use context value (would call host_context_get in full implementation)
            // For now, fall back to the context key as a static value
            context_key.to_string()
        } else {
            // Use partition_key as a static key (same limit for all requests)
            self.partition_key.clone()
        }
    }

    /// Call the host rate limiter.
    fn check_rate_limit(&self, key: &str) -> Option<RateLimitResult> {
        // Call host function to check rate limit
        let result_len = call_rate_limit_check(key, self.quota, self.window);
        if result_len < 0 {
            return None;
        }

        // Read the result
        let mut buf = vec![0u8; result_len as usize];
        let read_len = call_rate_limit_read_result(&mut buf);
        if read_len <= 0 {
            return None;
        }

        // Parse the JSON result
        serde_json::from_slice(&buf[..read_len as usize]).ok()
    }

    /// Generate a 503 response used when the limiter is unavailable and the
    /// plugin is configured to fail closed.
    fn unavailable_response(&self) -> Response {
        ProblemDetails::new(
            503,
            "urn:barbacane:error:rate-limiter-unavailable",
            "Service Unavailable",
        )
        .detail("Rate limiter is unavailable; request rejected (fail closed).")
        .into_response()
    }

    /// Generate 429 Too Many Requests response.
    fn too_many_requests_response(
        &self,
        result: &RateLimitResult,
        policy_header: &str,
    ) -> Response {
        let mut resp = ProblemDetails::new(
            429,
            "urn:barbacane:error:rate-limit-exceeded",
            "Too Many Requests",
        )
        .detail(format!(
            "Rate limit exceeded. Limit: {} requests per {} seconds.",
            self.quota, self.window
        ))
        .into_response();

        // IETF draft rate limit headers
        resp.headers
            .insert("ratelimit-policy".to_string(), policy_header.to_string());
        resp.headers.insert(
            "ratelimit".to_string(),
            format!(
                "limit={}, remaining=0, reset={}",
                result.limit, result.reset
            ),
        );

        // Retry-After header
        if let Some(retry_after) = result.retry_after {
            resp.headers
                .insert("retry-after".to_string(), retry_after.to_string());
        }

        resp
    }
}

// ============================================================================
// Host function bindings (WASM)
// ============================================================================

/// Call host_rate_limit_check with a string key.
#[cfg(target_arch = "wasm32")]
fn call_rate_limit_check(key: &str, quota: u32, window_secs: u32) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_rate_limit_check(key_ptr: i32, key_len: i32, quota: u32, window_secs: u32) -> i32;
    }
    unsafe { host_rate_limit_check(key.as_ptr() as i32, key.len() as i32, quota, window_secs) }
}

/// Read rate limit result into buffer.
#[cfg(target_arch = "wasm32")]
fn call_rate_limit_read_result(buf: &mut [u8]) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_rate_limit_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }
    unsafe { host_rate_limit_read_result(buf.as_mut_ptr() as i32, buf.len() as i32) }
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

// ============================================================================
// Mock host functions (Native)
// ============================================================================

#[cfg(not(target_arch = "wasm32"))]
mod mock_host {
    use std::cell::RefCell;

    thread_local! {
        static RATE_LIMIT_RESULT: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }

    #[cfg(test)]
    pub fn set_rate_limit_result(result_json: &str) {
        RATE_LIMIT_RESULT.with(|r| *r.borrow_mut() = Some(result_json.as_bytes().to_vec()));
    }

    pub fn call_rate_limit_check(_key: &str, _quota: u32, _window_secs: u32) -> i32 {
        RATE_LIMIT_RESULT.with(|r| r.borrow().as_ref().map(|v| v.len() as i32).unwrap_or(-1))
    }

    pub fn call_rate_limit_read_result(buf: &mut [u8]) -> i32 {
        RATE_LIMIT_RESULT.with(|r| {
            if let Some(data) = r.borrow().as_ref() {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                len as i32
            } else {
                -1
            }
        })
    }

    #[cfg(test)]
    pub fn reset() {
        RATE_LIMIT_RESULT.with(|r| *r.borrow_mut() = None);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn call_rate_limit_check(key: &str, quota: u32, window_secs: u32) -> i32 {
    mock_host::call_rate_limit_check(key, quota, window_secs)
}

#[cfg(not(target_arch = "wasm32"))]
fn call_rate_limit_read_result(buf: &mut [u8]) -> i32 {
    mock_host::call_rate_limit_read_result(buf)
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(_level: i32, _msg: &str) {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_extract_partition_key_client_ip_from_x_forwarded_for() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "x-forwarded-for".to_string(),
            "192.168.1.1, 10.0.0.1".to_string(),
        );

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        // PL-4: without a trusted proxy a spoofed XFF is ignored; the real peer
        // IP is used so a client can't rotate XFF to get fresh buckets.
        assert_eq!(rate_limit.extract_partition_key(&req), "127.0.0.1");
    }

    #[test]
    fn test_partition_key_xff_honored_behind_trusted_proxy() {
        let mut headers = BTreeMap::new();
        headers.insert("x-forwarded-for".to_string(), "192.168.1.1".to_string());
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "10.9.9.9".to_string(),
        };
        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec!["10.9.9.9".to_string()],
            fail_open: false,
        };
        assert_eq!(rate_limit.extract_partition_key(&req), "192.168.1.1");
    }

    #[test]
    fn test_extract_partition_key_client_ip_from_x_real_ip() {
        let mut headers = BTreeMap::new();
        headers.insert("x-real-ip".to_string(), "192.168.1.2".to_string());

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        // X-Real-IP from an untrusted peer is ignored too.
        assert_eq!(rate_limit.extract_partition_key(&req), "127.0.0.1");
    }

    #[test]
    fn test_extract_partition_key_client_ip_fallback_unknown() {
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        // No forwarded headers: the real peer IP is used (not "unknown").
        assert_eq!(rate_limit.extract_partition_key(&req), "127.0.0.1");
    }

    #[test]
    fn test_extract_partition_key_from_header() {
        let mut headers = BTreeMap::new();
        headers.insert("x-custom".to_string(), "custom-value".to_string());

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "header:x-custom".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        assert_eq!(rate_limit.extract_partition_key(&req), "custom-value");
    }

    #[test]
    fn test_extract_partition_key_from_header_case_insensitive() {
        let mut headers = BTreeMap::new();
        headers.insert("x-custom".to_string(), "custom-value".to_string());

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "header:X-Custom".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        assert_eq!(rate_limit.extract_partition_key(&req), "custom-value");
    }

    #[test]
    fn test_extract_partition_key_from_header_missing() {
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "header:x-custom".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        assert_eq!(rate_limit.extract_partition_key(&req), "unknown");
    }

    #[test]
    fn test_extract_partition_key_from_context() {
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "context:user_id".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        assert_eq!(rate_limit.extract_partition_key(&req), "user_id");
    }

    #[test]
    fn test_extract_partition_key_static() {
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "global-limit".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        assert_eq!(rate_limit.extract_partition_key(&req), "global-limit");
    }

    #[test]
    fn test_too_many_requests_response_status() {
        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        let result = RateLimitResult {
            allowed: false,
            remaining: 0,
            reset: 1234567890,
            limit: 10,
            retry_after: None,
        };

        let response = rate_limit.too_many_requests_response(&result, "default;q=10;w=60");

        assert_eq!(response.status, 429);
    }

    #[test]
    fn test_too_many_requests_response_headers() {
        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        let result = RateLimitResult {
            allowed: false,
            remaining: 0,
            reset: 1234567890,
            limit: 10,
            retry_after: None,
        };

        let response = rate_limit.too_many_requests_response(&result, "default;q=10;w=60");

        assert_eq!(
            response.headers.get("content-type").unwrap(),
            "application/problem+json"
        );
        assert_eq!(
            response.headers.get("ratelimit-policy").unwrap(),
            "default;q=10;w=60"
        );
        assert_eq!(
            response.headers.get("ratelimit").unwrap(),
            "limit=10, remaining=0, reset=1234567890"
        );
        assert!(!response.headers.contains_key("retry-after"));
    }

    #[test]
    fn test_too_many_requests_response_with_retry_after() {
        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        let result = RateLimitResult {
            allowed: false,
            remaining: 0,
            reset: 1234567890,
            limit: 10,
            retry_after: Some(30),
        };

        let response = rate_limit.too_many_requests_response(&result, "default;q=10;w=60");

        assert_eq!(response.headers.get("retry-after").unwrap(), "30");
    }

    #[test]
    fn test_too_many_requests_response_rfc9457_body() {
        let rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        let result = RateLimitResult {
            allowed: false,
            remaining: 0,
            reset: 1234567890,
            limit: 10,
            retry_after: None,
        };

        let response = rate_limit.too_many_requests_response(&result, "default;q=10;w=60");

        let body = String::from_utf8(response.body.unwrap()).unwrap();
        assert!(body.contains("\"type\":\"urn:barbacane:error:rate-limit-exceeded\""));
        assert!(body.contains("\"title\":\"Too Many Requests\""));
        assert!(body.contains("\"status\":429"));
        assert!(
            body.contains("\"detail\":\"Rate limit exceeded. Limit: 10 requests per 60 seconds.\"")
        );
    }

    #[test]
    fn test_config_deserialization_with_defaults() {
        let config = r#"{"quota": 100, "window": 3600}"#;
        let rate_limit: RateLimit = serde_json::from_str(config).unwrap();

        assert_eq!(rate_limit.quota, 100);
        assert_eq!(rate_limit.window, 3600);
        assert_eq!(rate_limit.policy_name, "default");
        assert_eq!(rate_limit.partition_key, "client_ip");
    }

    #[test]
    fn test_config_deserialization_custom_values() {
        let config = r#"{
            "quota": 50,
            "window": 1800,
            "policy_name": "custom-policy",
            "partition_key": "header:api-key"
        }"#;
        let rate_limit: RateLimit = serde_json::from_str(config).unwrap();

        assert_eq!(rate_limit.quota, 50);
        assert_eq!(rate_limit.window, 1800);
        assert_eq!(rate_limit.policy_name, "custom-policy");
        assert_eq!(rate_limit.partition_key, "header:api-key");
    }

    #[test]
    fn test_config_deserialization_missing_required_fields() {
        let config = r#"{"quota": 100}"#;
        let result: Result<RateLimit, _> = serde_json::from_str(config);
        assert!(result.is_err());

        let config = r#"{"window": 60}"#;
        let result: Result<RateLimit, _> = serde_json::from_str(config);
        assert!(result.is_err());
    }

    #[test]
    fn test_on_request_allowed() {
        mock_host::reset();

        let result_json = r#"{
            "allowed": true,
            "remaining": 5,
            "reset": 1234567890,
            "limit": 10
        }"#;
        mock_host::set_rate_limit_result(result_json);

        let mut rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "test-policy".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let action = rate_limit.on_request(req);

        match action {
            Action::Continue(modified_req) => {
                assert_eq!(
                    modified_req.headers.get("x-ratelimit-policy").unwrap(),
                    "test-policy;q=10;w=60"
                );
                assert_eq!(
                    modified_req.headers.get("x-ratelimit-remaining").unwrap(),
                    "5"
                );
                assert_eq!(
                    modified_req.headers.get("x-ratelimit-reset").unwrap(),
                    "1234567890"
                );
                assert_eq!(modified_req.headers.get("x-ratelimit-limit").unwrap(), "10");
            }
            _ => panic!("Expected Action::Continue"),
        }
    }

    #[test]
    fn test_on_request_denied() {
        mock_host::reset();

        let result_json = r#"{
            "allowed": false,
            "remaining": 0,
            "reset": 1234567890,
            "limit": 10,
            "retry_after": 30
        }"#;
        mock_host::set_rate_limit_result(result_json);

        let mut rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "test-policy".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let action = rate_limit.on_request(req);

        match action {
            Action::ShortCircuit(response) => {
                assert_eq!(response.status, 429);
                assert_eq!(response.headers.get("retry-after").unwrap(), "30");
                assert_eq!(
                    response.headers.get("ratelimit-policy").unwrap(),
                    "test-policy;q=10;w=60"
                );
            }
            _ => panic!("Expected Action::ShortCircuit"),
        }
    }

    #[test]
    fn test_on_request_rate_limiter_unavailable() {
        mock_host::reset();
        // Don't set any result - simulate unavailable rate limiter

        let mut rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "test-policy".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let action = rate_limit.on_request(req);

        // PL-4: default is fail CLOSED — a limiter outage rejects with 503
        // rather than silently disabling the limit.
        match action {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 503),
            _ => panic!("Expected Action::ShortCircuit (fail closed)"),
        }
    }

    #[test]
    fn test_on_request_rate_limiter_unavailable_fail_open_opt_in() {
        mock_host::reset();
        let mut rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "test-policy".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: true,
        };
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };
        match rate_limit.on_request(req) {
            Action::Continue(r) => assert!(!r.headers.contains_key("x-ratelimit-policy")),
            _ => panic!("Expected Action::Continue when fail_open is set"),
        }
    }

    #[test]
    fn test_on_response_passthrough() {
        let mut rate_limit = RateLimit {
            quota: 10,
            window: 60,
            policy_name: "default".to_string(),
            partition_key: "client_ip".to_string(),
            trusted_proxies: vec![],
            fail_open: false,
        };

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());

        let response = Response {
            status: 200,
            headers: headers.clone(),
            body: Some(br#"{"message":"ok"}"#.to_vec()),
        };

        let result = rate_limit.on_response(response);

        assert_eq!(result.status, 200);
        assert_eq!(result.headers, headers);
        assert_eq!(result.body.unwrap(), br#"{"message":"ok"}"#);
    }
}
