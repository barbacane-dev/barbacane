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

    // ── Gateway data-flow simulation tests ───────────────────────────────
    //
    // These tests reproduce the exact serialize → middleware → re-serialize
    // → dispatch path that the Barbacane host (main.rs) + WASM plugins
    // follow. Any mismatch in the JSON contract shows up here.

    /// Simulate the full gateway path for a request WITH a body:
    ///   1. Host builds Request with body, serializes to JSON (base64)
    ///   2. Middleware WASM deserializes → on_request → wraps in {"action":0,"data":req}
    ///   3. Host extracts `data` field from middleware output
    ///   4. Dispatcher WASM deserializes the `data` JSON as Request
    #[test]
    fn gateway_flow_request_with_body_through_middleware() {
        let body = b"{\"content\":\"hello\"}".to_vec();

        // Step 1: Host serializes the Request (body becomes base64)
        let host_request = Request {
            method: "POST".into(),
            path: "/api/channels/123/messages".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(body.clone()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let request_json = serde_json::to_vec(&host_request).unwrap();

        // Step 2: Middleware WASM deserializes the request
        let mw_req: Request = serde_json::from_slice(&request_json).unwrap();
        assert_eq!(mw_req.body, Some(body.clone()));

        // Middleware wraps in {"action":0, "data": <modified request>}
        let mw_output = serde_json::to_vec(&serde_json::json!({
            "action": 0,
            "data": mw_req
        }))
        .unwrap();

        // Step 3: Host parses middleware output, extracts `data`
        let parsed: serde_json::Value = serde_json::from_slice(&mw_output).unwrap();
        let data_json = serde_json::to_vec(&parsed["data"]).unwrap();

        // Step 4: Dispatcher WASM deserializes the data as Request
        let dispatch_req: Request = serde_json::from_slice(&data_json).unwrap();
        assert_eq!(dispatch_req.body, Some(body));
        assert_eq!(dispatch_req.method, "POST");
        assert_eq!(dispatch_req.path, "/api/channels/123/messages");
    }

    /// Same flow but with body: None (e.g. GET request or body-stripped path).
    #[test]
    fn gateway_flow_request_without_body_through_middleware() {
        let host_request = Request {
            method: "GET".into(),
            path: "/api/channels".into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let request_json = serde_json::to_vec(&host_request).unwrap();

        // Middleware deserializes
        let mw_req: Request = serde_json::from_slice(&request_json).unwrap();
        assert!(mw_req.body.is_none());

        // Middleware wraps and returns
        let mw_output = serde_json::to_vec(&serde_json::json!({
            "action": 0,
            "data": mw_req
        }))
        .unwrap();

        // Host extracts data
        let parsed: serde_json::Value = serde_json::from_slice(&mw_output).unwrap();
        let data_json = serde_json::to_vec(&parsed["data"]).unwrap();

        // Dispatcher deserializes
        let dispatch_req: Request = serde_json::from_slice(&data_json).unwrap();
        assert!(dispatch_req.body.is_none());
    }

    /// Simulate the current main.rs body-stripping path:
    ///   1. Host builds Request with body: None for middleware
    ///   2. Middleware passes through
    ///   3. Host re-inserts the body via serde_json::Value manipulation
    ///   4. Dispatcher deserializes
    #[test]
    fn gateway_flow_body_stripped_then_reattached() {
        use base64::Engine;

        let original_body = b"{\"content\":\"hello\"}".to_vec();

        // Step 1: Host sends body: None to middleware
        let mw_request = Request {
            method: "POST".into(),
            path: "/api/channels/123/messages".into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let mw_json = serde_json::to_vec(&mw_request).unwrap();

        // Step 2: Middleware deserializes, wraps, returns (body stays None)
        let mw_req: Request = serde_json::from_slice(&mw_json).unwrap();
        let mw_output = serde_json::to_vec(&serde_json::json!({
            "action": 0,
            "data": mw_req
        }))
        .unwrap();

        // Step 3: Host extracts data, injects body via Value manipulation
        let parsed: serde_json::Value = serde_json::from_slice(&mw_output).unwrap();
        let mut data_value = parsed["data"].clone();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&original_body);
        data_value
            .as_object_mut()
            .unwrap()
            .insert("body".to_string(), serde_json::Value::String(encoded));
        let dispatch_json = serde_json::to_vec(&data_value).unwrap();

        // Step 4: Dispatcher deserializes — does this work?
        let dispatch_req: Request = serde_json::from_slice(&dispatch_json).unwrap();
        assert_eq!(dispatch_req.body, Some(original_body));
    }

    /// Reproduce the exact main.rs no-middleware path:
    /// middleware_request has body: None, no middleware runs,
    /// host reattaches body via Value manipulation.
    #[test]
    fn gateway_flow_no_middleware_body_reattach() {
        use base64::Engine;

        let body = b"{\"test\":\"data\"}".to_vec();

        // Host builds middleware_request with body: None
        let mw_request = Request {
            method: "POST".into(),
            path: "/proxy/post".into(),
            query: None,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/json".into());
                h
            },
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: {
                let mut p = BTreeMap::new();
                p.insert("path".into(), "post".into());
                p
            },
        };

        // No middleware runs — final_mw_json = middleware_request_json
        let final_mw_json = serde_json::to_vec(&mw_request).unwrap();

        // Host reattaches body (exact code from main.rs)
        let final_request_json = {
            let mut val: serde_json::Value = serde_json::from_slice(&final_mw_json).unwrap();
            let obj = val.as_object_mut().unwrap();
            let encoded = base64::engine::general_purpose::STANDARD.encode(&body);
            obj.insert("body".to_string(), serde_json::Value::String(encoded));
            serde_json::to_vec(&val).unwrap()
        };

        // Print for debugging
        let json_str = std::str::from_utf8(&final_request_json).unwrap();
        eprintln!("dispatch JSON: {json_str}");

        // Dispatcher WASM does: serde_json::from_slice::<Request>(&request_bytes)
        let result = serde_json::from_slice::<Request>(&final_request_json);
        assert!(
            result.is_ok(),
            "failed to parse dispatch request: {:?}\nJSON: {}",
            result.err(),
            json_str
        );

        let req = result.unwrap();
        assert_eq!(req.body, Some(body));
    }

    // ── Response base64 roundtrip ────────────────────────────────────────

    #[test]
    fn response_base64_roundtrip_text() {
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"{\"ok\":true}".to_vec()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, resp.body);
    }

    #[test]
    fn response_base64_roundtrip_binary() {
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]), // PNG magic
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, resp.body);
    }

    #[test]
    fn response_base64_none_body() {
        let resp = Response {
            status: 204,
            headers: BTreeMap::new(),
            body: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert!(decoded.body.is_none());
    }

    // ── Empty body vs None ────────────────────────────────────────────────

    #[test]
    fn empty_body_is_not_none() {
        let req = Request {
            method: "POST".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(Vec::new()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, Some(Vec::new()));
        assert_eq!(decoded.body_str(), Some("")); // empty Vec<u8> is valid empty UTF-8
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

    // ── Null bytes and special content ─────────────────────────────────

    #[test]
    fn base64_roundtrip_null_bytes() {
        let req = Request {
            method: "POST".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(vec![0x00, 0x00, 0x00]),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, Some(vec![0x00, 0x00, 0x00]));
    }

    #[test]
    fn base64_roundtrip_all_byte_values() {
        let all_bytes: Vec<u8> = (0..=255).collect();
        let req = Request {
            method: "POST".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(all_bytes.clone()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body, Some(all_bytes));
    }

    // ── Middleware short-circuit with binary body ──────────────────────

    #[test]
    fn gateway_flow_middleware_short_circuit_with_body() {
        // Middleware returns ShortCircuit with a JSON error body
        let error_body = b"{\"error\":\"unauthorized\"}".to_vec();
        let resp = Response {
            status: 401,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/json".into());
                h
            },
            body: Some(error_body.clone()),
        };

        // Middleware wraps as {"action":1, "data": <response>}
        let mw_output = serde_json::to_vec(&serde_json::json!({
            "action": 1,
            "data": resp
        }))
        .unwrap();

        // Host parses middleware output, extracts data as Response
        let parsed: serde_json::Value = serde_json::from_slice(&mw_output).unwrap();
        let data_json = serde_json::to_vec(&parsed["data"]).unwrap();
        let short_circuit_resp: Response = serde_json::from_slice(&data_json).unwrap();

        assert_eq!(short_circuit_resp.status, 401);
        assert_eq!(short_circuit_resp.body, Some(error_body));
    }

    // ── Cross-type: Request body → Response body fidelity ─────────────

    #[test]
    fn request_body_survives_dispatcher_roundtrip() {
        // Simulates: host sends Request, dispatcher reads body, echoes it in Response
        let body = vec![0x89, 0x50, 0x4E, 0x47]; // PNG header bytes

        let req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(body.clone()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        // Serialize request (host → WASM)
        let req_json = serde_json::to_vec(&req).unwrap();

        // Dispatcher deserializes request
        let received: Request = serde_json::from_slice(&req_json).unwrap();

        // Dispatcher echoes body in response
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: received.body,
        };

        // Serialize response (WASM → host)
        let resp_json = serde_json::to_vec(&resp).unwrap();

        // Host deserializes response
        let final_resp: Response = serde_json::from_slice(&resp_json).unwrap();
        assert_eq!(final_resp.body, Some(body));
    }

    /// Test with binary body (multipart upload simulation).
    #[test]
    fn gateway_flow_binary_body_stripped_then_reattached() {
        use base64::Engine;

        // Simulate multipart boundary + binary content
        let binary_body: Vec<u8> = {
            let mut v =
                b"--boundary\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\n".to_vec();
            v.extend_from_slice(&[0x00, 0xFF, 0x80, 0x7F, 0xFE]); // binary content
            v.extend_from_slice(b"\r\n--boundary--\r\n");
            v
        };

        // Host sends body: None to middleware
        let mw_request = Request {
            method: "POST".into(),
            path: "/api/channels/123/messages".into(),
            query: None,
            headers: {
                let mut h = BTreeMap::new();
                h.insert(
                    "content-type".into(),
                    "multipart/form-data; boundary=boundary".into(),
                );
                h
            },
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let mw_json = serde_json::to_vec(&mw_request).unwrap();

        // Middleware round-trip
        let mw_req: Request = serde_json::from_slice(&mw_json).unwrap();
        let mw_output = serde_json::to_vec(&serde_json::json!({
            "action": 0,
            "data": mw_req
        }))
        .unwrap();

        // Host reattaches body
        let parsed: serde_json::Value = serde_json::from_slice(&mw_output).unwrap();
        let mut data_value = parsed["data"].clone();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&binary_body);
        data_value
            .as_object_mut()
            .unwrap()
            .insert("body".to_string(), serde_json::Value::String(encoded));
        let dispatch_json = serde_json::to_vec(&data_value).unwrap();

        // Dispatcher deserializes
        let dispatch_req: Request = serde_json::from_slice(&dispatch_json).unwrap();
        assert_eq!(dispatch_req.body, Some(binary_body));
    }
}
