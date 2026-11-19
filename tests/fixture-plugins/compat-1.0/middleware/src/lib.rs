//! A middleware written against the 1.0 plugin API. It stays as it is for all
//! of 1.x: CI builds it with the current SDK and macros, and an integration
//! test loads and runs it (`crates/barbacane-test/tests/plugin_compat.rs`). A
//! change to the SDK or the macros that breaks it breaks plugins written by
//! others, so CI refuses a change to this file.

use barbacane_plugin_sdk::prelude::*;
use barbacane_plugin_sdk::{context, log};
use serde::Deserialize;

#[barbacane_middleware]
#[derive(Deserialize)]
pub struct CompatMiddleware {
    /// The request header this middleware sets for the dispatcher.
    header: String,
    /// A path this middleware answers itself, with `403`.
    deny_path: String,
}

impl CompatMiddleware {
    pub fn on_request(&mut self, mut req: Request) -> Action<Request> {
        if req.path == self.deny_path {
            log::warn("compat-middleware: denied");
            return Action::ShortCircuit(
                ProblemDetails::new(403, "urn:barbacane:compat:denied", "Denied")
                    .detail("the compat middleware answers this path")
                    .into_response(),
            );
        }
        log::info("compat-middleware: on_request");
        context::set("compat-middleware", "seen");
        req.headers
            .insert(self.header.clone(), "set-by-middleware".to_string());
        Action::Continue(req)
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        let seen = context::get("compat-middleware").unwrap_or_default();
        resp.with_header("x-compat-on-response", seen)
    }
}
