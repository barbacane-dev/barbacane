//! HTTP logging middleware plugin for Barbacane API gateway.
//!
//! Captures request/response data and sends structured JSON log entries
//! to a configurable HTTP endpoint. Useful for centralized logging
//! with services like Datadog, Splunk, or custom log aggregators.
//!
//! Note: The outbound HTTP call is synchronous. For high-throughput
//! production use, prefer Kafka or NATS dispatchers for log shipping.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// HTTP logging middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct HttpLog {
    /// URL to send log entries to.
    endpoint: String,

    /// HTTP method for sending logs (POST or PUT).
    #[serde(default = "default_method")]
    method: String,

    /// Timeout in milliseconds for the log HTTP call.
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,

    /// Content-Type header for the log request.
    #[serde(default = "default_content_type")]
    content_type: String,

    /// Include request and response headers in the log entry.
    #[serde(default)]
    include_headers: bool,

    /// Include request and response body size in the log entry.
    #[serde(default)]
    include_body: bool,

    /// Static custom fields to include in every log entry.
    #[serde(default)]
    custom_fields: BTreeMap<String, String>,
}

fn default_method() -> String {
    "POST".to_string()
}

fn default_timeout_ms() -> u64 {
    2000
}

fn default_content_type() -> String {
    "application/json".to_string()
}

/// Structured log entry sent to the HTTP endpoint.
#[derive(Serialize)]
struct LogEntry {
    timestamp_ms: u64,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    request: RequestLog,
    response: ResponseLog,
    #[serde(flatten)]
    custom_fields: BTreeMap<String, String>,
}

/// Request portion of the log entry.
#[derive(Serialize)]
struct RequestLog {
    method: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    client_ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_size: Option<usize>,
}

/// Response portion of the log entry.
#[derive(Serialize)]
struct ResponseLog {
    status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_size: Option<usize>,
}

/// HTTP request for host_http_call.
#[derive(Serialize, Deserialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
    timeout_ms: Option<u64>,
}

impl HttpLog {
    /// Capture request metadata for the response phase.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        // Record start time
        let start_ms = host_time_now_ms();
        context_set("http_log_start_ms", &start_ms.to_string());

        // Store request metadata in context
        context_set("http_log_method", &req.method);
        context_set("http_log_path", &req.path);
        context_set("http_log_client_ip", &req.client_ip);

        if let Some(query) = &req.query {
            context_set("http_log_query", query);
        }

        if self.include_headers {
            if let Ok(headers_json) = serde_json::to_string(&req.headers) {
                context_set("http_log_req_headers", &headers_json);
            }
        }

        if self.include_body {
            if let Some(body) = &req.body {
                context_set("http_log_req_body_size", &body.len().to_string());
            }
        }

        Action::Continue(req)
    }

    /// Build and send the log entry.
    pub fn on_response(&mut self, resp: Response) -> Response {
        // Calculate duration
        let start_ms = context_get("http_log_start_ms")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let end_ms = host_time_now_ms();
        let duration_ms = end_ms.saturating_sub(start_ms);

        // Retrieve correlation ID if available (set by correlation-id middleware)
        let correlation_id = context_get("correlation-id");

        // Build request portion from context
        let request_headers = if self.include_headers {
            context_get("http_log_req_headers").and_then(|h| serde_json::from_str(&h).ok())
        } else {
            None
        };

        let req_body_size = if self.include_body {
            context_get("http_log_req_body_size").and_then(|s| s.parse::<usize>().ok())
        } else {
            None
        };

        let log_entry = LogEntry {
            timestamp_ms: start_ms,
            duration_ms,
            correlation_id,
            request: RequestLog {
                method: context_get("http_log_method").unwrap_or_default(),
                path: context_get("http_log_path").unwrap_or_default(),
                query: context_get("http_log_query"),
                client_ip: context_get("http_log_client_ip").unwrap_or_default(),
                headers: request_headers,
                body_size: req_body_size,
            },
            response: ResponseLog {
                status: resp.status,
                headers: if self.include_headers {
                    Some(resp.headers.clone())
                } else {
                    None
                },
                body_size: if self.include_body {
                    resp.body.as_ref().map(|b| b.len())
                } else {
                    None
                },
            },
            custom_fields: self.custom_fields.clone(),
        };

        // Serialize and send (best-effort, never affects the response)
        if let Ok(payload) = serde_json::to_string(&log_entry) {
            self.send_log(&payload);
        }

        resp
    }

    /// Send the log entry to the configured endpoint.
    fn send_log(&self, payload: &str) {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), self.content_type.clone());

        let http_request = HttpRequest {
            method: self.method.clone(),
            url: self.endpoint.clone(),
            headers,
            body: Some(payload.to_string()),
            timeout_ms: Some(self.timeout_ms),
        };

        let request_json = match serde_json::to_vec(&http_request) {
            Ok(json) => json,
            Err(e) => {
                log_message(
                    2, // WARN
                    &format!("http-log: failed to serialize request: {}", e),
                );
                return;
            }
        };

        if let Err(()) = host::http_call(&request_json) {
            log_message(2, "http-log: failed to send log entry (connection error)");
        }
    }
}

// ==================== Host function declarations ====================
//
// WASM implementations call real host functions via FFI.
// Native implementations (for unit testing) use thread-local mock state.

#[cfg(target_arch = "wasm32")]
mod host {
    /// Get current time in milliseconds.
    pub fn time_now_ms() -> u64 {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_time_now() -> i64;
        }
        unsafe { host_time_now() as u64 }
    }

    /// Log a message via host_log.
    pub fn log_message(level: i32, msg: &str) {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
        }
        unsafe {
            host_log(level, msg.as_ptr() as i32, msg.len() as i32);
        }
    }

    /// Store a value in the request context.
    pub fn context_set(key: &str, value: &str) {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_context_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32);
        }
        unsafe {
            host_context_set(
                key.as_ptr() as i32,
                key.len() as i32,
                value.as_ptr() as i32,
                value.len() as i32,
            );
        }
    }

    /// Get a value from the request context.
    pub fn context_get(key: &str) -> Option<String> {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_context_get(key_ptr: i32, key_len: i32) -> i32;
            fn host_context_read_result(buf_ptr: i32, buf_len: i32) -> i32;
        }

        unsafe {
            let len = host_context_get(key.as_ptr() as i32, key.len() as i32);
            if len <= 0 {
                return None;
            }

            let mut buf = vec![0u8; len as usize];
            let read_len = host_context_read_result(buf.as_mut_ptr() as i32, len);
            if read_len != len {
                return None;
            }

            String::from_utf8(buf).ok()
        }
    }

    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        #[link_name = "host_http_call"]
        fn ffi_http_call(req_ptr: i32, req_len: i32) -> i32;
        #[link_name = "host_http_read_result"]
        fn ffi_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }

    /// Make an outbound HTTP request. Returns Ok(()) on success, Err(()) on failure.
    /// The response body is discarded (fire-and-forget for logging).
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
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        static CONTEXT: RefCell<BTreeMap<String, String>> = RefCell::new(BTreeMap::new());
        static TIME_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static HTTP_CALLS: RefCell<Vec<Vec<u8>>> = RefCell::new(Vec::new());
        static LOG_MESSAGES: RefCell<Vec<(i32, String)>> = RefCell::new(Vec::new());
    }

    pub fn time_now_ms() -> u64 {
        TIME_MS.with(|t| t.get())
    }

    pub fn log_message(level: i32, msg: &str) {
        LOG_MESSAGES.with(|logs| logs.borrow_mut().push((level, msg.to_string())));
    }

    pub fn context_set(key: &str, value: &str) {
        CONTEXT.with(|ctx| ctx.borrow_mut().insert(key.to_string(), value.to_string()));
    }

    pub fn context_get(key: &str) -> Option<String> {
        CONTEXT.with(|ctx| ctx.borrow().get(key).cloned())
    }

    /// Mock HTTP call: records the request bytes and returns Ok.
    pub fn http_call(request_json: &[u8]) -> Result<(), ()> {
        HTTP_CALLS.with(|calls| calls.borrow_mut().push(request_json.to_vec()));
        Ok(())
    }

    // ==================== Test helpers ====================

    #[cfg(test)]
    pub fn set_mock_time(ms: u64) {
        TIME_MS.with(|t| t.set(ms));
    }

    #[cfg(test)]
    pub fn reset_mock_state() {
        CONTEXT.with(|ctx| ctx.borrow_mut().clear());
        TIME_MS.with(|t| t.set(0));
        HTTP_CALLS.with(|calls| calls.borrow_mut().clear());
        LOG_MESSAGES.with(|logs| logs.borrow_mut().clear());
    }

    #[cfg(test)]
    pub fn get_http_calls() -> Vec<Vec<u8>> {
        HTTP_CALLS.with(|calls| calls.borrow().clone())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn get_log_messages() -> Vec<(i32, String)> {
        LOG_MESSAGES.with(|logs| logs.borrow().clone())
    }
}

// Re-export host functions at crate level for use in impl methods.
fn host_time_now_ms() -> u64 {
    host::time_now_ms()
}
fn log_message(level: i32, msg: &str) {
    host::log_message(level, msg);
}
fn context_set(key: &str, value: &str) {
    host::context_set(key, value);
}
fn context_get(key: &str) -> Option<String> {
    host::context_get(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_plugin() -> HttpLog {
        HttpLog {
            endpoint: "http://localhost:9999/logs".to_string(),
            method: default_method(),
            timeout_ms: default_timeout_ms(),
            content_type: default_content_type(),
            include_headers: false,
            include_body: false,
            custom_fields: BTreeMap::new(),
        }
    }

    fn test_request() -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Request {
            method: "POST".to_string(),
            path: "/users".to_string(),
            query: Some("page=1".to_string()),
            headers,
            body: Some(r#"{"name":"alice"}"#.to_string()),
            client_ip: "10.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        }
    }

    fn test_response() -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Response {
            status: 201,
            headers,
            body: Some(r#"{"id":42}"#.to_string()),
        }
    }

    // ==================== Config deserialization ====================

    #[test]
    fn config_defaults() {
        let json = r#"{"endpoint":"http://example.com/logs"}"#;
        let config: HttpLog = serde_json::from_str(json).unwrap();
        assert_eq!(config.method, "POST");
        assert_eq!(config.timeout_ms, 2000);
        assert_eq!(config.content_type, "application/json");
        assert!(!config.include_headers);
        assert!(!config.include_body);
        assert!(config.custom_fields.is_empty());
    }

    #[test]
    fn config_full() {
        let json = r#"{
            "endpoint": "http://logs.example.com",
            "method": "PUT",
            "timeout_ms": 5000,
            "content_type": "application/x-ndjson",
            "include_headers": true,
            "include_body": true,
            "custom_fields": {"env": "prod", "service": "api"}
        }"#;
        let config: HttpLog = serde_json::from_str(json).unwrap();
        assert_eq!(config.method, "PUT");
        assert_eq!(config.timeout_ms, 5000);
        assert!(config.include_headers);
        assert!(config.include_body);
        assert_eq!(config.custom_fields.len(), 2);
        assert_eq!(config.custom_fields["env"], "prod");
    }

    // ==================== LogEntry serialization ====================

    #[test]
    fn log_entry_minimal_serialization() {
        let entry = LogEntry {
            timestamp_ms: 1000,
            duration_ms: 50,
            correlation_id: None,
            request: RequestLog {
                method: "GET".to_string(),
                path: "/health".to_string(),
                query: None,
                client_ip: "127.0.0.1".to_string(),
                headers: None,
                body_size: None,
            },
            response: ResponseLog {
                status: 200,
                headers: None,
                body_size: None,
            },
            custom_fields: BTreeMap::new(),
        };

        let json: serde_json::Value = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["timestamp_ms"], 1000);
        assert_eq!(json["duration_ms"], 50);
        assert!(json.get("correlation_id").is_none());
        assert_eq!(json["request"]["method"], "GET");
        assert_eq!(json["response"]["status"], 200);
        assert!(json["request"].get("headers").is_none());
        assert!(json["request"].get("body_size").is_none());
    }

    #[test]
    fn log_entry_with_optional_fields() {
        let mut req_headers = BTreeMap::new();
        req_headers.insert("accept".to_string(), "application/json".to_string());

        let mut custom = BTreeMap::new();
        custom.insert("env".to_string(), "staging".to_string());

        let entry = LogEntry {
            timestamp_ms: 2000,
            duration_ms: 120,
            correlation_id: Some("abc-123".to_string()),
            request: RequestLog {
                method: "POST".to_string(),
                path: "/users".to_string(),
                query: Some("verbose=true".to_string()),
                client_ip: "10.0.0.1".to_string(),
                headers: Some(req_headers),
                body_size: Some(256),
            },
            response: ResponseLog {
                status: 201,
                headers: None,
                body_size: Some(64),
            },
            custom_fields: custom,
        };

        let json: serde_json::Value = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["correlation_id"], "abc-123");
        assert_eq!(json["request"]["query"], "verbose=true");
        assert_eq!(json["request"]["headers"]["accept"], "application/json");
        assert_eq!(json["request"]["body_size"], 256);
        assert_eq!(json["response"]["body_size"], 64);
        // custom_fields are flattened
        assert_eq!(json["env"], "staging");
    }

    // ==================== on_request context capture ====================

    #[test]
    fn on_request_captures_metadata_in_context() {
        host::reset_mock_state();
        host::set_mock_time(5000);

        let mut plugin = test_plugin();
        let req = test_request();
        let result = plugin.on_request(req);

        assert!(matches!(result, Action::Continue(_)));
        assert_eq!(host::context_get("http_log_start_ms").unwrap(), "5000");
        assert_eq!(host::context_get("http_log_method").unwrap(), "POST");
        assert_eq!(host::context_get("http_log_path").unwrap(), "/users");
        assert_eq!(host::context_get("http_log_client_ip").unwrap(), "10.0.0.1");
        assert_eq!(host::context_get("http_log_query").unwrap(), "page=1");
    }

    #[test]
    fn on_request_skips_headers_when_disabled() {
        host::reset_mock_state();

        let mut plugin = test_plugin();
        plugin.include_headers = false;
        plugin.on_request(test_request());

        assert!(host::context_get("http_log_req_headers").is_none());
    }

    #[test]
    fn on_request_captures_headers_when_enabled() {
        host::reset_mock_state();

        let mut plugin = test_plugin();
        plugin.include_headers = true;
        plugin.on_request(test_request());

        let headers_json = host::context_get("http_log_req_headers").unwrap();
        let headers: BTreeMap<String, String> = serde_json::from_str(&headers_json).unwrap();
        assert_eq!(headers["content-type"], "application/json");
    }

    #[test]
    fn on_request_captures_body_size_when_enabled() {
        host::reset_mock_state();

        let mut plugin = test_plugin();
        plugin.include_body = true;
        plugin.on_request(test_request());

        let size = host::context_get("http_log_req_body_size").unwrap();
        assert_eq!(size, "16"); // r#"{"name":"alice"}"# is 16 bytes
    }

    // ==================== on_response log sending ====================

    #[test]
    fn on_response_sends_log_entry() {
        host::reset_mock_state();
        host::set_mock_time(1000);

        let mut plugin = test_plugin();
        plugin.on_request(test_request());

        // Advance mock time to simulate processing
        host::set_mock_time(1050);
        plugin.on_response(test_response());

        let calls = host::get_http_calls();
        assert_eq!(calls.len(), 1);

        // Verify the HTTP request sent to the log endpoint
        let http_req: HttpRequest = serde_json::from_slice(&calls[0]).unwrap();
        assert_eq!(http_req.method, "POST");
        assert_eq!(http_req.url, "http://localhost:9999/logs");
        assert_eq!(http_req.timeout_ms, Some(2000));

        // Verify the log entry payload
        let payload: serde_json::Value =
            serde_json::from_str(http_req.body.as_ref().unwrap()).unwrap();
        assert_eq!(payload["timestamp_ms"], 1000);
        assert_eq!(payload["duration_ms"], 50);
        assert_eq!(payload["request"]["method"], "POST");
        assert_eq!(payload["request"]["path"], "/users");
        assert_eq!(payload["response"]["status"], 201);
    }

    #[test]
    fn on_response_includes_custom_fields() {
        host::reset_mock_state();

        let mut plugin = test_plugin();
        plugin
            .custom_fields
            .insert("service".to_string(), "my-api".to_string());

        plugin.on_request(test_request());
        plugin.on_response(test_response());

        let calls = host::get_http_calls();
        let http_req: HttpRequest = serde_json::from_slice(&calls[0]).unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(http_req.body.as_ref().unwrap()).unwrap();
        assert_eq!(payload["service"], "my-api");
    }

    #[test]
    fn on_response_never_modifies_response() {
        host::reset_mock_state();

        let mut plugin = test_plugin();
        plugin.on_request(test_request());

        let resp = test_response();
        let returned = plugin.on_response(resp.clone());

        assert_eq!(returned.status, resp.status);
        assert_eq!(returned.headers, resp.headers);
        assert_eq!(returned.body, resp.body);
    }

    // ==================== HttpRequest serialization ====================

    #[test]
    fn http_request_uses_configured_method_and_content_type() {
        host::reset_mock_state();

        let mut plugin = test_plugin();
        plugin.method = "PUT".to_string();
        plugin.content_type = "application/x-ndjson".to_string();

        plugin.on_request(test_request());
        plugin.on_response(test_response());

        let calls = host::get_http_calls();
        let http_req: HttpRequest = serde_json::from_slice(&calls[0]).unwrap();
        assert_eq!(http_req.method, "PUT");
        assert_eq!(http_req.headers["content-type"], "application/x-ndjson");
    }
}
