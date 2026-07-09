//! CORS middleware plugin for Barbacane API gateway.
//!
//! Implements Cross-Origin Resource Sharing (CORS) per the Fetch specification.
//! Handles preflight OPTIONS requests and adds appropriate CORS headers to responses.

use barbacane_plugin_sdk::log::log as log_message;
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

        // A literal `*` with credentials is invalid per the Fetch spec; warn so
        // the misconfiguration is visible (behavior already fails closed above).
        if self.allow_credentials && self.allowed_origins.iter().any(|o| o == "*") {
            log_message(
                3,
                "CORS: allowed_origins contains '*' with allow_credentials=true; \
                 the wildcard is ignored. List explicit origins instead.",
            );
        }

        // Validate origin
        if !self.is_origin_allowed(&origin) {
            log_message(2, &format!("CORS: origin not allowed: {}", origin));
            return Action::ShortCircuit(self.forbidden_response(&origin));
        }

        // Check if this is a preflight request
        if req.method.eq_ignore_ascii_case("OPTIONS") {
            if let Some(requested_method) = req.headers.get("access-control-request-method") {
                // This is a preflight request
                return Action::ShortCircuit(self.preflight_response(
                    &origin,
                    requested_method,
                    &req,
                ));
            }
        }

        // Regular CORS request: stash the validated origin in request context so
        // on_response can reflect it (the response itself carries no Origin).
        context_set("cors.origin", &origin);
        Action::Continue(req)
    }

    /// Add CORS headers to response.
    pub fn on_response(&mut self, mut resp: Response) -> Response {
        if self.wildcard_active() {
            // Non-credentialed wildcard: a constant `*` works without the origin.
            resp.headers
                .insert("access-control-allow-origin".to_string(), "*".to_string());
        } else if let Some(origin) = context_get("cors.origin") {
            // Reflect the specific origin validated in on_request, and Vary on it
            // so caches don't serve one origin's response to another.
            resp.headers.insert(
                "access-control-allow-origin".to_string(),
                self.allow_origin_value(&origin),
            );
            resp.headers
                .insert("vary".to_string(), "Origin".to_string());
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

    /// Whether a literal `*` wildcard is in effect. Per the Fetch spec, `*` is
    /// incompatible with credentials, so when `allow_credentials` is set the
    /// wildcard is ignored entirely and only explicit origins may match.
    fn wildcard_active(&self) -> bool {
        !self.allow_credentials && self.allowed_origins.iter().any(|o| o == "*")
    }

    /// Check if the origin is allowed.
    fn is_origin_allowed(&self, origin: &str) -> bool {
        if self.allowed_origins.is_empty() {
            return false;
        }

        // `*` allows any origin, but never together with credentials.
        if self.wildcard_active() {
            return true;
        }

        // Otherwise require an explicit exact or suffix-wildcard match. Note a
        // bare "*" entry does NOT match here, so `["*"]` + credentials denies
        // rather than reflecting an arbitrary origin with credentials.
        self.allowed_origins
            .iter()
            .filter(|allowed| *allowed != "*")
            .any(|allowed| allowed == origin || self.matches_wildcard_origin(allowed, origin))
    }

    /// The value to emit in `Access-Control-Allow-Origin` for an already-validated
    /// origin: `*` only in non-credentialed wildcard mode, otherwise the specific
    /// origin echoed back (required when credentials are allowed).
    fn allow_origin_value(&self, origin: &str) -> String {
        if self.wildcard_active() {
            "*".to_string()
        } else {
            origin.to_string()
        }
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
        self.allowed_methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case(method))
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
            let allowed = self
                .allowed_headers
                .iter()
                .any(|h| h.eq_ignore_ascii_case(&header) || h == "*");

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
            log_message(
                2,
                &format!("CORS: method not allowed: {}", requested_method),
            );
            return self.forbidden_response(origin);
        }

        // Check if requested headers are allowed
        let requested_headers = req
            .headers
            .get("access-control-request-headers")
            .map(|h| h.as_str())
            .unwrap_or("");

        if !self.are_headers_allowed(requested_headers) {
            log_message(
                2,
                &format!("CORS: headers not allowed: {}", requested_headers),
            );
            return self.forbidden_response(origin);
        }

        // Set origin header (never `*` when credentials are allowed)
        headers.insert(
            "access-control-allow-origin".to_string(),
            self.allow_origin_value(origin),
        );

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
        let mut resp = ProblemDetails::new(
            403,
            "urn:barbacane:error:cors-not-allowed",
            "CORS Not Allowed",
        )
        .detail(format!("Origin '{}' is not allowed by CORS policy", origin))
        .into_response();
        resp.headers
            .insert("vary".to_string(), "Origin".to_string());
        resp
    }
}

/// Check if a header is a CORS simple header.
fn is_simple_header(header: &str) -> bool {
    matches!(
        header,
        "accept" | "accept-language" | "content-language" | "content-type"
    )
}

/// Store a value in the request context (WASM).
#[cfg(target_arch = "wasm32")]
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

/// Get a value from the request context (WASM).
#[cfg(target_arch = "wasm32")]
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

#[cfg(not(target_arch = "wasm32"))]
fn context_set(key: &str, value: &str) {
    mock_host::context_set(key, value)
}

#[cfg(not(target_arch = "wasm32"))]
fn context_get(key: &str) -> Option<String> {
    mock_host::context_get(key)
}

/// Native mock context for tests.
#[cfg(not(target_arch = "wasm32"))]
mod mock_host {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static CONTEXT: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    }

    pub fn context_set(key: &str, value: &str) {
        CONTEXT.with(|c| c.borrow_mut().insert(key.to_string(), value.to_string()));
    }

    pub fn context_get(key: &str) -> Option<String> {
        CONTEXT.with(|c| c.borrow().get(key).cloned())
    }

    #[cfg(test)]
    pub fn reset() {
        CONTEXT.with(|c| c.borrow_mut().clear());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_cors(allowed_origins: Vec<String>) -> Cors {
        Cors {
            allowed_origins,
            allowed_methods: vec!["GET".to_string(), "POST".to_string()],
            allowed_headers: vec![],
            expose_headers: vec![],
            max_age: 3600,
            allow_credentials: false,
        }
    }

    fn create_request(method: &str, origin: Option<&str>) -> Request {
        let mut headers = BTreeMap::new();
        if let Some(o) = origin {
            headers.insert("origin".to_string(), o.to_string());
        }

        Request {
            method: method.to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    #[test]
    fn test_is_origin_allowed_exact_match() {
        let cors = create_test_cors(vec!["https://example.com".to_string()]);
        assert!(cors.is_origin_allowed("https://example.com"));
        assert!(!cors.is_origin_allowed("https://other.com"));
    }

    #[test]
    fn test_is_origin_allowed_wildcard() {
        let cors = create_test_cors(vec!["*".to_string()]);
        assert!(cors.is_origin_allowed("https://example.com"));
        assert!(cors.is_origin_allowed("https://any-domain.com"));
        assert!(cors.is_origin_allowed("http://localhost:3000"));
    }

    #[test]
    fn test_is_origin_allowed_multiple_origins() {
        let cors = create_test_cors(vec![
            "https://example.com".to_string(),
            "https://test.com".to_string(),
        ]);
        assert!(cors.is_origin_allowed("https://example.com"));
        assert!(cors.is_origin_allowed("https://test.com"));
        assert!(!cors.is_origin_allowed("https://other.com"));
    }

    #[test]
    fn test_is_origin_allowed_empty_list() {
        let cors = create_test_cors(vec![]);
        assert!(!cors.is_origin_allowed("https://example.com"));
        assert!(!cors.is_origin_allowed("*"));
    }

    #[test]
    fn test_matches_wildcard_origin_https() {
        let cors = create_test_cors(vec!["*.example.com".to_string()]);
        assert!(cors.matches_wildcard_origin("*.example.com", "https://sub.example.com"));
        assert!(cors.matches_wildcard_origin("*.example.com", "https://api.example.com"));
        assert!(!cors.matches_wildcard_origin("*.example.com", "https://example.com"));
        assert!(!cors.matches_wildcard_origin("*.example.com", "https://other.com"));
    }

    #[test]
    fn test_matches_wildcard_origin_http() {
        let cors = create_test_cors(vec!["*.example.com".to_string()]);
        assert!(cors.matches_wildcard_origin("*.example.com", "http://sub.example.com"));
        assert!(cors.matches_wildcard_origin("*.example.com", "http://api.example.com"));
        assert!(!cors.matches_wildcard_origin("*.example.com", "http://example.com"));
    }

    #[test]
    fn test_matches_wildcard_origin_no_wildcard() {
        let cors = create_test_cors(vec!["https://example.com".to_string()]);
        assert!(!cors.matches_wildcard_origin("https://example.com", "https://sub.example.com"));
    }

    #[test]
    fn test_is_origin_allowed_with_wildcard_pattern() {
        let cors = create_test_cors(vec!["*.example.com".to_string()]);
        assert!(cors.is_origin_allowed("https://sub.example.com"));
        assert!(cors.is_origin_allowed("http://api.example.com"));
        assert!(!cors.is_origin_allowed("https://example.com"));
        assert!(!cors.is_origin_allowed("https://other.com"));
    }

    #[test]
    fn test_is_method_allowed_case_insensitive() {
        let cors = create_test_cors(vec!["*".to_string()]);
        assert!(cors.is_method_allowed("GET"));
        assert!(cors.is_method_allowed("get"));
        assert!(cors.is_method_allowed("Get"));
        assert!(cors.is_method_allowed("POST"));
        assert!(cors.is_method_allowed("post"));
        assert!(!cors.is_method_allowed("DELETE"));
    }

    #[test]
    fn test_is_method_allowed_custom_methods() {
        let mut cors = create_test_cors(vec!["*".to_string()]);
        cors.allowed_methods = vec!["GET".to_string(), "POST".to_string(), "PUT".to_string()];
        assert!(cors.is_method_allowed("PUT"));
        assert!(cors.is_method_allowed("put"));
        assert!(!cors.is_method_allowed("DELETE"));
    }

    #[test]
    fn test_are_headers_allowed_empty_config() {
        let cors = create_test_cors(vec!["*".to_string()]);
        assert!(cors.are_headers_allowed(""));
        assert!(!cors.are_headers_allowed("x-custom-header"));
    }

    #[test]
    fn test_are_headers_allowed_simple_headers() {
        let mut cors = create_test_cors(vec!["*".to_string()]);
        cors.allowed_headers = vec!["x-custom".to_string()];
        assert!(cors.are_headers_allowed("accept"));
        assert!(cors.are_headers_allowed("content-type"));
        assert!(cors.are_headers_allowed("accept-language"));
        assert!(cors.are_headers_allowed("content-language"));
    }

    #[test]
    fn test_are_headers_allowed_custom_headers() {
        let mut cors = create_test_cors(vec!["*".to_string()]);
        cors.allowed_headers = vec!["x-custom".to_string(), "authorization".to_string()];
        assert!(cors.are_headers_allowed("x-custom"));
        assert!(cors.are_headers_allowed("X-Custom"));
        assert!(cors.are_headers_allowed("authorization"));
        assert!(!cors.are_headers_allowed("x-other"));
    }

    #[test]
    fn test_are_headers_allowed_wildcard() {
        let mut cors = create_test_cors(vec!["*".to_string()]);
        cors.allowed_headers = vec!["*".to_string()];
        assert!(cors.are_headers_allowed("x-custom-header"));
        assert!(cors.are_headers_allowed("authorization"));
        assert!(cors.are_headers_allowed("any-header"));
    }

    #[test]
    fn test_are_headers_allowed_multiple_requested() {
        let mut cors = create_test_cors(vec!["*".to_string()]);
        cors.allowed_headers = vec!["x-custom".to_string(), "authorization".to_string()];
        assert!(cors.are_headers_allowed("x-custom, authorization"));
        assert!(cors.are_headers_allowed("x-custom, accept"));
        assert!(!cors.are_headers_allowed("x-custom, x-other"));
    }

    #[test]
    fn test_is_simple_header() {
        assert!(is_simple_header("accept"));
        assert!(is_simple_header("accept-language"));
        assert!(is_simple_header("content-language"));
        assert!(is_simple_header("content-type"));
        assert!(!is_simple_header("authorization"));
        assert!(!is_simple_header("x-custom"));
    }

    #[test]
    fn test_forbidden_response_format() {
        let cors = create_test_cors(vec!["https://allowed.com".to_string()]);
        let response = cors.forbidden_response("https://not-allowed.com");

        assert_eq!(response.status, 403);
        assert_eq!(
            response.headers.get("content-type"),
            Some(&"application/problem+json".to_string())
        );
        assert_eq!(response.headers.get("vary"), Some(&"Origin".to_string()));

        let body = String::from_utf8(response.body.expect("Response should have a body")).unwrap();
        assert!(body.contains("urn:barbacane:error:cors-not-allowed"));
        assert!(body.contains("CORS Not Allowed"));
        assert!(body.contains("403"));
        assert!(body.contains("https://not-allowed.com"));
    }

    #[test]
    fn test_on_request_no_origin_passthrough() {
        let mut cors = create_test_cors(vec!["https://example.com".to_string()]);
        let req = create_request("GET", None);

        match cors.on_request(req) {
            Action::Continue(returned_req) => {
                assert_eq!(returned_req.method, "GET");
                assert_eq!(returned_req.path, "/test");
            }
            Action::ShortCircuit(_) => panic!("Expected Continue, got ShortCircuit"),
        }
    }

    #[test]
    fn test_on_request_allowed_origin() {
        mock_host::reset();
        let mut cors = create_test_cors(vec!["https://example.com".to_string()]);
        let req = create_request("GET", Some("https://example.com"));

        match cors.on_request(req) {
            Action::Continue(returned_req) => {
                assert_eq!(returned_req.method, "GET");
                // The validated origin is stashed in context for on_response.
                assert_eq!(
                    context_get("cors.origin"),
                    Some("https://example.com".to_string())
                );
            }
            Action::ShortCircuit(_) => panic!("Expected Continue, got ShortCircuit"),
        }
    }

    // PL-3 regression: `*` with credentials must NOT allow an arbitrary origin.
    #[test]
    fn wildcard_with_credentials_denies_arbitrary_origin() {
        let mut cors = create_test_cors(vec!["*".to_string()]);
        cors.allow_credentials = true;
        assert!(!cors.is_origin_allowed("https://evil.com"));
        match cors.on_request(create_request("GET", Some("https://evil.com"))) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            Action::Continue(_) => panic!("wildcard+credentials must not allow arbitrary origins"),
        }
    }

    // With credentials, an explicitly-listed origin is allowed and echoed back
    // specifically (never `*`).
    #[test]
    fn credentials_echo_specific_origin_on_response() {
        mock_host::reset();
        let mut cors = create_test_cors(vec!["https://app.example.com".to_string()]);
        cors.allow_credentials = true;
        let _ = cors.on_request(create_request("GET", Some("https://app.example.com")));
        let resp = cors.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });
        assert_eq!(
            resp.headers.get("access-control-allow-origin"),
            Some(&"https://app.example.com".to_string())
        );
        assert_eq!(resp.headers.get("vary"), Some(&"Origin".to_string()));
        assert_eq!(
            resp.headers.get("access-control-allow-credentials"),
            Some(&"true".to_string())
        );
    }

    #[test]
    fn test_on_request_disallowed_origin() {
        let mut cors = create_test_cors(vec!["https://example.com".to_string()]);
        let req = create_request("GET", Some("https://evil.com"));

        match cors.on_request(req) {
            Action::ShortCircuit(response) => {
                assert_eq!(response.status, 403);
                let body =
                    String::from_utf8(response.body.expect("Response should have a body")).unwrap();
                assert!(body.contains("https://evil.com"));
            }
            Action::Continue(_) => panic!("Expected ShortCircuit, got Continue"),
        }
    }

    #[test]
    fn test_on_request_preflight_valid_method() {
        let mut cors = create_test_cors(vec!["https://example.com".to_string()]);
        let mut req = create_request("OPTIONS", Some("https://example.com"));
        req.headers.insert(
            "access-control-request-method".to_string(),
            "POST".to_string(),
        );

        match cors.on_request(req) {
            Action::ShortCircuit(response) => {
                assert_eq!(response.status, 204);
                assert_eq!(
                    response.headers.get("access-control-allow-origin"),
                    Some(&"https://example.com".to_string())
                );
                assert!(response
                    .headers
                    .contains_key("access-control-allow-methods"));
            }
            Action::Continue(_) => panic!("Expected ShortCircuit, got Continue"),
        }
    }

    #[test]
    fn test_on_request_preflight_invalid_method() {
        let mut cors = create_test_cors(vec!["https://example.com".to_string()]);
        let mut req = create_request("OPTIONS", Some("https://example.com"));
        req.headers.insert(
            "access-control-request-method".to_string(),
            "DELETE".to_string(),
        );

        match cors.on_request(req) {
            Action::ShortCircuit(response) => {
                assert_eq!(response.status, 403);
            }
            Action::Continue(_) => panic!("Expected ShortCircuit, got Continue"),
        }
    }

    #[test]
    fn test_on_request_preflight_with_headers() {
        let mut cors = create_test_cors(vec!["https://example.com".to_string()]);
        cors.allowed_headers = vec!["authorization".to_string()];
        let mut req = create_request("OPTIONS", Some("https://example.com"));
        req.headers.insert(
            "access-control-request-method".to_string(),
            "POST".to_string(),
        );
        req.headers.insert(
            "access-control-request-headers".to_string(),
            "authorization".to_string(),
        );

        match cors.on_request(req) {
            Action::ShortCircuit(response) => {
                assert_eq!(response.status, 204);
                assert_eq!(
                    response.headers.get("access-control-allow-headers"),
                    Some(&"authorization".to_string())
                );
            }
            Action::Continue(_) => panic!("Expected ShortCircuit, got Continue"),
        }
    }

    #[test]
    fn test_on_response_wildcard_origin() {
        mock_host::reset();
        let mut cors = create_test_cors(vec!["*".to_string()]);
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"test".to_vec()),
        };

        let modified = cors.on_response(response);
        assert_eq!(
            modified.headers.get("access-control-allow-origin"),
            Some(&"*".to_string())
        );
    }

    #[test]
    fn test_on_response_with_expose_headers() {
        let mut cors = create_test_cors(vec!["*".to_string()]);
        cors.expose_headers = vec!["x-custom".to_string(), "x-other".to_string()];
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"test".to_vec()),
        };

        let modified = cors.on_response(response);
        assert_eq!(
            modified.headers.get("access-control-expose-headers"),
            Some(&"x-custom, x-other".to_string())
        );
    }

    #[test]
    fn test_on_response_with_credentials() {
        let mut cors = create_test_cors(vec!["https://example.com".to_string()]);
        cors.allow_credentials = true;
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"test".to_vec()),
        };

        let modified = cors.on_response(response);
        assert_eq!(
            modified.headers.get("access-control-allow-credentials"),
            Some(&"true".to_string())
        );
    }

    #[test]
    fn test_on_response_wildcard_with_credentials_no_wildcard_header() {
        mock_host::reset();
        let mut cors = create_test_cors(vec!["*".to_string()]);
        cors.allow_credentials = true;
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"test".to_vec()),
        };

        let modified = cors.on_response(response);
        // Should not add wildcard ACAO when credentials are enabled
        assert!(!modified.headers.contains_key("access-control-allow-origin"));
        assert_eq!(
            modified.headers.get("access-control-allow-credentials"),
            Some(&"true".to_string())
        );
    }

    #[test]
    fn test_preflight_response_format() {
        let cors = create_test_cors(vec!["https://example.com".to_string()]);
        let req = create_request("OPTIONS", Some("https://example.com"));
        let response = cors.preflight_response("https://example.com", "GET", &req);

        assert_eq!(response.status, 204);
        assert_eq!(response.body, None);
        assert_eq!(
            response.headers.get("access-control-allow-origin"),
            Some(&"https://example.com".to_string())
        );
        assert_eq!(
            response.headers.get("access-control-allow-methods"),
            Some(&"GET, POST".to_string())
        );
        assert_eq!(
            response.headers.get("access-control-max-age"),
            Some(&"3600".to_string())
        );
        assert_eq!(
            response.headers.get("vary"),
            Some(
                &"Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
                    .to_string()
            )
        );
    }

    #[test]
    fn test_preflight_response_with_wildcard() {
        let cors = create_test_cors(vec!["*".to_string()]);
        let req = create_request("OPTIONS", Some("https://example.com"));
        let response = cors.preflight_response("https://example.com", "GET", &req);

        assert_eq!(
            response.headers.get("access-control-allow-origin"),
            Some(&"*".to_string())
        );
    }

    #[test]
    fn test_preflight_response_with_credentials() {
        let mut cors = create_test_cors(vec!["https://example.com".to_string()]);
        cors.allow_credentials = true;
        let req = create_request("OPTIONS", Some("https://example.com"));
        let response = cors.preflight_response("https://example.com", "GET", &req);

        assert_eq!(
            response.headers.get("access-control-allow-credentials"),
            Some(&"true".to_string())
        );
        // Should use specific origin, not wildcard
        assert_eq!(
            response.headers.get("access-control-allow-origin"),
            Some(&"https://example.com".to_string())
        );
    }

    #[test]
    fn test_config_deserialization_defaults() {
        let json = r#"{"allowed_origins": ["https://example.com"]}"#;
        let cors: Cors = serde_json::from_str(json).expect("Failed to deserialize");

        assert_eq!(cors.allowed_origins, vec!["https://example.com"]);
        assert_eq!(cors.allowed_methods, vec!["GET", "POST"]);
        assert_eq!(cors.allowed_headers, Vec::<String>::new());
        assert_eq!(cors.expose_headers, Vec::<String>::new());
        assert_eq!(cors.max_age, 3600);
        assert!(!cors.allow_credentials);
    }

    #[test]
    fn test_config_deserialization_full() {
        let json = r#"{
            "allowed_origins": ["https://example.com", "https://test.com"],
            "allowed_methods": ["GET", "POST", "PUT"],
            "allowed_headers": ["authorization", "x-custom"],
            "expose_headers": ["x-response-time"],
            "max_age": 7200,
            "allow_credentials": true
        }"#;
        let cors: Cors = serde_json::from_str(json).expect("Failed to deserialize");

        assert_eq!(
            cors.allowed_origins,
            vec!["https://example.com", "https://test.com"]
        );
        assert_eq!(cors.allowed_methods, vec!["GET", "POST", "PUT"]);
        assert_eq!(cors.allowed_headers, vec!["authorization", "x-custom"]);
        assert_eq!(cors.expose_headers, vec!["x-response-time"]);
        assert_eq!(cors.max_age, 7200);
        assert!(cors.allow_credentials);
    }

    #[test]
    fn test_config_deserialization_minimal() {
        let json = r#"{"allowed_origins": []}"#;
        let cors: Cors = serde_json::from_str(json).expect("Failed to deserialize");

        assert_eq!(cors.allowed_origins, Vec::<String>::new());
        assert_eq!(cors.allowed_methods, vec!["GET", "POST"]);
        assert_eq!(cors.max_age, 3600);
    }
}
