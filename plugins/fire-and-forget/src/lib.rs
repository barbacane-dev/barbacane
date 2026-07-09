//! Fire-and-forget dispatcher plugin for Barbacane API gateway.
//!
//! Forwards the incoming request to a configured upstream URL without
//! waiting for the result, and returns an immediate static response.
//! Useful for webhook ingestion, async job submission, and audit trails.
//!
//! The outbound HTTP call is best-effort: if the upstream is unreachable
//! or returns an error, the client still receives the configured response.

use barbacane_plugin_sdk::http;
#[cfg(target_arch = "wasm32")]
use barbacane_plugin_sdk::log::log as log_message;
use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
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

/// Mirror of [`barbacane_plugin_sdk::http::HttpRequest`] used only by the native
/// test mock to deserialize the request bytes captured by `host::http_call`.
/// The SDK's own `HttpRequest` is `Serialize`-only, so tests keep this local
/// `Deserialize` copy.
#[cfg(test)]
#[derive(serde::Deserialize)]
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
        let request = http::HttpRequest {
            method: req.method.clone(),
            url: self.url.clone(),
            headers: req.headers.clone(),
            timeout_ms: Some(self.timeout_ms),
        };

        // Best-effort: the request body travels via the side-channel inside
        // `http::call`, and the response is ignored (fire-and-forget). A
        // connection failure is logged but never affects the client response.
        if host::http_call(&request, req.body.as_deref()).is_err() {
            log_message(
                2, // WARN
                "fire-and-forget: upstream call failed (connection error)",
            );
        }
    }

    /// Build the configured static response.
    fn build_response(&self) -> Response {
        let mut headers = self.response.headers.clone();
        headers.insert(
            "content-type".to_string(),
            self.response.content_type.clone(),
        );

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
    /// Forward the request through the shared SDK HTTP helper. The response is
    /// discarded (fire-and-forget); any transport error maps to `Err(())`.
    pub fn http_call(
        request: &barbacane_plugin_sdk::http::HttpRequest,
        body: Option<&[u8]>,
    ) -> Result<(), ()> {
        barbacane_plugin_sdk::http::call(request, body)
            .map(|_| ())
            .map_err(|_| ())
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

    /// Mock HTTP call: serializes and records the request, returns Ok (or Err
    /// when `set_http_call_should_fail(true)`). Mirrors the shared SDK helper's
    /// signature so the production code path is identical on native and wasm.
    pub fn http_call(
        request: &barbacane_plugin_sdk::http::HttpRequest,
        _body: Option<&[u8]>,
    ) -> Result<(), ()> {
        if HTTP_CALL_SHOULD_FAIL.with(|f| f.get()) {
            return Err(());
        }
        let json = serde_json::to_vec(request).unwrap_or_default();
        HTTP_CALLS.with(|calls| calls.borrow_mut().push(json));
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

#[cfg(not(target_arch = "wasm32"))]
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
        assert!(logs
            .iter()
            .any(|(level, msg)| *level == 2 && msg.contains("connection error")));
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
        req.headers
            .insert("authorization".to_string(), "Bearer tok".to_string());
        req.headers
            .insert("x-custom".to_string(), "value".to_string());
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
