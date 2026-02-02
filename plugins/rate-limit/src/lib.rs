//! Rate limiting middleware plugin for Barbacane API gateway.
//!
//! Implements rate limiting with IETF draft-ietf-httpapi-ratelimit-headers support.
//! Uses the host's sliding window rate limiter via host_rate_limit_check.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

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
                // Rate limiter unavailable - fail open (allow request)
                log_message(1, "rate limiter unavailable, allowing request");
                return Action::Continue(req);
            }
        };

        // Build rate limit headers
        let policy_header = format!(
            "{};q={};w={}",
            self.policy_name, self.quota, self.window
        );

        if result.allowed {
            // Request allowed - add headers and continue
            let mut modified_req = req;
            modified_req.headers.insert(
                "x-ratelimit-policy".to_string(),
                policy_header,
            );
            modified_req.headers.insert(
                "x-ratelimit-remaining".to_string(),
                result.remaining.to_string(),
            );
            modified_req.headers.insert(
                "x-ratelimit-reset".to_string(),
                result.reset.to_string(),
            );
            modified_req.headers.insert(
                "x-ratelimit-limit".to_string(),
                result.limit.to_string(),
            );
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
            // Use client IP from x-forwarded-for or x-real-ip header
            req.headers
                .get("x-forwarded-for")
                .and_then(|v| v.split(',').next().map(|s| s.trim().to_string()))
                .or_else(|| req.headers.get("x-real-ip").cloned())
                .unwrap_or_else(|| "unknown".to_string())
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

    /// Generate 429 Too Many Requests response.
    fn too_many_requests_response(&self, result: &RateLimitResult, policy_header: &str) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/problem+json".to_string());

        // IETF draft rate limit headers
        headers.insert("ratelimit-policy".to_string(), policy_header.to_string());
        headers.insert("ratelimit".to_string(), format!(
            "limit={}, remaining=0, reset={}",
            result.limit, result.reset
        ));

        // Retry-After header
        if let Some(retry_after) = result.retry_after {
            headers.insert("retry-after".to_string(), retry_after.to_string());
        }

        let body = serde_json::json!({
            "type": "urn:barbacane:error:rate-limit-exceeded",
            "title": "Too Many Requests",
            "status": 429,
            "detail": format!(
                "Rate limit exceeded. Limit: {} requests per {} seconds.",
                self.quota, self.window
            )
        });

        Response {
            status: 429,
            headers,
            body: Some(body.to_string()),
        }
    }
}

/// Call host_rate_limit_check with a string key.
fn call_rate_limit_check(key: &str, quota: u32, window_secs: u32) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_rate_limit_check(key_ptr: i32, key_len: i32, quota: u32, window_secs: u32) -> i32;
    }
    unsafe {
        host_rate_limit_check(key.as_ptr() as i32, key.len() as i32, quota, window_secs)
    }
}

/// Read rate limit result into buffer.
fn call_rate_limit_read_result(buf: &mut [u8]) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_rate_limit_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }
    unsafe {
        host_rate_limit_read_result(buf.as_mut_ptr() as i32, buf.len() as i32)
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
