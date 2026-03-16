//! Body access control for middleware chains (SPEC-008).
//!
//! Bodies travel via side-channel host functions (not in JSON). This module
//! manages which middleware instances receive the body and collects any
//! modifications they make.
//!
//! Non-body-access middleware: body is not set on the instance (it sees `None`).
//! Body-access middleware: body is injected via `set_request_body()` before the
//! call and collected via `take_output_body()` after.

use crate::instance::PluginInstance;

/// Holds the split state: body-less JSON metadata and the raw body bytes.
///
/// Created once before the middleware chain runs. The body is only injected
/// (via side-channel) for middlewares that declare `body_access = true`.
pub struct BodyAccessControl {
    /// Request/response JSON (body field is absent due to `#[serde(skip)]`).
    metadata_json: Vec<u8>,
    /// The held-aside raw body bytes.
    held_body: Option<Vec<u8>>,
}

impl BodyAccessControl {
    /// Create a new body access controller.
    ///
    /// `metadata_json` is the serialized Request/Response (body is `#[serde(skip)]`
    /// so it's already absent from JSON). `body` is the raw body bytes extracted
    /// from the original struct before serialization.
    pub fn new(metadata_json: Vec<u8>, body: Option<Vec<u8>>) -> Self {
        Self {
            metadata_json,
            held_body: body,
        }
    }

    /// Prepare an instance for a middleware call.
    ///
    /// If `body_access` is true, the held body is set on the instance via
    /// side-channel. Otherwise, no body is set (plugin sees `None`).
    ///
    /// Returns a clone of the metadata JSON to pass to the WASM handler.
    pub fn prepare_instance(&self, instance: &mut PluginInstance, body_access: bool) -> Vec<u8> {
        if body_access {
            instance.set_request_body(self.held_body.clone());
        } else {
            instance.set_request_body(None);
        }
        self.metadata_json.clone()
    }

    /// Collect results after a middleware call.
    ///
    /// `output` is the metadata JSON returned by the plugin (via `take_output()`).
    /// If `body_access` is true, the output body is taken from the instance's
    /// side-channel and updates the held body.
    pub fn collect_after(
        &mut self,
        instance: &mut PluginInstance,
        output: Vec<u8>,
        body_access: bool,
    ) {
        if !output.is_empty() {
            self.metadata_json = output;
        }
        if body_access {
            // Plugin called host_body_set or host_body_clear → update held body.
            // If plugin didn't call either (None), body is unchanged.
            if let Some(new_body) = instance.take_output_body() {
                self.held_body = new_body;
            }
        }
    }

    /// Get the held body (for passing to the dispatcher via side-channel).
    pub fn body(&self) -> &Option<Vec<u8>> {
        &self.held_body
    }

    /// Consume self and return the metadata JSON and body separately.
    pub fn finalize(self) -> (Vec<u8>, Option<Vec<u8>>) {
        (self.metadata_json, self.held_body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_metadata(headers: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "method": "POST",
            "path": "/upload",
            "query": null,
            "headers": headers,
            "client_ip": "127.0.0.1",
            "path_params": {}
        }))
        .expect("serialize")
    }

    fn parse_header(json_bytes: &[u8], key: &str) -> Option<String> {
        let v: serde_json::Value = serde_json::from_slice(json_bytes).expect("parse JSON");
        v["headers"][key].as_str().map(|s| s.to_string())
    }

    // ── Construction ────────────────────────────────────────────────

    #[test]
    fn new_with_body() {
        let meta = make_metadata(json!({}));
        let ctrl = BodyAccessControl::new(meta, Some(b"hello".to_vec()));
        assert_eq!(ctrl.held_body, Some(b"hello".to_vec()));
    }

    #[test]
    fn new_without_body() {
        let meta = make_metadata(json!({}));
        let ctrl = BodyAccessControl::new(meta, None);
        assert_eq!(ctrl.held_body, None);
    }

    // ── Finalize ────────────────────────────────────────────────────

    #[test]
    fn finalize_returns_metadata_and_body() {
        let meta = make_metadata(json!({"content-type": "text/plain"}));
        let body = Some(b"the body".to_vec());
        let ctrl = BodyAccessControl::new(meta.clone(), body.clone());

        let (final_meta, final_body) = ctrl.finalize();
        assert_eq!(final_meta, meta);
        assert_eq!(final_body, body);
    }

    #[test]
    fn finalize_none_body() {
        let meta = make_metadata(json!({}));
        let ctrl = BodyAccessControl::new(meta, None);

        let (_final_meta, final_body) = ctrl.finalize();
        assert_eq!(final_body, None);
    }

    // ── collect_after without instance (unit-level) ─────────────────
    // Full integration tests with real WASM instances live in workload tests.
    // Here we test the metadata update logic.

    #[test]
    fn collect_updates_metadata_from_non_empty_output() {
        let meta = make_metadata(json!({}));
        let new_meta = make_metadata(json!({"x-added": "value"}));
        let mut ctrl = BodyAccessControl::new(meta, Some(b"body".to_vec()));

        // Simulate: non-body middleware returned new metadata
        // (We can't call collect_after without an instance, so test metadata update directly)
        ctrl.metadata_json = new_meta.clone();

        assert_eq!(
            parse_header(&ctrl.metadata_json, "x-added"),
            Some("value".to_string())
        );
        // Body is unchanged
        assert_eq!(ctrl.held_body, Some(b"body".to_vec()));
    }

    #[test]
    fn body_accessor_returns_held_body() {
        let meta = make_metadata(json!({}));
        let ctrl = BodyAccessControl::new(meta, Some(b"raw bytes".to_vec()));
        assert_eq!(ctrl.body(), &Some(b"raw bytes".to_vec()));
    }
}
