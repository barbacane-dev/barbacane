//! CORS middleware plugin for Barbacane API gateway.
//!
//! Implements Cross-Origin Resource Sharing (CORS) per the Fetch specification.
//! Handles preflight OPTIONS requests and adds appropriate CORS headers to responses.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// CORS middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct Cors {
    /// Allowed origins. Use ["*"] to allow any origin.
    /// Default: [] (no origins allowed)
    #[serde(default)]
    allowed_origins: Vec<String>,

    /// Allowed HTTP methods.
    /// Default: ["GET", "POST"]
    #[serde(default = "default_methods")]
    allowed_methods: Vec<String>,

    /// Allowed request headers.
    /// Default: [] (only simple headers allowed)
    #[serde(default)]
    allowed_headers: Vec<String>,

    /// Headers to expose to the client.
    /// Default: []
    #[serde(default)]
    expose_headers: Vec<String>,

    /// Max age for preflight cache in seconds.
    /// Default: 3600 (1 hour)
    #[serde(default = "default_max_age")]
    max_age: u32,

    /// Whether to allow credentials (cookies, auth headers).
    /// Default: false
    #[serde(default)]
    allow_credentials: bool,
}

fn default_methods() -> Vec<String> {
    vec!["GET".to_string(), "POST".to_string()]
}

fn default_max_age() -> u32 {
    3600
}

impl Cors {
    /// Handle incoming request - check for preflight and validate origin.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        let origin = match req.headers.get("origin") {
            Some(o) => o.clone(),
            None => {
                // No Origin header - not a CORS request, pass through
                return Action::Continue(req);
            }
        };

        // Validate origin
        if !self.is_origin_allowed(&origin) {
            log_message(2, &format!("CORS: origin not allowed: {}", origin));
            return Action::ShortCircuit(self.forbidden_response(&origin));
        }

        // Check if this is a preflight request
        if req.method.eq_ignore_ascii_case("OPTIONS") {
            if let Some(requested_method) = req.headers.get("access-control-request-method") {
                // This is a preflight request
                return Action::ShortCircuit(
                    self.preflight_response(&origin, requested_method, &req)
                );
            }
        }

        // Regular CORS request - add origin to context for response handling
        let mut modified_req = req;
        modified_req.headers.insert(
            "x-cors-origin".to_string(),
            origin,
        );
        Action::Continue(modified_req)
    }

    /// Add CORS headers to response.
    pub fn on_response(&mut self, mut resp: Response) -> Response {
        // Check if we stored the origin during request processing
        // In a real implementation, we'd use context storage
        // For now, we add headers based on configuration

        // The origin was validated in on_request, add CORS headers
        if self.allowed_origins.contains(&"*".to_string()) && !self.allow_credentials {
            resp.headers.insert(
                "access-control-allow-origin".to_string(),
                "*".to_string(),
            );
        }

        // Expose headers if configured
        if !self.expose_headers.is_empty() {
            resp.headers.insert(
                "access-control-expose-headers".to_string(),
                self.expose_headers.join(", "),
            );
        }

        // Credentials
        if self.allow_credentials {
            resp.headers.insert(
                "access-control-allow-credentials".to_string(),
                "true".to_string(),
            );
        }

        resp
    }

    /// Check if the origin is allowed.
    fn is_origin_allowed(&self, origin: &str) -> bool {
        if self.allowed_origins.is_empty() {
            return false;
        }

        // Wildcard allows any origin (but not with credentials)
        if self.allowed_origins.contains(&"*".to_string()) {
            return true;
        }

        // Check exact match
        self.allowed_origins.iter().any(|allowed| {
            allowed == origin || self.matches_wildcard_origin(allowed, origin)
        })
    }

    /// Check if origin matches a wildcard pattern like "*.example.com".
    fn matches_wildcard_origin(&self, pattern: &str, origin: &str) -> bool {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            // Pattern: *.example.com should match sub.example.com
            if let Some(origin_host) = origin.strip_prefix("https://") {
                return origin_host.ends_with(suffix) && origin_host != suffix;
            }
            if let Some(origin_host) = origin.strip_prefix("http://") {
                return origin_host.ends_with(suffix) && origin_host != suffix;
            }
        }
        false
    }

    /// Check if the requested method is allowed.
    fn is_method_allowed(&self, method: &str) -> bool {
        self.allowed_methods.iter().any(|m| m.eq_ignore_ascii_case(method))
    }

    /// Check if all requested headers are allowed.
    fn are_headers_allowed(&self, requested_headers: &str) -> bool {
        if self.allowed_headers.is_empty() {
            // Only simple headers allowed by default
            return requested_headers.is_empty();
        }

        // Check each requested header
        for header in requested_headers.split(',') {
            let header = header.trim().to_lowercase();
            if header.is_empty() {
                continue;
            }

            // Simple headers are always allowed
            if is_simple_header(&header) {
                continue;
            }

            // Check against allowed headers
            let allowed = self.allowed_headers.iter().any(|h| {
                h.eq_ignore_ascii_case(&header) || h == "*"
            });

            if !allowed {
                return false;
            }
        }
        true
    }

    /// Generate preflight response.
    fn preflight_response(&self, origin: &str, requested_method: &str, req: &Request) -> Response {
        let mut headers = BTreeMap::new();

        // Check if the requested method is allowed
        if !self.is_method_allowed(requested_method) {
            log_message(2, &format!("CORS: method not allowed: {}", requested_method));
            return self.forbidden_response(origin);
        }

        // Check if requested headers are allowed
        let requested_headers = req
            .headers
            .get("access-control-request-headers")
            .map(|h| h.as_str())
            .unwrap_or("");

        if !self.are_headers_allowed(requested_headers) {
            log_message(2, &format!("CORS: headers not allowed: {}", requested_headers));
            return self.forbidden_response(origin);
        }

        // Set origin header
        if self.allowed_origins.contains(&"*".to_string()) && !self.allow_credentials {
            headers.insert("access-control-allow-origin".to_string(), "*".to_string());
        } else {
            headers.insert("access-control-allow-origin".to_string(), origin.to_string());
        }

        // Allow methods
        headers.insert(
            "access-control-allow-methods".to_string(),
            self.allowed_methods.join(", "),
        );

        // Allow headers
        if !self.allowed_headers.is_empty() {
            headers.insert(
                "access-control-allow-headers".to_string(),
                self.allowed_headers.join(", "),
            );
        }

        // Max age
        headers.insert(
            "access-control-max-age".to_string(),
            self.max_age.to_string(),
        );

        // Credentials
        if self.allow_credentials {
            headers.insert(
                "access-control-allow-credentials".to_string(),
                "true".to_string(),
            );
        }

        // Vary header for caching
        headers.insert(
            "vary".to_string(),
            "Origin, Access-Control-Request-Method, Access-Control-Request-Headers".to_string(),
        );

        Response {
            status: 204,
            headers,
            body: None,
        }
    }

    /// Generate forbidden response for invalid CORS request.
    fn forbidden_response(&self, origin: &str) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/problem+json".to_string());
        headers.insert("vary".to_string(), "Origin".to_string());

        let body = serde_json::json!({
            "type": "urn:barbacane:error:cors-not-allowed",
            "title": "CORS Not Allowed",
            "status": 403,
            "detail": format!("Origin '{}' is not allowed by CORS policy", origin)
        });

        Response {
            status: 403,
            headers,
            body: Some(body.to_string()),
        }
    }
}

/// Check if a header is a CORS simple header.
fn is_simple_header(header: &str) -> bool {
    matches!(
        header,
        "accept" | "accept-language" | "content-language" | "content-type"
    )
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
