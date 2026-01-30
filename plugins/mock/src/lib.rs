//! Mock dispatcher plugin for Barbacane API gateway.
//!
//! Returns static responses configured in the OpenAPI spec.
//! Useful for health checks, stubs, and testing.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Mock dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct MockDispatcher {
    /// HTTP status code to return (default: 200).
    #[serde(default = "default_status")]
    status: u16,

    /// Response body to return (default: empty string).
    #[serde(default)]
    body: String,

    /// Additional response headers (BTreeMap to avoid WASI random dependency).
    #[serde(default)]
    headers: BTreeMap<String, String>,

    /// Content-Type header value (default: application/json).
    #[serde(default = "default_content_type")]
    content_type: String,
}

fn default_status() -> u16 {
    200
}

fn default_content_type() -> String {
    "application/json".to_string()
}

impl MockDispatcher {
    /// Handle a request and return the configured static response.
    pub fn dispatch(&mut self, _req: Request) -> Response {
        let mut headers = self.headers.clone();
        headers.insert("content-type".to_string(), self.content_type.clone());

        Response {
            status: self.status,
            headers,
            body: if self.body.is_empty() {
                None
            } else {
                Some(self.body.clone())
            },
        }
    }
}
