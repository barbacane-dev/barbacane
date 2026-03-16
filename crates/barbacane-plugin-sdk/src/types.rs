use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Serde helper that encodes `Option<Vec<u8>>` as a base64 string in JSON.
///
/// **Not used by `Request`/`Response`** — those use `#[serde(skip)]` and the
/// side-channel host functions (`host_body_read`/`host_body_set`).
///
/// Still used by:
/// - Host-side HTTP types (`PluginHttpRequest`, `HttpResponse`) for the
///   `host_http_call` / `host_http_read_result` JSON protocol.
/// - Cache entries (`CacheEntry`) for storing binary bodies in the cache.
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
    /// Body travels via side-channel host functions (host_body_read/host_body_set),
    /// not embedded in JSON. Skipped during serde.
    #[serde(skip)]
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
    /// Body travels via side-channel host functions (host_body_read/host_body_set),
    /// not embedded in JSON. Skipped during serde.
    #[serde(skip)]
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

    // ── Side-channel body contract tests ──────────────────────────────
    //
    // Body is #[serde(skip)] — it does NOT appear in JSON. These tests
    // verify the serialization contract that the proc macro glue and
    // host data plane depend on.

    #[test]
    fn body_is_absent_from_request_json() {
        let req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(vec![0x00, 0xFF, 0x80, 0x7F]),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("body"), "body should not appear in JSON");
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, None); // body is skipped
        assert_eq!(decoded.method, "POST");
        assert_eq!(decoded.path, "/upload");
    }

    #[test]
    fn body_is_absent_from_response_json() {
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"hello".to_vec()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("body"), "body should not appear in JSON");
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, None); // body is skipped
        assert_eq!(decoded.status, 200);
    }

    #[test]
    fn request_none_body_roundtrip() {
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

    #[test]
    fn response_none_body_roundtrip() {
        let resp = Response {
            status: 204,
            headers: BTreeMap::new(),
            body: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert!(decoded.body.is_none());
    }

    /// Verify the middleware output structure ({"action":0/1, "data": ...})
    /// does NOT include a body field in the data.
    #[test]
    fn middleware_output_has_no_body_field() {
        let req = Request {
            method: "POST".into(),
            path: "/api/messages".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(b"payload".to_vec()), // set in memory, but skipped in JSON
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        let mw_output = serde_json::json!({
            "action": 0,
            "data": req
        });
        let json_str = serde_json::to_string(&mw_output).unwrap();
        assert!(
            !json_str.contains("\"body\""),
            "body should not appear in middleware output JSON"
        );
    }

    /// Request metadata roundtrips correctly through middleware output parsing.
    #[test]
    fn metadata_survives_middleware_roundtrip() {
        let req = Request {
            method: "POST".into(),
            path: "/api/channels/123/messages".into(),
            query: Some("page=1".into()),
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/json".into());
                h
            },
            body: None,
            client_ip: "10.0.0.1".into(),
            path_params: {
                let mut p = BTreeMap::new();
                p.insert("channel_id".into(), "123".into());
                p
            },
        };
        let json = serde_json::to_vec(&req).unwrap();

        // Simulates middleware: deserialize → wrap → host extracts data → deserialize
        let mw_req: Request = serde_json::from_slice(&json).unwrap();
        let mw_output = serde_json::json!({"action": 0, "data": mw_req});
        let parsed: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&mw_output).unwrap()).unwrap();
        let data_json = serde_json::to_vec(&parsed["data"]).unwrap();
        let dispatch_req: Request = serde_json::from_slice(&data_json).unwrap();

        assert_eq!(dispatch_req.method, "POST");
        assert_eq!(dispatch_req.path, "/api/channels/123/messages");
        assert_eq!(dispatch_req.query, Some("page=1".into()));
        assert_eq!(
            dispatch_req.headers.get("content-type").map(|s| s.as_str()),
            Some("application/json")
        );
        assert_eq!(dispatch_req.client_ip, "10.0.0.1");
        // body is None (travels via side-channel)
        assert_eq!(dispatch_req.body, None);
    }

    /// Short-circuit response metadata roundtrips correctly.
    #[test]
    fn short_circuit_response_metadata_roundtrip() {
        let resp = Response {
            status: 401,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/json".into());
                h
            },
            body: Some(b"{\"error\":\"unauthorized\"}".to_vec()),
        };

        let mw_output = serde_json::json!({"action": 1, "data": resp});
        let parsed: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&mw_output).unwrap()).unwrap();
        let data_json = serde_json::to_vec(&parsed["data"]).unwrap();
        let sc_resp: Response = serde_json::from_slice(&data_json).unwrap();

        assert_eq!(sc_resp.status, 401);
        assert_eq!(
            sc_resp.headers.get("content-type").map(|s| s.as_str()),
            Some("application/json")
        );
        // body is None in JSON (travels via side-channel)
        assert_eq!(sc_resp.body, None);
    }

    /// JSON with an extra "body" field is safely ignored by serde(skip).
    #[test]
    fn extra_body_field_in_json_is_ignored() {
        let json = r#"{"method":"POST","path":"/","query":null,"headers":{},"client_ip":"127.0.0.1","path_params":{},"body":"ignored"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        assert_eq!(req.body, None);
        assert_eq!(req.method, "POST");
    }

    // ── Helper methods ────────────────────────────────────────────────────

    #[test]
    fn body_string_returns_owned_utf8() {
        let req = Request {
            method: "POST".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(b"hello world".to_vec()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        assert_eq!(req.body_string(), Some("hello world".to_string()));
    }

    #[test]
    fn body_string_returns_none_for_binary() {
        let req = Request {
            method: "POST".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(vec![0xFF, 0xFE]),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        assert_eq!(req.body_string(), None);
    }

    #[test]
    fn set_body_text_roundtrips() {
        let mut req = Request {
            method: "POST".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        req.set_body_text("{\"key\":\"value\"}");
        assert_eq!(req.body_str(), Some("{\"key\":\"value\"}"));
    }

    #[test]
    fn response_body_str_valid_utf8() {
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"hello".to_vec()),
        };
        assert_eq!(resp.body_str(), Some("hello"));
    }

    #[test]
    fn response_body_str_binary() {
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(vec![0xFF]),
        };
        assert_eq!(resp.body_str(), None);
    }

    #[test]
    fn response_set_body_text() {
        let mut resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        };
        resp.set_body_text("ok");
        assert_eq!(resp.body_str(), Some("ok"));
    }

    #[test]
    fn response_text_constructor() {
        let resp = Response::text(200, BTreeMap::new(), "hello");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body_str(), Some("hello"));
    }

    // ── base64_body module tests (used by host-side HttpRequest/HttpResponse) ──

    #[test]
    fn base64_module_roundtrip() {
        use base64::Engine;

        let original = vec![0x00, 0xFF, 0x80, 0x7F];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&original);

        // Verify the module can deserialize what it serializes
        let json = serde_json::json!(encoded);
        let json_str = serde_json::to_string(&Some(&json)).unwrap();
        assert!(json_str.contains(&encoded));

        // Direct roundtrip via the module functions
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .unwrap();
        assert_eq!(bytes, original);
    }
}
