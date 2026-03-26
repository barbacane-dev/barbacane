//! Fire-and-forget dispatcher plugin for Barbacane API gateway.
//!
//! Forwards the incoming request to a configured upstream URL without
//! waiting for the result, and returns an immediate static response.
//! Useful for webhook ingestion, async job submission, and audit trails.
//!
//! The outbound HTTP call is best-effort: if the upstream is unreachable
//! or returns an error, the client still receives the configured response.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Fire-and-forget dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct FireAndForgetDispatcher {
    /// Upstream URL to forward the request to.
    url: String,

    /// Timeout in milliseconds for the upstream HTTP call (default: 5000).
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,

    /// Static response returned to the client.
    #[serde(default)]
    response: ResponseConfig,
}

/// Static response configuration.
#[derive(Deserialize, Clone)]
pub struct ResponseConfig {
    /// HTTP status code (default: 202).
    #[serde(default = "default_response_status")]
    status: u16,

    /// Response headers (default: empty).
    #[serde(default)]
    headers: BTreeMap<String, String>,

    /// Content-Type header value (default: application/json).
    #[serde(default = "default_content_type")]
    content_type: String,

    /// Response body string (default: empty).
    #[serde(default)]
    body: String,
}

impl Default for ResponseConfig {
    fn default() -> Self {
        Self {
            status: default_response_status(),
            headers: BTreeMap::new(),
            content_type: default_content_type(),
            body: String::new(),
        }
    }
}

fn default_timeout_ms() -> u64 {
    5000
}

fn default_response_status() -> u16 {
    202
}

fn default_content_type() -> String {
    "application/json".to_string()
}

/// HTTP request format for host_http_call.
///
/// Body travels via side-channel (`set_http_request_body`), not in JSON.
#[derive(Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
struct HttpRequest {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    timeout_ms: Option<u64>,
}

impl FireAndForgetDispatcher {
    /// Forward the request to upstream (best-effort), then return the static response.
    pub fn dispatch(&mut self, req: Request) -> Response {
        // Forward the request to upstream (fire-and-forget)
        self.forward_to_upstream(&req);

        // Build and return the static response
        self.build_response()
    }

    /// Forward the incoming request to the configured upstream URL.
    /// Errors are logged but never affect the client response.
    fn forward_to_upstream(&self, req: &Request) {
        // Send request body via side-channel
        if let Some(ref body) = req.body {
            set_http_request_body(body);
        }

        let http_request = HttpRequest {
            method: req.method.clone(),
            url: self.url.clone(),
            headers: req.headers.clone(),
            timeout_ms: Some(self.timeout_ms),
        };

        let request_json = match serde_json::to_vec(&http_request) {
            Ok(json) => json,
            Err(e) => {
                log_message(
                    2, // WARN
                    &format!("fire-and-forget: failed to serialize request: {e}"),
                );
                return;
            }
        };

        // Call upstream — read and discard the response
        if let Err(()) = host::http_call(&request_json) {
            log_message(
                2, // WARN
                "fire-and-forget: upstream call failed (connection error)",
            );
        }
    }

    /// Build the configured static response.
    fn build_response(&self) -> Response {
        let mut headers = self.response.headers.clone();
        headers.insert("content-type".to_string(), self.response.content_type.clone());

        let body = if self.response.body.is_empty() {
            None
        } else {
            Some(self.response.body.as_bytes().to_vec())
        };

        Response {
            status: self.response.status,
            headers,
            body,
        }
    }
}

// ==================== Host function declarations ====================
//
// WASM implementations call real host functions via FFI.
// Native implementations (for unit testing) use thread-local mock state.

#[cfg(target_arch = "wasm32")]
mod host {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        #[link_name = "host_http_call"]
        fn ffi_http_call(req_ptr: i32, req_len: i32) -> i32;
        #[link_name = "host_http_read_result"]
        fn ffi_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }

    /// Make an outbound HTTP request. Returns Ok(()) on success, Err(()) on failure.
    /// The response is read and discarded (fire-and-forget).
    pub fn http_call(request_json: &[u8]) -> Result<(), ()> {
        unsafe {
            let result_len = ffi_http_call(request_json.as_ptr() as i32, request_json.len() as i32);
            if result_len < 0 {
                return Err(());
            }
            // Read and discard the response
            let mut buf = vec![0u8; result_len as usize];
            ffi_http_read_result(buf.as_mut_ptr() as i32, result_len);
            Ok(())
        }
    }

    /// Log a message via host_log.
    pub fn log_message(level: i32, msg: &str) {
        unsafe {
            host_log(level, msg.as_ptr() as i32, msg.len() as i32);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::cell::RefCell;

    thread_local! {
        static HTTP_CALLS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
        static LOG_MESSAGES: RefCell<Vec<(i32, String)>> = const { RefCell::new(Vec::new()) };
        static HTTP_CALL_SHOULD_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Mock HTTP call: records the request bytes and returns Ok.
    pub fn http_call(request_json: &[u8]) -> Result<(), ()> {
        if HTTP_CALL_SHOULD_FAIL.with(|f| f.get()) {
            return Err(());
        }
        HTTP_CALLS.with(|calls| calls.borrow_mut().push(request_json.to_vec()));
        Ok(())
    }

    pub fn log_message(level: i32, msg: &str) {
        LOG_MESSAGES.with(|logs| logs.borrow_mut().push((level, msg.to_string())));
    }

    // ==================== Test helpers ====================

    #[cfg(test)]
    pub fn reset_mock_state() {
        HTTP_CALLS.with(|calls| calls.borrow_mut().clear());
        LOG_MESSAGES.with(|logs| logs.borrow_mut().clear());
        HTTP_CALL_SHOULD_FAIL.with(|f| f.set(false));
    }

    #[cfg(test)]
    pub fn get_http_calls() -> Vec<Vec<u8>> {
        HTTP_CALLS.with(|calls| calls.borrow().clone())
    }

    #[cfg(test)]
    pub fn get_log_messages() -> Vec<(i32, String)> {
        LOG_MESSAGES.with(|logs| logs.borrow().clone())
    }

    #[cfg(test)]
    pub fn set_http_call_should_fail(fail: bool) {
        HTTP_CALL_SHOULD_FAIL.with(|f| f.set(fail));
    }
}

fn log_message(level: i32, msg: &str) {
    host::log_message(level, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request() -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Request {
            method: "POST".to_string(),
            path: "/webhooks/ingest".to_string(),
            query: None,
            headers,
            body: Some(br#"{"event":"order.created"}"#.to_vec()),
            client_ip: "10.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        }
    }

    fn minimal_plugin() -> FireAndForgetDispatcher {
        serde_json::from_value(serde_json::json!({
            "url": "http://backend:3000/ingest"
        }))
        .expect("minimal config should deserialize")
    }

    // ==================== Config deserialization ====================

    #[test]
    fn config_minimal() {
        let plugin = minimal_plugin();
        assert_eq!(plugin.url, "http://backend:3000/ingest");
        assert_eq!(plugin.timeout_ms, 5000);
        assert_eq!(plugin.response.status, 202);
        assert_eq!(plugin.response.content_type, "application/json");
        assert!(plugin.response.body.is_empty());
        assert!(plugin.response.headers.is_empty());
    }

    #[test]
    fn config_full() {
        let plugin: FireAndForgetDispatcher = serde_json::from_value(serde_json::json!({
            "url": "http://backend:3000/ingest",
            "timeout_ms": 10000,
            "response": {
                "status": 200,
                "content_type": "text/plain",
                "body": "OK",
                "headers": { "x-custom": "value" }
            }
        }))
        .expect("full config should deserialize");

        assert_eq!(plugin.timeout_ms, 10000);
        assert_eq!(plugin.response.status, 200);
        assert_eq!(plugin.response.content_type, "text/plain");
        assert_eq!(plugin.response.body, "OK");
        assert_eq!(plugin.response.headers["x-custom"], "value");
    }

    #[test]
    fn config_missing_url_fails() {
        let result: Result<FireAndForgetDispatcher, _> =
            serde_json::from_value(serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn config_response_partial_defaults() {
        let plugin: FireAndForgetDispatcher = serde_json::from_value(serde_json::json!({
            "url": "http://backend:3000",
            "response": { "status": 204 }
        }))
        .expect("partial response config should deserialize");

        assert_eq!(plugin.response.status, 204);
        assert_eq!(plugin.response.content_type, "application/json");
        assert!(plugin.response.body.is_empty());
    }

    // ==================== dispatch behavior ====================

    #[test]
    fn dispatch_returns_configured_response() {
        host::reset_mock_state();

        let mut plugin: FireAndForgetDispatcher = serde_json::from_value(serde_json::json!({
            "url": "http://backend:3000/ingest",
            "response": {
                "status": 202,
                "body": "{\"accepted\":true}",
                "headers": { "x-request-id": "abc" }
            }
        }))
        .unwrap();

        let resp = plugin.dispatch(test_request());

        assert_eq!(resp.status, 202);
        assert_eq!(resp.body_str(), Some("{\"accepted\":true}"));
        assert_eq!(resp.headers["content-type"], "application/json");
        assert_eq!(resp.headers["x-request-id"], "abc");
    }

    #[test]
    fn dispatch_empty_body_returns_none() {
        host::reset_mock_state();

        let mut plugin = minimal_plugin();
        let resp = plugin.dispatch(test_request());

        assert_eq!(resp.status, 202);
        assert!(resp.body.is_none());
    }

    #[test]
    fn dispatch_forwards_request_to_upstream() {
        host::reset_mock_state();

        let mut plugin = minimal_plugin();
        plugin.dispatch(test_request());

        let calls = host::get_http_calls();
        assert_eq!(calls.len(), 1);

        let http_req: HttpRequest = serde_json::from_slice(&calls[0]).unwrap();
        assert_eq!(http_req.method, "POST");
        assert_eq!(http_req.url, "http://backend:3000/ingest");
        assert_eq!(http_req.timeout_ms, Some(5000));
        assert_eq!(http_req.headers["content-type"], "application/json");
    }

    #[test]
    fn dispatch_forwards_request_without_body() {
        host::reset_mock_state();

        let mut plugin = minimal_plugin();
        let mut req = test_request();
        req.method = "GET".to_string();
        req.body = None;
        plugin.dispatch(req);

        let calls = host::get_http_calls();
        assert_eq!(calls.len(), 1);

        let http_req: HttpRequest = serde_json::from_slice(&calls[0]).unwrap();
        assert_eq!(http_req.method, "GET");
    }

    #[test]
    fn dispatch_still_responds_on_upstream_failure() {
        host::reset_mock_state();
        host::set_http_call_should_fail(true);

        let mut plugin: FireAndForgetDispatcher = serde_json::from_value(serde_json::json!({
            "url": "http://unreachable:9999",
            "response": { "status": 202, "body": "{\"accepted\":true}" }
        }))
        .unwrap();

        let resp = plugin.dispatch(test_request());

        // Client still gets the configured response
        assert_eq!(resp.status, 202);
        assert_eq!(resp.body_str(), Some("{\"accepted\":true}"));

        // No HTTP call recorded (it failed)
        assert!(host::get_http_calls().is_empty());

        // Warning was logged
        let logs = host::get_log_messages();
        assert!(logs.iter().any(|(level, msg)| *level == 2 && msg.contains("connection error")));
    }

    #[test]
    fn dispatch_no_log_on_success() {
        host::reset_mock_state();

        let mut plugin = minimal_plugin();
        plugin.dispatch(test_request());

        let logs = host::get_log_messages();
        assert!(logs.is_empty());
    }

    #[test]
    fn dispatch_preserves_all_request_headers() {
        host::reset_mock_state();

        let mut plugin = minimal_plugin();
        let mut req = test_request();
        req.headers.insert("authorization".to_string(), "Bearer tok".to_string());
        req.headers.insert("x-custom".to_string(), "value".to_string());
        plugin.dispatch(req);

        let calls = host::get_http_calls();
        let http_req: HttpRequest = serde_json::from_slice(&calls[0]).unwrap();
        assert_eq!(http_req.headers["authorization"], "Bearer tok");
        assert_eq!(http_req.headers["x-custom"], "value");
        assert_eq!(http_req.headers["content-type"], "application/json");
    }

    #[test]
    fn dispatch_custom_timeout() {
        host::reset_mock_state();

        let mut plugin: FireAndForgetDispatcher = serde_json::from_value(serde_json::json!({
            "url": "http://backend:3000",
            "timeout_ms": 500
        }))
        .unwrap();

        plugin.dispatch(test_request());

        let calls = host::get_http_calls();
        let http_req: HttpRequest = serde_json::from_slice(&calls[0]).unwrap();
        assert_eq!(http_req.timeout_ms, Some(500));
    }
}
