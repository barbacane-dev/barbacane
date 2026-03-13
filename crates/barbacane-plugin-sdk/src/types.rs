use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Serde helper that encodes `Option<Vec<u8>>` as a base64 string in JSON.
///
/// This allows binary bodies (e.g. multipart/form-data with file uploads) to
/// pass through JSON serialization between the host and WASM plugins without
/// data loss.
pub mod base64_body {
    use base64::Engine;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(data: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match data {
            Some(bytes) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                serializer.serialize_some(&encoded)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            Some(s) => base64::engine::general_purpose::STANDARD
                .decode(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

/// An HTTP request as seen by plugins.
///
/// Uses BTreeMap instead of HashMap to avoid WASI random dependency in WASM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: BTreeMap<String, String>,
    #[serde(with = "base64_body")]
    pub body: Option<Vec<u8>>,
    pub client_ip: String,
    pub path_params: BTreeMap<String, String>,
}

impl Request {
    /// Return the body as a UTF-8 string slice, or `None` if absent or not
    /// valid UTF-8.
    pub fn body_str(&self) -> Option<&str> {
        self.body
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
    }

    /// Return the body as an owned `String`, or `None` if absent or not
    /// valid UTF-8.
    pub fn body_string(&self) -> Option<String> {
        self.body.clone().and_then(|b| String::from_utf8(b).ok())
    }

    /// Set the body from a UTF-8 string.
    pub fn set_body_text(&mut self, text: &str) {
        self.body = Some(text.as_bytes().to_vec());
    }
}

/// An HTTP response as produced by plugins.
///
/// Uses BTreeMap instead of HashMap to avoid WASI random dependency in WASM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    #[serde(with = "base64_body")]
    pub body: Option<Vec<u8>>,
}

impl Response {
    /// Return the body as a UTF-8 string slice, or `None` if absent or not
    /// valid UTF-8.
    pub fn body_str(&self) -> Option<&str> {
        self.body
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
    }

    /// Set the body from a UTF-8 string.
    pub fn set_body_text(&mut self, text: &str) {
        self.body = Some(text.as_bytes().to_vec());
    }

    /// Create a response with a text body.
    pub fn text(status: u16, headers: BTreeMap<String, String>, text: &str) -> Self {
        Self {
            status,
            headers,
            body: Some(text.as_bytes().to_vec()),
        }
    }
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
            body: Some(b"ok".to_vec()),
        };
        assert_ne!(streamed_response().status, normal.status);
    }

    #[test]
    fn body_str_returns_valid_utf8() {
        let req = Request {
            method: "GET".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(b"hello".to_vec()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        assert_eq!(req.body_str(), Some("hello"));
    }

    #[test]
    fn body_str_returns_none_for_binary() {
        let req = Request {
            method: "GET".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(vec![0xFF, 0xFE]),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        assert_eq!(req.body_str(), None);
    }

    #[test]
    fn base64_roundtrip() {
        let original = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(vec![0x00, 0xFF, 0x80, 0x7F]),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(original.body, decoded.body);
    }

    #[test]
    fn base64_none_body_roundtrip() {
        let original = Request {
            method: "GET".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, None);
    }
}
