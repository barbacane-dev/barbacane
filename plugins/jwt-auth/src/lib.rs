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

    /// Whether to skip signature validation (for testing only).
    #[serde(default)]
    skip_signature_validation: bool,

    /// JWKS URL for fetching public keys (not yet implemented).
    #[allow(dead_code)]
    #[serde(default)]
    jwks_url: Option<String>,

    /// Inline public key in PEM format for signature validation (not yet implemented).
    #[allow(dead_code)]
    #[serde(default)]
    public_key_pem: Option<String>,
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

/// JWT claims (registered claims).
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
    #[allow(dead_code)]
    signing_input: String,
    #[allow(dead_code)]
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
                }
                if let Ok(claims_json) = serde_json::to_string(&claims) {
                    modified_req
                        .headers
                        .insert("x-auth-claims".to_string(), claims_json);
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

        // Validate signature (if not skipped)
        if !self.skip_signature_validation {
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

    /// Validate the JWT signature.
    ///
    /// NOTE: Cryptographic signature validation is not yet implemented.
    /// When `skip_signature_validation` is false, this will always fail.
    /// Use `skip_signature_validation: true` until JWKS support is added.
    fn validate_signature(&self, _parsed: &ParsedJwt) -> Result<(), JwtError> {
        // Signature validation requires either:
        // 1. A host function for crypto (host_verify_signature) - not yet implemented
        // 2. WASM-compatible crypto library - complex to integrate
        //
        // For now, signature validation always fails unless explicitly skipped.
        // This is intentional: we don't want to silently accept unsigned tokens.
        Err(JwtError::SignatureInvalid)
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

        // Validate audience (aud)
        if let Some(expected_aud) = &self.audience {
            match &claims.aud {
                Some(aud) if aud.contains(expected_aud) => {}
                _ => return Err(JwtError::InvalidAudience),
            }
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
            body: Some(body.to_string()),
        }
    }
}

/// Get current Unix timestamp (WASM version using host function).
#[cfg(target_arch = "wasm32")]
fn current_timestamp() -> u64 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_get_unix_timestamp() -> u64;
    }
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
        };
        let claims = JwtClaims {
            sub: Some("user123".to_string()),
            iss: Some("test-issuer".to_string()),
            aud: Some(Audience::Single("test-audience".to_string())),
            exp: Some(2000),
            nbf: Some(500),
            iat: Some(500),
            jti: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: Some(2000),
            nbf: None,
            iat: None,
            jti: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: None,
            nbf: Some(500),
            iat: None,
            jti: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: Some("wrong-issuer".to_string()),
            aud: None,
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: Some(Audience::Single("wrong-audience".to_string())),
            exp: None,
            nbf: None,
            iat: None,
            jti: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
        };
        let claims = JwtClaims {
            sub: None,
            iss: None,
            aud: None,
            exp: Some(2000),
            nbf: None,
            iat: None,
            jti: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
                assert!(modified_req.headers.contains_key("x-auth-claims"));
            }
            Action::ShortCircuit(_) => panic!("Expected request to be allowed"),
        }
    }

    #[test]
    fn test_on_request_missing_token() {
        let mut config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
        };

        let token = create_test_jwt(
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"sub":"user123","exp":2000}"#,
        );
        let req = create_test_request(Some(&format!("Bearer {}", token)));

        match config.on_request(req) {
            Action::ShortCircuit(response) => {
                assert_eq!(response.status, 401);
                let body = response.body.unwrap();
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
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
        };

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
    fn test_extract_token_authorization_header_capitalized() {
        let config = JwtAuth {
            issuer: None,
            audience: None,
            clock_skew_seconds: 60,
            skip_signature_validation: true,
            jwks_url: None,
            public_key_pem: None,
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
}
