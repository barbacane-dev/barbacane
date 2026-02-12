//! Request transformer middleware plugin for Barbacane API gateway.
//!
//! Provides declarative request transformations for:
//! - Headers (add, set, remove, rename)
//! - Query parameters (add, remove, rename)
//! - Path rewriting (strip_prefix, add_prefix, regex replace)
//! - JSON body (add, remove, rename using JSON Pointer - RFC 6901)
//!
//! Supports variable interpolation: $client_ip, $path.<name>, $header.<name>,
//! $query.<name>, context:<key>

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;

mod body;
mod config;
mod headers;
mod interpolation;
mod path;
mod query;

use config::{BodyConfig, HeaderConfig, PathConfig, QueryConfig};

/// Request transformer middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct RequestTransformer {
    /// Header transformations.
    #[serde(default)]
    headers: Option<HeaderConfig>,

    /// Query parameter transformations.
    #[serde(default)]
    querystring: Option<QueryConfig>,

    /// Path rewriting operations.
    #[serde(default)]
    path: Option<PathConfig>,

    /// JSON body transformations (JSON Pointer).
    #[serde(default)]
    body: Option<BodyConfig>,
}

impl RequestTransformer {
    /// Handle incoming request - apply transformations.
    ///
    /// Transformations are applied in this order:
    /// 1. Path (affects routing, must be first)
    /// 2. Headers
    /// 3. Query parameters
    /// 4. Body
    pub fn on_request(&mut self, mut req: Request) -> Action<Request> {
        // Apply path transformations
        if let Some(path_config) = &self.path {
            req.path = path::transform_path(&req.path, path_config);
        }

        // Apply header transformations
        if let Some(header_config) = &self.headers {
            // Clone request for interpolation to avoid borrow checker issues
            let req_clone = req.clone();
            headers::transform_headers(&mut req.headers, header_config, &req_clone);
        }

        // Apply query string transformations
        if let Some(query_config) = &self.querystring {
            req.query = query::transform_query(&req.query, query_config, &req);
        }

        // Apply body transformations
        if let Some(body_config) = &self.body {
            req.body = body::transform_body(&req.body, body_config, &req);
        }

        Action::Continue(req)
    }

    /// Pass through responses unchanged (request-transformer only).
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }
}
