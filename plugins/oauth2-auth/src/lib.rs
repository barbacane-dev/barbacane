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
                    // Convert space-separated scopes to comma-separated groups
                    let groups = scope.split_whitespace().collect::<Vec<_>>().join(",");
                    if !groups.is_empty() {
                        modified_req
                            .headers
                            .insert("x-auth-consumer-groups".to_string(), groups);
                    }
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

                // Standard consumer header: sub takes precedence, then username
                let consumer = introspection
                    .sub
                    .as_ref()
                    .or(introspection.username.as_ref());
                if let Some(consumer_id) = consumer {
                    modified_req
                        .headers
                        .insert("x-auth-consumer".to_string(), consumer_id.clone());
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
        let request_json = serde_json::to_vec(&http_request).map_err(|e| {
            OAuth2Error::IntrospectionFailed(format!("request serialization: {}", e))
        })?;

        // Call introspection endpoint
        let result_len =
            unsafe { host_http_call(request_json.as_ptr() as i32, request_json.len() as i32) };

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

// Host function declarations (WASM only)
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Make an HTTP request. Returns the response length, or -1 on error.
    fn host_http_call(req_ptr: i32, req_len: i32) -> i32;

    /// Read the HTTP response into the provided buffer. Returns bytes read.
    fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}

// Mock host functions for native tests (not called in pure logic tests)
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_call(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> OAuth2Auth {
        OAuth2Auth {
            introspection_endpoint: "https://auth.example.com/introspect".to_string(),
            client_id: "test_client".to_string(),
            client_secret: "test_secret".to_string(),
            required_scopes: None,
            timeout: 5.0,
        }
    }

    fn create_request_with_auth(auth_value: &str) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("authorization".to_string(), auth_value.to_string());

        Request {
            method: "GET".to_string(),
            path: "/api/resource".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    #[test]
    fn test_extract_token_valid_bearer() {
        let config = create_test_config();
        let req = create_request_with_auth("Bearer test_token_12345");

        let result = config.extract_token(&req);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test_token_12345");
    }

    #[test]
    fn test_extract_token_lowercase_bearer() {
        let config = create_test_config();
        let req = create_request_with_auth("bearer lowercase_token");

        let result = config.extract_token(&req);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "lowercase_token");
    }

    #[test]
    fn test_extract_token_with_extra_spaces() {
        let config = create_test_config();
        let req = create_request_with_auth("Bearer   spaced_token   ");

        let result = config.extract_token(&req);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "spaced_token");
    }

    #[test]
    fn test_extract_token_missing_header() {
        let config = create_test_config();
        let req = Request {
            method: "GET".to_string(),
            path: "/api/resource".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };

        let result = config.extract_token(&req);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), OAuth2Error::MissingToken));
    }

    #[test]
    fn test_extract_token_non_bearer_scheme() {
        let config = create_test_config();
        let req = create_request_with_auth("Basic dXNlcjpwYXNz");

        let result = config.extract_token(&req);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OAuth2Error::InvalidAuthHeader
        ));
    }

    #[test]
    fn test_extract_token_case_insensitive_header() {
        let config = create_test_config();
        let mut req = Request {
            method: "GET".to_string(),
            path: "/api/resource".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };
        req.headers
            .insert("Authorization".to_string(), "Bearer cap_token".to_string());

        let result = config.extract_token(&req);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "cap_token");
    }

    #[test]
    fn test_check_scopes_all_present() {
        let config = create_test_config();
        let introspection = IntrospectionResponse {
            active: true,
            scope: Some("read write admin".to_string()),
            client_id: None,
            username: None,
            token_type: None,
            exp: None,
            iat: None,
            nbf: None,
            sub: None,
            aud: None,
            iss: None,
            jti: None,
        };

        let result = config.check_scopes(&introspection, "read write");
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_scopes_missing_scope() {
        let config = create_test_config();
        let introspection = IntrospectionResponse {
            active: true,
            scope: Some("read".to_string()),
            client_id: None,
            username: None,
            token_type: None,
            exp: None,
            iat: None,
            nbf: None,
            sub: None,
            aud: None,
            iss: None,
            jti: None,
        };

        let result = config.check_scopes(&introspection, "read write");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OAuth2Error::InsufficientScope
        ));
    }

    #[test]
    fn test_check_scopes_no_scopes_on_token() {
        let config = create_test_config();
        let introspection = IntrospectionResponse {
            active: true,
            scope: None,
            client_id: None,
            username: None,
            token_type: None,
            exp: None,
            iat: None,
            nbf: None,
            sub: None,
            aud: None,
            iss: None,
            jti: None,
        };

        let result = config.check_scopes(&introspection, "read");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OAuth2Error::InsufficientScope
        ));
    }

    #[test]
    fn test_check_scopes_empty_required() {
        let config = create_test_config();
        let introspection = IntrospectionResponse {
            active: true,
            scope: Some("read write".to_string()),
            client_id: None,
            username: None,
            token_type: None,
            exp: None,
            iat: None,
            nbf: None,
            sub: None,
            aud: None,
            iss: None,
            jti: None,
        };

        let result = config.check_scopes(&introspection, "");
        assert!(result.is_ok());
    }

    #[test]
    fn test_error_response_401() {
        let config = create_test_config();
        let error = OAuth2Error::TokenInactive;

        let response = config.error_response(&error);
        assert_eq!(response.status, 401);
        assert_eq!(
            response.headers.get("content-type").unwrap(),
            "application/json"
        );
        assert!(response.headers.contains_key("www-authenticate"));

        let body = response.body.unwrap();
        assert!(body.contains("\"status\":401"));
        assert!(body.contains("Authentication failed"));
        assert!(body.contains("urn:barbacane:error:authentication-failed"));
    }

    #[test]
    fn test_error_response_403() {
        let config = create_test_config();
        let error = OAuth2Error::InsufficientScope;

        let response = config.error_response(&error);
        assert_eq!(response.status, 403);
        assert_eq!(
            response.headers.get("content-type").unwrap(),
            "application/json"
        );

        let body = response.body.unwrap();
        assert!(body.contains("\"status\":403"));
        assert!(body.contains("Authorization failed"));
        assert!(body.contains("urn:barbacane:error:authorization-failed"));
    }

    #[test]
    fn test_error_response_www_authenticate_header() {
        let config = create_test_config();
        let error = OAuth2Error::MissingToken;

        let response = config.error_response(&error);
        let www_auth = response.headers.get("www-authenticate").unwrap();
        assert!(www_auth.contains("Bearer realm=\"api\""));
        assert!(www_auth.contains("error=\"missing_token\""));
        assert!(www_auth.contains("Bearer token required"));
    }

    #[test]
    fn test_base64_encode_empty() {
        let result = base64_encode(b"");
        assert_eq!(result, "");
    }

    #[test]
    fn test_base64_encode_single_byte() {
        let result = base64_encode(b"A");
        assert_eq!(result, "QQ==");
    }

    #[test]
    fn test_base64_encode_two_bytes() {
        let result = base64_encode(b"AB");
        assert_eq!(result, "QUI=");
    }

    #[test]
    fn test_base64_encode_three_bytes() {
        let result = base64_encode(b"ABC");
        assert_eq!(result, "QUJD");
    }

    #[test]
    fn test_base64_encode_credentials() {
        let result = base64_encode(b"client_id:client_secret");
        assert_eq!(result, "Y2xpZW50X2lkOmNsaWVudF9zZWNyZXQ=");
    }

    #[test]
    fn test_base64_encode_long_string() {
        let result = base64_encode(b"Hello, World! This is a test.");
        assert_eq!(result, "SGVsbG8sIFdvcmxkISBUaGlzIGlzIGEgdGVzdC4=");
    }

    #[test]
    fn test_url_encode_safe_characters() {
        let result = url_encode("ABCabc123-_.~");
        assert_eq!(result, "ABCabc123-_.~");
    }

    #[test]
    fn test_url_encode_space() {
        let result = url_encode("hello world");
        assert_eq!(result, "hello%20world");
    }

    #[test]
    fn test_url_encode_special_characters() {
        let result = url_encode("hello@world.com");
        assert_eq!(result, "hello%40world.com");
    }

    #[test]
    fn test_url_encode_plus_equals() {
        let result = url_encode("a+b=c");
        assert_eq!(result, "a%2Bb%3Dc");
    }

    #[test]
    fn test_url_encode_slash() {
        let result = url_encode("path/to/resource");
        assert_eq!(result, "path%2Fto%2Fresource");
    }

    #[test]
    fn test_url_encode_percent() {
        let result = url_encode("100%");
        assert_eq!(result, "100%25");
    }

    #[test]
    fn test_url_encode_unicode() {
        let result = url_encode("hello🌍");
        assert_eq!(result, "hello%F0%9F%8C%8D");
    }

    #[test]
    fn test_config_deserialization_minimal() {
        let json = r#"{
            "introspection_endpoint": "https://auth.example.com/introspect",
            "client_id": "my_client",
            "client_secret": "my_secret"
        }"#;

        let config: OAuth2Auth = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.introspection_endpoint,
            "https://auth.example.com/introspect"
        );
        assert_eq!(config.client_id, "my_client");
        assert_eq!(config.client_secret, "my_secret");
        assert!(config.required_scopes.is_none());
        assert_eq!(config.timeout, 5.0);
    }

    #[test]
    fn test_config_deserialization_with_scopes() {
        let json = r#"{
            "introspection_endpoint": "https://auth.example.com/introspect",
            "client_id": "my_client",
            "client_secret": "my_secret",
            "required_scopes": "read write"
        }"#;

        let config: OAuth2Auth = serde_json::from_str(json).unwrap();
        assert_eq!(config.required_scopes.unwrap(), "read write");
    }

    #[test]
    fn test_config_deserialization_with_timeout() {
        let json = r#"{
            "introspection_endpoint": "https://auth.example.com/introspect",
            "client_id": "my_client",
            "client_secret": "my_secret",
            "timeout": 10.5
        }"#;

        let config: OAuth2Auth = serde_json::from_str(json).unwrap();
        assert_eq!(config.timeout, 10.5);
    }

    #[test]
    fn test_config_deserialization_missing_required_fields() {
        let json = r#"{
            "introspection_endpoint": "https://auth.example.com/introspect"
        }"#;

        let result: Result<OAuth2Auth, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_on_response_passthrough() {
        let mut config = create_test_config();
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some("test body".to_string()),
        };

        let result = config.on_response(response.clone());
        assert_eq!(result.status, response.status);
        assert_eq!(result.body, response.body);
    }

    #[test]
    fn test_oauth2_error_as_str() {
        assert_eq!(OAuth2Error::MissingToken.as_str(), "missing_token");
        assert_eq!(OAuth2Error::InvalidAuthHeader.as_str(), "invalid_request");
        assert_eq!(
            OAuth2Error::IntrospectionFailed("test".to_string()).as_str(),
            "server_error"
        );
        assert_eq!(OAuth2Error::TokenInactive.as_str(), "invalid_token");
        assert_eq!(
            OAuth2Error::InsufficientScope.as_str(),
            "insufficient_scope"
        );
    }

    #[test]
    fn test_oauth2_error_status_code() {
        assert_eq!(OAuth2Error::MissingToken.status_code(), 401);
        assert_eq!(OAuth2Error::InvalidAuthHeader.status_code(), 401);
        assert_eq!(
            OAuth2Error::IntrospectionFailed("test".to_string()).status_code(),
            401
        );
        assert_eq!(OAuth2Error::TokenInactive.status_code(), 401);
        assert_eq!(OAuth2Error::InsufficientScope.status_code(), 403);
    }

    #[test]
    fn test_default_timeout() {
        assert_eq!(default_timeout(), 5.0);
    }

    // --- Consumer header mapping tests ---

    #[test]
    fn consumer_header_from_sub() {
        let introspection = IntrospectionResponse {
            active: true,
            scope: Some("read write".to_string()),
            client_id: Some("app1".to_string()),
            username: Some("alice".to_string()),
            token_type: None,
            exp: None,
            iat: None,
            nbf: None,
            sub: Some("user-123".to_string()),
            aud: None,
            iss: None,
            jti: None,
        };

        // Simulate on_request header logic: sub takes precedence over username
        let consumer = introspection.sub.as_ref().or(introspection.username.as_ref());
        assert_eq!(consumer.unwrap(), "user-123");
    }

    #[test]
    fn consumer_header_fallback_to_username() {
        let introspection = IntrospectionResponse {
            active: true,
            scope: None,
            client_id: Some("app1".to_string()),
            username: Some("alice".to_string()),
            token_type: None,
            exp: None,
            iat: None,
            nbf: None,
            sub: None,
            aud: None,
            iss: None,
            jti: None,
        };

        let consumer = introspection.sub.as_ref().or(introspection.username.as_ref());
        assert_eq!(consumer.unwrap(), "alice");
    }

    #[test]
    fn consumer_groups_from_scope() {
        let scope = "read write admin";
        let groups = scope.split_whitespace().collect::<Vec<_>>().join(",");
        assert_eq!(groups, "read,write,admin");
    }
}
