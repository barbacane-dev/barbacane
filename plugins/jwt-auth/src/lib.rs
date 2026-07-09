//! JWT authentication middleware plugin for Barbacane API gateway.
//!
//! Validates Bearer tokens in the Authorization header and rejects
//! unauthenticated requests with 401 Unauthorized.

use barbacane_plugin_sdk::prelude::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// JWT authentication middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct JwtAuth {
    /// Expected issuer (iss claim). If set, tokens must match.
    #[serde(default)]
    issuer: Option<String>,

    /// Expected audience (aud claim). If set, tokens must match.
    #[serde(default)]
    audience: Option<String>,

    /// Clock skew tolerance in seconds for exp/nbf validation.
    #[serde(default = "default_clock_skew")]
    clock_skew_seconds: u64,

    /// Optional claim name to extract consumer groups from.
    /// When set, the value of this claim is read from the JWT payload and set
    /// as `x-auth-consumer-groups` (comma-separated). Common values: "roles",
    /// "groups", "permissions".
    #[serde(default)]
    groups_claim: Option<String>,

    /// Skip signature validation. **Test only** — this flag is ignored in the
    /// compiled WASM plugin (it is honored solely under `#[cfg(test)]`), so a
    /// production deployment can never be configured to accept unsigned tokens.
    #[serde(default)]
    skip_signature_validation: bool,

    /// JWKS URL for fetching public keys. Not handled by `jwt-auth`; use the
    /// `oidc-auth` plugin for JWKS-based verification.
    #[allow(dead_code)]
    #[serde(default)]
    jwks_url: Option<String>,

    /// Inline public key in PEM format. Not handled by `jwt-auth`; supply
    /// `public_key_jwk` instead, or use `oidc-auth`.
    #[allow(dead_code)]
    #[serde(default)]
    public_key_pem: Option<String>,

    /// Inline public key as a JWK, used to verify the token signature via the
    /// host `verify_signature` capability. Supports RSA (`RS256/384/512`) and
    /// EC (`ES256/384`) keys. When set, every token must carry a valid
    /// signature under this key.
    #[serde(default)]
    public_key_jwk: Option<Jwk>,
}

/// A JSON Web Key (public part) accepted by the host `verify_signature`
/// capability.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default, rename = "use")]
    use_: Option<String>,
    // RSA
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    // EC
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default)]
    crv: Option<String>,
}

/// Request payload for the host `verify_signature` capability.
#[derive(Serialize)]
struct VerifyRequest {
    algorithm: String,
    jwk: serde_json::Value,
    message: String,
    signature: Vec<u8>,
}

fn default_clock_skew() -> u64 {
    60 // 1 minute tolerance
}

/// JWT header.
#[derive(Debug, Deserialize)]
struct JwtHeader {
    alg: String,
    #[allow(dead_code)]
    #[serde(default)]
    typ: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    kid: Option<String>,
}

/// JWT claims (registered claims + extra for custom claims like groups).
#[derive(Debug, Deserialize, Serialize)]
struct JwtClaims {
    /// Subject
    #[serde(default)]
    sub: Option<String>,
    /// Issuer
    #[serde(default)]
    iss: Option<String>,
    /// Audience (can be string or array)
    #[serde(default)]
    aud: Option<Audience>,
    /// Expiration time (Unix timestamp)
    #[serde(default)]
    exp: Option<u64>,
    /// Not before (Unix timestamp)
    #[serde(default)]
    nbf: Option<u64>,
    /// Issued at (Unix timestamp)
    #[serde(default)]
    iat: Option<u64>,
    /// JWT ID
    #[serde(default)]
    jti: Option<String>,
    /// All other claims (roles, groups, permissions, etc.)
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
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

/// JWT validation error.
#[derive(Debug)]
enum JwtError {
    MissingAuthHeader,
    InvalidAuthHeader,
    MalformedToken,
    InvalidBase64,
    InvalidJson,
    TokenExpired,
    TokenNotYetValid,
    InvalidIssuer,
    InvalidAudience,
    UnsupportedAlgorithm(String),
    SignatureInvalid,
}

impl JwtError {
    fn as_str(&self) -> &'static str {
        match self {
            JwtError::MissingAuthHeader => "missing_token",
            JwtError::InvalidAuthHeader => "invalid_request",
            JwtError::MalformedToken => "invalid_token",
            JwtError::InvalidBase64 => "invalid_token",
            JwtError::InvalidJson => "invalid_token",
            JwtError::TokenExpired => "invalid_token",
            JwtError::TokenNotYetValid => "invalid_token",
            JwtError::InvalidIssuer => "invalid_token",
            JwtError::InvalidAudience => "invalid_token",
            JwtError::UnsupportedAlgorithm(_) => "invalid_token",
            JwtError::SignatureInvalid => "invalid_token",
        }
    }

    fn description(&self) -> String {
        match self {
            JwtError::MissingAuthHeader => "Bearer token required".to_string(),
            JwtError::InvalidAuthHeader => "Invalid Authorization header format".to_string(),
            JwtError::MalformedToken => "Malformed JWT token".to_string(),
            JwtError::InvalidBase64 => "Invalid base64 encoding in token".to_string(),
            JwtError::InvalidJson => "Invalid JSON in token".to_string(),
            JwtError::TokenExpired => "Token has expired".to_string(),
            JwtError::TokenNotYetValid => "Token is not yet valid".to_string(),
            JwtError::InvalidIssuer => "Token issuer mismatch".to_string(),
            JwtError::InvalidAudience => "Token audience mismatch".to_string(),
            JwtError::UnsupportedAlgorithm(alg) => format!("Unsupported algorithm: {}", alg),
            JwtError::SignatureInvalid => "Token signature verification failed".to_string(),
        }
    }
}

impl JwtAuth {
    /// Handle incoming request - validate JWT token.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        match self.validate_request(&req) {
            Ok(claims) => {
                // Add auth context to request headers for downstream use
                let mut modified_req = req;
                if let Some(sub) = &claims.sub {
                    modified_req
                        .headers
                        .insert("x-auth-sub".to_string(), sub.clone());
                    // Standard consumer header for ACL and downstream middlewares
                    modified_req
                        .headers
                        .insert("x-auth-consumer".to_string(), sub.clone());
                }
                if let Ok(claims_json) = serde_json::to_string(&claims) {
                    modified_req
                        .headers
                        .insert("x-auth-claims".to_string(), claims_json);
                }

                // Extract consumer groups from configured claim
                if let Some(ref groups_claim) = self.groups_claim {
                    if let Some(groups_val) = claims.extra.get(groups_claim) {
                        let groups_csv = match groups_val {
                            serde_json::Value::Array(arr) => arr
                                .iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(","),
                            serde_json::Value::String(s) => {
                                // Support space-separated or already comma-separated
                                if s.contains(',') {
                                    s.clone()
                                } else {
                                    s.split_whitespace().collect::<Vec<_>>().join(",")
                                }
                            }
                            _ => String::new(),
                        };
                        if !groups_csv.is_empty() {
                            modified_req
                                .headers
                                .insert("x-auth-consumer-groups".to_string(), groups_csv);
                        }
                    }
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

    /// Validate the JWT token in the request.
    fn validate_request(&self, req: &Request) -> Result<JwtClaims, JwtError> {
        // Extract token from Authorization header
        let token = self.extract_token(req)?;

        // Parse the JWT
        let parsed = self.parse_jwt(&token)?;

        // Validate algorithm
        self.validate_algorithm(&parsed.header)?;

        // Validate signature. `skip_signature_validation` is honored only in
        // unit tests (`cfg!(test)`); in the compiled plugin it has no effect,
        // so a forged/unsigned token is never accepted in production.
        let skip = self.skip_signature_validation && cfg!(test);
        if !skip {
            self.validate_signature(&parsed)?;
        }

        // Validate claims
        self.validate_claims(&parsed.claims)?;

        Ok(parsed.claims)
    }

    /// Extract Bearer token from Authorization header.
    fn extract_token(&self, req: &Request) -> Result<String, JwtError> {
        let auth_header = req
            .headers
            .get("authorization")
            .or_else(|| req.headers.get("Authorization"))
            .ok_or(JwtError::MissingAuthHeader)?;

        if !auth_header.starts_with("Bearer ") && !auth_header.starts_with("bearer ") {
            return Err(JwtError::InvalidAuthHeader);
        }

        Ok(auth_header[7..].trim().to_string())
    }

    /// Parse a JWT token into its components.
    fn parse_jwt(&self, token: &str) -> Result<ParsedJwt, JwtError> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(JwtError::MalformedToken);
        }

        let header_bytes = URL_SAFE_NO_PAD
            .decode(parts[0])
            .map_err(|_| JwtError::InvalidBase64)?;
        let claims_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| JwtError::InvalidBase64)?;
        let signature = URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|_| JwtError::InvalidBase64)?;

        let header: JwtHeader =
            serde_json::from_slice(&header_bytes).map_err(|_| JwtError::InvalidJson)?;
        let claims: JwtClaims =
            serde_json::from_slice(&claims_bytes).map_err(|_| JwtError::InvalidJson)?;

        Ok(ParsedJwt {
            header,
            claims,
            signing_input: format!("{}.{}", parts[0], parts[1]),
            signature,
        })
    }

    /// Validate the JWT algorithm.
    fn validate_algorithm(&self, header: &JwtHeader) -> Result<(), JwtError> {
        match header.alg.as_str() {
            "RS256" | "RS384" | "RS512" | "ES256" | "ES384" | "ES512" => Ok(()),
            "none" => Err(JwtError::UnsupportedAlgorithm("none".to_string())),
            "HS256" | "HS384" | "HS512" => {
                // HMAC algorithms are not recommended for distributed systems
                Err(JwtError::UnsupportedAlgorithm(header.alg.clone()))
            }
            alg => Err(JwtError::UnsupportedAlgorithm(alg.to_string())),
        }
    }

    /// Validate the JWT signature using the host `verify_signature` capability.
    ///
    /// Requires `public_key_jwk` to be configured. JWKS-over-network and PEM
    /// keys are intentionally not handled here — use the `oidc-auth` plugin for
    /// those. Fails closed when no inline key is configured.
    fn validate_signature(&self, parsed: &ParsedJwt) -> Result<(), JwtError> {
        let jwk = self
            .public_key_jwk
            .as_ref()
            .ok_or(JwtError::SignatureInvalid)?;

        // Bind the key to the token algorithm (RFC 8725): a key tagged with a
        // specific `alg`/`use` must match what the token claims, and the key
        // type must match the algorithm family.
        if let Some(key_alg) = &jwk.alg {
            if key_alg != &parsed.header.alg {
                return Err(JwtError::SignatureInvalid);
            }
        }
        if let Some(use_) = &jwk.use_ {
            if use_ != "sig" {
                return Err(JwtError::SignatureInvalid);
            }
        }
        let expected_kty = match parsed.header.alg.as_str() {
            "RS256" | "RS384" | "RS512" => "RSA",
            "ES256" | "ES384" | "ES512" => "EC",
            other => return Err(JwtError::UnsupportedAlgorithm(other.to_string())),
        };
        if jwk.kty != expected_kty {
            return Err(JwtError::SignatureInvalid);
        }

        let request = VerifyRequest {
            algorithm: parsed.header.alg.clone(),
            jwk: serde_json::to_value(jwk).map_err(|_| JwtError::SignatureInvalid)?,
            message: parsed.signing_input.clone(),
            signature: parsed.signature.clone(),
        };
        let request_json = serde_json::to_vec(&request).map_err(|_| JwtError::SignatureInvalid)?;

        // SAFETY: passing a pointer/length into the host, which copies the bytes
        // out of guest memory before returning.
        let result = unsafe {
            host_verify_signature(request_json.as_ptr() as i32, request_json.len() as i32)
        };
        match result {
            1 => Ok(()),
            _ => Err(JwtError::SignatureInvalid),
        }
    }

    /// Validate JWT claims.
    fn validate_claims(&self, claims: &JwtClaims) -> Result<(), JwtError> {
        let now = current_timestamp();

        // Validate expiration (exp)
        if let Some(exp) = claims.exp {
            if now > exp + self.clock_skew_seconds {
                return Err(JwtError::TokenExpired);
            }
        }

        // Validate not before (nbf)
        if let Some(nbf) = claims.nbf {
            if now + self.clock_skew_seconds < nbf {
                return Err(JwtError::TokenNotYetValid);
            }
        }

        // Validate issuer (iss)
        if let Some(expected_iss) = &self.issuer {
            match &claims.iss {
                Some(iss) if iss == expected_iss => {}
                _ => return Err(JwtError::InvalidIssuer),
            }
        }

        // Validate audience (aud). When unset, any token minted for any relying
        // party at this issuer is accepted (confused-deputy risk on shared
        // IdPs), so warn once to surface the gap.
        if let Some(expected_aud) = &self.audience {
            match &claims.aud {
                Some(aud) if aud.contains(expected_aud) => {}
                _ => return Err(JwtError::InvalidAudience),
            }
        } else {
            warn_once_no_audience();
        }

        Ok(())
    }

    /// Generate 401 Unauthorized response.
    fn unauthorized_response(&self, error: &JwtError) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());

        // RFC 6750: WWW-Authenticate header for Bearer token scheme
        let www_auth = format!(
            "Bearer realm=\"api\", error=\"{}\", error_description=\"{}\"",
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
            body: Some(body.to_string().into_bytes()),
        }
    }
}

/// Host capability bindings (WASM).
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    fn host_get_unix_timestamp() -> u64;
    fn host_verify_signature(req_ptr: i32, req_len: i32) -> i32;
}

/// Non-WASM stub: signature verification is unavailable off-target, so it
/// fails closed. Unit tests use `skip_signature_validation` instead.
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_verify_signature(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

/// Warn (once) that no `audience` is configured, so tokens for any relying party
/// at the issuer are accepted. Logged via host_log on the WASM target.
#[cfg(target_arch = "wasm32")]
fn warn_once_no_audience() {
    use core::cell::Cell;
    thread_local! { static WARNED: Cell<bool> = const { Cell::new(false) }; }
    WARNED.with(|w| {
        if !w.get() {
            w.set(true);
            let msg = "jwt-auth: no 'audience' configured; tokens for any audience at this issuer are accepted (confused-deputy risk). Set 'audience' for multi-RP IdPs.";
            barbacane_plugin_sdk::log::warn(msg);
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn warn_once_no_audience() {}

/// Get current Unix timestamp (WASM version using host function).
#[cfg(target_arch = "wasm32")]
fn current_timestamp() -> u64 {
    unsafe { host_get_unix_timestamp() }
}

/// Mock time support for testing on non-WASM targets.
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    /// Helper to create a valid JWT token for testing.
    fn create_test_jwt(header: &str, claims: &str) -> String {
        let header_b64 = URL_SAFE_NO_PAD.encode(header.as_bytes());
        let claims_b64 = URL_SAFE_NO_PAD.encode(claims.as_bytes());
        format!("{}.{}.sig", header_b64, claims_b64)
    }

    /// Helper to create a test request with authorization header.
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

    #[test]
    fn test_extract_token_valid_bearer() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let req = create_test_request(Some("Bearer my.jwt.token"));
        let token = config.extract_token(&req).unwrap();
        assert_eq!(token, "my.jwt.token");
    }

    #[test]
    fn test_extract_token_case_insensitive() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let req = create_test_request(Some("bearer my.jwt.token"));
        let token = config.extract_token(&req).unwrap();
        assert_eq!(token, "my.jwt.token");
    }

    #[test]
    fn test_extract_token_with_whitespace() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let req = create_test_request(Some("Bearer  my.jwt.token  "));
        let token = config.extract_token(&req).unwrap();
        assert_eq!(token, "my.jwt.token");
    }

    #[test]
    fn test_extract_token_missing_header() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let req = create_test_request(None);
        let result = config.extract_token(&req);
        assert!(matches!(result, Err(JwtError::MissingAuthHeader)));
    }

    #[test]
    fn test_extract_token_non_bearer_scheme() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let req = create_test_request(Some("Basic dXNlcjpwYXNz"));
        let result = config.extract_token(&req);
        assert!(matches!(result, Err(JwtError::InvalidAuthHeader)));
    }

    #[test]
    fn test_parse_jwt_valid() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
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
    fn test_parse_jwt_wrong_number_of_parts() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let result = config.parse_jwt("header.payload");
        assert!(matches!(result, Err(JwtError::MalformedToken)));
    }

    #[test]
    fn test_parse_jwt_invalid_base64() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let result = config.parse_jwt("invalid!!!.payload.sig");
        assert!(matches!(result, Err(JwtError::InvalidBase64)));
    }

    #[test]
    fn test_parse_jwt_invalid_json() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let header_b64 = URL_SAFE_NO_PAD.encode(b"not json");
        let claims_b64 = URL_SAFE_NO_PAD.encode(b"{}");
        let token = format!("{}.{}.sig", header_b64, claims_b64);
        let result = config.parse_jwt(&token);
        assert!(matches!(result, Err(JwtError::InvalidJson)));
    }

    #[test]
    fn test_validate_algorithm_rs256_allowed() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let header = JwtHeader {
            alg: "RS256".to_string(),
            typ: Some("JWT".to_string()),
            kid: None,
        };
        assert!(config.validate_algorithm(&header).is_ok());
    }

    #[test]
    fn test_validate_algorithm_es256_allowed() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let header = JwtHeader {
            alg: "ES256".to_string(),
            typ: Some("JWT".to_string()),
            kid: None,
        };
        assert!(config.validate_algorithm(&header).is_ok());
    }

    #[test]
    fn test_validate_algorithm_none_rejected() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let header = JwtHeader {
            alg: "none".to_string(),
            typ: Some("JWT".to_string()),
            kid: None,
        };
        let result = config.validate_algorithm(&header);
        assert!(matches!(result, Err(JwtError::UnsupportedAlgorithm(_))));
    }

    #[test]
    fn test_validate_algorithm_hs256_rejected() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let header = JwtHeader {
            alg: "HS256".to_string(),
            typ: Some("JWT".to_string()),
            kid: None,
        };
        let result = config.validate_algorithm(&header);
        assert!(matches!(result, Err(JwtError::UnsupportedAlgorithm(_))));
    }

    #[test]
    fn test_validate_claims_valid() {
        mock_time::set_mock_timestamp(1000);
        let config = JwtAuth {
            issuer: Some("test-issuer".to_string()),
            audience: Some("test-audience".to_string()),
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let claims = JwtClaims {
            sub: Some("user123".to_string()),
            iss: Some("test-issuer".to_string()),
            aud: Some(Audience::Single("test-audience".to_string())),
            exp: Some(2000),
            nbf: Some(500),
            iat: Some(500),
            jti: None,
            extra: BTreeMap::new(),
        };
        assert!(config.validate_claims(&claims).is_ok());
    }

    #[test]
    fn test_validate_claims_expired_token() {
        mock_time::set_mock_timestamp(2100);
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: Some(2000),
            nbf: None,
            iat: None,
            jti: None,
            extra: BTreeMap::new(),
        };
        let result = config.validate_claims(&claims);
        assert!(matches!(result, Err(JwtError::TokenExpired)));
    }

    #[test]
    fn test_validate_claims_not_yet_valid() {
        mock_time::set_mock_timestamp(400);
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: None,
            nbf: Some(500),
            iat: None,
            jti: None,
            extra: BTreeMap::new(),
        };
        let result = config.validate_claims(&claims);
        assert!(matches!(result, Err(JwtError::TokenNotYetValid)));
    }

    #[test]
    fn test_validate_claims_wrong_issuer() {
        mock_time::set_mock_timestamp(1000);
        let config = JwtAuth {
            issuer: Some("expected-issuer".to_string()),
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: Some("wrong-issuer".to_string()),
            aud: None,
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
            extra: BTreeMap::new(),
        };
        let result = config.validate_claims(&claims);
        assert!(matches!(result, Err(JwtError::InvalidIssuer)));
    }

    #[test]
    fn test_validate_claims_wrong_audience() {
        mock_time::set_mock_timestamp(1000);
        let config = JwtAuth {
            issuer: None,
            audience: Some("expected-audience".to_string()),
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: Some(Audience::Single("wrong-audience".to_string())),
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
            extra: BTreeMap::new(),
        };
        let result = config.validate_claims(&claims);
        assert!(matches!(result, Err(JwtError::InvalidAudience)));
    }

    #[test]
    fn test_validate_claims_with_clock_skew() {
        // Token expires at 2000, current time is 2050, but clock_skew is 60
        mock_time::set_mock_timestamp(2050);
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: Some(2000),
            nbf: None,
            iat: None,
            jti: None,
            extra: BTreeMap::new(),
        };
        // Should still be valid due to clock skew
        assert!(config.validate_claims(&claims).is_ok());
    }

    #[test]
    fn test_audience_contains_single() {
        let aud = Audience::Single("api.example.com".to_string());
        assert!(aud.contains("api.example.com"));
        assert!(!aud.contains("other.example.com"));
    }

    #[test]
    fn test_audience_contains_multiple() {
        let aud = Audience::Multiple(vec![
            "api.example.com".to_string(),
            "admin.example.com".to_string(),
        ]);
        assert!(aud.contains("api.example.com"));
        assert!(aud.contains("admin.example.com"));
        assert!(!aud.contains("other.example.com"));
    }

    #[test]
    fn test_unauthorized_response_format() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let error = JwtError::MissingAuthHeader;
        let response = config.unauthorized_response(&error);

        assert_eq!(response.status, 401);
        assert_eq!(
            response.headers.get("content-type"),
            Some(&"application/json".to_string())
        );
        assert!(response.headers.contains_key("www-authenticate"));

        let www_auth = response.headers.get("www-authenticate").unwrap();
        assert!(www_auth.starts_with("Bearer realm=\"api\""));
        assert!(www_auth.contains("error=\"missing_token\""));
        assert!(www_auth.contains("error_description=\"Bearer token required\""));

        let body = response.body.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 401);
        assert_eq!(json["title"], "Authentication failed");
        assert_eq!(json["type"], "urn:barbacane:error:authentication-failed");
        assert_eq!(json["detail"], "Bearer token required");
    }

    #[test]
    fn test_config_defaults() {
        let json = r#"{}"#;
        let config: JwtAuth = serde_json::from_str(json).unwrap();
        assert_eq!(config.clock_skew_seconds, 60);
        assert!(!config.skip_signature_validation);
        assert!(config.issuer.is_none());
        assert!(config.audience.is_none());
    }

    #[test]
    fn test_config_custom_values() {
        let json = r#"{
            "issuer": "my-issuer",
            "audience": "my-audience",
            "clock_skew_seconds": 120,
            "skip_signature_validation": true
        }"#;
        let config: JwtAuth = serde_json::from_str(json).unwrap();
        assert_eq!(config.issuer, Some("my-issuer".to_string()));
        assert_eq!(config.audience, Some("my-audience".to_string()));
        assert_eq!(config.clock_skew_seconds, 120);
        assert!(config.skip_signature_validation);
    }

    #[test]
    fn test_on_request_with_skip_signature_validation() {
        mock_time::set_mock_timestamp(1000);
        let mut config = JwtAuth {
            issuer: Some("test-issuer".to_string()),
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };

        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"user123","iss":"test-issuer","exp":2000}"#,
        );
        let req = create_test_request(Some(&format!("Bearer {}", token)));

        match config.on_request(req) {
            Action::Continue(modified_req) => {
                assert_eq!(
                    modified_req.headers.get("x-auth-sub"),
                    Some(&"user123".to_string())
                );
                assert_eq!(
                    modified_req.headers.get("x-auth-consumer"),
                    Some(&"user123".to_string())
                );
                assert!(modified_req.headers.contains_key("x-auth-claims"));
                // No groups_claim configured, so no consumer-groups header
                assert!(!modified_req.headers.contains_key("x-auth-consumer-groups"));
            }
            Action::ShortCircuit(_) => panic!("Expected request to be allowed"),
        }
    }

    #[test]
    fn test_on_request_rejects_when_no_key_and_not_skipped() {
        // Production-shaped config: signature validation is NOT skipped and no
        // verification key is configured. A structurally valid, unexpired token
        // must be rejected (fail closed) — this is the CR-1 regression guard
        // proving the old "skip-or-bypass" hole is gone.
        mock_time::set_mock_timestamp(1000);
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: false,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"attacker","exp":2000}"#,
        );
        let req = create_test_request(Some(&format!("Bearer {}", token)));
        match config.on_request(req) {
            Action::ShortCircuit(response) => assert_eq!(response.status, 401),
            Action::Continue(_) => panic!("forged/unsigned token must be rejected"),
        }
    }

    #[test]
    fn test_on_request_missing_token() {
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };

        let req = create_test_request(None);

        match config.on_request(req) {
            Action::ShortCircuit(response) => {
                assert_eq!(response.status, 401);
            }
            Action::Continue(_) => panic!("Expected request to be rejected"),
        }
    }

    #[test]
    fn test_on_request_expired_token() {
        mock_time::set_mock_timestamp(2100);
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };

        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"user123","exp":2000}"#,
        );
        let req = create_test_request(Some(&format!("Bearer {}", token)));

        match config.on_request(req) {
            Action::ShortCircuit(response) => {
                assert_eq!(response.status, 401);
                let body = String::from_utf8(response.body.unwrap()).unwrap();
                assert!(body.contains("Token has expired"));
            }
            Action::Continue(_) => panic!("Expected expired token to be rejected"),
        }
    }

    #[test]
    fn test_on_response_passthrough() {
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };

        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"test body".to_vec()),
        };

        let result = config.on_response(response.clone());
        assert_eq!(result.status, response.status);
        assert_eq!(result.body, response.body);
    }

    #[test]
    fn test_extract_token_authorization_header_capitalized() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };
        let mut headers = BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer my.jwt.token".to_string(),
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
        assert_eq!(token, "my.jwt.token");
    }

    #[test]
    fn test_on_request_sets_consumer_from_sub() {
        mock_time::set_mock_timestamp(1000);
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: None,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };

        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"alice","exp":2000}"#,
        );
        let req = create_test_request(Some(&format!("Bearer {}", token)));

        match config.on_request(req) {
            Action::Continue(r) => {
                assert_eq!(r.headers.get("x-auth-consumer").unwrap(), "alice");
                assert_eq!(r.headers.get("x-auth-sub").unwrap(), "alice");
            }
            Action::ShortCircuit(_) => panic!("Expected request to be allowed"),
        }
    }

    #[test]
    fn test_on_request_groups_claim_array() {
        mock_time::set_mock_timestamp(1000);
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: Some("roles".to_string()),
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };

        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"alice","exp":2000,"roles":["admin","editor"]}"#,
        );
        let req = create_test_request(Some(&format!("Bearer {}", token)));

        match config.on_request(req) {
            Action::Continue(r) => {
                assert_eq!(r.headers.get("x-auth-consumer").unwrap(), "alice");
                assert_eq!(
                    r.headers.get("x-auth-consumer-groups").unwrap(),
                    "admin,editor"
                );
            }
            Action::ShortCircuit(_) => panic!("Expected request to be allowed"),
        }
    }

    #[test]
    fn test_on_request_groups_claim_space_separated_string() {
        mock_time::set_mock_timestamp(1000);
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: Some("permissions".to_string()),
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };

        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"bob","exp":2000,"permissions":"read write execute"}"#,
        );
        let req = create_test_request(Some(&format!("Bearer {}", token)));

        match config.on_request(req) {
            Action::Continue(r) => {
                assert_eq!(r.headers.get("x-auth-consumer").unwrap(), "bob");
                assert_eq!(
                    r.headers.get("x-auth-consumer-groups").unwrap(),
                    "read,write,execute"
                );
            }
            Action::ShortCircuit(_) => panic!("Expected request to be allowed"),
        }
    }

    #[test]
    fn test_on_request_groups_claim_missing_claim() {
        mock_time::set_mock_timestamp(1000);
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            groups_claim: Some("roles".to_string()),
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
            public_key_jwk: None,
        };

        // JWT has no "roles" claim
        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"alice","exp":2000}"#,
        );
        let req = create_test_request(Some(&format!("Bearer {}", token)));

        match config.on_request(req) {
            Action::Continue(r) => {
                assert_eq!(r.headers.get("x-auth-consumer").unwrap(), "alice");
                assert!(!r.headers.contains_key("x-auth-consumer-groups"));
            }
            Action::ShortCircuit(_) => panic!("Expected request to be allowed"),
        }
    }

    #[test]
    fn test_config_groups_claim_deserialization() {
        let json = r#"{
            "groups_claim": "roles",
            "skip_signature_validation": true
        }"#;
        let config: JwtAuth = serde_json::from_str(json).unwrap();
        assert_eq!(config.groups_claim, Some("roles".to_string()));
    }
}
