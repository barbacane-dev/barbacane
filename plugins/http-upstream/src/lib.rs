//! HTTP upstream reverse proxy dispatcher plugin for Barbacane API gateway.
//!
//! Proxies requests to HTTP/HTTPS backends with support for:
//! - Path parameter substitution
//! - Path rewriting
//! - Header forwarding
//! - Configurable timeouts

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
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

/// HTTP request for host_http_call.
#[derive(Serialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
    timeout_ms: Option<u64>,
}

/// HTTP response from host_http_call.
#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Option<Vec<u8>>,
}

impl HttpUpstreamDispatcher {
    /// Proxy the request to the upstream and return the response.
    // TODO: Forward query string from original request
    // TODO: Handle binary response bodies (currently only UTF-8 is supported)
    pub fn dispatch(&mut self, req: Request) -> Response {
        // Build the upstream path
        let upstream_path = match &self.path {
            Some(template) => self.substitute_path_params(template, &req.path_params),
            None => req.path.clone(),
        };

        // Construct the full URL
        let full_url = if self.url.ends_with('/') || upstream_path.starts_with('/') {
            format!("{}{}", self.url.trim_end_matches('/'), upstream_path)
        } else {
            format!("{}{}", self.url, upstream_path)
        };

        // Build headers to send to upstream
        let mut headers: BTreeMap<String, String> = BTreeMap::new();

        // Forward incoming headers (filter hop-by-hop headers)
        for (key, value) in &req.headers {
            let key_lower = key.to_lowercase();
            if !matches!(
                key_lower.as_str(),
                "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade"
            ) {
                headers.insert(key.clone(), value.clone());
            }
        }

        // Add X-Forwarded headers
        if let Some(host) = req.headers.get("host") {
            headers.insert("x-forwarded-host".to_string(), host.clone());
        }
        // TODO: Detect actual protocol instead of hardcoding "http"
        headers.insert("x-forwarded-proto".to_string(), "http".to_string());

        // Build the HTTP request
        let http_request = HttpRequest {
            method: req.method.clone(),
            url: full_url.clone(),
            headers,
            body: req.body.clone(),
            timeout_ms: Some((self.timeout * 1000.0) as u64),
        };

        // Serialize request
        let request_json = match serde_json::to_vec(&http_request) {
            Ok(json) => json,
            Err(e) => {
                return self.error_response(500, "Bad Gateway", "failed to serialize request", &e.to_string());
            }
        };

        // Call upstream via host_http_call
        let result_len = unsafe { host_http_call(request_json.as_ptr() as i32, request_json.len() as i32) };

        if result_len < 0 {
            return self.error_response(502, "Bad Gateway", "upstream connection failed", "host_http_call returned error");
        }

        // Read the response
        let mut response_buf = vec![0u8; result_len as usize];
        let bytes_read = unsafe {
            host_http_read_result(response_buf.as_mut_ptr() as i32, result_len)
        };

        if bytes_read <= 0 {
            return self.error_response(502, "Bad Gateway", "upstream connection failed", "failed to read response");
        }

        // Parse the HTTP response
        let http_response: HttpResponse = match serde_json::from_slice(&response_buf[..bytes_read as usize]) {
            Ok(resp) => resp,
            Err(e) => {
                return self.error_response(502, "Bad Gateway", "invalid upstream response", &e.to_string());
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
            body: http_response.body.and_then(|b| String::from_utf8(b).ok()),
        }
    }

    /// Substitute path parameters in the template.
    /// Replaces `{param}` with the actual value from path_params.
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

        // Include debug info in detail for development
        let full_detail = format!("{}: {}", detail, debug);

        let body = serde_json::json!({
            "type": error_type,
            "title": title,
            "status": status,
            "detail": full_detail
        });

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/problem+json".to_string());

        Response {
            status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

// Host function declarations
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Make an HTTP request. Returns the response length, or -1 on error.
    fn host_http_call(req_ptr: i32, req_len: i32) -> i32;

    /// Read the HTTP response into the provided buffer. Returns bytes read.
    fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}
