//! Streaming-echo fixture plugin for integration tests.
//!
//! A minimal dispatcher that proxies to an upstream URL using `host_http_stream`,
//! allowing integration tests to exercise the ADR-0023 streaming code path.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Configuration: just the upstream URL to stream from.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct StreamingEcho {
    /// Full URL of the upstream endpoint to stream from.
    url: String,
}

/// HTTP request passed to `host_http_stream` (same layout as `host_http_call`).
#[derive(Serialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
    timeout_ms: Option<u64>,
}

impl StreamingEcho {
    pub fn dispatch(&mut self, req: Request) -> Response {
        let http_request = HttpRequest {
            method: req.method.clone(),
            url: self.url.clone(),
            headers: req.headers.clone(),
            body: req.body_string(),
            timeout_ms: Some(30_000),
        };

        let request_json = match serde_json::to_vec(&http_request) {
            Ok(j) => j,
            Err(e) => {
                return error_response(500, &format!("failed to serialize request: {e}"));
            }
        };

        // Call host_http_stream — streams the response to the client in real time
        // while buffering a copy for the on_response middleware chain.
        let result =
            unsafe { host_http_stream(request_json.as_ptr() as i32, request_json.len() as i32) };

        if result < 0 {
            return error_response(502, "upstream stream failed");
        }

        // Return the streaming sentinel — the host recognises status=0 and
        // skips building a new HTTP response (client already received chunks).
        streamed_response()
    }
}

fn error_response(status: u16, detail: &str) -> Response {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/problem+json".to_string());
    let body = serde_json::json!({
        "type": "urn:barbacane:error:upstream-unavailable",
        "title": "Bad Gateway",
        "status": status,
        "detail": detail,
    });
    Response {
        status,
        headers,
        body: Some(body.to_string().into_bytes()),
    }
}

// Host function declarations (WASM target)
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Stream an HTTP response to the client. Returns byte count or -1 on error.
    fn host_http_stream(req_ptr: i32, req_len: i32) -> i32;
}

// Native stubs for unit tests (non-WASM)
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_stream(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_deserializes() {
        let json = r#"{"url":"https://example.com/stream"}"#;
        let cfg: StreamingEcho = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.url, "https://example.com/stream");
    }

    #[test]
    fn dispatch_returns_502_on_native_stub() {
        // On native the host stub returns -1, so dispatch returns a 502 error response.
        let mut plugin = StreamingEcho {
            url: "http://example.com/stream".into(),
        };
        let req = Request {
            method: "GET".into(),
            path: "/stream".into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let resp = plugin.dispatch(req);
        assert_eq!(resp.status, 502);
    }
}
