//! LDAP directory access via the `host_ldap_bind` / `host_ldap_search` imports
//! (capability `ldap`).
//!
//! Requests carry their own connection parameters, so the host holds no plugin
//! configuration. Values taken from user input must be escaped with
//! [`escape_filter_value`] before they are placed in a filter, and with
//! [`escape_dn_value`] before they are placed in a DN.
//!
//! On non-wasm targets (unit tests) [`bind`] and [`search`] return
//! [`LdapError::Unsupported`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Connection parameters shared by bind and search.
#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    /// `ldap://host[:port]` or `ldaps://host[:port]`.
    pub url: String,
    /// DN to bind as; empty for an anonymous bind.
    pub bind_dn: String,
    /// Password for `bind_dn`.
    pub password: String,
    /// Upgrade a plaintext connection with StartTLS before binding.
    pub starttls: bool,
    /// Send a password over a plaintext `ldap://` connection without StartTLS.
    /// The host refuses such binds unless this is set.
    pub allow_plaintext: bool,
    /// Per-operation timeout in milliseconds (clamped by the host).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl Connection {
    /// Connection to `url` bound as `bind_dn` / `password`.
    pub fn new(
        url: impl Into<String>,
        bind_dn: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            bind_dn: bind_dn.into(),
            password: password.into(),
            starttls: false,
            allow_plaintext: false,
            timeout_ms: None,
        }
    }

    /// Request StartTLS on a plaintext connection.
    pub fn starttls(mut self, on: bool) -> Self {
        self.starttls = on;
        self
    }

    /// Allow a password to cross a plaintext `ldap://` connection.
    pub fn allow_plaintext(mut self, on: bool) -> Self {
        self.allow_plaintext = on;
        self
    }

    /// Set the per-operation timeout in milliseconds.
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

/// Search scope (RFC 4511 §4.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Base,
    One,
    Sub,
}

/// A search request.
#[derive(Debug, Clone, Serialize)]
pub struct SearchRequest {
    #[serde(flatten)]
    pub conn: Connection,
    pub base_dn: String,
    pub scope: Scope,
    /// RFC 4515 filter; escape interpolated values with [`escape_filter_value`].
    pub filter: String,
    /// Attributes to return; empty means all user attributes.
    pub attributes: Vec<String>,
    /// Maximum entries to return (clamped by the host).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_limit: Option<u32>,
}

impl SearchRequest {
    /// Subtree search under `base_dn` with `filter`, returning `attributes`.
    pub fn new(conn: Connection, base_dn: impl Into<String>, filter: impl Into<String>) -> Self {
        Self {
            conn,
            base_dn: base_dn.into(),
            scope: Scope::Sub,
            filter: filter.into(),
            attributes: Vec::new(),
            size_limit: None,
        }
    }

    pub fn scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    pub fn attributes<I, S>(mut self, attrs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.attributes = attrs.into_iter().map(Into::into).collect();
        self
    }

    pub fn size_limit(mut self, limit: u32) -> Self {
        self.size_limit = Some(limit);
        self
    }
}

/// One directory entry. Binary attributes are not returned.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub dn: String,
    #[serde(default)]
    pub attrs: BTreeMap<String, Vec<String>>,
}

impl Entry {
    /// First value of `attr`, matched case-insensitively (attribute names are
    /// case-insensitive in LDAP).
    pub fn first(&self, attr: &str) -> Option<&str> {
        self.values(attr)
            .and_then(|v| v.first())
            .map(String::as_str)
    }

    /// All values of `attr`, matched case-insensitively.
    pub fn values(&self, attr: &str) -> Option<&Vec<String>> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(attr))
            .map(|(_, v)| v)
    }
}

/// Failure modes of [`bind`] and [`search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LdapError {
    /// The directory rejected the DN/password pair.
    InvalidCredentials,
    /// The host rejected the request (`code` = `invalid_request`).
    InvalidRequest(String),
    /// Any other directory-side failure: `(code, message)` from the host, where
    /// `code` is `connection_failed`, `bind_failed`, `search_failed`,
    /// `timeout` or `blocked`.
    Directory(String, String),
    /// The host could not run the call (`host_ldap_*` < 0).
    Unreachable,
    /// The host returned an empty result.
    Empty,
    /// Reading the result buffer returned an unexpected length.
    ReadFailed,
    /// The result was not valid JSON for this call.
    InvalidResponse,
    /// Called on a non-wasm target (no host available).
    Unsupported,
}

impl LdapError {
    /// True when the failure is on the directory side rather than a rejected
    /// credential or a caller mistake.
    pub fn is_directory_failure(&self) -> bool {
        matches!(
            self,
            LdapError::Directory(..)
                | LdapError::Unreachable
                | LdapError::Empty
                | LdapError::ReadFailed
                | LdapError::InvalidResponse
        )
    }
}

/// Result shape written by the host for both calls.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Deserialize)]
struct HostResult {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    entries: Vec<Entry>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl HostResult {
    fn into_result(self) -> Result<Vec<Entry>, LdapError> {
        if self.success {
            return Ok(self.entries);
        }
        let message = self.error.unwrap_or_default();
        match self.code.as_deref() {
            Some("invalid_credentials") => Err(LdapError::InvalidCredentials),
            Some("invalid_request") => Err(LdapError::InvalidRequest(message)),
            Some(code) => Err(LdapError::Directory(code.to_string(), message)),
            None => Err(LdapError::Directory("unknown".to_string(), message)),
        }
    }
}

/// Escape a value for use inside an RFC 4515 filter assertion.
///
/// `*`, `(`, `)`, `\` and NUL become `\2a`, `\28`, `\29`, `\5c`, `\00`.
pub fn escape_filter_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'*' => out.push_str("\\2a"),
            b'(' => out.push_str("\\28"),
            b')' => out.push_str("\\29"),
            b'\\' => out.push_str("\\5c"),
            0 => out.push_str("\\00"),
            _ => out.push(b as char),
        }
    }
    // Non-ASCII bytes were pushed as Latin-1 chars above; rebuild from bytes
    // so multi-byte UTF-8 survives untouched.
    if value.is_ascii() {
        out
    } else {
        let mut bytes = Vec::with_capacity(value.len() + 8);
        for b in value.bytes() {
            match b {
                b'*' => bytes.extend_from_slice(b"\\2a"),
                b'(' => bytes.extend_from_slice(b"\\28"),
                b')' => bytes.extend_from_slice(b"\\29"),
                b'\\' => bytes.extend_from_slice(b"\\5c"),
                0 => bytes.extend_from_slice(b"\\00"),
                _ => bytes.push(b),
            }
        }
        String::from_utf8(bytes).unwrap_or_default()
    }
}

/// Escape an attribute value for use inside an RFC 4514 distinguished name.
///
/// Escapes `,` `+` `"` `\` `<` `>` `;`, a leading `#` or space, a trailing
/// space, and NUL.
pub fn escape_dn_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    let chars: Vec<char> = value.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let leading = i == 0;
        let trailing = i + 1 == chars.len();
        match c {
            ',' | '+' | '"' | '\\' | '<' | '>' | ';' => {
                out.push('\\');
                out.push(c);
            }
            '#' if leading => out.push_str("\\#"),
            ' ' if leading || trailing => out.push_str("\\ "),
            '\0' => out.push_str("\\00"),
            _ => out.push(c),
        }
    }
    out
}

/// Verify `conn.bind_dn` / `conn.password` with a simple bind on a fresh
/// connection.
#[cfg(target_arch = "wasm32")]
pub fn bind(conn: &Connection) -> Result<(), LdapError> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_ldap_bind(req_ptr: i32, req_len: i32) -> i32;
    }
    let serialized = serde_json::to_vec(conn).map_err(|_| LdapError::InvalidResponse)?;
    let len = unsafe { host_ldap_bind(serialized.as_ptr() as i32, serialized.len() as i32) };
    read_result(len).map(|_| ())
}

/// Run a search on a pooled connection bound as `req.conn.bind_dn`.
#[cfg(target_arch = "wasm32")]
pub fn search(req: &SearchRequest) -> Result<Vec<Entry>, LdapError> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_ldap_search(req_ptr: i32, req_len: i32) -> i32;
    }
    let serialized = serde_json::to_vec(req).map_err(|_| LdapError::InvalidResponse)?;
    let len = unsafe { host_ldap_search(serialized.as_ptr() as i32, serialized.len() as i32) };
    read_result(len)
}

#[cfg(target_arch = "wasm32")]
fn read_result(result_len: i32) -> Result<Vec<Entry>, LdapError> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_ldap_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }
    if result_len < 0 {
        return Err(LdapError::Unreachable);
    }
    if result_len == 0 {
        return Err(LdapError::Empty);
    }
    let mut buf = vec![0u8; result_len as usize];
    let read = unsafe { host_ldap_read_result(buf.as_mut_ptr() as i32, result_len) };
    if read != result_len {
        return Err(LdapError::ReadFailed);
    }
    let result: HostResult =
        serde_json::from_slice(&buf).map_err(|_| LdapError::InvalidResponse)?;
    result.into_result()
}

/// Non-wasm stub: no host to call.
#[cfg(not(target_arch = "wasm32"))]
pub fn bind(_conn: &Connection) -> Result<(), LdapError> {
    Err(LdapError::Unsupported)
}

/// Non-wasm stub: no host to call.
#[cfg(not(target_arch = "wasm32"))]
pub fn search(_req: &SearchRequest) -> Result<Vec<Entry>, LdapError> {
    Err(LdapError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_serializes_flat_into_search_request() {
        let req = SearchRequest::new(
            Connection::new("ldap://dir:389", "cn=svc,dc=x", "pw").timeout_ms(2500),
            "ou=people,dc=x",
            "(uid=alice)",
        )
        .attributes(["memberOf", "mail"])
        .size_limit(2);
        let v: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["url"], "ldap://dir:389");
        assert_eq!(v["bind_dn"], "cn=svc,dc=x");
        assert_eq!(v["password"], "pw");
        assert_eq!(v["starttls"], false);
        assert_eq!(v["allow_plaintext"], false);
        assert_eq!(v["timeout_ms"], 2500);
        assert_eq!(v["base_dn"], "ou=people,dc=x");
        assert_eq!(v["scope"], "sub");
        assert_eq!(v["filter"], "(uid=alice)");
        assert_eq!(v["attributes"][1], "mail");
        assert_eq!(v["size_limit"], 2);
        assert!(v.get("conn").is_none());
    }

    #[test]
    fn optional_fields_are_omitted_when_unset() {
        let v: serde_json::Value =
            serde_json::to_value(Connection::new("ldap://d", "", "")).unwrap();
        assert!(v.get("timeout_ms").is_none());
        let v: serde_json::Value = serde_json::to_value(SearchRequest::new(
            Connection::new("ldap://d", "", ""),
            "dc=x",
            "(a=b)",
        ))
        .unwrap();
        assert!(v.get("size_limit").is_none());
    }

    #[test]
    fn host_result_maps_codes() {
        let ok: HostResult = serde_json::from_str(
            r#"{"success":true,"entries":[{"dn":"uid=a,dc=x","attrs":{"mail":["a@x"]}}]}"#,
        )
        .unwrap();
        let entries = ok.into_result().unwrap();
        assert_eq!(entries[0].dn, "uid=a,dc=x");
        assert_eq!(entries[0].first("MAIL"), Some("a@x"));

        let bad: HostResult =
            serde_json::from_str(r#"{"success":false,"code":"invalid_credentials","error":"x"}"#)
                .unwrap();
        assert_eq!(
            bad.into_result().unwrap_err(),
            LdapError::InvalidCredentials
        );

        let down: HostResult =
            serde_json::from_str(r#"{"success":false,"code":"timeout","error":"timeout"}"#)
                .unwrap();
        let err = down.into_result().unwrap_err();
        assert!(err.is_directory_failure());
        assert_eq!(
            err,
            LdapError::Directory("timeout".into(), "timeout".into())
        );

        let req: HostResult = serde_json::from_str(
            r#"{"success":false,"code":"invalid_request","error":"filter is required"}"#,
        )
        .unwrap();
        assert_eq!(
            req.into_result().unwrap_err(),
            LdapError::InvalidRequest("filter is required".into())
        );
    }

    #[test]
    fn filter_escaping_follows_rfc_4515() {
        assert_eq!(escape_filter_value("alice"), "alice");
        assert_eq!(escape_filter_value("*"), "\\2a");
        assert_eq!(escape_filter_value("a)(b"), "a\\29\\28b");
        assert_eq!(escape_filter_value("back\\slash"), "back\\5cslash");
        assert_eq!(escape_filter_value("nul\0"), "nul\\00");
        assert_eq!(escape_filter_value("héllo*"), "héllo\\2a");
        // The classic injection attempt becomes an inert literal.
        assert_eq!(
            escape_filter_value("*)(uid=*))(|(uid=*"),
            "\\2a\\29\\28uid=\\2a\\29\\29\\28|\\28uid=\\2a"
        );
    }

    #[test]
    fn dn_escaping_follows_rfc_4514() {
        assert_eq!(escape_dn_value("alice"), "alice");
        assert_eq!(escape_dn_value("Smith, John"), "Smith\\, John");
        assert_eq!(
            escape_dn_value("a+b\"c\\d<e>f;g"),
            "a\\+b\\\"c\\\\d\\<e\\>f\\;g"
        );
        assert_eq!(escape_dn_value("#hash"), "\\#hash");
        assert_eq!(escape_dn_value(" padded "), "\\ padded\\ ");
        assert_eq!(escape_dn_value("in side"), "in side");
    }

    #[test]
    fn calls_are_unsupported_on_native() {
        let conn = Connection::new("ldap://d", "cn=x", "pw");
        assert_eq!(bind(&conn).unwrap_err(), LdapError::Unsupported);
        assert_eq!(
            search(&SearchRequest::new(conn, "dc=x", "(a=b)")).unwrap_err(),
            LdapError::Unsupported
        );
    }
}
