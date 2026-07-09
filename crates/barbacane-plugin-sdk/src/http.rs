//! Outbound HTTP via the `host_http_call` import.
//!
//! Plugins each redeclared the `host_http_call` / `host_http_read_result`
//! externs and the request/response types + call orchestration. This centralizes
//! them. The request/response body travels via the side-channel body functions
//! in [`crate::body`], not in the JSON.
//!
//! On non-wasm targets (unit tests) [`call`] returns [`HttpError::Unsupported`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// Only used by the wasm `call` implementation below.
#[cfg(target_arch = "wasm32")]
use crate::body;

/// An outbound HTTP request. The body is passed separately to [`call`] and sent
/// via the side-channel, so it is not part of this JSON.
#[derive(Debug, Clone, Serialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl HttpRequest {
    /// Build a request with method + url and no headers/timeout.
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: BTreeMap::new(),
            timeout_ms: None,
        }
    }

    /// Add a header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// Set the request timeout in milliseconds.
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

/// An HTTP response from the host. The body is read from the side-channel and
/// attached by [`call`].
#[derive(Debug, Clone, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(skip)]
    pub body: Option<Vec<u8>>,
}

impl HttpResponse {
    /// The response body as a UTF-8 string, if present and valid.
    pub fn body_str(&self) -> Option<&str> {
        self.body
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
    }
}

/// Failure modes of [`call`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// The host could not reach the target (`host_http_call` < 0).
    Unreachable,
    /// The host returned an empty result.
    Empty,
    /// Reading the result buffer returned an unexpected length.
    ReadFailed,
    /// The result was not a valid `HttpResponse` JSON.
    InvalidResponse,
    /// Called on a non-wasm target (no host available).
    Unsupported,
}

/// Perform an outbound HTTP request through the host. `body` (if any) is sent via
/// the side-channel; the response body is read back the same way.
#[cfg(target_arch = "wasm32")]
pub fn call(request: &HttpRequest, body_bytes: Option<&[u8]>) -> Result<HttpResponse, HttpError> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_http_call(req_ptr: i32, req_len: i32) -> i32;
        fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }

    body::set_http_request_body(body_bytes.unwrap_or(&[]));

    let serialized = serde_json::to_vec(request).map_err(|_| HttpError::InvalidResponse)?;
    let result_len = unsafe { host_http_call(serialized.as_ptr() as i32, serialized.len() as i32) };
    if result_len < 0 {
        return Err(HttpError::Unreachable);
    }
    if result_len == 0 {
        return Err(HttpError::Empty);
    }

    let mut buf = vec![0u8; result_len as usize];
    let bytes_read = unsafe { host_http_read_result(buf.as_mut_ptr() as i32, result_len) };
    if bytes_read != result_len {
        return Err(HttpError::ReadFailed);
    }

    let mut resp: HttpResponse =
        serde_json::from_slice(&buf).map_err(|_| HttpError::InvalidResponse)?;
    resp.body = body::read_http_response_body();
    Ok(resp)
}

/// Non-wasm stub: no host to call.
#[cfg(not(target_arch = "wasm32"))]
pub fn call(_request: &HttpRequest, _body_bytes: Option<&[u8]>) -> Result<HttpResponse, HttpError> {
    Err(HttpError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_without_body_field() {
        let req = HttpRequest::new("POST", "https://upstream/x")
            .header("content-type", "application/json")
            .timeout_ms(5000);
        let v: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["method"], "POST");
        assert_eq!(v["url"], "https://upstream/x");
        assert_eq!(v["headers"]["content-type"], "application/json");
        assert_eq!(v["timeout_ms"], 5000);
        assert!(v.get("body").is_none());
    }

    #[test]
    fn timeout_omitted_when_unset() {
        let v: serde_json::Value =
            serde_json::to_value(HttpRequest::new("GET", "https://x")).unwrap();
        assert!(v.get("timeout_ms").is_none());
    }

    #[test]
    fn call_is_unsupported_on_native() {
        assert!(matches!(
            call(&HttpRequest::new("GET", "https://x"), None),
            Err(HttpError::Unsupported)
        ));
    }
}
