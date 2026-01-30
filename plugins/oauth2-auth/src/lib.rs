//! OAuth2 token introspection middleware plugin for Barbacane API gateway.
//!
//! Validates Bearer tokens via RFC 7662 token introspection and rejects
//! unauthenticated requests with 401 Unauthorized or 403 Forbidden.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// OAuth2 token introspection middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct OAuth2Auth {
    /// Token introspection endpoint URL (RFC 7662).
    introspection_endpoint: String,

    /// Client ID for introspection request.
    client_id: String,

    /// Client secret for introspection request.
    client_secret: String,

    /// Required scopes (space-separated). If set, token must have all these scopes.
    #[serde(default)]
    required_scopes: Option<String>,

    /// Request timeout in seconds for introspection call.
    #[serde(default = "default_timeout")]
    timeout: f64,
}

fn default_timeout() -> f64 {
    5.0 // 5 seconds default for auth calls
}

/// HTTP request for host_http_call.
#[derive(Serialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
    timeout_ms: Option<u64>,
}

/// HTTP response from host_http_call.
#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    #[allow(dead_code)]
    headers: BTreeMap<String, String>,
    body: Option<Vec<u8>>,
}

/// RFC 7662 Token Introspection Response.
#[derive(Debug, Deserialize, Serialize)]
struct IntrospectionResponse {
    /// REQUIRED. Whether the token is active.
    active: bool,

    /// OPTIONAL. Space-separated list of scopes.
    #[serde(default)]
    scope: Option<String>,

    /// OPTIONAL. Client identifier for the token.
    #[serde(default)]
    client_id: Option<String>,

    /// OPTIONAL. Human-readable identifier for the resource owner.
    #[serde(default)]
    username: Option<String>,

    /// OPTIONAL. Type of the token (e.g., "Bearer").
    #[serde(default)]
    token_type: Option<String>,

    /// OPTIONAL. Expiration time (Unix timestamp).
    #[serde(default)]
    exp: Option<u64>,

    /// OPTIONAL. Issued at time (Unix timestamp).
    #[serde(default)]
    iat: Option<u64>,

    /// OPTIONAL. Not before time (Unix timestamp).
    #[serde(default)]
    nbf: Option<u64>,

    /// OPTIONAL. Subject of the token.
    #[serde(default)]
    sub: Option<String>,

    /// OPTIONAL. Audience of the token.
    #[serde(default)]
    aud: Option<serde_json::Value>,

    /// OPTIONAL. Issuer of the token.
    #[serde(default)]
    iss: Option<String>,

    /// OPTIONAL. JWT ID.
    #[serde(default)]
    jti: Option<String>,
}

/// OAuth2 validation error.
#[derive(Debug)]
enum OAuth2Error {
    MissingToken,
    InvalidAuthHeader,
    IntrospectionFailed(String),
    TokenInactive,
    InsufficientScope,
}

impl OAuth2Error {
    fn as_str(&self) -> &'static str {
        match self {
            OAuth2Error::MissingToken => "missing_token",
            OAuth2Error::InvalidAuthHeader => "invalid_request",
            OAuth2Error::IntrospectionFailed(_) => "server_error",
            OAuth2Error::TokenInactive => "invalid_token",
            OAuth2Error::InsufficientScope => "insufficient_scope",
        }
    }

    fn description(&self) -> String {
        match self {
            OAuth2Error::MissingToken => "Bearer token required".to_string(),
            OAuth2Error::InvalidAuthHeader => "Invalid Authorization header format".to_string(),
            OAuth2Error::IntrospectionFailed(msg) => format!("Token introspection failed: {}", msg),
            OAuth2Error::TokenInactive => "Token is not active".to_string(),
            OAuth2Error::InsufficientScope => "Token does not have required scopes".to_string(),
        }
    }

    fn status_code(&self) -> u16 {
        match self {
            OAuth2Error::InsufficientScope => 403,
            _ => 401,
        }
    }
}

impl OAuth2Auth {
    /// Handle incoming request - validate OAuth2 token via introspection.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        match self.validate_request(&req) {
            Ok(introspection) => {
                // Add auth context to request headers for downstream use
                let mut modified_req = req;

                if let Some(sub) = &introspection.sub {
                    modified_req
                        .headers
                        .insert("x-auth-sub".to_string(), sub.clone());
                }

                if let Some(scope) = &introspection.scope {
                    modified_req
                        .headers
                        .insert("x-auth-scope".to_string(), scope.clone());
                }

                if let Some(client_id) = &introspection.client_id {
                    modified_req
                        .headers
                        .insert("x-auth-client-id".to_string(), client_id.clone());
                }

                if let Some(username) = &introspection.username {
                    modified_req
                        .headers
                        .insert("x-auth-username".to_string(), username.clone());
                }

                // Serialize full introspection response for downstream
                if let Ok(claims_json) = serde_json::to_string(&introspection) {
                    modified_req
                        .headers
                        .insert("x-auth-claims".to_string(), claims_json);
                }

                Action::Continue(modified_req)
            }
            Err(e) => Action::ShortCircuit(self.error_response(&e)),
        }
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Validate the OAuth2 token via introspection.
    fn validate_request(&self, req: &Request) -> Result<IntrospectionResponse, OAuth2Error> {
        // Extract Bearer token
        let token = self.extract_token(req)?;

        // Call introspection endpoint
        let introspection = self.introspect_token(&token)?;

        // Check if token is active
        if !introspection.active {
            return Err(OAuth2Error::TokenInactive);
        }

        // Check required scopes
        if let Some(required) = &self.required_scopes {
            self.check_scopes(&introspection, required)?;
        }

        Ok(introspection)
    }

    /// Extract Bearer token from Authorization header.
    fn extract_token(&self, req: &Request) -> Result<String, OAuth2Error> {
        let auth_header = req
            .headers
            .get("authorization")
            .or_else(|| req.headers.get("Authorization"))
            .ok_or(OAuth2Error::MissingToken)?;

        if !auth_header.starts_with("Bearer ") && !auth_header.starts_with("bearer ") {
            return Err(OAuth2Error::InvalidAuthHeader);
        }

        Ok(auth_header[7..].trim().to_string())
    }

    /// Call the introspection endpoint to validate the token.
    fn introspect_token(&self, token: &str) -> Result<IntrospectionResponse, OAuth2Error> {
        // Build request headers
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        );
        headers.insert("accept".to_string(), "application/json".to_string());

        // Add Basic auth header
        let credentials = format!("{}:{}", self.client_id, self.client_secret);
        let encoded = base64_encode(credentials.as_bytes());
        headers.insert("authorization".to_string(), format!("Basic {}", encoded));

        // Build request body (application/x-www-form-urlencoded)
        let body = format!("token={}", url_encode(token));

        let http_request = HttpRequest {
            method: "POST".to_string(),
            url: self.introspection_endpoint.clone(),
            headers,
            body: Some(body),
            timeout_ms: Some((self.timeout * 1000.0) as u64),
        };

        // Serialize request
        let request_json = serde_json::to_vec(&http_request)
            .map_err(|e| OAuth2Error::IntrospectionFailed(format!("request serialization: {}", e)))?;

        // Call introspection endpoint
        let result_len = unsafe { host_http_call(request_json.as_ptr() as i32, request_json.len() as i32) };

        if result_len < 0 {
            return Err(OAuth2Error::IntrospectionFailed(
                "connection failed".to_string(),
            ));
        }

        // Read the response
        let mut response_buf = vec![0u8; result_len as usize];
        let bytes_read =
            unsafe { host_http_read_result(response_buf.as_mut_ptr() as i32, result_len) };

        if bytes_read <= 0 {
            return Err(OAuth2Error::IntrospectionFailed(
                "failed to read response".to_string(),
            ));
        }

        // Parse the HTTP response
        let http_response: HttpResponse =
            serde_json::from_slice(&response_buf[..bytes_read as usize]).map_err(|e| {
                OAuth2Error::IntrospectionFailed(format!("invalid response format: {}", e))
            })?;

        // Check HTTP status
        if http_response.status != 200 {
            return Err(OAuth2Error::IntrospectionFailed(format!(
                "endpoint returned status {}",
                http_response.status
            )));
        }

        // Parse introspection response body
        let body = http_response
            .body
            .ok_or_else(|| OAuth2Error::IntrospectionFailed("empty response body".to_string()))?;

        serde_json::from_slice(&body).map_err(|e| {
            OAuth2Error::IntrospectionFailed(format!("invalid introspection response: {}", e))
        })
    }

    /// Check if the token has all required scopes.
    fn check_scopes(
        &self,
        introspection: &IntrospectionResponse,
        required: &str,
    ) -> Result<(), OAuth2Error> {
        let token_scopes: Vec<&str> = introspection
            .scope
            .as_ref()
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();

        let required_scopes: Vec<&str> = required.split_whitespace().collect();

        for scope in required_scopes {
            if !token_scopes.contains(&scope) {
                return Err(OAuth2Error::InsufficientScope);
            }
        }

        Ok(())
    }

    /// Generate error response.
    fn error_response(&self, error: &OAuth2Error) -> Response {
        let status = error.status_code();
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());

        // RFC 6750: WWW-Authenticate header
        let www_auth = format!(
            "Bearer realm=\"api\", error=\"{}\", error_description=\"{}\"",
            error.as_str(),
            error.description()
        );
        headers.insert("www-authenticate".to_string(), www_auth);

        let error_type = match status {
            403 => "urn:barbacane:error:authorization-failed",
            _ => "urn:barbacane:error:authentication-failed",
        };

        let body = serde_json::json!({
            "type": error_type,
            "title": if status == 403 { "Authorization failed" } else { "Authentication failed" },
            "status": status,
            "detail": error.description()
        });

        Response {
            status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

/// Simple Base64 encoding (URL-safe without padding not needed for Basic auth).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        result.push(ALPHABET[b0 >> 2] as char);
        result.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if chunk.len() > 1 {
            result.push(ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[b2 & 0x3f] as char);
        } else {
            result.push('=');
        }
    }

    result
}

/// Simple URL encoding for form data.
fn url_encode(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => result.push(c),
            _ => {
                for b in c.to_string().as_bytes() {
                    result.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    result
}

// Host function declarations
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Make an HTTP request. Returns the response length, or -1 on error.
    fn host_http_call(req_ptr: i32, req_len: i32) -> i32;

    /// Read the HTTP response into the provided buffer. Returns bytes read.
    fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}
