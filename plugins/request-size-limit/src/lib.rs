//! Request size limit middleware plugin for Barbacane API gateway.
//!
//! Rejects requests that exceed a configurable size limit.
//! Checks both Content-Length header and actual body size.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Request size limit middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct RequestSizeLimit {
    /// Maximum allowed request body size in bytes.
    /// Default: 1048576 (1 MiB)
    #[serde(default = "default_max_bytes")]
    max_bytes: u64,

    /// Whether to check Content-Length header for early rejection.
    /// Default: true
    #[serde(default = "default_check_content_length")]
    check_content_length: bool,
}

fn default_max_bytes() -> u64 {
    1_048_576 // 1 MiB
}

fn default_check_content_length() -> bool {
    true
}

impl RequestSizeLimit {
    /// Handle incoming request - check size limits.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        // Check Content-Length header first for early rejection
        if self.check_content_length {
            if let Some(content_length) = req.headers.get("content-length") {
                if let Ok(size) = content_length.parse::<u64>() {
                    if size > self.max_bytes {
                        return Action::ShortCircuit(self.payload_too_large_response(size));
                    }
                }
            }
        }

        // Check actual body size
        if let Some(body) = &req.body {
            let size = body.len() as u64;
            if size > self.max_bytes {
                return Action::ShortCircuit(self.payload_too_large_response(size));
            }
        }

        Action::Continue(req)
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Generate 413 Payload Too Large response.
    fn payload_too_large_response(&self, actual_size: u64) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let body = serde_json::json!({
            "type": "urn:barbacane:error:payload-too-large",
            "title": "Payload Too Large",
            "status": 413,
            "detail": format!(
                "Request body size {} bytes exceeds maximum allowed size of {} bytes.",
                actual_size, self.max_bytes
            )
        });

        Response {
            status: 413,
            headers,
            body: Some(body.to_string()),
        }
    }
}
