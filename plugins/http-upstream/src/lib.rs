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
    /// Base URL of the upstream (e.g., https://api.example.com). Exclusive
    /// with `endpoints`.
    #[serde(default)]
    url: Option<String>,

    /// Instances of the upstream to balance requests across (ADR-0035).
    #[serde(default)]
    endpoints: Option<Vec<http::Endpoint>>,

    /// With `endpoints`: how many other endpoints a failed request may be
    /// sent to (default 1).
    #[serde(default)]
    retries: Option<u32>,

    /// Path template for the upstream request.
    /// Supports `{param}` substitution from path parameters.
    /// If not specified, uses the original request path.
    #[serde(default)]
    path: Option<String>,

    /// Request timeout in seconds (default: 30).
    #[serde(default = "default_timeout")]
    timeout: f64,

    /// TLS settings: a client certificate for mutual TLS, a CA for the server.
    #[serde(default)]
    tls: Option<http::TlsSettings>,
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

        let path_and_query = match &query {
            Some(qs) if !qs.is_empty() => format!("{}?{}", upstream_path, qs),
            _ => upstream_path,
        };

        // A single upstream gets the full URL; a pool gets the path and query,
        // and the host puts the endpoint it selects in front of it.
        let (url, scheme_source) = match (&self.url, &self.endpoints) {
            (Some(base), _) => {
                let full = if base.ends_with('/') || path_and_query.starts_with('/') {
                    format!("{}{}", base.trim_end_matches('/'), path_and_query)
                } else {
                    format!("{}{}", base, path_and_query)
                };
                (full, base.as_str())
            }
            (None, Some(endpoints)) if !endpoints.is_empty() => {
                (path_and_query, endpoints[0].url.as_str())
            }
            _ => {
                return self.error_response(
                    500,
                    "Internal Server Error",
                    "no upstream configured",
                    "neither url nor endpoints is set",
                );
            }
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
        let proto = if scheme_source.starts_with("https://") {
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
            url,
            headers,
            timeout_ms: Some(timeout_ms),
            tls: self.tls.clone(),
            endpoints: self.endpoints.clone().unwrap_or_default(),
            retries: self.retries,
        };

        let result = if http_request.endpoints.is_empty() {
            http::call(&http_request, body.as_deref())
        } else {
            http::pool_call(&http_request, body.as_deref())
        };
        let http_response = match result {
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
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
        };

        let mut params = BTreeMap::new();
        params.insert("id".to_string(), "123".to_string());

        let result = dispatcher.substitute_path_params("/users/{id}", &params);
        assert_eq!(result, "/users/123");
    }

    #[test]
    fn test_substitute_path_params_multiple() {
        let dispatcher = HttpUpstreamDispatcher {
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
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
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
        };

        let mut params = BTreeMap::new();
        params.insert("id".to_string(), "123".to_string());

        let result = dispatcher.substitute_path_params("/users/{id}/posts/{post_id}", &params);
        assert_eq!(result, "/users/123/posts/{post_id}");
    }

    #[test]
    fn test_substitute_path_params_no_params() {
        let dispatcher = HttpUpstreamDispatcher {
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
        };

        let params = BTreeMap::new();

        let result = dispatcher.substitute_path_params("/users", &params);
        assert_eq!(result, "/users");
    }

    #[test]
    fn test_error_response_502() {
        let dispatcher = HttpUpstreamDispatcher {
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
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
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
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
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
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
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
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
        assert_eq!(config.url.as_deref(), Some("http://api.example.com"));
        assert_eq!(config.path, None);
        assert_eq!(config.timeout, 30.0);
    }

    #[test]
    fn endpoints_and_retries_deserialize_and_travel_with_the_request() {
        let json = r#"{"endpoints": [
            {"url": "https://a.internal:8443"},
            {"url": "https://b.internal:8443", "weight": 2}
        ], "retries": 1}"#;
        let config: HttpUpstreamDispatcher = serde_json::from_str(json).unwrap();
        let endpoints = config.endpoints.clone().expect("endpoints");
        assert_eq!(endpoints[0].weight, 1, "weight defaults to 1");
        assert_eq!(endpoints[1].weight, 2);

        let mut request = http::HttpRequest::new("GET", "/orders/1?x=y");
        request.endpoints = endpoints;
        request.retries = config.retries;
        let sent: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(sent["url"], "/orders/1?x=y");
        assert_eq!(sent["endpoints"][1]["url"], "https://b.internal:8443");
        assert_eq!(sent["retries"], 1);
    }

    #[test]
    fn a_single_url_request_carries_no_endpoints() {
        let sent =
            serde_json::to_value(http::HttpRequest::new("GET", "https://a.example")).unwrap();
        assert!(
            sent.get("endpoints").is_none() && sent.get("retries").is_none(),
            "{sent}"
        );
    }

    #[test]
    fn tls_settings_deserialize_and_travel_with_the_request() {
        let json = r#"{"url": "https://api.example.com", "tls": {
            "client_cert": "certs/client.pem", "client_key": "certs/client.key", "ca": "certs/ca.pem"
        }}"#;
        let config: HttpUpstreamDispatcher = serde_json::from_str(json).unwrap();
        let tls = config.tls.clone().expect("tls");
        assert_eq!(tls.client_cert.as_deref(), Some("certs/client.pem"));
        assert_eq!(tls.ca.as_deref(), Some("certs/ca.pem"));

        let request = http::HttpRequest::new("GET", "https://api.example.com/x").tls(tls);
        let sent: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(sent["tls"]["client_key"], "certs/client.key");
    }

    #[test]
    fn a_request_without_tls_sends_no_tls_field() {
        let sent =
            serde_json::to_value(http::HttpRequest::new("GET", "https://a.example")).unwrap();
        assert!(sent.get("tls").is_none(), "{sent}");
    }

    #[test]
    fn an_unknown_tls_key_is_refused() {
        let json = r#"{"url": "https://a.example", "tls": {"client_crt": "x"}}"#;
        assert!(serde_json::from_str::<HttpUpstreamDispatcher>(json).is_err());
    }

    #[test]
    fn test_config_deserialization_with_path() {
        let json = r#"{"url": "http://api.example.com", "path": "/v1/users/{id}"}"#;
        let config: HttpUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.url.as_deref(), Some("http://api.example.com"));
        assert_eq!(config.path, Some("/v1/users/{id}".to_string()));
    }

    #[test]
    fn test_config_deserialization_with_timeout() {
        let json = r#"{"url": "http://api.example.com", "timeout": 60.0}"#;
        let config: HttpUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.timeout, 60.0);
    }

    /// The config schema requires one of `url` and `endpoints` (E1023); a
    /// config that reaches the plugin with neither is answered 500.
    #[test]
    fn a_config_without_url_or_endpoints_answers_500() {
        let mut dispatcher: HttpUpstreamDispatcher =
            serde_json::from_str(r#"{"timeout": 60.0}"#).unwrap();
        let response = dispatcher.dispatch(Request {
            method: "GET".to_string(),
            path: "/x".to_string(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        });
        assert_eq!(response.status, 500);
    }

    #[test]
    fn test_dispatch_returns_502_on_native() {
        let mut dispatcher = HttpUpstreamDispatcher {
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
        };

        let req = make_request("GET", "/test", BTreeMap::new(), None, None, BTreeMap::new());
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }

    #[test]
    fn test_dispatch_with_body_returns_502_on_native() {
        let mut dispatcher = HttpUpstreamDispatcher {
            url: Some("http://example.com".to_string()),
            endpoints: None,
            retries: None,
            path: None,
            timeout: 30.0,
            tls: None,
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
