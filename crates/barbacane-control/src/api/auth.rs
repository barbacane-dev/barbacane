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

/// Authentication policy applied to protected control-plane routes.
#[derive(Clone)]
pub enum AdminAuth {
    /// Require a bearer token whose SHA-256 digest matches this value.
    Token(Arc<[u8; 32]>),
    /// Authentication disabled. **Test only** — the production binary never
    /// constructs this variant; `barbacane-control serve` always uses
    /// [`AdminAuth::from_token`] and fails to start without a token. Only
    /// reachable from `#[cfg(test)]` harnesses, hence the scoped allow.
    #[cfg_attr(not(test), allow(dead_code))]
    Disabled,
}

impl AdminAuth {
    /// Build a policy that requires the given token.
    pub fn from_token(token: &str) -> Self {
        AdminAuth::Token(Arc::new(sha256(token.as_bytes())))
    }

    /// Whether `presented` is an acceptable credential under this policy.
    fn accepts(&self, presented: &str) -> bool {
        match self {
            AdminAuth::Disabled => true,
            AdminAuth::Token(expected) => ct_eq(&sha256(presented.as_bytes()), expected),
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

    let authorized = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|token| auth.accepts(token))
        .unwrap_or(false);

    if authorized {
        next.run(req).await
    } else {
        let mut response = ProblemDetails {
            error_type: "urn:barbacane:error:unauthorized".into(),
            title: "Unauthorized".into(),
            status: StatusCode::UNAUTHORIZED.as_u16(),
            detail: Some("a valid admin bearer token is required".into()),
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
}
