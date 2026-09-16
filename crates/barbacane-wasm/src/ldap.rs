//! Shared types for the LDAP host functions (`host_ldap_bind`, `host_ldap_search`).
//!
//! Defines the request, result, and error types exchanged between plugins and
//! the `LdapClient` in `ldap_client.rs`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// Errors from LDAP operations.
#[derive(Debug, Error)]
pub enum LdapError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("invalid credentials")]
    InvalidCredentials,

    #[error("bind failed: {0}")]
    BindFailed(String),

    #[error("search failed: {0}")]
    SearchFailed(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("timeout")]
    Timeout,

    #[error("LDAP target blocked by SSRF policy: {0}")]
    Blocked(String),
}

impl LdapError {
    /// Stable machine-readable code carried in [`LdapResult::code`], so a
    /// plugin can map invalid credentials to 401 and everything else to 502.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConnectionFailed(_) => "connection_failed",
            Self::InvalidCredentials => "invalid_credentials",
            Self::BindFailed(_) => "bind_failed",
            Self::SearchFailed(_) => "search_failed",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Timeout => "timeout",
            Self::Blocked(_) => "blocked",
        }
    }
}

/// Connection parameters shared by bind and search requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LdapConnection {
    /// Directory URL: `ldap://host[:port]` or `ldaps://host[:port]`.
    pub url: String,

    /// DN to bind as. An empty string is an anonymous bind.
    #[serde(default)]
    pub bind_dn: String,

    /// Password for `bind_dn`.
    #[serde(default)]
    pub password: String,

    /// Upgrade a plaintext `ldap://` connection with StartTLS before binding.
    /// The connection fails closed when the server refuses the upgrade.
    #[serde(default)]
    pub starttls: bool,

    /// Per-operation timeout in milliseconds. Clamped by the host.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Request for `host_ldap_bind`: verify that `bind_dn` and `password` are
/// accepted by the directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LdapBindRequest {
    #[serde(flatten)]
    pub conn: LdapConnection,
}

/// Search scope (RFC 4511 §4.5.1).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LdapScope {
    Base,
    One,
    #[default]
    Sub,
}

/// Request for `host_ldap_search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LdapSearchRequest {
    #[serde(flatten)]
    pub conn: LdapConnection,

    /// Search base DN.
    pub base_dn: String,

    #[serde(default)]
    pub scope: LdapScope,

    /// RFC 4515 filter. Values interpolated from user input must already be
    /// escaped by the caller.
    pub filter: String,

    /// Attributes to return. Empty means all user attributes.
    #[serde(default)]
    pub attributes: Vec<String>,

    /// Maximum entries to return. Clamped by the host.
    #[serde(default)]
    pub size_limit: Option<u32>,
}

/// One directory entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct LdapEntry {
    pub dn: String,

    /// String-valued attributes. Binary attributes are omitted.
    #[serde(default)]
    pub attrs: BTreeMap<String, Vec<String>>,
}

/// Result of a bind or search, serialized for `host_ldap_read_result`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LdapResult {
    pub success: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// One of the [`LdapError::code`] values when `success` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<LdapEntry>,
}

impl LdapResult {
    /// A successful bind.
    pub fn bound() -> Self {
        Self {
            success: true,
            error: None,
            code: None,
            entries: Vec::new(),
        }
    }

    /// A successful search.
    pub fn entries(entries: Vec<LdapEntry>) -> Self {
        Self {
            success: true,
            error: None,
            code: None,
            entries,
        }
    }

    /// A failed operation.
    pub fn failure(error: &LdapError) -> Self {
        Self {
            success: false,
            error: Some(error.to_string()),
            code: Some(error.code().to_string()),
            entries: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_request_deserializes_with_defaults() {
        let req: LdapBindRequest = serde_json::from_str(
            r#"{"url":"ldap://ldap.example:389","bind_dn":"uid=alice,dc=example","password":"pw"}"#,
        )
        .expect("bind request");
        assert_eq!(req.conn.url, "ldap://ldap.example:389");
        assert!(!req.conn.starttls);
        assert_eq!(req.conn.timeout_ms, None);
    }

    #[test]
    fn search_request_scope_defaults_to_sub() {
        let req: LdapSearchRequest = serde_json::from_str(
            r#"{"url":"ldap://h","base_dn":"dc=example","filter":"(uid=alice)"}"#,
        )
        .expect("search request");
        assert_eq!(req.scope, LdapScope::Sub);
        assert!(req.attributes.is_empty());
        assert_eq!(req.size_limit, None);
        assert_eq!(req.conn.bind_dn, "");
    }

    #[test]
    fn search_request_scope_parses_lowercase_names() {
        for (name, scope) in [
            ("base", LdapScope::Base),
            ("one", LdapScope::One),
            ("sub", LdapScope::Sub),
        ] {
            let json = format!(
                r#"{{"url":"ldap://h","base_dn":"dc=x","filter":"(a=b)","scope":"{name}"}}"#
            );
            let req: LdapSearchRequest = serde_json::from_str(&json).expect("scope");
            assert_eq!(req.scope, scope);
        }
    }

    #[test]
    fn failure_result_carries_code_and_message() {
        let json = serde_json::to_value(LdapResult::failure(&LdapError::InvalidCredentials))
            .expect("serialize");
        assert_eq!(json["success"], false);
        assert_eq!(json["code"], "invalid_credentials");
        assert_eq!(json["error"], "invalid credentials");
        assert!(json.get("entries").is_none());
    }

    #[test]
    fn success_results_omit_error_fields() {
        let bound = serde_json::to_value(LdapResult::bound()).expect("serialize");
        assert_eq!(bound["success"], true);
        assert!(bound.get("error").is_none());
        assert!(bound.get("code").is_none());

        let entry = LdapEntry {
            dn: "uid=alice,dc=example".into(),
            attrs: BTreeMap::from([("mail".to_string(), vec!["a@example".to_string()])]),
        };
        let found = serde_json::to_value(LdapResult::entries(vec![entry])).expect("serialize");
        assert_eq!(found["entries"][0]["dn"], "uid=alice,dc=example");
        assert_eq!(found["entries"][0]["attrs"]["mail"][0], "a@example");
    }

    #[test]
    fn every_error_has_a_distinct_code() {
        let errors = [
            LdapError::ConnectionFailed("x".into()),
            LdapError::InvalidCredentials,
            LdapError::BindFailed("x".into()),
            LdapError::SearchFailed("x".into()),
            LdapError::InvalidRequest("x".into()),
            LdapError::Timeout,
            LdapError::Blocked("x".into()),
        ];
        let mut codes: Vec<&str> = errors.iter().map(LdapError::code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), errors.len());
    }
}
