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
    /// Handle a request and return the configured response.
    ///
    /// Supports `{{placeholder}}` interpolation in the body:
    /// - `{{request.method}}`, `{{request.path}}`, `{{request.query}}`, `{{request.client_ip}}`
    /// - `{{headers.<name>}}` — request header value
    /// - `{{path_params.<name>}}` — path parameter value
    ///
    /// Unresolved placeholders are left as-is.
    pub fn dispatch(&mut self, req: Request) -> Response {
        let mut headers = self.headers.clone();
        headers.insert("content-type".to_string(), self.content_type.clone());

        let body = if self.body.is_empty() {
            None
        } else {
            Some(interpolate(&self.body, &req).into_bytes())
        };

        Response {
            status: self.status,
            headers,
            body,
        }
    }
}

/// Replace `{{...}}` placeholders in `template` with values from `req`.
fn interpolate(template: &str, req: &Request) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        if let Some(end) = after_open.find("}}") {
            let key = after_open[..end].trim();
            if let Some(value) = resolve_placeholder(key, req) {
                result.push_str(&value);
            } else {
                // Keep unresolved placeholder as-is
                result.push_str(&rest[start..start + 2 + end + 2]);
            }
            rest = &after_open[end + 2..];
        } else {
            // No closing braces — keep the rest as-is
            result.push_str(&rest[start..]);
            rest = "";
        }
    }
    result.push_str(rest);
    result
}

/// Resolve a single placeholder key against the request.
fn resolve_placeholder(key: &str, req: &Request) -> Option<String> {
    if let Some(name) = key.strip_prefix("headers.") {
        // Try exact match, then lowercase
        req.headers
            .get(name)
            .or_else(|| req.headers.get(&name.to_lowercase()))
            .cloned()
    } else if let Some(name) = key.strip_prefix("path_params.") {
        req.path_params.get(name).cloned()
    } else {
        match key {
            "request.method" => Some(req.method.clone()),
            "request.path" => Some(req.path.clone()),
            "request.query" => req.query.clone(),
            "request.client_ip" => Some(req.client_ip.clone()),
            _ => None,
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
        assert_eq!(resp.body_str(), Some("{\"message\":\"hello\"}"));
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
    fn test_dispatch_static_body_unchanged() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "status": 200,
            "body": "static"
        }))
        .unwrap();
        let mut req = test_request();
        req.method = "POST".to_string();
        req.body = Some(b"request body".to_vec());
        let resp = plugin.dispatch(req);
        assert_eq!(resp.body_str(), Some("static"));
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

    // --- interpolation ---

    #[test]
    fn test_interpolate_request_method() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "method={{request.method}}"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.body_str(), Some("method=GET"));
    }

    #[test]
    fn test_interpolate_request_path() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "path={{request.path}}"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.body_str(), Some("path=/test"));
    }

    #[test]
    fn test_interpolate_request_client_ip() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "ip={{request.client_ip}}"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.body_str(), Some("ip=127.0.0.1"));
    }

    #[test]
    fn test_interpolate_request_query() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "q={{request.query}}"
        }))
        .unwrap();
        let mut req = test_request();
        req.query = Some("foo=bar".to_string());
        let resp = plugin.dispatch(req);
        assert_eq!(resp.body_str(), Some("q=foo=bar"));
    }

    #[test]
    fn test_interpolate_request_query_missing() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "q={{request.query}}"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.body_str(), Some("q={{request.query}}"));
    }

    #[test]
    fn test_interpolate_headers() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "consumer={{headers.x-auth-consumer}}"
        }))
        .unwrap();
        let mut req = test_request();
        req.headers
            .insert("x-auth-consumer".to_string(), "alice".to_string());
        let resp = plugin.dispatch(req);
        assert_eq!(resp.body_str(), Some("consumer=alice"));
    }

    #[test]
    fn test_interpolate_headers_case_insensitive() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "key={{headers.X-Auth-Key-Name}}"
        }))
        .unwrap();
        let mut req = test_request();
        req.headers
            .insert("x-auth-key-name".to_string(), "prod-key".to_string());
        let resp = plugin.dispatch(req);
        assert_eq!(resp.body_str(), Some("key=prod-key"));
    }

    #[test]
    fn test_interpolate_path_params() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "{\"id\": \"{{path_params.userId}}\"}"
        }))
        .unwrap();
        let mut req = test_request();
        req.path_params
            .insert("userId".to_string(), "42".to_string());
        let resp = plugin.dispatch(req);
        assert_eq!(resp.body_str(), Some("{\"id\": \"42\"}"));
    }

    #[test]
    fn test_interpolate_multiple_placeholders() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "{{request.method}} {{request.path}} by {{headers.x-auth-consumer}}"
        }))
        .unwrap();
        let mut req = test_request();
        req.headers
            .insert("x-auth-consumer".to_string(), "bob".to_string());
        let resp = plugin.dispatch(req);
        assert_eq!(resp.body_str(), Some("GET /test by bob"));
    }

    #[test]
    fn test_interpolate_unresolved_kept_as_is() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "val={{unknown.key}}"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.body_str(), Some("val={{unknown.key}}"));
    }

    #[test]
    fn test_interpolate_unclosed_braces() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "val={{request.method"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.body_str(), Some("val={{request.method"));
    }

    #[test]
    fn test_interpolate_whitespace_in_placeholder() {
        let mut plugin: MockDispatcher = serde_json::from_value(serde_json::json!({
            "body": "m={{ request.method }}"
        }))
        .unwrap();
        let resp = plugin.dispatch(test_request());
        assert_eq!(resp.body_str(), Some("m=GET"));
    }
}
