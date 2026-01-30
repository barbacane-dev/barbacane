//! AWS Lambda dispatcher plugin for Barbacane API gateway.
//!
//! Invokes AWS Lambda functions via Lambda Function URLs.
//! Supports passing request headers and body to Lambda.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Lambda dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct LambdaDispatcher {
    /// Lambda Function URL (e.g., https://<id>.lambda-url.<region>.on.aws/).
    url: String,

    /// Request timeout in seconds (default: 30).
    #[serde(default = "default_timeout")]
    timeout: f64,

    /// Pass incoming request headers to Lambda (default: true).
    #[serde(default = "default_pass_through_headers")]
    pass_through_headers: bool,
}

fn default_timeout() -> f64 {
    30.0
}

fn default_pass_through_headers() -> bool {
    true
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

impl LambdaDispatcher {
    /// Invoke the Lambda function and return the response.
    pub fn dispatch(&mut self, req: Request) -> Response {
        // Build headers to send to Lambda
        let mut headers: BTreeMap<String, String> = BTreeMap::new();

        if self.pass_through_headers {
            // Pass through incoming headers (filter hop-by-hop headers)
            for (key, value) in &req.headers {
                let key_lower = key.to_lowercase();
                if !matches!(
                    key_lower.as_str(),
                    "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade" | "host"
                ) {
                    headers.insert(key.clone(), value.clone());
                }
            }
        }

        // Always set content-type if we have a body
        if req.body.is_some() && !headers.contains_key("content-type") {
            headers.insert("content-type".to_string(), "application/json".to_string());
        }

        // Build the HTTP request to Lambda
        let http_request = HttpRequest {
            method: req.method.clone(),
            url: self.url.clone(),
            headers,
            body: req.body.clone(),
            timeout_ms: Some((self.timeout * 1000.0) as u64),
        };

        // Serialize request
        let request_json = match serde_json::to_vec(&http_request) {
            Ok(json) => json,
            Err(e) => {
                return self.error_response(500, "failed to serialize request", &e.to_string());
            }
        };

        // Call Lambda via host_http_call
        let result_len = unsafe { host_http_call(request_json.as_ptr() as i32, request_json.len() as i32) };

        if result_len < 0 {
            return self.error_response(502, "Lambda invocation failed", "host_http_call returned error");
        }

        // Read the response
        let mut response_buf = vec![0u8; result_len as usize];
        let bytes_read = unsafe {
            host_http_read_result(response_buf.as_mut_ptr() as i32, result_len)
        };

        if bytes_read <= 0 {
            return self.error_response(502, "Lambda invocation failed", "failed to read response");
        }

        // Parse the HTTP response
        let http_response: HttpResponse = match serde_json::from_slice(&response_buf[..bytes_read as usize]) {
            Ok(resp) => resp,
            Err(e) => {
                return self.error_response(502, "invalid Lambda response", &e.to_string());
            }
        };

        // Build the response
        let mut response_headers = http_response.headers;

        // Ensure content-type is set
        if !response_headers.contains_key("content-type") {
            response_headers.insert("content-type".to_string(), "application/json".to_string());
        }

        Response {
            status: http_response.status,
            headers: response_headers,
            body: http_response.body.and_then(|b| String::from_utf8(b).ok()),
        }
    }

    /// Create an error response in RFC 9457 format.
    fn error_response(&self, status: u16, title: &str, detail: &str) -> Response {
        let error_type = match status {
            502 => "urn:barbacane:error:lambda-invocation-failed",
            503 => "urn:barbacane:error:lambda-unavailable",
            504 => "urn:barbacane:error:lambda-timeout",
            _ => "urn:barbacane:error:internal",
        };

        let body = serde_json::json!({
            "type": error_type,
            "title": title,
            "status": status,
            "detail": detail
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
