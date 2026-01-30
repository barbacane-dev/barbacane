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
                    modified_req
                        .headers
                        .insert("x-auth-key-scopes".to_string(), key_entry.scopes.join(","));
                }
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
