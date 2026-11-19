//! A dispatcher written against the 1.0 plugin API. It stays as it is for all
//! of 1.x: CI builds it with the current SDK and macros, and an integration
//! test loads and runs it (`crates/barbacane-test/tests/plugin_compat.rs`). A
//! change to the SDK or the macros that breaks it breaks plugins written by
//! others, so CI refuses a change to this file.

use barbacane_plugin_sdk::prelude::*;
use barbacane_plugin_sdk::{context, hash};
use serde::Deserialize;

#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct CompatDispatcher {
    /// Echoed back in every response.
    greeting: String,
}

impl CompatDispatcher {
    pub fn dispatch(&mut self, req: Request) -> Response {
        let body = req.body.clone().unwrap_or_default();
        let echo = serde_json::json!({
            "greeting": self.greeting,
            "method": req.method,
            "path": req.path,
            "query": req.query,
            "id": req.path_params.get("id"),
            "header": req.headers.get("x-compat"),
            "context": context::get("compat-middleware"),
            "body": req.body_str(),
            "body_sha256": hash::sha256_hex(&body),
        });
        Response::new(200)
            .with_header("content-type", "application/json")
            .with_body(echo.to_string().into_bytes())
    }
}
