//! Cross-type serde compatibility tests.
//!
//! These tests verify wire-format compatibility between the host-side types
//! (in `barbacane-wasm`) and the plugin-side types (in `barbacane-plugin-sdk`).
//!
//! The types intentionally live in separate crates and have different field
//! sets (e.g. `PluginHttpRequest.timeout_ms` vs `HttpRequest.timeout`), but
//! they must agree on shared fields — especially `body`, which uses
//! `#[serde(with = "base64_body")]` on both sides.
//!
//! If a new field is added or a serde attribute changes on one side, these
//! tests will catch the mismatch at `cargo test` time rather than at runtime.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use base64::Engine;
    use serde_json::json;

    use barbacane_plugin_sdk::types::{base64_body, Request, Response};

    use crate::http_client::HttpResponse;
    use crate::instance::PluginHttpRequest;

    // ── Plugin → Host: PluginHttpRequest ────────────────────────────────
    //
    // Plugins serialize an HttpRequest struct (with `timeout_ms: Option<u64>`)
    // and the host deserializes it as `PluginHttpRequest`. They must agree
    // on the `body` field encoding.

    /// A plugin-serialized HTTP request (using base64_body) must deserialize
    /// correctly as the host-side PluginHttpRequest.
    #[test]
    fn plugin_http_request_compat_with_host() {
        let binary_body: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0xFF, 0x00, 0x01];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&binary_body);

        // This is what a plugin (e.g. http-upstream) serializes
        let plugin_json = json!({
            "method": "POST",
            "url": "https://upstream.example.com/api",
            "headers": {"content-type": "application/octet-stream"},
            "body": b64,
            "timeout_ms": 5000_u64,
        });

        let host_parsed: PluginHttpRequest =
            serde_json::from_value(plugin_json).expect("host should parse plugin HttpRequest");

        assert_eq!(host_parsed.method, "POST");
        assert_eq!(host_parsed.url, "https://upstream.example.com/api");
        assert_eq!(host_parsed.body, Some(binary_body));
        assert_eq!(host_parsed.timeout_ms, Some(5000));
    }

    /// Plugin sends null body — host should parse it as None.
    #[test]
    fn plugin_http_request_null_body_compat() {
        let plugin_json = json!({
            "method": "GET",
            "url": "https://example.com",
        });

        let host_parsed: PluginHttpRequest =
            serde_json::from_value(plugin_json).expect("host should parse null-body request");

        assert!(host_parsed.body.is_none());
    }

    // ── Host → Plugin: HttpResponse ─────────────────────────────────────
    //
    // The host serializes HttpResponse (with base64_body) and the plugin
    // deserializes it as its own HttpResponse struct. Since plugins define
    // their own HttpResponse type, we test against a generic struct that
    // mirrors what plugins use.

    /// Host-serialized HttpResponse must be parseable by plugin-side code.
    #[test]
    fn host_http_response_compat_with_plugin() {
        let binary_body: Vec<u8> = vec![0x00, 0x01, 0x80, 0xFF, 0xFE];

        let host_resp = HttpResponse {
            status: 200,
            headers: {
                let mut h = std::collections::HashMap::new();
                h.insert(
                    "content-type".to_string(),
                    "application/octet-stream".to_string(),
                );
                h
            },
            body: Some(binary_body.clone()),
        };

        let json = serde_json::to_value(&host_resp).expect("host should serialize");

        // Plugin-side: parse as a struct with base64_body
        #[derive(serde::Deserialize)]
        struct PluginHttpResponse {
            status: u16,
            headers: BTreeMap<String, String>,
            #[serde(default, with = "base64_body")]
            body: Option<Vec<u8>>,
        }

        let plugin_parsed: PluginHttpResponse =
            serde_json::from_value(json).expect("plugin should parse host HttpResponse");

        assert_eq!(plugin_parsed.status, 200);
        assert_eq!(plugin_parsed.body, Some(binary_body));
        assert_eq!(
            plugin_parsed
                .headers
                .get("content-type")
                .map(|s| s.as_str()),
            Some("application/octet-stream")
        );
    }

    // ── Host → Plugin: Request (gateway main path) ──────────────────────
    //
    // The host builds a `Request` (from barbacane_plugin_sdk), serializes
    // it to JSON (with body as base64), writes it into WASM memory, and the
    // plugin deserializes it. This must always round-trip correctly.

    /// Host-serialized Request with binary body must be parseable by plugin.
    #[test]
    fn host_request_roundtrip_with_binary_body() {
        let body: Vec<u8> = (0..=255).collect();
        let req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: Some("fmt=raw".into()),
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/octet-stream".into());
                h
            },
            body: Some(body.clone()),
            client_ip: "10.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        let json_bytes = serde_json::to_vec(&req).unwrap();
        let parsed: Request = serde_json::from_slice(&json_bytes).unwrap();

        assert_eq!(parsed.body, Some(body));
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.query, Some("fmt=raw".into()));
    }

    // ── Plugin → Host: Response (dispatcher output) ─────────────────────
    //
    // The dispatcher plugin serializes a `Response` and writes it to the
    // output buffer. The host reads this buffer and deserializes it.

    /// Plugin-serialized Response with binary body must be parseable by host.
    #[test]
    fn plugin_response_roundtrip_with_binary_body() {
        let body: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let resp = Response {
            status: 200,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "image/png".into());
                h
            },
            body: Some(body.clone()),
        };

        let json_bytes = serde_json::to_vec(&resp).unwrap();

        // Host parses as serde_json::Value (actual host code path)
        let value: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(value["status"], 200);

        // Host then extracts body — it expects a base64 string
        let body_b64 = value["body"]
            .as_str()
            .expect("body should be a string (base64)");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(body_b64)
            .expect("body should be valid base64");
        assert_eq!(decoded, body);
    }

    // ── Middleware action output → Host extraction ───────────────────────
    //
    // Middleware wraps its output as {"action": 0, "data": <Request>} (Continue)
    // or {"action": 1, "data": <Response>} (ShortCircuit). The host extracts
    // `data` via Value manipulation and re-serializes it. The body field must
    // survive this Value→String→parse roundtrip.

    /// Middleware Continue action: body survives Value extraction and re-parse.
    #[test]
    fn middleware_continue_body_survives_value_extraction() {
        let body: Vec<u8> = vec![0x00, 0xFF, 0x80, 0x7F];
        let req = Request {
            method: "POST".into(),
            path: "/api/data".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(body.clone()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        // Middleware serializes: {"action": 0, "data": <request>}
        let output = json!({"action": 0, "data": req});
        let output_bytes = serde_json::to_vec(&output).unwrap();

        // Host extracts "data" as Value, then re-serializes to bytes
        let parsed: serde_json::Value = serde_json::from_slice(&output_bytes).unwrap();
        let data_bytes = serde_json::to_vec(&parsed["data"]).unwrap();

        // Dispatcher receives this and deserializes as Request
        let dispatch_req: Request = serde_json::from_slice(&data_bytes).unwrap();
        assert_eq!(dispatch_req.body, Some(body));
    }

    /// Middleware ShortCircuit action: response body survives Value extraction.
    #[test]
    fn middleware_shortcircuit_body_survives_value_extraction() {
        let body = b"{\"error\":\"forbidden\"}".to_vec();
        let resp = Response {
            status: 403,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/json".into());
                h
            },
            body: Some(body.clone()),
        };

        let output = json!({"action": 1, "data": resp});
        let output_bytes = serde_json::to_vec(&output).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&output_bytes).unwrap();
        let data_bytes = serde_json::to_vec(&parsed["data"]).unwrap();

        let host_resp: Response = serde_json::from_slice(&data_bytes).unwrap();
        assert_eq!(host_resp.status, 403);
        assert_eq!(host_resp.body, Some(body));
    }

    // ── Body stripping + reattachment (BodyAccessControl path) ──────────
    //
    // When middleware has body_access=false, the host strips the body
    // (sets to null) before calling the middleware, then reattaches it
    // after. This path uses Value manipulation to inject the base64 body.

    /// Body stripped for middleware, then reattached via Value injection.
    #[test]
    fn body_strip_and_reattach_via_value_injection() {
        let original_body: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF];

        // Host sends body: None to middleware
        let stripped_req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let stripped_json = serde_json::to_vec(&stripped_req).unwrap();

        // Middleware passes through unchanged
        let mw_req: Request = serde_json::from_slice(&stripped_json).unwrap();
        assert!(mw_req.body.is_none());

        let mw_output = serde_json::to_vec(&json!({"action": 0, "data": mw_req})).unwrap();

        // Host extracts data, injects body
        let parsed: serde_json::Value = serde_json::from_slice(&mw_output).unwrap();
        let mut data_value = parsed["data"].clone();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&original_body);
        data_value
            .as_object_mut()
            .unwrap()
            .insert("body".to_string(), serde_json::Value::String(b64));
        let final_json = serde_json::to_vec(&data_value).unwrap();

        // Dispatcher deserializes
        let dispatch_req: Request = serde_json::from_slice(&final_json).unwrap();
        assert_eq!(dispatch_req.body, Some(original_body));
    }

    // ── Large binary body cross-type ────────────────────────────────────
    //
    // Verify that a large payload (simulating file upload) survives the full
    // host↔plugin serde boundary without truncation or corruption.

    /// 1MB binary payload roundtrips through Request serialize/deserialize.
    #[test]
    fn large_binary_body_request_roundtrip() {
        let body: Vec<u8> = (0..1_000_000).map(|i| (i % 256) as u8).collect();
        let req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(body.clone()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        let json_bytes = serde_json::to_vec(&req).unwrap();
        let parsed: Request = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(parsed.body.as_ref().unwrap().len(), 1_000_000);
        assert_eq!(parsed.body, Some(body));
    }

    /// 1MB body survives the full middleware Continue extraction path.
    #[test]
    fn large_binary_body_middleware_extraction_roundtrip() {
        let body: Vec<u8> = (0..1_000_000).map(|i| (i % 256) as u8).collect();
        let req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(body.clone()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        let output = json!({"action": 0, "data": req});
        let output_bytes = serde_json::to_vec(&output).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&output_bytes).unwrap();
        let data_bytes = serde_json::to_vec(&parsed["data"]).unwrap();
        let dispatch_req: Request = serde_json::from_slice(&data_bytes).unwrap();

        assert_eq!(dispatch_req.body.as_ref().unwrap().len(), 1_000_000);
        assert_eq!(&dispatch_req.body.unwrap()[..256], &body[..256]);
    }
}
