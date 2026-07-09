//! JWT parsing helpers shared by the auth plugins.
//!
//! These cover the *parsing* that jwt-auth / oidc-auth / oauth2-auth each
//! duplicated: Bearer extraction, base64url segment decoding, an `aud` claim
//! that may be a string or array, and decoding the payload for inspection.
//!
//! Signature verification is **not** done here — that is the host's job
//! (`host_verify_signature`) or the introspection endpoint's. These helpers only
//! decode; callers still verify before trusting claims.

use base64::Engine;
use serde::Deserialize;

/// The JWT `aud` claim: a single audience or a list (RFC 7519 §4.1.3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Audience {
    Single(String),
    Multiple(Vec<String>),
}

impl Audience {
    /// Whether `value` is among the audience(s).
    pub fn contains(&self, value: &str) -> bool {
        match self {
            Audience::Single(s) => s == value,
            Audience::Multiple(v) => v.iter().any(|s| s == value),
        }
    }
}

/// Errors decoding a JWT's structure (not signature validity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtDecodeError {
    /// The token is not three `.`-separated segments.
    Malformed,
    /// A segment was not valid base64url.
    InvalidBase64,
    /// The decoded payload was not valid JSON for the target type.
    InvalidJson,
}

/// Extract the token from an `Authorization` header value, if it uses the
/// `Bearer` scheme (case-insensitive per RFC 7235). Returns `None` otherwise.
pub fn bearer_token(authorization: &str) -> Option<&str> {
    let (scheme, rest) = authorization.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let token = rest.trim();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Split a compact JWT into its three base64url segments (header, payload,
/// signature). Returns `None` if the token is not exactly three segments.
pub fn split(token: &str) -> Option<(&str, &str, &str)> {
    let mut parts = token.split('.');
    let header = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some((header, payload, signature))
}

/// Decode a single base64url (no padding) JWT segment to bytes.
pub fn decode_segment(segment: &str) -> Result<Vec<u8>, JwtDecodeError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| JwtDecodeError::InvalidBase64)
}

/// Decode the payload (claims) of a compact JWT **without verifying the
/// signature**. Use only after the signature has been verified elsewhere.
pub fn decode_claims_unverified<T>(token: &str) -> Result<T, JwtDecodeError>
where
    T: for<'de> Deserialize<'de>,
{
    let (_, payload, _) = split(token).ok_or(JwtDecodeError::Malformed)?;
    let bytes = decode_segment(payload)?;
    serde_json::from_slice(&bytes).map_err(|_| JwtDecodeError::InvalidJson)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audience_matches_single_and_multiple() {
        assert!(Audience::Single("a".into()).contains("a"));
        assert!(!Audience::Single("a".into()).contains("b"));
        let multi = Audience::Multiple(vec!["a".into(), "b".into()]);
        assert!(multi.contains("b"));
        assert!(!multi.contains("c"));
    }

    #[test]
    fn audience_deserializes_string_or_array() {
        let s: Audience = serde_json::from_str(r#""api""#).unwrap();
        assert_eq!(s, Audience::Single("api".into()));
        let m: Audience = serde_json::from_str(r#"["a","b"]"#).unwrap();
        assert_eq!(m, Audience::Multiple(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn bearer_extraction_is_case_insensitive() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer  abc "), Some("abc"));
        assert_eq!(bearer_token("BEARER xyz"), Some("xyz"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("token"), None);
    }

    #[test]
    fn decode_claims_reads_payload_without_verifying() {
        // {"sub":"u1","aud":"api"} as base64url, dummy header/sig.
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub":"u1","aud":"api"}"#);
        let token = format!("aGVhZGVy.{payload}.c2ln");

        #[derive(Deserialize)]
        struct Claims {
            sub: String,
            aud: Audience,
        }
        let claims: Claims = decode_claims_unverified(&token).unwrap();
        assert_eq!(claims.sub, "u1");
        assert!(claims.aud.contains("api"));
    }

    #[test]
    fn malformed_tokens_rejected() {
        assert_eq!(split("a.b"), None);
        assert_eq!(split("a.b.c.d"), None);
        assert_eq!(
            decode_claims_unverified::<serde_json::Value>("a.b"),
            Err(JwtDecodeError::Malformed)
        );
    }
}
