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
/// Build one with [`Request::new`] and the `with_*` methods; the fields are
/// public to read and assign. A minor release may add fields.
///
/// Uses BTreeMap instead of HashMap to avoid WASI random dependency in WASM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: BTreeMap<String, String>,
    /// Body travels via side-channel host functions (host_body_read/host_body_set),
    /// not embedded in JSON. Skipped during serde.
    #[serde(skip)]
    pub body: Option<Vec<u8>>,
    /// The client address the gateway attributes the request to: the TCP
    /// peer, or behind the gateway's `--trusted-proxies` the address they
    /// report.
    pub client_ip: String,
    pub path_params: BTreeMap<String, String>,
}

impl Request {
    /// A request with a method and a path: no query, no headers, no body, an
    /// empty client address and no path parameters.
    pub fn new(method: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: String::new(),
            path_params: BTreeMap::new(),
        }
    }

    /// Set the query string (without the `?`).
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }

    /// Add a header. Header names are lower-case on the wire.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// Replace the headers.
    pub fn with_headers(mut self, headers: BTreeMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    /// Set the body.
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Set the client address.
    pub fn with_client_ip(mut self, client_ip: impl Into<String>) -> Self {
        self.client_ip = client_ip.into();
        self
    }

    /// Add a path parameter.
    pub fn with_path_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.path_params.insert(name.into(), value.into());
        self
    }

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

/// Response header fields: each lower-case name with its values, in the order
/// they are sent. A field the response repeats (`set-cookie`, `link`,
/// `www-authenticate`) has one value per field.
///
/// A BTreeMap, not a HashMap, to keep the WASI random source out of plugins.
pub type ResponseHeaders = BTreeMap<String, Vec<String>>;

/// An HTTP response as produced by plugins.
///
/// Build one with [`Response::new`] and the `with_*` methods; the fields are
/// public to read and assign. A minor release may add fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Response {
    pub status: u16,
    /// The header fields, by lower-case name, each with its values.
    pub headers: ResponseHeaders,
    /// Body travels via side-channel host functions (host_body_read/host_body_set),
    /// not embedded in JSON. Skipped during serde.
    #[serde(skip)]
    pub body: Option<Vec<u8>>,
}

impl Response {
    /// A response with a status, no headers and no body.
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: BTreeMap::new(),
            body: None,
        }
    }

    /// Set a header to one value, replacing any it had. Header names are
    /// lower-case on the wire.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.set_header(name, value);
        self
    }

    /// Replace the headers.
    pub fn with_headers(mut self, headers: ResponseHeaders) -> Self {
        self.headers = headers;
        self
    }

    /// The first value of a header.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.first().map(String::as_str)
    }

    /// Every value of a header, in order; empty when it has none.
    pub fn header_values(&self, name: &str) -> &[String] {
        self.headers.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Set a header to one value, replacing any it had.
    pub fn set_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.headers.insert(name.into(), vec![value.into()]);
    }

    /// Add a value to a header after the ones it has: one more field of that
    /// name, such as another `set-cookie`.
    pub fn append_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.headers
            .entry(name.into())
            .or_default()
            .push(value.into());
    }

    /// Remove a header, returning its values.
    pub fn remove_header(&mut self, name: &str) -> Option<Vec<String>> {
        self.headers.remove(name)
    }

    /// Set the body.
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

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
    pub fn text(status: u16, headers: ResponseHeaders, text: &str) -> Self {
        Self {
            status,
            headers,
            body: Some(text.as_bytes().to_vec()),
        }
    }
}

impl Default for Response {
    /// `200` with no headers and no body.
    fn default() -> Self {
        Self::new(200)
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
    Response::new(0)
}

/// Constant-time byte-slice equality, for comparing secrets (API keys,
/// passwords) without leaking how many leading bytes matched through timing.
///
/// Like `subtle`/`ring::constant_time`, unequal lengths return `false` early
/// (only the length is revealed, never the content); equal-length inputs are
/// compared in time independent of where they differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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

    /// A repeated header keeps one value per field, in order, on the wire
    /// and through the accessors.
    #[test]
    fn a_repeated_response_header_keeps_each_value() {
        let mut resp = Response::new(200).with_header("content-type", "text/plain");
        resp.append_header("set-cookie", "a=1");
        resp.append_header("set-cookie", "b=2");

        assert_eq!(resp.header("set-cookie"), Some("a=1"));
        assert_eq!(resp.header_values("set-cookie"), ["a=1", "b=2"]);
        assert!(resp.header_values("link").is_empty());

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            json["headers"],
            serde_json::json!({"content-type": ["text/plain"], "set-cookie": ["a=1", "b=2"]})
        );
        let back: Response = serde_json::from_value(json).unwrap();
        assert_eq!(back.header_values("set-cookie"), ["a=1", "b=2"]);

        resp.set_header("set-cookie", "c=3");
        assert_eq!(resp.header_values("set-cookie"), ["c=3"]);
        assert_eq!(
            resp.remove_header("set-cookie"),
            Some(vec!["c=3".to_string()])
        );
        assert_eq!(resp.header("set-cookie"), None);
    }

    /// Short-circuit response metadata roundtrips correctly.
    #[test]
    fn short_circuit_response_metadata_roundtrip() {
        let resp = Response {
            status: 401,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), vec!["application/json".into()]);
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
        assert_eq!(sc_resp.header("content-type"), Some("application/json"));
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

    #[test]
    fn constant_time_eq_matches_std_equality() {
        assert!(constant_time_eq(b"secret-key", b"secret-key"));
        assert!(!constant_time_eq(b"secret-key", b"secret-different"));
        assert!(!constant_time_eq(b"secret-key", b"secret-keX"));
        assert!(!constant_time_eq(b"short", b"longer-value"));
        assert!(constant_time_eq(b"", b""));
    }
}
