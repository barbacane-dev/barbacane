//! Correlation ID middleware plugin for Barbacane API gateway.
//!
//! Propagates or generates correlation IDs (UUID v7) for distributed tracing.
//! The correlation ID is passed to upstream services and optionally included
//! in the response.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;

/// Correlation ID middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct CorrelationId {
    /// Header name for the correlation ID.
    /// Default: "x-correlation-id"
    #[serde(default = "default_header_name")]
    header_name: String,

    /// Generate a new correlation ID if none is provided.
    /// Default: true
    #[serde(default = "default_generate_if_missing")]
    generate_if_missing: bool,

    /// Trust and propagate correlation IDs from incoming requests.
    /// Default: true
    #[serde(default = "default_trust_incoming")]
    trust_incoming: bool,

    /// Include the correlation ID in the response headers.
    /// Default: true
    #[serde(default = "default_include_in_response")]
    include_in_response: bool,
}

fn default_header_name() -> String {
    "x-correlation-id".to_string()
}

fn default_generate_if_missing() -> bool {
    true
}

fn default_trust_incoming() -> bool {
    true
}

fn default_include_in_response() -> bool {
    true
}

impl CorrelationId {
    /// Handle incoming request - propagate or generate correlation ID.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        let mut modified_req = req;
        let header_name_lower = self.header_name.to_lowercase();

        // Check for existing correlation ID
        let existing_id = if self.trust_incoming {
            modified_req.headers.get(&header_name_lower).cloned()
        } else {
            None
        };

        // Determine the correlation ID to use
        let correlation_id = match existing_id {
            Some(id) => id,
            None if self.generate_if_missing => {
                // Generate new UUID v7
                match generate_uuid() {
                    Some(uuid) => uuid,
                    None => {
                        log_message(0, "correlation-id: failed to generate UUID");
                        return Action::Continue(modified_req);
                    }
                }
            }
            None => {
                // No ID and not generating - pass through without modification
                return Action::Continue(modified_req);
            }
        };

        // Set the correlation ID header (overwrites if exists and not trusted)
        modified_req
            .headers
            .insert(header_name_lower.clone(), correlation_id.clone());

        // Store for response handling using context storage
        context_set("correlation-id", &correlation_id);

        Action::Continue(modified_req)
    }

    /// Add correlation ID to response headers.
    pub fn on_response(&mut self, resp: Response) -> Response {
        if !self.include_in_response {
            return resp;
        }

        // Retrieve the correlation ID from context storage
        let mut modified_resp = resp;
        if let Some(correlation_id) = context_get("correlation-id") {
            let header_name_lower = self.header_name.to_lowercase();
            modified_resp
                .headers
                .insert(header_name_lower, correlation_id);
        }

        modified_resp
    }
}

/// Generate a UUID v7 using the host function.
fn generate_uuid() -> Option<String> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_uuid_generate() -> i32;
        fn host_uuid_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }

    unsafe {
        let len = host_uuid_generate();
        if len <= 0 {
            return None;
        }

        let mut buf = vec![0u8; len as usize];
        let read_len = host_uuid_read_result(buf.as_mut_ptr() as i32, len);
        if read_len != len {
            return None;
        }

        String::from_utf8(buf).ok()
    }
}

/// Log a message via host_log.
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

/// Store a value in the request context.
fn context_set(key: &str, value: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_context_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32);
    }
    unsafe {
        host_context_set(
            key.as_ptr() as i32,
            key.len() as i32,
            value.as_ptr() as i32,
            value.len() as i32,
        );
    }
}

/// Get a value from the request context.
fn context_get(key: &str) -> Option<String> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_context_get(key_ptr: i32, key_len: i32) -> i32;
        fn host_context_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }

    unsafe {
        let len = host_context_get(key.as_ptr() as i32, key.len() as i32);
        if len <= 0 {
            return None;
        }

        let mut buf = vec![0u8; len as usize];
        let read_len = host_context_read_result(buf.as_mut_ptr() as i32, len);
        if read_len != len {
            return None;
        }

        String::from_utf8(buf).ok()
    }
}
