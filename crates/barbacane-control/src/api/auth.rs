//! Admin authentication for the control-plane API.
//!
//! The control plane is an administrative surface: every mutating route can
//! create projects, upload plugin WASM, deploy artifacts, and mint data-plane
//! API keys. It must never be reachable without a credential.
//!
//! A single shared admin bearer token is required on all routes except
//! `/health` (liveness) and `/ws/data-plane` (which authenticates with its own
//! per-data-plane API key). The token is supplied via the
//! `BARBACANE_CONTROL_ADMIN_TOKEN` environment variable; the server refuses to
//! start without it (fail-closed). Comparison is done over SHA-256 digests so
//! request handling does not leak the token through timing.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};

use crate::error::ProblemDetails;

/// API-key scopes (`api_keys.scopes`). A key being valid for a project is not
/// sufficient — it must also carry the scope required by the action it performs.
pub mod scopes {
    /// Permits a data plane to register and connect over `/ws/data-plane`.
    pub const DATA_PLANE_CONNECT: &str = "data-plane:connect";
}

/// Whether an API key's granted `scopes` include `required`.
pub fn key_has_scope(scopes: &[String], required: &str) -> bool {
    scopes.iter().any(|s| s == required)
}

/// Shortest admin token accepted, in bytes: a token is the one credential
/// to every administrative operation, so a short one is refused at start.
pub const MIN_ADMIN_TOKEN_BYTES: usize = 32;

/// Authentication policy applied to protected control-plane routes.
#[derive(Clone)]
pub enum AdminAuth {
    /// Require a bearer token whose SHA-256 digest is one of these: the
    /// current token, and the previous one while a rotation is under way.
    Token(Arc<Vec<[u8; 32]>>),
    /// Authentication disabled. **Test only** — the production binary never
    /// constructs this variant; `barbacane-control serve` always uses
    /// [`AdminAuth::from_token`] and fails to start without a token. Only
    /// reachable from `#[cfg(test)]` harnesses, hence the scoped allow.
    #[cfg_attr(not(test), allow(dead_code))]
    Disabled,
}

impl AdminAuth {
    /// Build a policy that accepts `current`, and `previous` while clients
    /// move to the new token.
    pub fn from_tokens(current: &str, previous: Option<&str>) -> Self {
        let mut digests = vec![sha256(current.as_bytes())];
        if let Some(previous) = previous {
            digests.push(sha256(previous.as_bytes()));
        }
        AdminAuth::Token(Arc::new(digests))
    }

    /// The policy `BARBACANE_CONTROL_ADMIN_TOKEN` and the optional
    /// `BARBACANE_CONTROL_ADMIN_TOKEN_PREVIOUS` describe: each must be at
    /// least [`MIN_ADMIN_TOKEN_BYTES`] long, the current one must be set.
    pub fn from_env_values(current: Option<&str>, previous: Option<&str>) -> Result<Self, String> {
        let current = current
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or("BARBACANE_CONTROL_ADMIN_TOKEN must be set to a non-empty value; the control-plane API refuses to start unauthenticated")?;
        let previous = previous.map(str::trim).filter(|t| !t.is_empty());
        for (name, token) in [
            ("BARBACANE_CONTROL_ADMIN_TOKEN", Some(current)),
            ("BARBACANE_CONTROL_ADMIN_TOKEN_PREVIOUS", previous),
        ] {
            if token.is_some_and(|t| t.len() < MIN_ADMIN_TOKEN_BYTES) {
                return Err(format!(
                    "{name} is shorter than {MIN_ADMIN_TOKEN_BYTES} bytes; use a token of at least that length, such as `openssl rand -hex 32`"
                ));
            }
        }
        Ok(Self::from_tokens(current, previous))
    }

    /// Whether `presented` is an acceptable credential under this policy.
    /// Every accepted digest is compared, so the time taken does not say
    /// which one matched.
    pub(crate) fn accepts(&self, presented: &str) -> bool {
        match self {
            AdminAuth::Disabled => true,
            AdminAuth::Token(expected) => {
                let digest = sha256(presented.as_bytes());
                expected
                    .iter()
                    .fold(false, |found, candidate| ct_eq(&digest, candidate) | found)
            }
        }
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Constant-time comparison of two fixed-size digests.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Axum middleware enforcing the admin bearer token.
pub async fn require_admin(
    State(auth): State<AdminAuth>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if let AdminAuth::Disabled = auth {
        return next.run(req).await;
    }

    let authorized = bearer_token(req.headers()).is_some_and(|token| auth.accepts(token));

    if authorized {
        next.run(req).await
    } else {
        unauthorized("a valid admin bearer token is required")
    }
}

/// The bearer token of `headers`, if any.
pub(crate) fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// A 401 problem response with `detail`, asking for a bearer token.
pub(crate) fn unauthorized(detail: &str) -> Response {
    let mut response = ProblemDetails {
        error_type: "urn:barbacane:error:unauthorized".into(),
        title: "Unauthorized".into(),
        status: StatusCode::UNAUTHORIZED.as_u16(),
        detail: Some(detail.into()),
        instance: None,
        errors: vec![],
    }
    .into_response();
    response.headers_mut().insert(
        axum::http::header::WWW_AUTHENTICATE,
        axum::http::HeaderValue::from_static("Bearer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: &str = "current-admin-token-of-thirty-two-bytes!";
    const PREVIOUS: &str = "previous-admin-token-of-thirty-two-bytes";

    #[test]
    fn a_previous_token_is_accepted_during_a_rotation() {
        let auth = AdminAuth::from_tokens(CURRENT, Some(PREVIOUS));
        assert!(auth.accepts(CURRENT));
        assert!(auth.accepts(PREVIOUS));
        assert!(!auth.accepts("some-other-token-of-thirty-two-bytes!!"));
        assert!(!AdminAuth::from_tokens(CURRENT, None).accepts(PREVIOUS));
    }

    #[test]
    fn a_short_or_missing_token_refuses_to_start() {
        assert!(AdminAuth::from_env_values(None, None).is_err());
        assert!(AdminAuth::from_env_values(Some("  "), None).is_err());
        assert!(AdminAuth::from_env_values(Some("short"), None).is_err());
        assert!(AdminAuth::from_env_values(Some(CURRENT), Some("short")).is_err());
        assert!(AdminAuth::from_env_values(Some(CURRENT), None).is_ok());
        assert!(AdminAuth::from_env_values(Some(CURRENT), Some(PREVIOUS)).is_ok());
    }

    #[test]
    fn key_has_scope_matches_exactly() {
        let scopes = vec!["data-plane:connect".to_string(), "project:read".to_string()];
        assert!(key_has_scope(&scopes, scopes::DATA_PLANE_CONNECT));
        assert!(key_has_scope(&scopes, "project:read"));
        // No partial / prefix matching.
        assert!(!key_has_scope(&scopes, "data-plane"));
        assert!(!key_has_scope(&scopes, "project:write"));
        // A key without the scope is rejected.
        assert!(!key_has_scope(
            &["project:read".to_string()],
            scopes::DATA_PLANE_CONNECT
        ));
        assert!(!key_has_scope(&[], scopes::DATA_PLANE_CONNECT));
    }
}
