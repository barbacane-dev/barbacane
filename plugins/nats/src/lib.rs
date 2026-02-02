//! NATS dispatcher plugin for Barbacane API gateway.
//!
//! Publishes incoming HTTP requests to NATS subjects and returns 202 Accepted.
//! Implements the sync-to-async bridge pattern for event-driven architectures.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// NATS dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct NatsDispatcher {
    /// NATS subject to publish messages to.
    subject: String,

    /// Custom acknowledgment response configuration.
    #[serde(default)]
    ack_response: Option<AckResponse>,

    /// Request headers to pass as NATS message headers.
    #[serde(default)]
    headers_from_request: Vec<String>,
}

/// Custom acknowledgment response.
#[derive(Default, Deserialize)]
struct AckResponse {
    body: Option<serde_json::Value>,
    headers: Option<BTreeMap<String, String>>,
}

/// Message to send to host_nats_publish.
#[derive(Serialize)]
struct BrokerMessage {
    topic: String, // "topic" field is used for subject in NATS
    payload: String,
    headers: BTreeMap<String, String>,
}

/// Result from host_nats_publish.
#[derive(Deserialize)]
struct PublishResult {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    topic: String,
}

impl NatsDispatcher {
    /// Dispatch a request by publishing to NATS and returning 202 Accepted.
    pub fn dispatch(&mut self, req: Request) -> Response {
        // Build message headers from request headers
        let mut msg_headers = BTreeMap::new();
        for header_name in &self.headers_from_request {
            if let Some(value) = req.headers.get(header_name) {
                msg_headers.insert(header_name.clone(), value.clone());
            }
        }

        // Add standard metadata headers
        if let Some(request_id) = req.headers.get("x-request-id") {
            msg_headers.insert("x-request-id".to_string(), request_id.clone());
        }
        if let Some(trace_id) = req.headers.get("x-trace-id") {
            msg_headers.insert("x-trace-id".to_string(), trace_id.clone());
        }

        // Build the broker message
        let message = BrokerMessage {
            topic: self.subject.clone(),
            payload: req.body.clone().unwrap_or_default(),
            headers: msg_headers,
        };

        // Serialize and publish
        let message_json = match serde_json::to_vec(&message) {
            Ok(json) => json,
            Err(e) => return self.error_response(500, "failed to serialize message", &e.to_string()),
        };

        let result_len = unsafe {
            host_nats_publish(message_json.as_ptr() as i32, message_json.len() as i32)
        };

        if result_len < 0 {
            return self.error_response(502, "NATS publish failed", "host_nats_publish returned error");
        }

        // Read the publish result
        let mut result_buf = vec![0u8; result_len as usize];
        let bytes_read = unsafe {
            host_broker_read_result(result_buf.as_mut_ptr() as i32, result_len)
        };

        if bytes_read <= 0 {
            return self.error_response(502, "NATS publish failed", "failed to read publish result");
        }

        // Parse the publish result
        let publish_result: PublishResult = match serde_json::from_slice(&result_buf[..bytes_read as usize]) {
            Ok(r) => r,
            Err(e) => return self.error_response(502, "invalid publish result", &e.to_string()),
        };

        if !publish_result.success {
            let detail = publish_result.error.unwrap_or_else(|| "unknown error".to_string());
            return self.error_response(502, "NATS publish failed", &detail);
        }

        // Build the 202 Accepted response
        self.accepted_response(&publish_result)
    }

    /// Build the 202 Accepted response.
    fn accepted_response(&self, result: &PublishResult) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());

        // Add custom headers from config
        if let Some(ref ack) = self.ack_response {
            if let Some(ref custom_headers) = ack.headers {
                for (k, v) in custom_headers {
                    headers.insert(k.clone(), v.clone());
                }
            }
        }

        // Build response body
        let body = if let Some(ref ack) = self.ack_response {
            if let Some(ref custom_body) = ack.body {
                custom_body.to_string()
            } else {
                self.default_ack_body(result)
            }
        } else {
            self.default_ack_body(result)
        };

        Response {
            status: 202,
            headers,
            body: Some(body),
        }
    }

    /// Build the default acknowledgment body.
    fn default_ack_body(&self, result: &PublishResult) -> String {
        serde_json::json!({
            "status": "accepted",
            "subject": result.topic
        }).to_string()
    }

    /// Create an error response in RFC 9457 format.
    fn error_response(&self, status: u16, title: &str, detail: &str) -> Response {
        let error_type = match status {
            502 => "urn:barbacane:error:nats-publish-failed",
            503 => "urn:barbacane:error:nats-unavailable",
            _ => "urn:barbacane:error:internal",
        };

        let body = serde_json::json!({
            "type": error_type,
            "title": title,
            "status": status,
            "detail": detail
        });

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/problem+json".to_string());

        Response {
            status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

// Host function declarations
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Publish a message to NATS. Returns the result length, or -1 on error.
    fn host_nats_publish(msg_ptr: i32, msg_len: i32) -> i32;

    /// Read the broker publish result into the provided buffer. Returns bytes read.
    fn host_broker_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}
