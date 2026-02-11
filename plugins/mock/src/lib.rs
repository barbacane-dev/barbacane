//! Mock dispatcher plugin for Barbacane API gateway.
//!
//! Returns static responses configured in the OpenAPI spec.
//! Useful for health checks, stubs, and testing.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Mock dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct MockDispatcher {
    /// HTTP status code to return (default: 200).
    #[serde(default = "default_status")]
    status: u16,

    /// Response body to return (default: empty string).
    #[serde(default)]
    body: String,

    /// Additional response headers (BTreeMap to avoid WASI random dependency).
    #[serde(default)]
    headers: BTreeMap<String, String>,

    /// Content-Type header value (default: application/json).
    #[serde(default = "default_content_type")]
    content_type: String,
}

fn default_status() -> u16 {
    200
}

fn default_content_type() -> String {
    "application/json".to_string()
}

impl MockDispatcher {
    /// Handle a request and return the configured static response.
    pub fn dispatch(&mut self, _req: Request) -> Response {
        let mut headers = self.headers.clone();
        headers.insert("content-type".to_string(), self.content_type.clone());

        Response {
            status: self.status,
            headers,
            body: if self.body.is_empty() {
                None
            } else {
                Some(self.body.clone())
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request() -> Request {
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

    #[test]
    fn test_default_config() {
        let plugin: MockDispatcher = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(plugin.status, 200);
        assert_eq!(plugin.body, "");
        assert!(plugin.headers.is_empty());
        assert_eq!(plugin.content_type, "application/json");
    }

    #[test]
    fn test_dispatch_default_response() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({})).unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.status, 200);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/json"
        );
        assert!(resp.body.is_none());
    }

    #[test]
    fn test_dispatch_custom_body() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "{\"message\":\"hello\"}"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.body.as_deref(), Some("{\"message\":\"hello\"}"));
    }

    #[test]
    fn test_dispatch_custom_status() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "status": 201,
            "body": "created"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.status, 201);
    }

    #[test]
    fn test_dispatch_custom_headers() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "headers": { "x-custom": "value" }
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.headers.get("x-custom").unwrap(), "value");
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn test_dispatch_custom_content_type() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "content_type": "text/plain",
            "body": "hello"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.headers.get("content-type").unwrap(), "text/plain");
    }

    #[test]
    fn test_dispatch_ignores_request() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "status": 200,
            "body": "static"
        }))
        .unwrap();
        let mut req = test_request();
        req.method = "POST".to_string();
        req.body = Some("request body".to_string());
        let resp = plugin.dispatch(req);
        assert_eq!(resp.body.as_deref(), Some("static"));
    }

    #[test]
    fn test_empty_body_returns_none() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": ""
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert!(resp.body.is_none());
    }
}
