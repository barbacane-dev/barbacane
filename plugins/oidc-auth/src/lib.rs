//! OIDC authentication middleware plugin for Barbacane API gateway.
//!
//! Validates Bearer tokens against an OIDC provider using auto-discovery,
//! JWKS key rotation, and cryptographic signature verification via the
//! `host_verify_signature` host function.

use barbacane_plugin_sdk::prelude::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// OIDC authentication middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct OidcAuth {
    /// OIDC issuer URL (e.g., "https://accounts.google.com").
    issuer_url: String,

    /// Expected audience (aud claim). If set, tokens must match.
    #[serde(default)]
    audience: Option<String>,

    /// Required scopes (space-separated). If set, token must have all.
    #[serde(default)]
    required_scopes: Option<String>,

    /// Clock skew tolerance in seconds for exp/nbf validation.
    #[serde(default = "default_clock_skew")]
    clock_skew_seconds: u64,

    /// How often to refresh JWKS keys (seconds).
    #[serde(default = "default_jwks_refresh")]
    jwks_refresh_seconds: u64,

    /// HTTP timeout for discovery/JWKS calls (seconds).
    #[serde(default = "default_timeout")]
    timeout: f64,

    /// Cached OIDC discovery document.
    #[serde(skip)]
    discovery: Option<DiscoveryDoc>,

    /// Cached JWKS keys.
    #[serde(skip)]
    jwks_cache: Option<JwksCache>,
}

fn default_clock_skew() -> u64 {
    60
}

fn default_jwks_refresh() -> u64 {
    300
}

fn default_timeout() -> f64 {
    5.0
}

// --- Internal types ---

/// Cached OIDC discovery document.
struct DiscoveryDoc {
    issuer: String,
    jwks_uri: String,
}

/// Cached JWKS keys with fetch timestamp.
struct JwksCache {
    keys: Vec<Jwk>,
    fetched_at: u64,
}

/// A JSON Web Key.
#[derive(Clone, Deserialize, Serialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default, rename = "use")]
    use_: Option<String>,
    // RSA fields
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    // EC fields
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default)]
    crv: Option<String>,
}

/// JWKS document from the provider.
#[derive(Deserialize)]
struct JwksDocument {
    keys: Vec<Jwk>,
}

/// Partial OIDC discovery response.
#[derive(Deserialize)]
struct DiscoveryResponse {
    issuer: String,
    jwks_uri: String,
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

/// Signature verification request for host_verify_signature.
#[derive(Serialize)]
struct VerifyRequest {
    algorithm: String,
    jwk: serde_json::Value,
    message: String,
    signature: Vec<u8>,
}

/// JWT header.
#[derive(Debug, Deserialize)]
struct JwtHeader {
    alg: String,
    #[allow(dead_code)]
    #[serde(default)]
    typ: Option<String>,
    #[serde(default)]
    kid: Option<String>,
}

/// JWT claims.
#[derive(Debug, Deserialize, Serialize)]
struct JwtClaims {
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    aud: Option<Audience>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    nbf: Option<u64>,
    #[serde(default)]
    iat: Option<u64>,
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// Audience can be a single string or array of strings.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
enum Audience {
    Single(String),
    Multiple(Vec<String>),
}

impl Audience {
    fn contains(&self, value: &str) -> bool {
        match self {
            Audience::Single(s) => s == value,
            Audience::Multiple(v) => v.iter().any(|s| s == value),
        }
    }
}

/// Parsed JWT token.
struct ParsedJwt {
    header: JwtHeader,
    claims: JwtClaims,
    signing_input: String,
    signature: Vec<u8>,
}

/// OIDC validation error.
#[derive(Debug)]
enum OidcError {
    MissingToken,
    InvalidAuthHeader,
    DiscoveryFailed(String),
    JwksFetchFailed(String),
    MalformedToken,
    InvalidBase64,
    InvalidJson,
    KeyNotFound(String),
    SignatureInvalid,
    SignatureVerificationFailed(String),
    TokenExpired,
    TokenNotYetValid,
    InvalidIssuer,
    InvalidAudience,
    InsufficientScope,
    UnsupportedAlgorithm(String),
}

impl OidcError {
    fn as_str(&self) -> &'static str {
        match self {
            OidcError::MissingToken => "missing_token",
            OidcError::InvalidAuthHeader => "invalid_request",
            OidcError::DiscoveryFailed(_) => "server_error",
            OidcError::JwksFetchFailed(_) => "server_error",
            OidcError::MalformedToken => "invalid_token",
            OidcError::InvalidBase64 => "invalid_token",
            OidcError::InvalidJson => "invalid_token",
            OidcError::KeyNotFound(_) => "invalid_token",
            OidcError::SignatureInvalid => "invalid_token",
            OidcError::SignatureVerificationFailed(_) => "invalid_token",
            OidcError::TokenExpired => "invalid_token",
            OidcError::TokenNotYetValid => "invalid_token",
            OidcError::InvalidIssuer => "invalid_token",
            OidcError::InvalidAudience => "invalid_token",
            OidcError::InsufficientScope => "insufficient_scope",
            OidcError::UnsupportedAlgorithm(_) => "invalid_token",
        }
    }

    fn description(&self) -> String {
        match self {
            OidcError::MissingToken => "Bearer token required".to_string(),
            OidcError::InvalidAuthHeader => "Invalid Authorization header format".to_string(),
            OidcError::DiscoveryFailed(msg) => format!("OIDC discovery failed: {}", msg),
            OidcError::JwksFetchFailed(msg) => format!("JWKS fetch failed: {}", msg),
            OidcError::MalformedToken => "Malformed JWT token".to_string(),
            OidcError::InvalidBase64 => "Invalid base64 encoding in token".to_string(),
            OidcError::InvalidJson => "Invalid JSON in token".to_string(),
            OidcError::KeyNotFound(kid) => format!("No matching key found (kid: {})", kid),
            OidcError::SignatureInvalid => "Token signature verification failed".to_string(),
            OidcError::SignatureVerificationFailed(msg) => {
                format!("Signature verification error: {}", msg)
            }
            OidcError::TokenExpired => "Token has expired".to_string(),
            OidcError::TokenNotYetValid => "Token is not yet valid".to_string(),
            OidcError::InvalidIssuer => "Token issuer mismatch".to_string(),
            OidcError::InvalidAudience => "Token audience mismatch".to_string(),
            OidcError::InsufficientScope => "Token does not have required scopes".to_string(),
            OidcError::UnsupportedAlgorithm(alg) => format!("Unsupported algorithm: {}", alg),
        }
    }

    fn status_code(&self) -> u16 {
        match self {
            OidcError::InsufficientScope => 403,
            _ => 401,
        }
    }
}

// --- Implementation ---

impl OidcAuth {
    /// Handle incoming request - validate OIDC token.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        match self.validate_request(&req) {
            Ok(claims) => {
                let mut modified_req = req;

                if let Some(sub) = &claims.sub {
                    modified_req
                        .headers
                        .insert("x-auth-sub".to_string(), sub.clone());
                }

                if let Some(scope) = &claims.scope {
                    modified_req
                        .headers
                        .insert("x-auth-scope".to_string(), scope.clone());
                }

                if let Ok(claims_json) = serde_json::to_string(&claims) {
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

    /// Validate the OIDC token in the request.
    fn validate_request(&mut self, req: &Request) -> Result<JwtClaims, OidcError> {
        let token = self.extract_token(req)?;
        let parsed = self.parse_jwt(&token)?;
        self.validate_algorithm(&parsed.header.alg)?;

        // Ensure we have discovery and JWKS
        self.ensure_discovery()?;
        self.ensure_jwks()?;

        // Find the matching key
        let jwk = self.find_key(parsed.header.kid.as_deref(), &parsed.header.alg)?;

        // Verify signature
        self.verify_token_signature(
            &jwk,
            &parsed.signing_input,
            &parsed.signature,
            &parsed.header.alg,
        )?;

        // Validate claims
        self.validate_claims(&parsed.claims)?;

        // Check scopes
        if let Some(required) = &self.required_scopes.clone() {
            self.check_scopes(&parsed.claims, required)?;
        }

        Ok(parsed.claims)
    }

    /// Extract Bearer token from Authorization header.
    fn extract_token(&self, req: &Request) -> Result<String, OidcError> {
        let auth_header = req
            .headers
            .get("authorization")
            .or_else(|| req.headers.get("Authorization"))
            .ok_or(OidcError::MissingToken)?;

        if !auth_header.starts_with("Bearer ") && !auth_header.starts_with("bearer ") {
            return Err(OidcError::InvalidAuthHeader);
        }

        Ok(auth_header[7..].trim().to_string())
    }

    /// Parse a JWT token into its components.
    fn parse_jwt(&self, token: &str) -> Result<ParsedJwt, OidcError> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(OidcError::MalformedToken);
        }

        let header_bytes = URL_SAFE_NO_PAD
            .decode(parts[0])
            .map_err(|_| OidcError::InvalidBase64)?;
        let claims_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| OidcError::InvalidBase64)?;
        let signature = URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|_| OidcError::InvalidBase64)?;

        let header: JwtHeader =
            serde_json::from_slice(&header_bytes).map_err(|_| OidcError::InvalidJson)?;
        let claims: JwtClaims =
            serde_json::from_slice(&claims_bytes).map_err(|_| OidcError::InvalidJson)?;

        Ok(ParsedJwt {
            header,
            claims,
            signing_input: format!("{}.{}", parts[0], parts[1]),
            signature,
        })
    }

    /// Validate the JWT algorithm.
    fn validate_algorithm(&self, alg: &str) -> Result<(), OidcError> {
        match alg {
            "RS256" | "RS384" | "RS512" | "ES256" | "ES384" => Ok(()),
            "none" | "HS256" | "HS384" | "HS512" => {
                Err(OidcError::UnsupportedAlgorithm(alg.to_string()))
            }
            other => Err(OidcError::UnsupportedAlgorithm(other.to_string())),
        }
    }

    /// Ensure OIDC discovery document is fetched.
    fn ensure_discovery(&mut self) -> Result<(), OidcError> {
        if self.discovery.is_some() {
            return Ok(());
        }

        let url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer_url.trim_end_matches('/')
        );

        let response = self
            .http_get(&url)
            .map_err(|e| OidcError::DiscoveryFailed(e))?;

        if response.status != 200 {
            return Err(OidcError::DiscoveryFailed(format!(
                "status {}",
                response.status
            )));
        }

        let body = response
            .body
            .ok_or_else(|| OidcError::DiscoveryFailed("empty response".to_string()))?;

        let doc: DiscoveryResponse = serde_json::from_slice(&body)
            .map_err(|e| OidcError::DiscoveryFailed(format!("invalid JSON: {}", e)))?;

        self.discovery = Some(DiscoveryDoc {
            issuer: doc.issuer,
            jwks_uri: doc.jwks_uri,
        });

        Ok(())
    }

    /// Ensure JWKS is fetched and not expired.
    fn ensure_jwks(&mut self) -> Result<(), OidcError> {
        let now = current_timestamp();

        if let Some(cache) = &self.jwks_cache {
            if now.saturating_sub(cache.fetched_at) < self.jwks_refresh_seconds {
                return Ok(());
            }
        }

        self.refresh_jwks(now)
    }

    /// Refresh the JWKS cache.
    fn refresh_jwks(&mut self, now: u64) -> Result<(), OidcError> {
        let jwks_uri = self
            .discovery
            .as_ref()
            .map(|d| d.jwks_uri.clone())
            .ok_or_else(|| OidcError::JwksFetchFailed("no discovery document".to_string()))?;

        let response = self
            .http_get(&jwks_uri)
            .map_err(|e| OidcError::JwksFetchFailed(e))?;

        if response.status != 200 {
            return Err(OidcError::JwksFetchFailed(format!(
                "status {}",
                response.status
            )));
        }

        let body = response
            .body
            .ok_or_else(|| OidcError::JwksFetchFailed("empty response".to_string()))?;

        let doc: JwksDocument = serde_json::from_slice(&body)
            .map_err(|e| OidcError::JwksFetchFailed(format!("invalid JSON: {}", e)))?;

        self.jwks_cache = Some(JwksCache {
            keys: doc.keys,
            fetched_at: now,
        });

        Ok(())
    }

    /// Find a key in the JWKS by kid and algorithm compatibility.
    fn find_key(&self, kid: Option<&str>, alg: &str) -> Result<Jwk, OidcError> {
        let cache = self
            .jwks_cache
            .as_ref()
            .ok_or_else(|| OidcError::KeyNotFound("no JWKS cached".to_string()))?;

        let expected_kty = match alg {
            "RS256" | "RS384" | "RS512" => "RSA",
            "ES256" | "ES384" => "EC",
            _ => return Err(OidcError::UnsupportedAlgorithm(alg.to_string())),
        };

        // Try to match by kid first
        if let Some(kid) = kid {
            for key in &cache.keys {
                if key.kid.as_deref() == Some(kid) && key.kty == expected_kty {
                    // Check use field if present (must be "sig")
                    if key.use_.as_deref().is_none() || key.use_.as_deref() == Some("sig") {
                        return Ok(key.clone());
                    }
                }
            }
            return Err(OidcError::KeyNotFound(kid.to_string()));
        }

        // No kid — find first compatible key
        for key in &cache.keys {
            if key.kty == expected_kty {
                if key.use_.as_deref().is_none() || key.use_.as_deref() == Some("sig") {
                    return Ok(key.clone());
                }
            }
        }

        Err(OidcError::KeyNotFound("none".to_string()))
    }

    /// Verify the JWT signature via host_verify_signature.
    fn verify_token_signature(
        &self,
        jwk: &Jwk,
        signing_input: &str,
        signature: &[u8],
        alg: &str,
    ) -> Result<(), OidcError> {
        let jwk_value = serde_json::to_value(jwk)
            .map_err(|e| OidcError::SignatureVerificationFailed(e.to_string()))?;

        let request = VerifyRequest {
            algorithm: alg.to_string(),
            jwk: jwk_value,
            message: signing_input.to_string(),
            signature: signature.to_vec(),
        };

        let request_json = serde_json::to_vec(&request)
            .map_err(|e| OidcError::SignatureVerificationFailed(e.to_string()))?;

        let result = unsafe {
            host_verify_signature(request_json.as_ptr() as i32, request_json.len() as i32)
        };

        match result {
            1 => Ok(()),
            0 => Err(OidcError::SignatureInvalid),
            _ => Err(OidcError::SignatureVerificationFailed(
                "host function error".to_string(),
            )),
        }
    }

    /// Validate JWT claims.
    fn validate_claims(&self, claims: &JwtClaims) -> Result<(), OidcError> {
        let now = current_timestamp();

        // Validate expiration
        if let Some(exp) = claims.exp {
            if now > exp + self.clock_skew_seconds {
                return Err(OidcError::TokenExpired);
            }
        }

        // Validate not before
        if let Some(nbf) = claims.nbf {
            if now + self.clock_skew_seconds < nbf {
                return Err(OidcError::TokenNotYetValid);
            }
        }

        // Validate issuer against discovery document
        if let Some(discovery) = &self.discovery {
            match &claims.iss {
                Some(iss) if iss == &discovery.issuer => {}
                Some(_) => return Err(OidcError::InvalidIssuer),
                None => return Err(OidcError::InvalidIssuer),
            }
        }

        // Validate audience
        if let Some(expected_aud) = &self.audience {
            match &claims.aud {
                Some(aud) if aud.contains(expected_aud) => {}
                _ => return Err(OidcError::InvalidAudience),
            }
        }

        Ok(())
    }

    /// Check if the token has all required scopes.
    fn check_scopes(&self, claims: &JwtClaims, required: &str) -> Result<(), OidcError> {
        let token_scopes: Vec<&str> = claims
            .scope
            .as_ref()
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();

        for scope in required.split_whitespace() {
            if !token_scopes.contains(&scope) {
                return Err(OidcError::InsufficientScope);
            }
        }

        Ok(())
    }

    /// Make an HTTP GET request via host_http_call.
    fn http_get(&self, url: &str) -> Result<HttpResponse, String> {
        let mut headers = BTreeMap::new();
        headers.insert("accept".to_string(), "application/json".to_string());

        let http_request = HttpRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers,
            body: None,
            timeout_ms: Some((self.timeout * 1000.0) as u64),
        };

        let request_json = serde_json::to_vec(&http_request)
            .map_err(|e| format!("request serialization: {}", e))?;

        let result_len =
            unsafe { host_http_call(request_json.as_ptr() as i32, request_json.len() as i32) };

        if result_len < 0 {
            return Err("connection failed".to_string());
        }

        let mut response_buf = vec![0u8; result_len as usize];
        let bytes_read =
            unsafe { host_http_read_result(response_buf.as_mut_ptr() as i32, result_len) };

        if bytes_read <= 0 {
            return Err("failed to read response".to_string());
        }

        serde_json::from_slice(&response_buf[..bytes_read as usize])
            .map_err(|e| format!("invalid response format: {}", e))
    }

    /// Build the discovery URL from the issuer.
    #[cfg(test)]
    fn discovery_url(&self) -> String {
        format!(
            "{}/.well-known/openid-configuration",
            self.issuer_url.trim_end_matches('/')
        )
    }

    /// Generate error response.
    fn error_response(&self, error: &OidcError) -> Response {
        let status = error.status_code();
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());

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

// --- Host function declarations ---

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    fn host_http_call(req_ptr: i32, req_len: i32) -> i32;
    fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    fn host_verify_signature(req_ptr: i32, req_len: i32) -> i32;
    fn host_get_unix_timestamp() -> u64;
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_call(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_verify_signature(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

// --- Time functions ---

#[cfg(target_arch = "wasm32")]
fn current_timestamp() -> u64 {
    unsafe { host_get_unix_timestamp() }
}

#[cfg(not(target_arch = "wasm32"))]
mod mock_time {
    use std::cell::Cell;

    thread_local! {
        static MOCK_TIMESTAMP: Cell<u64> = const { Cell::new(0) };
    }

    #[allow(dead_code)]
    pub fn set_mock_timestamp(ts: u64) {
        MOCK_TIMESTAMP.with(|c| c.set(ts));
    }

    pub fn current_timestamp() -> u64 {
        MOCK_TIMESTAMP.with(|c| c.get())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn current_timestamp() -> u64 {
    mock_time::current_timestamp()
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    fn create_test_config() -> OidcAuth {
        OidcAuth {
            issuer_url: "https://auth.example.com".to_string(),
            audience: None,
            required_scopes: None,
            clock_skew_seconds: 60,
            jwks_refresh_seconds: 300,
            timeout: 5.0,
            discovery: None,
            jwks_cache: None,
        }
    }

    fn create_test_jwt(header: &str, claims: &str) -> String {
        let header_b64 = URL_SAFE_NO_PAD.encode(header.as_bytes());
        let claims_b64 = URL_SAFE_NO_PAD.encode(claims.as_bytes());
        format!("{}.{}.sig", header_b64, claims_b64)
    }

    fn create_test_request(auth_value: Option<&str>) -> Request {
        let mut headers = BTreeMap::new();
        if let Some(value) = auth_value {
            headers.insert("authorization".to_string(), value.to_string());
        }
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    fn create_test_jwk_rsa() -> Jwk {
        Jwk {
            kty: "RSA".to_string(),
            kid: Some("test-key-1".to_string()),
            alg: Some("RS256".to_string()),
            use_: Some("sig".to_string()),
            n: Some("0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw".to_string()),
            e: Some("AQAB".to_string()),
            x: None,
            y: None,
            crv: None,
        }
    }

    fn create_test_jwk_ec() -> Jwk {
        Jwk {
            kty: "EC".to_string(),
            kid: Some("ec-key-1".to_string()),
            alg: Some("ES256".to_string()),
            use_: Some("sig".to_string()),
            n: None,
            e: None,
            x: Some("f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU".to_string()),
            y: Some("x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0".to_string()),
            crv: Some("P-256".to_string()),
        }
    }

    // --- Token extraction tests ---

    #[test]
    fn extract_token_valid_bearer() {
        let config = create_test_config();
        let req = create_test_request(Some("Bearer my.jwt.token"));
        let token = config.extract_token(&req).unwrap();
        assert_eq!(token, "my.jwt.token");
    }

    #[test]
    fn extract_token_case_insensitive() {
        let config = create_test_config();
        let req = create_test_request(Some("bearer my.jwt.token"));
        let token = config.extract_token(&req).unwrap();
        assert_eq!(token, "my.jwt.token");
    }

    #[test]
    fn extract_token_with_whitespace() {
        let config = create_test_config();
        let req = create_test_request(Some("Bearer  my.jwt.token  "));
        let token = config.extract_token(&req).unwrap();
        assert_eq!(token, "my.jwt.token");
    }

    #[test]
    fn extract_token_missing_header() {
        let config = create_test_config();
        let req = create_test_request(None);
        let result = config.extract_token(&req);
        assert!(matches!(result, Err(OidcError::MissingToken)));
    }

    #[test]
    fn extract_token_non_bearer_scheme() {
        let config = create_test_config();
        let req = create_test_request(Some("Basic dXNlcjpwYXNz"));
        let result = config.extract_token(&req);
        assert!(matches!(result, Err(OidcError::InvalidAuthHeader)));
    }

    #[test]
    fn extract_token_capitalized_header() {
        let config = create_test_config();
        let mut headers = BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer cap.token.here".to_string(),
        );
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        };
        let token = config.extract_token(&req).unwrap();
        assert_eq!(token, "cap.token.here");
    }

    // --- JWT parsing tests ---

    #[test]
    fn parse_jwt_valid() {
        let config = create_test_config();
        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"user123","iss":"test-issuer"}"#,
        );
        let parsed = config.parse_jwt(&token).unwrap();
        assert_eq!(parsed.header.alg, "RS256");
        assert_eq!(parsed.claims.sub, Some("user123".to_string()));
        assert_eq!(parsed.claims.iss, Some("test-issuer".to_string()));
    }

    #[test]
    fn parse_jwt_wrong_number_of_parts() {
        let config = create_test_config();
        let result = config.parse_jwt("header.payload");
        assert!(matches!(result, Err(OidcError::MalformedToken)));
    }

    #[test]
    fn parse_jwt_invalid_base64() {
        let config = create_test_config();
        let result = config.parse_jwt("invalid!!!.payload.sig");
        assert!(matches!(result, Err(OidcError::InvalidBase64)));
    }

    #[test]
    fn parse_jwt_invalid_json() {
        let config = create_test_config();
        let header_b64 = URL_SAFE_NO_PAD.encode(b"not json");
        let claims_b64 = URL_SAFE_NO_PAD.encode(b"{}");
        let token = format!("{}.{}.sig", header_b64, claims_b64);
        let result = config.parse_jwt(&token);
        assert!(matches!(result, Err(OidcError::InvalidJson)));
    }

    // --- Algorithm validation tests ---

    #[test]
    fn validate_algorithm_rs256_allowed() {
        let config = create_test_config();
        assert!(config.validate_algorithm("RS256").is_ok());
    }

    #[test]
    fn validate_algorithm_es256_allowed() {
        let config = create_test_config();
        assert!(config.validate_algorithm("ES256").is_ok());
    }

    #[test]
    fn validate_algorithm_none_rejected() {
        let config = create_test_config();
        assert!(matches!(
            config.validate_algorithm("none"),
            Err(OidcError::UnsupportedAlgorithm(_))
        ));
    }

    #[test]
    fn validate_algorithm_hs256_rejected() {
        let config = create_test_config();
        assert!(matches!(
            config.validate_algorithm("HS256"),
            Err(OidcError::UnsupportedAlgorithm(_))
        ));
    }

    // --- Claims validation tests ---

    #[test]
    fn validate_claims_valid() {
        mock_time::set_mock_timestamp(1000);
        let mut config = create_test_config();
        config.discovery = Some(DiscoveryDoc {
            issuer: "https://auth.example.com".to_string(),
            jwks_uri: "https://auth.example.com/.well-known/jwks.json".to_string(),
        });
        config.audience = Some("my-api".to_string());

        let claims = JwtClaims {
            sub: Some("user123".to_string()),
            iss: Some("https://auth.example.com".to_string()),
            aud: Some(Audience::Single("my-api".to_string())),
            exp: Some(2000),
            nbf: Some(500),
            iat: Some(500),
            jti: None,
            scope: None,
        };
        assert!(config.validate_claims(&claims).is_ok());
    }

    #[test]
    fn validate_claims_expired() {
        mock_time::set_mock_timestamp(2100);
        let config = create_test_config();

        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: Some(2000),
            nbf: None,
            iat: None,
            jti: None,
            scope: None,
        };
        assert!(matches!(
            config.validate_claims(&claims),
            Err(OidcError::TokenExpired)
        ));
    }

    #[test]
    fn validate_claims_not_yet_valid() {
        mock_time::set_mock_timestamp(400);
        let config = create_test_config();

        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: None,
            nbf: Some(500),
            iat: None,
            jti: None,
            scope: None,
        };
        assert!(matches!(
            config.validate_claims(&claims),
            Err(OidcError::TokenNotYetValid)
        ));
    }

    #[test]
    fn validate_claims_wrong_issuer() {
        mock_time::set_mock_timestamp(1000);
        let mut config = create_test_config();
        config.discovery = Some(DiscoveryDoc {
            issuer: "https://auth.example.com".to_string(),
            jwks_uri: "https://auth.example.com/.well-known/jwks.json".to_string(),
        });

        let claims = JwtClaims {
            sub: None,
            iss: Some("https://evil.example.com".to_string()),
            aud: None,
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
            scope: None,
        };
        assert!(matches!(
            config.validate_claims(&claims),
            Err(OidcError::InvalidIssuer)
        ));
    }

    #[test]
    fn validate_claims_wrong_audience() {
        mock_time::set_mock_timestamp(1000);
        let mut config = create_test_config();
        config.audience = Some("my-api".to_string());

        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: Some(Audience::Single("other-api".to_string())),
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
            scope: None,
        };
        assert!(matches!(
            config.validate_claims(&claims),
            Err(OidcError::InvalidAudience)
        ));
    }

    #[test]
    fn validate_claims_clock_skew() {
        mock_time::set_mock_timestamp(2050);
        let config = create_test_config();

        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: Some(2000),
            nbf: None,
            iat: None,
            jti: None,
            scope: None,
        };
        // 2050 > 2000 but 2050 <= 2000 + 60 (clock_skew) => still valid
        assert!(config.validate_claims(&claims).is_ok());
    }

    // --- Scope checking tests ---

    #[test]
    fn check_scopes_all_present() {
        let config = create_test_config();
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
            scope: Some("read write admin".to_string()),
        };
        assert!(config.check_scopes(&claims, "read write").is_ok());
    }

    #[test]
    fn check_scopes_missing_scope() {
        let config = create_test_config();
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
            scope: Some("read".to_string()),
        };
        assert!(matches!(
            config.check_scopes(&claims, "read write"),
            Err(OidcError::InsufficientScope)
        ));
    }

    #[test]
    fn check_scopes_no_scopes_on_token() {
        let config = create_test_config();
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
            scope: None,
        };
        assert!(matches!(
            config.check_scopes(&claims, "read"),
            Err(OidcError::InsufficientScope)
        ));
    }

    // --- Error response tests ---

    #[test]
    fn error_response_401() {
        let config = create_test_config();
        let error = OidcError::MissingToken;
        let response = config.error_response(&error);

        assert_eq!(response.status, 401);
        assert_eq!(
            response.headers.get("content-type"),
            Some(&"application/json".to_string())
        );
        assert!(response.headers.contains_key("www-authenticate"));

        let www_auth = response.headers.get("www-authenticate").unwrap();
        assert!(www_auth.contains("error=\"missing_token\""));

        let body = response.body.unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], 401);
        assert_eq!(json["type"], "urn:barbacane:error:authentication-failed");
    }

    #[test]
    fn error_response_403() {
        let config = create_test_config();
        let error = OidcError::InsufficientScope;
        let response = config.error_response(&error);

        assert_eq!(response.status, 403);
        let body = response.body.unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], 403);
        assert_eq!(json["type"], "urn:barbacane:error:authorization-failed");
    }

    #[test]
    fn error_response_www_authenticate_header() {
        let config = create_test_config();
        let error = OidcError::TokenExpired;
        let response = config.error_response(&error);
        let www_auth = response.headers.get("www-authenticate").unwrap();
        assert!(www_auth.contains("Bearer realm=\"api\""));
        assert!(www_auth.contains("error=\"invalid_token\""));
        assert!(www_auth.contains("Token has expired"));
    }

    // --- Config deserialization tests ---

    #[test]
    fn config_minimal() {
        let json = r#"{"issuer_url": "https://auth.example.com"}"#;
        let config: OidcAuth = serde_json::from_str(json).unwrap();
        assert_eq!(config.issuer_url, "https://auth.example.com");
        assert_eq!(config.clock_skew_seconds, 60);
        assert_eq!(config.jwks_refresh_seconds, 300);
        assert_eq!(config.timeout, 5.0);
        assert!(config.audience.is_none());
        assert!(config.required_scopes.is_none());
    }

    #[test]
    fn config_full() {
        let json = r#"{
            "issuer_url": "https://auth.example.com",
            "audience": "my-api",
            "required_scopes": "read write",
            "clock_skew_seconds": 120,
            "jwks_refresh_seconds": 600,
            "timeout": 10.0
        }"#;
        let config: OidcAuth = serde_json::from_str(json).unwrap();
        assert_eq!(config.audience, Some("my-api".to_string()));
        assert_eq!(config.required_scopes, Some("read write".to_string()));
        assert_eq!(config.clock_skew_seconds, 120);
        assert_eq!(config.jwks_refresh_seconds, 600);
        assert_eq!(config.timeout, 10.0);
    }

    #[test]
    fn config_missing_required_field() {
        let json = r#"{"audience": "my-api"}"#;
        let result: Result<OidcAuth, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // --- JWKS cache tests ---

    #[test]
    fn find_key_by_kid() {
        let mut config = create_test_config();
        config.jwks_cache = Some(JwksCache {
            keys: vec![create_test_jwk_rsa(), create_test_jwk_ec()],
            fetched_at: 1000,
        });

        let key = config.find_key(Some("test-key-1"), "RS256").unwrap();
        assert_eq!(key.kty, "RSA");
        assert_eq!(key.kid, Some("test-key-1".to_string()));
    }

    #[test]
    fn find_key_by_algorithm_no_kid() {
        let mut config = create_test_config();
        config.jwks_cache = Some(JwksCache {
            keys: vec![create_test_jwk_rsa(), create_test_jwk_ec()],
            fetched_at: 1000,
        });

        let key = config.find_key(None, "ES256").unwrap();
        assert_eq!(key.kty, "EC");
    }

    #[test]
    fn find_key_not_found() {
        let mut config = create_test_config();
        config.jwks_cache = Some(JwksCache {
            keys: vec![create_test_jwk_rsa()],
            fetched_at: 1000,
        });

        let result = config.find_key(Some("nonexistent-kid"), "RS256");
        assert!(matches!(result, Err(OidcError::KeyNotFound(_))));
    }

    // --- Discovery URL test ---

    #[test]
    fn discovery_url_construction() {
        let config = create_test_config();
        assert_eq!(
            config.discovery_url(),
            "https://auth.example.com/.well-known/openid-configuration"
        );
    }

    #[test]
    fn discovery_url_strips_trailing_slash() {
        let mut config = create_test_config();
        config.issuer_url = "https://auth.example.com/".to_string();
        assert_eq!(
            config.discovery_url(),
            "https://auth.example.com/.well-known/openid-configuration"
        );
    }

    // --- Audience tests ---

    #[test]
    fn audience_single_contains() {
        let aud = Audience::Single("api.example.com".to_string());
        assert!(aud.contains("api.example.com"));
        assert!(!aud.contains("other.example.com"));
    }

    #[test]
    fn audience_multiple_contains() {
        let aud = Audience::Multiple(vec![
            "api.example.com".to_string(),
            "admin.example.com".to_string(),
        ]);
        assert!(aud.contains("api.example.com"));
        assert!(aud.contains("admin.example.com"));
        assert!(!aud.contains("other.example.com"));
    }

    // --- Response passthrough test ---

    #[test]
    fn on_response_passthrough() {
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

    // --- Error type tests ---

    #[test]
    fn error_status_codes() {
        assert_eq!(OidcError::MissingToken.status_code(), 401);
        assert_eq!(OidcError::InvalidAuthHeader.status_code(), 401);
        assert_eq!(OidcError::TokenExpired.status_code(), 401);
        assert_eq!(OidcError::SignatureInvalid.status_code(), 401);
        assert_eq!(OidcError::InsufficientScope.status_code(), 403);
    }

    #[test]
    fn error_as_str() {
        assert_eq!(OidcError::MissingToken.as_str(), "missing_token");
        assert_eq!(OidcError::InvalidAuthHeader.as_str(), "invalid_request");
        assert_eq!(OidcError::TokenExpired.as_str(), "invalid_token");
        assert_eq!(OidcError::InsufficientScope.as_str(), "insufficient_scope");
        assert_eq!(
            OidcError::DiscoveryFailed("test".to_string()).as_str(),
            "server_error"
        );
    }
}
