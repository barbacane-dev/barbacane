//! API key authentication middleware plugin for Barbacane API gateway.
//!
//! Validates API keys from headers or query parameters and rejects
//! unauthenticated requests with 401 Unauthorized.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// API key authentication middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct ApiKeyAuth {
    /// Where to extract the API key from.
    /// Options: "header" (default), "query"
    #[serde(default = "default_key_location")]
    key_location: String,

    /// Header name to extract the key from (when key_location is "header").
    /// Default: "X-API-Key"
    #[serde(default = "default_header_name")]
    header_name: String,

    /// Query parameter name to extract the key from (when key_location is "query").
    /// Default: "api_key"
    #[serde(default = "default_query_param")]
    query_param: String,

    /// Map of valid API keys to their metadata.
    /// Key: the API key string
    /// Value: key metadata (id, name, optional scopes)
    #[serde(default)]
    keys: BTreeMap<String, ApiKeyEntry>,
}

/// Metadata for a single API key.
#[derive(Debug, Clone, Deserialize)]
struct ApiKeyEntry {
    /// Unique identifier for this key (for logging/auditing).
    id: String,

    /// Human-readable name for this key.
    #[serde(default)]
    name: Option<String>,

    /// Optional scopes/permissions associated with this key.
    #[serde(default)]
    scopes: Vec<String>,
}

fn default_key_location() -> String {
    "header".to_string()
}

fn default_header_name() -> String {
    "X-API-Key".to_string()
}

fn default_query_param() -> String {
    "api_key".to_string()
}

/// API key validation error.
#[derive(Debug)]
enum ApiKeyError {
    MissingKey,
    InvalidKey,
}

impl ApiKeyError {
    fn as_str(&self) -> &'static str {
        match self {
            ApiKeyError::MissingKey => "missing_key",
            ApiKeyError::InvalidKey => "invalid_key",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            ApiKeyError::MissingKey => "API key required",
            ApiKeyError::InvalidKey => "Invalid API key",
        }
    }
}

impl ApiKeyAuth {
    /// Handle incoming request - validate API key.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        match self.validate_request(&req) {
            Ok(key_entry) => {
                // Add auth context to request headers for downstream use
                let mut modified_req = req;
                modified_req
                    .headers
                    .insert("x-auth-key-id".to_string(), key_entry.id.clone());
                if let Some(name) = &key_entry.name {
                    modified_req
                        .headers
                        .insert("x-auth-key-name".to_string(), name.clone());
                }
                if !key_entry.scopes.is_empty() {
                    let scopes_csv = key_entry.scopes.join(",");
                    modified_req
                        .headers
                        .insert("x-auth-key-scopes".to_string(), scopes_csv.clone());
                    modified_req
                        .headers
                        .insert("x-auth-consumer-groups".to_string(), scopes_csv);
                }

                // Standard consumer header for ACL and downstream middlewares
                modified_req
                    .headers
                    .insert("x-auth-consumer".to_string(), key_entry.id.clone());

                Action::Continue(modified_req)
            }
            Err(e) => Action::ShortCircuit(self.unauthorized_response(&e)),
        }
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Validate the API key in the request.
    fn validate_request(&self, req: &Request) -> Result<&ApiKeyEntry, ApiKeyError> {
        let key = self.extract_key(req)?;
        self.lookup_key(&key)
    }

    /// Extract API key from the configured location.
    fn extract_key(&self, req: &Request) -> Result<String, ApiKeyError> {
        match self.key_location.as_str() {
            "header" => self.extract_from_header(req),
            "query" => self.extract_from_query(req),
            _ => self.extract_from_header(req), // Default to header
        }
    }

    /// Extract API key from header.
    fn extract_from_header(&self, req: &Request) -> Result<String, ApiKeyError> {
        // Try exact match first, then case-insensitive
        let key = req
            .headers
            .get(&self.header_name)
            .or_else(|| req.headers.get(&self.header_name.to_lowercase()))
            .ok_or(ApiKeyError::MissingKey)?;

        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(ApiKeyError::MissingKey);
        }

        Ok(trimmed.to_string())
    }

    /// Extract API key from query parameter.
    fn extract_from_query(&self, req: &Request) -> Result<String, ApiKeyError> {
        // Parse query string if present
        let query = req.query.as_ref().ok_or(ApiKeyError::MissingKey)?;

        // Simple query string parsing: key=value&key2=value2
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == self.query_param {
                    let decoded = url_decode(v);
                    if decoded.is_empty() {
                        return Err(ApiKeyError::MissingKey);
                    }
                    return Ok(decoded);
                }
            }
        }

        Err(ApiKeyError::MissingKey)
    }

    /// Look up the API key in the configured key store.
    fn lookup_key(&self, key: &str) -> Result<&ApiKeyEntry, ApiKeyError> {
        self.keys.get(key).ok_or(ApiKeyError::InvalidKey)
    }

    /// Generate 401 Unauthorized response.
    fn unauthorized_response(&self, error: &ApiKeyError) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());

        // WWW-Authenticate header for API key scheme
        let www_auth = format!(
            "ApiKey realm=\"api\", error=\"{}\", error_description=\"{}\"",
            error.as_str(),
            error.description()
        );
        headers.insert("www-authenticate".to_string(), www_auth);

        let body = serde_json::json!({
            "type": "urn:barbacane:error:authentication-failed",
            "title": "Authentication failed",
            "status": 401,
            "detail": error.description()
        });

        Response {
            status: 401,
            headers,
            body: Some(body.to_string()),
        }
    }
}

/// Simple URL decoding for query parameters.
fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            // Try to read two hex digits
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            // Invalid escape, keep as-is
            result.push('%');
            result.push_str(&hex);
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_plugin() -> ApiKeyAuth {
        serde_json::from_value(serde_json::json!({
            "keys": {
                "sk-test-123": { "id": "key1", "name": "Test Key", "scopes": ["read", "write"] },
                "sk-readonly": { "id": "key2", "scopes": ["read"] },
                "sk-noname": { "id": "key3" }
            }
        }))
        .unwrap()
    }

    fn request_with_headers(headers: Vec<(&str, &str)>) -> Request {
        let mut h = BTreeMap::new();
        for (k, v) in headers {
            h.insert(k.to_string(), v.to_string());
        }
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: h,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    fn request_with_query(query: &str) -> Request {
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: Some(query.to_string()),
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    // --- extract_from_header ---

    #[test]
    fn test_extract_from_header_exact_match() {
        let plugin = test_plugin();
        let req = request_with_headers(vec![("X-API-Key", "sk-test-123")]);
        assert_eq!(plugin.extract_from_header(&req).unwrap(), "sk-test-123");
    }

    #[test]
    fn test_extract_from_header_lowercase() {
        let plugin = test_plugin();
        let req = request_with_headers(vec![("x-api-key", "sk-test-123")]);
        assert_eq!(plugin.extract_from_header(&req).unwrap(), "sk-test-123");
    }

    #[test]
    fn test_extract_from_header_trims_whitespace() {
        let plugin = test_plugin();
        let req = request_with_headers(vec![("X-API-Key", "  sk-test-123  ")]);
        assert_eq!(plugin.extract_from_header(&req).unwrap(), "sk-test-123");
    }

    #[test]
    fn test_extract_from_header_missing() {
        let plugin = test_plugin();
        let req = request_with_headers(vec![]);
        assert!(matches!(
            plugin.extract_from_header(&req),
            Err(ApiKeyError::MissingKey)
        ));
    }

    #[test]
    fn test_extract_from_header_empty_value() {
        let plugin = test_plugin();
        let req = request_with_headers(vec![("X-API-Key", "  ")]);
        assert!(matches!(
            plugin.extract_from_header(&req),
            Err(ApiKeyError::MissingKey)
        ));
    }

    // --- extract_from_query ---

    #[test]
    fn test_extract_from_query_found() {
        let plugin = test_plugin();
        let req = request_with_query("api_key=sk-test-123&other=val");
        assert_eq!(plugin.extract_from_query(&req).unwrap(), "sk-test-123");
    }

    #[test]
    fn test_extract_from_query_url_encoded() {
        let plugin = test_plugin();
        let req = request_with_query("api_key=sk%2Dtest%2D123");
        assert_eq!(plugin.extract_from_query(&req).unwrap(), "sk-test-123");
    }

    #[test]
    fn test_extract_from_query_missing_param() {
        let plugin = test_plugin();
        let req = request_with_query("other=val");
        assert!(matches!(
            plugin.extract_from_query(&req),
            Err(ApiKeyError::MissingKey)
        ));
    }

    #[test]
    fn test_extract_from_query_no_query_string() {
        let plugin = test_plugin();
        let req = request_with_headers(vec![]);
        assert!(matches!(
            plugin.extract_from_query(&req),
            Err(ApiKeyError::MissingKey)
        ));
    }

    #[test]
    fn test_extract_from_query_empty_value() {
        let plugin = test_plugin();
        let req = request_with_query("api_key=");
        assert!(matches!(
            plugin.extract_from_query(&req),
            Err(ApiKeyError::MissingKey)
        ));
    }

    // --- lookup_key ---

    #[test]
    fn test_lookup_key_valid() {
        let plugin = test_plugin();
        let entry = plugin.lookup_key("sk-test-123").unwrap();
        assert_eq!(entry.id, "key1");
        assert_eq!(entry.name.as_deref(), Some("Test Key"));
    }

    #[test]
    fn test_lookup_key_invalid() {
        let plugin = test_plugin();
        assert!(matches!(
            plugin.lookup_key("sk-nonexistent"),
            Err(ApiKeyError::InvalidKey)
        ));
    }

    // --- on_request ---

    #[test]
    fn test_on_request_valid_key_injects_headers() {
        let mut plugin = test_plugin();
        let req = request_with_headers(vec![("X-API-Key", "sk-test-123")]);
        match plugin.on_request(req) {
            Action::Continue(r) => {
                assert_eq!(r.headers.get("x-auth-key-id").unwrap(), "key1");
                assert_eq!(r.headers.get("x-auth-key-name").unwrap(), "Test Key");
                assert_eq!(r.headers.get("x-auth-key-scopes").unwrap(), "read,write");
                assert_eq!(r.headers.get("x-auth-consumer").unwrap(), "key1");
                assert_eq!(
                    r.headers.get("x-auth-consumer-groups").unwrap(),
                    "read,write"
                );
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn test_on_request_key_without_name_or_scopes() {
        let mut plugin = test_plugin();
        let req = request_with_headers(vec![("X-API-Key", "sk-noname")]);
        match plugin.on_request(req) {
            Action::Continue(r) => {
                assert_eq!(r.headers.get("x-auth-key-id").unwrap(), "key3");
                assert!(r.headers.get("x-auth-key-name").is_none());
                assert!(r.headers.get("x-auth-key-scopes").is_none());
                assert_eq!(r.headers.get("x-auth-consumer").unwrap(), "key3");
                assert!(!r.headers.contains_key("x-auth-consumer-groups"));
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn test_on_request_missing_key_returns_401() {
        let mut plugin = test_plugin();
        let req = request_with_headers(vec![]);
        match plugin.on_request(req) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 401);
                assert!(r.headers.get("www-authenticate").unwrap().contains("missing_key"));
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_on_request_invalid_key_returns_401() {
        let mut plugin = test_plugin();
        let req = request_with_headers(vec![("X-API-Key", "wrong-key")]);
        match plugin.on_request(req) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 401);
                assert!(r.headers.get("www-authenticate").unwrap().contains("invalid_key"));
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_on_request_query_location() {
        let mut plugin: ApiKeyAuth = serde_json::from_value(serde_json::json!({
            "key_location": "query",
            "keys": { "mykey": { "id": "q1" } }
        }))
        .unwrap();
        let req = request_with_query("api_key=mykey");
        match plugin.on_request(req) {
            Action::Continue(r) => assert_eq!(r.headers.get("x-auth-key-id").unwrap(), "q1"),
            _ => panic!("expected Continue"),
        }
    }

    // --- unauthorized_response ---

    #[test]
    fn test_unauthorized_response_format() {
        let plugin = test_plugin();
        let resp = plugin.unauthorized_response(&ApiKeyError::MissingKey);
        assert_eq!(resp.status, 401);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/json"
        );
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:authentication-failed");
        assert_eq!(body["status"], 401);
    }

    // --- url_decode ---

    #[test]
    fn test_url_decode_percent_encoding() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("a%2Fb"), "a/b");
    }

    #[test]
    fn test_url_decode_plus_as_space() {
        assert_eq!(url_decode("hello+world"), "hello world");
    }

    #[test]
    fn test_url_decode_no_encoding() {
        assert_eq!(url_decode("plain-text"), "plain-text");
    }

    #[test]
    fn test_url_decode_invalid_escape() {
        assert_eq!(url_decode("a%ZZb"), "a%ZZb");
    }

    // --- config deserialization ---

    #[test]
    fn test_config_defaults() {
        let plugin: ApiKeyAuth = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(plugin.key_location, "header");
        assert_eq!(plugin.header_name, "X-API-Key");
        assert_eq!(plugin.query_param, "api_key");
        assert!(plugin.keys.is_empty());
    }

    #[test]
    fn test_config_custom_header() {
        let plugin: ApiKeyAuth = serde_json::from_value(serde_json::json!({
            "header_name": "Authorization",
            "keys": { "Bearer tok": { "id": "t1" } }
        }))
        .unwrap();
        assert_eq!(plugin.header_name, "Authorization");
    }

    // --- on_response passthrough ---

    #[test]
    fn test_on_response_passthrough() {
        let mut plugin = test_plugin();
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some("ok".to_string()),
        };
        let result = plugin.on_response(resp);
        assert_eq!(result.status, 200);
        assert_eq!(result.body.as_deref(), Some("ok"));
    }
}
