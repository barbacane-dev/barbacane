//! HTTP upstream reverse proxy dispatcher plugin for Barbacane API gateway.
//!
//! Proxies requests to HTTP/HTTPS backends with support for:
//! - Path parameter substitution
//! - Path rewriting
//! - Header forwarding
//! - Configurable timeouts

use barbacane_plugin_sdk::http;
use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// HTTP upstream dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct HttpUpstreamDispatcher {
    /// Base URL of the upstream (e.g., https://api.example.com).
    url: String,

    /// Path template for the upstream request.
    /// Supports `{param}` substitution from path parameters.
    /// If not specified, uses the original request path.
    #[serde(default)]
    path: Option<String>,

    /// Request timeout in seconds (default: 30).
    #[serde(default = "default_timeout")]
    timeout: f64,
}

fn default_timeout() -> f64 {
    30.0
}

impl HttpUpstreamDispatcher {
    /// Proxy the request to the upstream and return the response.
    pub fn dispatch(&mut self, req: Request) -> Response {
        let Request {
            method,
            path,
            query,
            headers: req_headers,
            body,
            client_ip: _,
            path_params,
        } = req;

        // Build the upstream path
        let upstream_path = match &self.path {
            Some(template) => self.substitute_path_params(template, &path_params),
            None => path,
        };

        // Construct the full URL with query string
        let base_url = if self.url.ends_with('/') || upstream_path.starts_with('/') {
            format!("{}{}", self.url.trim_end_matches('/'), upstream_path)
        } else {
            format!("{}{}", self.url, upstream_path)
        };

        let full_url = match &query {
            Some(qs) if !qs.is_empty() => format!("{}?{}", base_url, qs),
            _ => base_url,
        };

        // Build headers to send to upstream
        let mut headers: BTreeMap<String, String> = BTreeMap::new();

        // Forward incoming headers (filter hop-by-hop headers)
        for (key, value) in &req_headers {
            let key_lower = key.to_lowercase();
            if !matches!(
                key_lower.as_str(),
                "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade"
            ) {
                headers.insert(key.clone(), value.clone());
            }
        }

        // Add X-Forwarded headers
        if let Some(host) = req_headers.get("host") {
            headers.insert("x-forwarded-host".to_string(), host.clone());
        }
        let proto = if self.url.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        headers.insert("x-forwarded-proto".to_string(), proto.to_string());

        let timeout_ms = (self.timeout * 1000.0) as u64;

        // Body travels via the side-channel; `http::call` sends it and reads the
        // response body back the same way.
        let http_request = http::HttpRequest {
            method,
            url: full_url,
            headers,
            timeout_ms: Some(timeout_ms),
        };

        let http_response = match http::call(&http_request, body.as_deref()) {
            Ok(resp) => resp,
            Err(http::HttpError::Unreachable) | Err(http::HttpError::Unsupported) => {
                return self.error_response(
                    502,
                    "Bad Gateway",
                    "upstream connection failed",
                    "host_http_call returned error",
                );
            }
            Err(http::HttpError::Empty) | Err(http::HttpError::ReadFailed) => {
                return self.error_response(
                    502,
                    "Bad Gateway",
                    "upstream connection failed",
                    "failed to read response",
                );
            }
            Err(http::HttpError::InvalidResponse) => {
                return self.error_response(
                    502,
                    "Bad Gateway",
                    "invalid upstream response",
                    "failed to parse upstream response",
                );
            }
        };

        // Build the response, filtering hop-by-hop headers
        let mut response_headers: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in http_response.headers {
            let key_lower = key.to_lowercase();
            if !matches!(
                key_lower.as_str(),
                "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade"
            ) {
                response_headers.insert(key, value);
            }
        }

        Response {
            status: http_response.status,
            headers: response_headers,
            body: http_response.body,
        }
    }

    /// Substitute path parameters in the template.
    fn substitute_path_params(&self, template: &str, params: &BTreeMap<String, String>) -> String {
        let mut result = template.to_string();
        for (key, value) in params {
            result = result.replace(&format!("{{{}}}", key), value);
        }
        result
    }

    /// Create an error response in RFC 9457 format.
    fn error_response(&self, status: u16, title: &str, detail: &str, debug: &str) -> Response {
        let error_type = match status {
            502 => "urn:barbacane:error:upstream-unavailable",
            503 => "urn:barbacane:error:circuit-open",
            504 => "urn:barbacane:error:upstream-timeout",
            _ => "urn:barbacane:error:internal",
        };

        let full_detail = format!("{}: {}", detail, debug);

        ProblemDetails::new(status, error_type, title)
            .detail(full_detail)
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(
        method: &str,
        path: &str,
        headers: BTreeMap<String, String>,
        body: Option<Vec<u8>>,
        query: Option<String>,
        path_params: BTreeMap<String, String>,
    ) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            headers,
            body,
            query,
            path_params,
            client_ip: "127.0.0.1".to_string(),
        }
    }

    #[test]
    fn test_substitute_path_params_single() {
        let dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let mut params = BTreeMap::new();
        params.insert("id".to_string(), "123".to_string());

        let result = dispatcher.substitute_path_params("/users/{id}", &params);
        assert_eq!(result, "/users/123");
    }

    #[test]
    fn test_substitute_path_params_multiple() {
        let dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let mut params = BTreeMap::new();
        params.insert("org".to_string(), "acme".to_string());
        params.insert("repo".to_string(), "myapp".to_string());
        params.insert("id".to_string(), "456".to_string());

        let result =
            dispatcher.substitute_path_params("/orgs/{org}/repos/{repo}/issues/{id}", &params);
        assert_eq!(result, "/orgs/acme/repos/myapp/issues/456");
    }

    #[test]
    fn test_substitute_path_params_unmatched() {
        let dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let mut params = BTreeMap::new();
        params.insert("id".to_string(), "123".to_string());

        let result = dispatcher.substitute_path_params("/users/{id}/posts/{post_id}", &params);
        assert_eq!(result, "/users/123/posts/{post_id}");
    }

    #[test]
    fn test_substitute_path_params_no_params() {
        let dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let params = BTreeMap::new();

        let result = dispatcher.substitute_path_params("/users", &params);
        assert_eq!(result, "/users");
    }

    #[test]
    fn test_error_response_502() {
        let dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let response =
            dispatcher.error_response(502, "Bad Gateway", "connection failed", "tcp error");
        assert_eq!(response.status, 502);
        assert_eq!(
            response.headers.get("content-type").unwrap(),
            "application/problem+json"
        );

        let body: serde_json::Value =
            serde_json::from_slice(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:upstream-unavailable");
        assert_eq!(body["title"], "Bad Gateway");
        assert_eq!(body["status"], 502);
        assert_eq!(body["detail"], "connection failed: tcp error");
    }

    #[test]
    fn test_error_response_503() {
        let dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let response = dispatcher.error_response(
            503,
            "Service Unavailable",
            "circuit open",
            "too many failures",
        );
        assert_eq!(response.status, 503);

        let body: serde_json::Value =
            serde_json::from_slice(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:circuit-open");
    }

    #[test]
    fn test_error_response_504() {
        let dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let response =
            dispatcher.error_response(504, "Gateway Timeout", "request timeout", "30s exceeded");
        assert_eq!(response.status, 504);

        let body: serde_json::Value =
            serde_json::from_slice(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:upstream-timeout");
    }

    #[test]
    fn test_error_response_other_status() {
        let dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let response = dispatcher.error_response(
            500,
            "Internal Error",
            "unknown error",
            "something went wrong",
        );
        assert_eq!(response.status, 500);

        let body: serde_json::Value =
            serde_json::from_slice(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:internal");
    }

    #[test]
    fn test_config_deserialization_required_url() {
        let json = r#"{"url": "http://api.example.com"}"#;
        let config: HttpUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.url, "http://api.example.com");
        assert_eq!(config.path, None);
        assert_eq!(config.timeout, 30.0);
    }

    #[test]
    fn test_config_deserialization_with_path() {
        let json = r#"{"url": "http://api.example.com", "path": "/v1/users/{id}"}"#;
        let config: HttpUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.url, "http://api.example.com");
        assert_eq!(config.path, Some("/v1/users/{id}".to_string()));
    }

    #[test]
    fn test_config_deserialization_with_timeout() {
        let json = r#"{"url": "http://api.example.com", "timeout": 60.0}"#;
        let config: HttpUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.timeout, 60.0);
    }

    #[test]
    fn test_config_deserialization_missing_url() {
        let json = r#"{"timeout": 60.0}"#;
        let result: Result<HttpUpstreamDispatcher, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_dispatch_returns_502_on_native() {
        let mut dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let req = make_request("GET", "/test", BTreeMap::new(), None, None, BTreeMap::new());
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }

    #[test]
    fn test_dispatch_with_body_returns_502_on_native() {
        let mut dispatcher = HttpUpstreamDispatcher {
            url: "http://example.com".to_string(),
            path: None,
            timeout: 30.0,
        };

        let body = br#"{"name":"test"}"#.to_vec();
        let req = make_request(
            "POST",
            "/users",
            BTreeMap::new(),
            Some(body),
            None,
            BTreeMap::new(),
        );

        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }
}
