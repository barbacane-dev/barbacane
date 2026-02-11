//! Correlation ID middleware plugin for Barbacane API gateway.
//!
//! Propagates or generates correlation IDs (UUID v7) for distributed tracing.
//! The correlation ID is passed to upstream services and optionally included
//! in the response.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;

/// Correlation ID middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct CorrelationId {
    /// Header name for the correlation ID.
    /// Default: "x-correlation-id"
    #[serde(default = "default_header_name")]
    header_name: String,

    /// Generate a new correlation ID if none is provided.
    /// Default: true
    #[serde(default = "default_generate_if_missing")]
    generate_if_missing: bool,

    /// Trust and propagate correlation IDs from incoming requests.
    /// Default: true
    #[serde(default = "default_trust_incoming")]
    trust_incoming: bool,

    /// Include the correlation ID in the response headers.
    /// Default: true
    #[serde(default = "default_include_in_response")]
    include_in_response: bool,
}

fn default_header_name() -> String {
    "x-correlation-id".to_string()
}

fn default_generate_if_missing() -> bool {
    true
}

fn default_trust_incoming() -> bool {
    true
}

fn default_include_in_response() -> bool {
    true
}

impl CorrelationId {
    /// Handle incoming request - propagate or generate correlation ID.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        let mut modified_req = req;
        let header_name_lower = self.header_name.to_lowercase();

        // Check for existing correlation ID
        let existing_id = if self.trust_incoming {
            modified_req.headers.get(&header_name_lower).cloned()
        } else {
            None
        };

        // Determine the correlation ID to use
        let correlation_id = match existing_id {
            Some(id) => id,
            None if self.generate_if_missing => {
                // Generate new UUID v7
                match generate_uuid() {
                    Some(uuid) => uuid,
                    None => {
                        log_message(0, "correlation-id: failed to generate UUID");
                        return Action::Continue(modified_req);
                    }
                }
            }
            None => {
                // No ID and not generating - pass through without modification
                return Action::Continue(modified_req);
            }
        };

        // Set the correlation ID header (overwrites if exists and not trusted)
        modified_req
            .headers
            .insert(header_name_lower.clone(), correlation_id.clone());

        // Store for response handling using context storage
        context_set("correlation-id", &correlation_id);

        Action::Continue(modified_req)
    }

    /// Add correlation ID to response headers.
    pub fn on_response(&mut self, resp: Response) -> Response {
        if !self.include_in_response {
            return resp;
        }

        // Retrieve the correlation ID from context storage
        let mut modified_resp = resp;
        if let Some(correlation_id) = context_get("correlation-id") {
            let header_name_lower = self.header_name.to_lowercase();
            modified_resp
                .headers
                .insert(header_name_lower, correlation_id);
        }

        modified_resp
    }
}

// Native mock implementations for testing
#[cfg(not(target_arch = "wasm32"))]
mod mock_host {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static CONTEXT: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
        static UUID_COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    }

    pub fn context_set(key: &str, value: &str) {
        CONTEXT.with(|c| c.borrow_mut().insert(key.to_string(), value.to_string()));
    }

    pub fn context_get(key: &str) -> Option<String> {
        CONTEXT.with(|c| c.borrow().get(key).cloned())
    }

    pub fn generate_uuid() -> Option<String> {
        UUID_COUNTER.with(|c| {
            let n = c.get();
            c.set(n + 1);
            Some(format!("00000000-0000-7000-8000-{:012}", n))
        })
    }

    #[cfg(test)]
    pub fn reset() {
        CONTEXT.with(|c| c.borrow_mut().clear());
        UUID_COUNTER.with(|c| c.set(1));
    }
}

/// Generate a UUID v7 using the host function.
#[cfg(target_arch = "wasm32")]
fn generate_uuid() -> Option<String> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_uuid_generate() -> i32;
        fn host_uuid_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }

    unsafe {
        let len = host_uuid_generate();
        if len <= 0 {
            return None;
        }

        let mut buf = vec![0u8; len as usize];
        let read_len = host_uuid_read_result(buf.as_mut_ptr() as i32, len);
        if read_len != len {
            return None;
        }

        String::from_utf8(buf).ok()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn generate_uuid() -> Option<String> {
    mock_host::generate_uuid()
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
fn log_message(_level: i32, _msg: &str) {
    // No-op on native
}

/// Store a value in the request context.
#[cfg(target_arch = "wasm32")]
fn context_set(key: &str, value: &str) {
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

#[cfg(not(target_arch = "wasm32"))]
fn context_set(key: &str, value: &str) {
    mock_host::context_set(key, value)
}

/// Get a value from the request context.
#[cfg(target_arch = "wasm32")]
fn context_get(key: &str) -> Option<String> {
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

#[cfg(not(target_arch = "wasm32"))]
fn context_get(key: &str) -> Option<String> {
    mock_host::context_get(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn create_test_request() -> Request {
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    fn create_test_response() -> Response {
        Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        }
    }

    #[test]
    fn test_config_deserialization_defaults() {
        mock_host::reset();

        let config: CorrelationId = serde_json::from_str("{}").unwrap();
        assert_eq!(config.header_name, "x-correlation-id");
        assert!(config.generate_if_missing);
        assert!(config.trust_incoming);
        assert!(config.include_in_response);
    }

    #[test]
    fn test_config_deserialization_custom() {
        mock_host::reset();

        let config: CorrelationId = serde_json::from_str(
            r#"{
                "header_name": "x-custom-id",
                "generate_if_missing": false,
                "trust_incoming": false,
                "include_in_response": false
            }"#,
        )
        .unwrap();
        assert_eq!(config.header_name, "x-custom-id");
        assert!(!config.generate_if_missing);
        assert!(!config.trust_incoming);
        assert!(!config.include_in_response);
    }

    #[test]
    fn test_on_request_with_existing_correlation_id() {
        mock_host::reset();

        let mut middleware = CorrelationId {
            header_name: "x-correlation-id".to_string(),
            generate_if_missing: true,
            trust_incoming: true,
            include_in_response: true,
        };

        let mut req = create_test_request();
        req.headers.insert(
            "x-correlation-id".to_string(),
            "existing-id-123".to_string(),
        );

        let result = middleware.on_request(req);
        if let Action::Continue(modified_req) = result {
            assert_eq!(
                modified_req.headers.get("x-correlation-id"),
                Some(&"existing-id-123".to_string())
            );
            assert_eq!(
                context_get("correlation-id"),
                Some("existing-id-123".to_string())
            );
        } else {
            panic!("Expected Action::Continue");
        }
    }

    #[test]
    fn test_on_request_without_correlation_id_generates_one() {
        mock_host::reset();

        let mut middleware = CorrelationId {
            header_name: "x-correlation-id".to_string(),
            generate_if_missing: true,
            trust_incoming: true,
            include_in_response: true,
        };

        let req = create_test_request();
        let result = middleware.on_request(req);

        if let Action::Continue(modified_req) = result {
            let correlation_id = modified_req.headers.get("x-correlation-id");
            assert!(correlation_id.is_some());
            assert_eq!(
                correlation_id.unwrap(),
                "00000000-0000-7000-8000-000000000001"
            );
            assert_eq!(
                context_get("correlation-id"),
                Some("00000000-0000-7000-8000-000000000001".to_string())
            );
        } else {
            panic!("Expected Action::Continue");
        }
    }

    #[test]
    fn test_on_request_trust_incoming_false_ignores_existing() {
        mock_host::reset();

        let mut middleware = CorrelationId {
            header_name: "x-correlation-id".to_string(),
            generate_if_missing: true,
            trust_incoming: false,
            include_in_response: true,
        };

        let mut req = create_test_request();
        req.headers
            .insert("x-correlation-id".to_string(), "untrusted-id".to_string());

        let result = middleware.on_request(req);

        if let Action::Continue(modified_req) = result {
            let correlation_id = modified_req.headers.get("x-correlation-id").unwrap();
            assert_ne!(correlation_id, "untrusted-id");
            assert_eq!(correlation_id, "00000000-0000-7000-8000-000000000001");
            assert_eq!(
                context_get("correlation-id"),
                Some("00000000-0000-7000-8000-000000000001".to_string())
            );
        } else {
            panic!("Expected Action::Continue");
        }
    }

    #[test]
    fn test_on_request_generate_if_missing_false_passes_through() {
        mock_host::reset();

        let mut middleware = CorrelationId {
            header_name: "x-correlation-id".to_string(),
            generate_if_missing: false,
            trust_incoming: true,
            include_in_response: true,
        };

        let req = create_test_request();
        let result = middleware.on_request(req);

        if let Action::Continue(modified_req) = result {
            assert!(!modified_req.headers.contains_key("x-correlation-id"));
            assert!(context_get("correlation-id").is_none());
        } else {
            panic!("Expected Action::Continue");
        }
    }

    #[test]
    fn test_on_response_includes_correlation_id() {
        mock_host::reset();

        context_set("correlation-id", "test-id-456");

        let mut middleware = CorrelationId {
            header_name: "x-correlation-id".to_string(),
            generate_if_missing: true,
            trust_incoming: true,
            include_in_response: true,
        };

        let resp = create_test_response();
        let modified_resp = middleware.on_response(resp);

        assert_eq!(
            modified_resp.headers.get("x-correlation-id"),
            Some(&"test-id-456".to_string())
        );
    }

    #[test]
    fn test_on_response_include_in_response_false() {
        mock_host::reset();

        context_set("correlation-id", "test-id-789");

        let mut middleware = CorrelationId {
            header_name: "x-correlation-id".to_string(),
            generate_if_missing: true,
            trust_incoming: true,
            include_in_response: false,
        };

        let resp = create_test_response();
        let modified_resp = middleware.on_response(resp);

        assert!(!modified_resp.headers.contains_key("x-correlation-id"));
    }

    #[test]
    fn test_custom_header_name() {
        mock_host::reset();

        let mut middleware = CorrelationId {
            header_name: "x-request-id".to_string(),
            generate_if_missing: true,
            trust_incoming: true,
            include_in_response: true,
        };

        // Test on_request with custom header
        let mut req = create_test_request();
        req.headers
            .insert("x-request-id".to_string(), "custom-id-abc".to_string());

        let result = middleware.on_request(req);
        if let Action::Continue(modified_req) = result {
            assert_eq!(
                modified_req.headers.get("x-request-id"),
                Some(&"custom-id-abc".to_string())
            );
        } else {
            panic!("Expected Action::Continue");
        }

        // Test on_response with custom header
        let resp = create_test_response();
        let modified_resp = middleware.on_response(resp);

        assert_eq!(
            modified_resp.headers.get("x-request-id"),
            Some(&"custom-id-abc".to_string())
        );
    }

    #[test]
    fn test_on_response_without_context_value() {
        mock_host::reset();

        let mut middleware = CorrelationId {
            header_name: "x-correlation-id".to_string(),
            generate_if_missing: true,
            trust_incoming: true,
            include_in_response: true,
        };

        let resp = create_test_response();
        let modified_resp = middleware.on_response(resp);

        assert!(!modified_resp.headers.contains_key("x-correlation-id"));
    }
}
