//! Cross-type serde compatibility tests.
//!
//! These tests verify wire-format compatibility between the host-side types
//! (in `barbacane-wasm`) and the plugin-side types (in `barbacane-plugin-sdk`).
//!
//! Plugin Request/Response body uses `#[serde(skip)]` — body travels via
//! side-channel host functions, never in JSON. Host-side HTTP types
//! (PluginHttpRequest, HttpResponse) still use `base64_body` in JSON for
//! the host_http_call/host_http_read_result protocol.

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
    // Plugins serialize an HttpRequest struct (with `base64_body` for body
    // and `timeout_ms: Option<u64>`) and the host deserializes it as
    // `PluginHttpRequest`. They must agree on the `body` field encoding.

    /// A plugin-serialized HTTP request (using base64_body) must deserialize
    /// correctly as the host-side PluginHttpRequest.
    #[test]
    fn plugin_http_request_compat_with_host() {
        let binary_body: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0xFF, 0x00, 0x01];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&binary_body);

        // This is what a plugin (e.g. http-upstream) serializes for host_http_call
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
    // deserializes it. Since body now travels via side-channel for the
    // main Request/Response path, this only applies to host_http_call responses.

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

    // ── Plugin Request/Response: body is serde(skip) ────────────────────
    //
    // Body does NOT appear in JSON for Request and Response. These tests
    // verify the side-channel contract.

    /// Request body is absent from JSON (travels via side-channel).
    #[test]
    fn request_body_absent_from_json() {
        let req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: Some("fmt=raw".into()),
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/octet-stream".into());
                h
            },
            body: Some((0..=255).collect()),
            client_ip: "10.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        let json_bytes = serde_json::to_vec(&req).unwrap();
        let json_str = std::str::from_utf8(&json_bytes).unwrap();
        assert!(
            !json_str.contains("body"),
            "body should not appear in Request JSON"
        );

        // Metadata survives
        let parsed: Request = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.query, Some("fmt=raw".into()));
        assert_eq!(parsed.body, None); // body is skipped
    }

    /// Response body is absent from JSON (travels via side-channel).
    #[test]
    fn response_body_absent_from_json() {
        let resp = Response {
            status: 200,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "image/png".into());
                h
            },
            body: Some(vec![0x89, 0x50, 0x4E, 0x47]),
        };

        let json_bytes = serde_json::to_vec(&resp).unwrap();
        let json_str = std::str::from_utf8(&json_bytes).unwrap();
        assert!(
            !json_str.contains("body"),
            "body should not appear in Response JSON"
        );

        assert_eq!(resp.status, 200);
    }

    /// Middleware Continue output: metadata survives Value extraction.
    /// Body is absent from JSON (travels via side-channel).
    #[test]
    fn middleware_continue_metadata_survives_value_extraction() {
        let req = Request {
            method: "POST".into(),
            path: "/api/data".into(),
            query: None,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("x-consumer-id".into(), "user-42".into());
                h
            },
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        let output = json!({"action": 0, "data": req});
        let output_bytes = serde_json::to_vec(&output).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&output_bytes).unwrap();
        let data_bytes = serde_json::to_vec(&parsed["data"]).unwrap();

        let dispatch_req: Request = serde_json::from_slice(&data_bytes).unwrap();
        assert_eq!(dispatch_req.method, "POST");
        assert_eq!(dispatch_req.path, "/api/data");
        assert_eq!(
            dispatch_req
                .headers
                .get("x-consumer-id")
                .map(|s| s.as_str()),
            Some("user-42")
        );
        assert_eq!(dispatch_req.body, None);
    }

    /// Middleware ShortCircuit: response metadata survives Value extraction.
    #[test]
    fn middleware_shortcircuit_metadata_survives_value_extraction() {
        let resp = Response {
            status: 403,
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/json".into());
                h
            },
            body: None,
        };

        let output = json!({"action": 1, "data": resp});
        let output_bytes = serde_json::to_vec(&output).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&output_bytes).unwrap();
        let data_bytes = serde_json::to_vec(&parsed["data"]).unwrap();

        let host_resp: Response = serde_json::from_slice(&data_bytes).unwrap();
        assert_eq!(host_resp.status, 403);
        assert_eq!(host_resp.body, None);
    }

    // ── Large body is NOT in JSON ───────────────────────────────────────
    //
    // Verify JSON size is small regardless of body size (body is in side-channel).

    /// JSON size is independent of body size.
    #[test]
    fn large_body_does_not_inflate_json() {
        let body: Vec<u8> = vec![0xAA; 1_000_000]; // 1MB
        let req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(body),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        let json_bytes = serde_json::to_vec(&req).unwrap();
        // JSON should be tiny — just metadata, no body
        assert!(
            json_bytes.len() < 200,
            "JSON should be small (was {} bytes); body must not be in JSON",
            json_bytes.len()
        );
    }

    /// Large body in middleware output doesn't inflate JSON.
    #[test]
    fn large_body_middleware_output_stays_small() {
        let req = Request {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(vec![0xBB; 1_000_000]),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };

        let output = json!({"action": 0, "data": req});
        let output_bytes = serde_json::to_vec(&output).unwrap();
        assert!(
            output_bytes.len() < 300,
            "middleware output JSON should be small (was {} bytes)",
            output_bytes.len()
        );
    }
}
