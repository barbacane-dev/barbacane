//! SDK for building Barbacane WASM plugins.
//!
//! Provides `Request`, `Response`, and `Action` types, along with
//! `#[barbacane_middleware]` and `#[barbacane_dispatcher]` macros
//! that generate the required WASM export glue.
//!
//! # Example
//!
//! ```ignore
//! use barbacane_plugin_sdk::prelude::*;
//!
//! #[barbacane_middleware]
//! #[derive(serde::Deserialize)]
//! struct RateLimiter {
//!     quota: u32,
//!     window: u32,
//! }
//!
//! impl RateLimiter {
//!     fn on_request(&mut self, req: Request) -> Action<Request> {
//!         Action::Continue(req)
//!     }
//!
//!     fn on_response(&mut self, resp: Response) -> Response {
//!         resp
//!     }
//! }
//! ```

pub mod body;
pub mod errors;
pub mod http;
pub mod jwt;
pub mod log;
pub mod types;

/// Re-export proc macros for plugin development.
pub use barbacane_plugin_macros::{barbacane_dispatcher, barbacane_middleware};

pub mod prelude {
    pub use crate::body::*;
    pub use crate::errors::ProblemDetails;
    pub use crate::types::*;
    pub use crate::{barbacane_dispatcher, barbacane_middleware};
    // `http`, `jwt`, and `log` are used via their module path (e.g. `log::warn`,
    // `jwt::Audience`, `http::call`) to keep the prelude unambiguous.
}
