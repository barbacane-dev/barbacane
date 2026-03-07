use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An HTTP request as seen by plugins.
///
/// Uses BTreeMap instead of HashMap to avoid WASI random dependency in WASM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
    pub client_ip: String,
    pub path_params: BTreeMap<String, String>,
}

/// An HTTP response as produced by plugins.
///
/// Uses BTreeMap instead of HashMap to avoid WASI random dependency in WASM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}

/// The action a middleware returns from `on_request`.
#[derive(Debug, Clone)]
pub enum Action<T> {
    /// Pass the (possibly modified) request to the next middleware.
    Continue(T),
    /// Stop the chain and return this response immediately.
    ShortCircuit(Response),
}

/// Marker response indicating the body was already streamed via `host_http_stream`.
///
/// Dispatcher plugins that use `host_http_stream` return this sentinel instead
/// of a normal `Response`. The host recognises `status == 0` and skips
/// building a new HTTP response (the client received chunks in real time).
///
/// The on_response middleware chain still runs with the buffered copy for
/// observability (logging, metrics), but any header/body modifications are
/// silently discarded since the response was already sent.
pub fn streamed_response() -> Response {
    Response {
        status: 0,
        headers: BTreeMap::new(),
        body: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_response_has_status_zero() {
        assert_eq!(streamed_response().status, 0);
    }

    #[test]
    fn streamed_response_has_empty_headers_and_no_body() {
        let r = streamed_response();
        assert!(r.headers.is_empty());
        assert!(r.body.is_none());
    }

    #[test]
    fn streamed_response_is_distinguishable_from_normal_response() {
        let normal = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some("ok".into()),
        };
        assert_ne!(streamed_response().status, normal.status);
    }
}
