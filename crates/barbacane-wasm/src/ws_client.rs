//! WebSocket client for the `host_ws_upgrade` host function (ADR-0026).
//!
//! Connects to an upstream WebSocket endpoint and returns the established
//! stream for the data plane to relay frames bidirectionally.

use std::collections::BTreeMap;
use std::time::Duration;

use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// The upstream WebSocket stream type after a successful connection.
pub type UpstreamWsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Request parameters for `host_ws_upgrade`.
#[derive(Debug, serde::Deserialize)]
pub struct WsUpgradeRequest {
    pub url: String,
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

fn default_connect_timeout_ms() -> u64 {
    5000
}

/// Connect to an upstream WebSocket endpoint.
///
/// Returns the established WebSocket stream on success, or an error string
/// on failure (to be stored in `last_http_result` for the plugin to read).
pub async fn connect_upstream(
    req: WsUpgradeRequest,
    allow_internal_egress: bool,
) -> Result<UpstreamWsStream, String> {
    // SSRF guard: resolve the upstream host and refuse internal / loopback /
    // link-local / cloud-metadata targets unless egress is explicitly allowed.
    // Mirrors the HTTP and broker guards so a plugin can't reach internal
    // services over ws:// / wss://.
    let parsed = reqwest::Url::parse(&req.url)
        .map_err(|e| format!("invalid WebSocket URL '{}': {}", req.url, e))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("WebSocket URL '{}' has no host", req.url))?;
    let port = parsed.port_or_known_default().unwrap_or(0);
    if let Err(e) = crate::http_client::guard_external_host(host, port, allow_internal_egress).await
    {
        return Err(match e {
            crate::http_client::HostGuardError::Blocked(h) => format!(
                "WebSocket target '{h}' is an internal address; set \
                 BARBACANE_ALLOW_INTERNAL_EGRESS to allow"
            ),
            crate::http_client::HostGuardError::Resolve(m) => {
                format!("WebSocket target resolution failed: {m}")
            }
        });
    }

    // Build the request with custom headers
    let mut ws_request = req
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("invalid WebSocket URL '{}': {}", req.url, e))?;

    for (key, value) in &req.headers {
        if let Ok(header_value) = HeaderValue::from_str(value) {
            if let Ok(header_name) =
                tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(key.as_bytes())
            {
                ws_request.headers_mut().insert(header_name, header_value);
            }
        }
    }

    // Connect with timeout
    let connect_future = tokio_tungstenite::connect_async(ws_request);
    let timeout = Duration::from_millis(req.connect_timeout_ms);

    match tokio::time::timeout(timeout, connect_future).await {
        Ok(Ok((ws_stream, _response))) => Ok(ws_stream),
        Ok(Err(e)) => Err(format!("WebSocket connection failed: {}", e)),
        Err(_) => Err(format!(
            "WebSocket connection timed out after {}ms",
            req.connect_timeout_ms
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_upstream_blocks_internal_target_when_egress_disallowed() {
        let req = WsUpgradeRequest {
            url: "ws://127.0.0.1:9/".to_string(),
            connect_timeout_ms: 1000,
            headers: BTreeMap::new(),
        };
        // Egress disallowed: the SSRF guard must reject the loopback target
        // before any connection attempt.
        let err = connect_upstream(req, false)
            .await
            .expect_err("loopback must be blocked when egress is disallowed");
        assert!(
            err.contains("internal address"),
            "expected SSRF block, got: {err}"
        );
    }
}
