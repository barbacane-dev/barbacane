//! Body-echo fixture plugin for workload integration tests.
//!
//! A dispatcher that echoes back the received request body, headers, method,
//! path, and query in a JSON response. This creates a closed-loop test:
//! send body -> gateway -> WASM dispatcher -> response, allowing exact
//! byte-level body integrity verification without an external mock server.

use barbacane_plugin_sdk::prelude::*;
use base64::Engine;
use serde::Deserialize;
use std::collections::BTreeMap;

#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct BodyEcho {}

impl BodyEcho {
    pub fn dispatch(&mut self, req: Request) -> Response {
        let echo = serde_json::json!({
            "method": req.method,
            "path": req.path,
            "query": req.query,
            "headers": req.headers,
            "path_params": req.path_params,
            "body_base64": req.body.as_ref().map(|b| {
                base64::engine::general_purpose::STANDARD.encode(b)
            }),
            "body_size": req.body.as_ref().map(|b| b.len()),
        });

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Response {
            status: 200,
            headers,
            body: Some(echo.to_string().into_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(body: Option<Vec<u8>>) -> Request {
        Request {
            method: "POST".into(),
            path: "/echo".into(),
            query: Some("foo=bar".into()),
            headers: {
                let mut h = BTreeMap::new();
                h.insert("content-type".into(), "application/octet-stream".into());
                h
            },
            body,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        }
    }

    #[test]
    fn config_deserializes() {
        let cfg: BodyEcho = serde_json::from_str("{}").unwrap();
        let _ = cfg;
    }

    #[test]
    fn echoes_body_as_base64() {
        let body = vec![0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF];
        let mut plugin = BodyEcho {};
        let resp = plugin.dispatch(make_request(Some(body.clone())));

        assert_eq!(resp.status, 200);
        let echo: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(echo["body_size"], body.len());

        let echoed_base64 = echo["body_base64"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(echoed_base64)
            .unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn echoes_none_body() {
        let mut plugin = BodyEcho {};
        let resp = plugin.dispatch(make_request(None));

        let echo: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert!(echo["body_base64"].is_null());
        assert!(echo["body_size"].is_null());
    }

    #[test]
    fn echoes_empty_body() {
        let mut plugin = BodyEcho {};
        let resp = plugin.dispatch(make_request(Some(Vec::new())));

        let echo: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(echo["body_base64"], "");
        assert_eq!(echo["body_size"], 0);
    }

    #[test]
    fn echoes_request_metadata() {
        let mut plugin = BodyEcho {};
        let resp = plugin.dispatch(make_request(Some(b"test".to_vec())));

        let echo: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(echo["method"], "POST");
        assert_eq!(echo["path"], "/echo");
        assert_eq!(echo["query"], "foo=bar");
        assert_eq!(echo["headers"]["content-type"], "application/octet-stream");
    }

    #[test]
    fn all_256_byte_values_roundtrip() {
        let all_bytes: Vec<u8> = (0..=255).collect();
        let mut plugin = BodyEcho {};
        let resp = plugin.dispatch(make_request(Some(all_bytes.clone())));

        let echo: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        let echoed_base64 = echo["body_base64"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(echoed_base64)
            .unwrap();
        assert_eq!(decoded, all_bytes);
    }
}
