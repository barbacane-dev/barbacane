//! Request size limit middleware plugin for Barbacane API gateway.
//!
//! Rejects requests that exceed a configurable size limit.
//! Checks both Content-Length header and actual body size.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Request size limit middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct RequestSizeLimit {
    /// Maximum allowed request body size in bytes.
    /// Default: 1048576 (1 MiB)
    #[serde(default = "default_max_bytes")]
    max_bytes: u64,

    /// Whether to check Content-Length header for early rejection.
    /// Default: true
    #[serde(default = "default_check_content_length")]
    check_content_length: bool,
}

fn default_max_bytes() -> u64 {
    1_048_576 // 1 MiB
}

fn default_check_content_length() -> bool {
    true
}

impl RequestSizeLimit {
    /// Handle incoming request - check size limits.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        // Check Content-Length header first for early rejection
        if self.check_content_length {
            if let Some(content_length) = req.headers.get("content-length") {
                if let Ok(size) = content_length.parse::<u64>() {
                    if size > self.max_bytes {
                        return Action::ShortCircuit(self.payload_too_large_response(size));
                    }
                }
            }
        }

        // Check actual body size
        if let Some(body) = &req.body {
            let size = body.len() as u64;
            if size > self.max_bytes {
                return Action::ShortCircuit(self.payload_too_large_response(size));
            }
        }

        Action::Continue(req)
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Generate 413 Payload Too Large response.
    fn payload_too_large_response(&self, actual_size: u64) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let body = serde_json::json!({
            "type": "urn:barbacane:error:payload-too-large",
            "title": "Payload Too Large",
            "status": 413,
            "detail": format!(
                "Request body size {} bytes exceeds maximum allowed size of {} bytes.",
                actual_size, self.max_bytes
            )
        });

        Response {
            status: 413,
            headers,
            body: Some(body.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_plugin() -> RequestSizeLimit {
        serde_json::from_value(serde_json::json!({
            "max_bytes": 1024
        }))
        .unwrap()
    }

    fn request_with_body(body: &str) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("content-length".to_string(), body.len().to_string());
        Request {
            method: "POST".to_string(),
            path: "/test".to_string(),
            headers,
            body: Some(body.to_string()),
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    fn request_with_content_length(content_length: u64) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("content-length".to_string(), content_length.to_string());
        Request {
            method: "POST".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    #[test]
    fn test_small_body_passes() {
        let mut plugin = test_plugin();
        let req = request_with_body("hello");
        assert!(matches!(plugin.on_request(req), Action::Continue(_)));
    }

    #[test]
    fn test_exact_limit_passes() {
        let mut plugin = test_plugin();
        let body = "x".repeat(1024);
        let req = request_with_body(&body);
        assert!(matches!(plugin.on_request(req), Action::Continue(_)));
    }

    #[test]
    fn test_over_limit_rejected() {
        let mut plugin = test_plugin();
        let body = "x".repeat(1025);
        let req = request_with_body(&body);
        match plugin.on_request(req) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 413),
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_content_length_header_rejected() {
        let mut plugin = test_plugin();
        let req = request_with_content_length(2048);
        match plugin.on_request(req) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 413);
                let body: serde_json::Value =
                    serde_json::from_str(r.body.as_ref().unwrap()).unwrap();
                assert_eq!(body["type"], "urn:barbacane:error:payload-too-large");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_content_length_check_disabled() {
        let mut plugin: RequestSizeLimit = serde_json::from_value(serde_json::json!({
            "max_bytes": 1024,
            "check_content_length": false
        }))
        .unwrap();
        let req = request_with_content_length(2048);
        assert!(matches!(plugin.on_request(req), Action::Continue(_)));
    }

    #[test]
    fn test_no_body_passes() {
        let mut plugin = test_plugin();
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };
        assert!(matches!(plugin.on_request(req), Action::Continue(_)));
    }

    #[test]
    fn test_config_defaults() {
        let plugin: RequestSizeLimit = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(plugin.max_bytes, 1_048_576);
        assert!(plugin.check_content_length);
    }

    #[test]
    fn test_payload_too_large_response_format() {
        let plugin = test_plugin();
        let resp = plugin.payload_too_large_response(2048);
        assert_eq!(resp.status, 413);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["status"], 413);
        assert!(body["detail"].as_str().unwrap().contains("2048"));
        assert!(body["detail"].as_str().unwrap().contains("1024"));
    }

    #[test]
    fn test_on_response_passthrough() {
        let mut plugin = test_plugin();
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some("ok".to_string()),
        };
        let result = plugin.on_response(resp);
        assert_eq!(result.status, 200);
    }
}
