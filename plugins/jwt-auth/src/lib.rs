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

/// Get current Unix timestamp.
/// In WASM, this uses a simplified approach - in production,
/// this should use the host's time or WASI clock.
fn current_timestamp() -> u64 {
    // For WASM without WASI, we need to get time from the host.
    // For now, we use a host function or accept timestamp from request.
    // This is a placeholder that should be replaced with proper time handling.
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_get_unix_timestamp() -> u64;
    }
    unsafe { host_get_unix_timestamp() }
}
