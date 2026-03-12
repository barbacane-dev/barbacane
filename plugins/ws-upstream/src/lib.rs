//! WebSocket transparent proxy dispatcher plugin for Barbacane API gateway.
//!
//! Proxies WebSocket connections to upstream services. The plugin handles the
//! upgrade handshake; the host runtime manages bidirectional frame relay.
//! See ADR-0026 for the full design.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// WebSocket upstream dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct WsUpstreamDispatcher {
    /// Upstream WebSocket URL (e.g., ws://service:8080/ws or wss://service:443/ws).
    url: String,

    /// Connection timeout in seconds (default: 5).
    #[serde(default = "default_connect_timeout")]
    connect_timeout: f64,

    /// Path template for the upstream request.
    /// Supports `{param}` substitution from path parameters.
    /// If not specified, uses the original request path.
    #[serde(default)]
    path: Option<String>,
}

fn default_connect_timeout() -> f64 {
    5.0
}

/// WebSocket upgrade request for host_ws_upgrade.
#[derive(Serialize)]
struct WsUpgradeRequest {
    url: String,
    connect_timeout_ms: u64,
    headers: BTreeMap<String, String>,
}

impl WsUpstreamDispatcher {
    /// Handle the WebSocket upgrade request.
    ///
    /// Validates the upgrade headers, calls `host_ws_upgrade` to connect to the
    /// upstream, and returns a 101 sentinel response on success. The host runtime
    /// takes over frame relay after this point.
    pub fn dispatch(&mut self, req: Request) -> Response {
        // Validate this is a WebSocket upgrade request
        let upgrade = req
            .headers
            .get("upgrade")
            .map(|v| v.to_lowercase())
            .unwrap_or_default();

        if upgrade != "websocket" {
            return self.error_response(
                400,
                "Bad Request",
                "missing or invalid Upgrade header",
                "expected Upgrade: websocket",
            );
        }

        // Build the upstream URL
        let upstream_path = match &self.path {
            Some(template) => self.substitute_path_params(template, &req.path_params),
            None => req.path.clone(),
        };

        let base_url = self.url.trim_end_matches('/');
        let full_url = if upstream_path.starts_with('/') {
            format!("{}{}", base_url, upstream_path)
        } else {
            format!("{}/{}", base_url, upstream_path)
        };

        // Append query string if present
        let full_url = match &req.query {
            Some(qs) if !qs.is_empty() => format!("{}?{}", full_url, qs),
            _ => full_url,
        };

        // Forward headers (filter hop-by-hop, keep WebSocket-specific ones)
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in &req.headers {
            let key_lower = key.to_lowercase();
            if !matches!(
                key_lower.as_str(),
                "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade"
            ) {
                headers.insert(key.clone(), value.clone());
            }
        }

        // Add forwarding headers
        if let Some(host) = req.headers.get("host") {
            headers.insert("x-forwarded-host".to_string(), host.clone());
        }
        headers.insert("x-forwarded-for".to_string(), req.client_ip.clone());

        // Build and serialize upgrade request
        let ws_request = WsUpgradeRequest {
            url: full_url,
            connect_timeout_ms: (self.connect_timeout * 1000.0) as u64,
            headers,
        };

        let request_json = match serde_json::to_vec(&ws_request) {
            Ok(json) => json,
            Err(e) => {
                return self.error_response(
                    500,
                    "Internal Server Error",
                    "failed to serialize upgrade request",
                    &e.to_string(),
                );
            }
        };

        // Call host_ws_upgrade — connects to upstream WebSocket
        let result =
            unsafe { host_ws_upgrade(request_json.as_ptr() as i32, request_json.len() as i32) };

        if result < 0 {
            // Read the error from the result buffer
            let error_detail = self.read_error_result();
            return self.error_response(
                502,
                "Bad Gateway",
                "upstream WebSocket connection failed",
                &error_detail,
            );
        }

        // Success — return 101 sentinel. The host runtime takes over frame relay.
        Response {
            status: 101,
            headers: BTreeMap::new(),
            body: None,
        }
    }

    /// Read error details from host_http_read_result after a failed host_ws_upgrade.
    fn read_error_result(&self) -> String {
        // The error length is not known; try a reasonable buffer
        let mut buf = vec![0u8; 4096];
        let bytes_read =
            unsafe { host_http_read_result(buf.as_mut_ptr() as i32, buf.len() as i32) };
        if bytes_read > 0 {
            String::from_utf8_lossy(&buf[..bytes_read as usize]).to_string()
        } else {
            "unknown error".to_string()
        }
    }

    /// Substitute path parameters in the template.
    fn substitute_path_params(&self, template: &str, params: &BTreeMap<String, String>) -> String {
        let mut result = template.to_string();
        for (key, value) in params {
            result = result.replace(&format!("{{{}}}", key), value);
        }
        result
    }

    /// Create an error response in RFC 9457 format.
    fn error_response(&self, status: u16, title: &str, detail: &str, debug: &str) -> Response {
        let error_type = match status {
            400 => "urn:barbacane:error:bad-request",
            502 => "urn:barbacane:error:upstream-unavailable",
            _ => "urn:barbacane:error:internal",
        };

        let full_detail = format!("{}: {}", detail, debug);

        let body = serde_json::json!({
            "type": error_type,
            "title": title,
            "status": status,
            "detail": full_detail
        });

        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        Response {
            status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

// Host function declarations
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Initiate a WebSocket upgrade to the upstream. Returns 0 on success, -1 on error.
    fn host_ws_upgrade(req_ptr: i32, req_len: i32) -> i32;

    /// Read result buffer (error details on failure). Returns bytes read.
    fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}

// Native stubs for testing (non-WASM targets)
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_ws_upgrade(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(
        method: &str,
        path: &str,
        headers: BTreeMap<String, String>,
        query: Option<String>,
        path_params: BTreeMap<String, String>,
    ) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            headers,
            body: None,
            query,
            path_params,
            client_ip: "10.0.0.1".to_string(),
        }
    }

    fn ws_headers() -> BTreeMap<String, String> {
        let mut h = BTreeMap::new();
        h.insert("upgrade".to_string(), "websocket".to_string());
        h.insert("connection".to_string(), "Upgrade".to_string());
        h.insert(
            "sec-websocket-key".to_string(),
            "dGhlIHNhbXBsZSBub25jZQ==".to_string(),
        );
        h.insert("sec-websocket-version".to_string(), "13".to_string());
        h.insert("host".to_string(), "gateway.example.com".to_string());
        h
    }

    // -- Config deserialization tests --

    #[test]
    fn config_minimal() {
        let json = r#"{"url": "ws://service:8080/ws"}"#;
        let config: WsUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.url, "ws://service:8080/ws");
        assert_eq!(config.connect_timeout, 5.0);
        assert_eq!(config.path, None);
    }

    #[test]
    fn config_full() {
        let json = r#"{
            "url": "wss://service:443/ws",
            "connect_timeout": 10.0,
            "path": "/v1/stream/{channel}"
        }"#;
        let config: WsUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.url, "wss://service:443/ws");
        assert_eq!(config.connect_timeout, 10.0);
        assert_eq!(config.path, Some("/v1/stream/{channel}".to_string()));
    }

    #[test]
    fn config_missing_url() {
        let json = r#"{"connect_timeout": 5.0}"#;
        let result: Result<WsUpstreamDispatcher, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // -- Upgrade validation tests --

    #[test]
    fn reject_missing_upgrade_header() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let req = make_request("GET", "/ws", BTreeMap::new(), None, BTreeMap::new());
        let response = dispatcher.dispatch(req);

        assert_eq!(response.status, 400);
        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:bad-request");
        assert!(body["detail"].as_str().unwrap().contains("Upgrade header"));
    }

    #[test]
    fn reject_non_websocket_upgrade() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let mut headers = BTreeMap::new();
        headers.insert("upgrade".to_string(), "h2c".to_string());
        let req = make_request("GET", "/ws", headers, None, BTreeMap::new());
        let response = dispatcher.dispatch(req);

        assert_eq!(response.status, 400);
    }

    // -- Dispatch tests (host_ws_upgrade returns -1 in native stubs) --

    #[test]
    fn dispatch_returns_502_on_upstream_failure() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let req = make_request("GET", "/ws", ws_headers(), None, BTreeMap::new());
        let response = dispatcher.dispatch(req);

        assert_eq!(response.status, 502);
        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:upstream-unavailable");
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("WebSocket connection failed"));
    }

    // -- Path construction tests --

    #[test]
    fn path_substitution() {
        let dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080".to_string(),
            connect_timeout: 5.0,
            path: Some("/ws/{room}".to_string()),
        };

        let mut params = BTreeMap::new();
        params.insert("room".to_string(), "general".to_string());
        let result = dispatcher.substitute_path_params("/ws/{room}", &params);
        assert_eq!(result, "/ws/general");
    }

    #[test]
    fn path_substitution_multiple_params() {
        let dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let mut params = BTreeMap::new();
        params.insert("org".to_string(), "acme".to_string());
        params.insert("channel".to_string(), "alerts".to_string());
        let result = dispatcher.substitute_path_params("/ws/{org}/{channel}", &params);
        assert_eq!(result, "/ws/acme/alerts");
    }

    #[test]
    fn dispatch_with_path_template() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080".to_string(),
            connect_timeout: 5.0,
            path: Some("/ws/{room}".to_string()),
        };

        let mut params = BTreeMap::new();
        params.insert("room".to_string(), "general".to_string());
        let req = make_request("GET", "/chat/general", ws_headers(), None, params);

        // Returns 502 because native stub returns -1
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }

    #[test]
    fn dispatch_with_query_string() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let req = make_request(
            "GET",
            "/ws",
            ws_headers(),
            Some("token=abc123".to_string()),
            BTreeMap::new(),
        );

        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }

    #[test]
    fn dispatch_filters_hop_by_hop_headers() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let mut headers = ws_headers();
        headers.insert("keep-alive".to_string(), "timeout=5".to_string());
        headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        headers.insert("x-custom".to_string(), "should-forward".to_string());

        let req = make_request("GET", "/ws", headers, None, BTreeMap::new());

        // We can't inspect the forwarded headers directly (native stub fails),
        // but we verify dispatch runs without panicking
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }

    // -- Error response format tests --

    #[test]
    fn error_response_400() {
        let dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let response = dispatcher.error_response(400, "Bad Request", "test", "debug");
        assert_eq!(response.status, 400);
        assert_eq!(
            response.headers.get("content-type").unwrap(),
            "application/problem+json"
        );

        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:bad-request");
    }

    #[test]
    fn error_response_502() {
        let dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let response =
            dispatcher.error_response(502, "Bad Gateway", "connection failed", "timeout");
        assert_eq!(response.status, 502);

        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:upstream-unavailable");
        assert_eq!(body["detail"], "connection failed: timeout");
    }

    // -- Upgrade header case-insensitivity --

    #[test]
    fn upgrade_header_case_insensitive() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let mut headers = ws_headers();
        headers.remove("upgrade");
        headers.insert("upgrade".to_string(), "WebSocket".to_string());

        let req = make_request("GET", "/ws", headers, None, BTreeMap::new());
        let response = dispatcher.dispatch(req);

        // Should pass validation (not 400) — will get 502 from native stub
        assert_eq!(response.status, 502);
    }

    // -- URL trailing slash handling --

    #[test]
    fn url_trailing_slash_handling() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let req = make_request("GET", "/ws", ws_headers(), None, BTreeMap::new());
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }

    // -- wss:// URL --

    #[test]
    fn config_wss_url() {
        let json = r#"{"url": "wss://secure.example.com/ws"}"#;
        let config: WsUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.url, "wss://secure.example.com/ws");
    }

    #[test]
    fn dispatch_wss_url() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "wss://secure.example.com:443/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let req = make_request("GET", "/ws", ws_headers(), None, BTreeMap::new());
        let response = dispatcher.dispatch(req);
        // 502 from native stub, but validates the wss:// flow doesn't panic
        assert_eq!(response.status, 502);
    }

    // -- Path without leading slash --

    #[test]
    fn path_without_leading_slash() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let req = make_request("GET", "ws/chat", ws_headers(), None, BTreeMap::new());
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }

    // -- Unmatched path params left as-is --

    #[test]
    fn path_substitution_unmatched_params() {
        let dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let mut params = BTreeMap::new();
        params.insert("room".to_string(), "general".to_string());
        let result = dispatcher.substitute_path_params("/ws/{room}/sub/{unknown}", &params);
        assert_eq!(result, "/ws/general/sub/{unknown}");
    }

    // -- Error response unknown status --

    #[test]
    fn error_response_500() {
        let dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let response =
            dispatcher.error_response(500, "Internal Server Error", "serialize failed", "details");
        assert_eq!(response.status, 500);

        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:internal");
    }

    // -- X-Forwarded-For header --

    #[test]
    fn dispatch_adds_forwarding_headers() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        // The dispatch will fail (native stub), but exercise the header logic path
        let req = make_request("GET", "/ws", ws_headers(), None, BTreeMap::new());
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }

    // -- Custom connect timeout --

    #[test]
    fn config_custom_timeout() {
        let json = r#"{"url": "ws://service:8080/ws", "connect_timeout": 0.5}"#;
        let config: WsUpstreamDispatcher = serde_json::from_str(json).unwrap();
        assert_eq!(config.connect_timeout, 0.5);
    }

    // -- read_error_result with empty buffer --

    #[test]
    fn read_error_result_returns_unknown_on_empty() {
        let dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        // Native stub returns 0 for host_http_read_result
        let result = dispatcher.read_error_result();
        assert_eq!(result, "unknown error");
    }

    // -- Empty query string not appended --

    #[test]
    fn dispatch_with_empty_query_string() {
        let mut dispatcher = WsUpstreamDispatcher {
            url: "ws://service:8080/ws".to_string(),
            connect_timeout: 5.0,
            path: None,
        };

        let req = make_request(
            "GET",
            "/ws",
            ws_headers(),
            Some("".to_string()),
            BTreeMap::new(),
        );

        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }
}
