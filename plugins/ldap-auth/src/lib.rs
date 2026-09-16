//! LDAP / Active Directory authentication middleware plugin for Barbacane API gateway.
//!
//! Validates `Authorization: Basic` credentials (RFC 7617) against a directory:
//! the user entry is located with a search, the password is verified with a
//! bind as that entry, and group membership is read from the user entry or a
//! group search. Rejected requests get 401 with a `WWW-Authenticate` challenge;
//! a directory failure gets 503 and is never cached.

use barbacane_plugin_sdk::context;
use barbacane_plugin_sdk::ldap::{self, Connection, Entry, LdapError, SearchRequest};
use barbacane_plugin_sdk::log;
use barbacane_plugin_sdk::prelude::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Upper bound on cached credentials, so a credential-stuffing run cannot grow
/// plugin memory without limit.
const MAX_CACHE_ENTRIES: usize = 10_000;

/// Identity headers this plugin owns. Any client-supplied value is removed
/// before the verified identity is written.
const IDENTITY_HEADERS: [&str; 4] = [
    "x-auth-consumer",
    "x-auth-consumer-groups",
    "x-auth-user",
    "x-auth-dn",
];

/// LDAP authentication middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct LdapAuth {
    /// Directory URL: `ldap://host[:port]` or `ldaps://host[:port]`.
    url: String,

    /// Service-account DN for the user and group searches. Empty means anonymous.
    #[serde(default)]
    bind_dn: String,

    /// Service-account password. Supports secret references (e.g. `env://LDAP_BIND_PASSWORD`).
    #[serde(default)]
    bind_password: String,

    /// Upgrade a plaintext connection with StartTLS before binding.
    #[serde(default)]
    starttls: bool,

    /// Send passwords over a plaintext `ldap://` connection without StartTLS.
    /// The host refuses such binds unless this is set.
    #[serde(default)]
    allow_plaintext: bool,

    /// Base DN of the user search.
    user_base_dn: String,

    /// User search filter; `{username}` is replaced with the escaped username.
    #[serde(default = "default_user_filter")]
    user_filter: String,

    /// Attribute of the user entry used as `x-auth-consumer`.
    #[serde(default = "default_user_attr")]
    user_attr: String,

    /// Attribute of the user entry holding group DNs (used when `group_base_dn` is empty).
    #[serde(default = "default_group_attr")]
    group_attr: String,

    /// Base DN of a group search. When set, groups come from matching group entries.
    #[serde(default)]
    group_base_dn: String,

    /// Group search filter; `{dn}` and `{username}` are replaced with escaped values.
    #[serde(default = "default_group_filter")]
    group_filter: String,

    /// Attribute naming a group entry in group-search mode.
    #[serde(default = "default_group_name_attr")]
    group_name_attr: String,

    /// Emit the first RDN value of each group DN instead of the full DN.
    #[serde(default = "default_true")]
    group_name_from_dn: bool,

    /// Groups the user must belong to (any of). Empty means no requirement.
    #[serde(default)]
    required_groups: Vec<String>,

    /// Per-operation timeout in seconds.
    #[serde(default = "default_timeout")]
    timeout: f64,

    /// Cache TTL for verified and rejected credentials. 0 disables caching.
    #[serde(default = "default_cache_ttl")]
    cache_ttl_seconds: u64,

    /// Realm shown in the WWW-Authenticate challenge.
    #[serde(default = "default_realm")]
    realm: String,

    /// Remove the Authorization header before forwarding to upstream.
    #[serde(default = "default_true")]
    strip_credentials: bool,

    /// Credential cache keyed by username.
    #[serde(skip)]
    cache: HashMap<String, CacheEntry>,
}

fn default_user_filter() -> String {
    "(uid={username})".to_string()
}

fn default_user_attr() -> String {
    "uid".to_string()
}

fn default_group_attr() -> String {
    "memberOf".to_string()
}

fn default_group_filter() -> String {
    "(member={dn})".to_string()
}

fn default_group_name_attr() -> String {
    "cn".to_string()
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> f64 {
    5.0
}

fn default_cache_ttl() -> u64 {
    60
}

fn default_realm() -> String {
    "api".to_string()
}

/// A verified user.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    consumer: String,
    dn: String,
    groups: Vec<String>,
}

/// A remembered verification outcome for one username. `None` is a rejected
/// credential.
struct CacheEntry {
    password_hash: [u8; 32],
    outcome: Option<Identity>,
    expires_at_ms: u64,
}

/// Authentication failure.
#[derive(Debug)]
enum AuthError {
    MissingAuthHeader,
    InvalidAuthHeader,
    InvalidBase64,
    InvalidCredentialFormat,
    InvalidCredentials,
    Forbidden,
    DirectoryUnavailable(String),
}

impl AuthError {
    fn code(&self) -> &'static str {
        match self {
            AuthError::MissingAuthHeader => "missing_credentials",
            AuthError::InvalidAuthHeader
            | AuthError::InvalidBase64
            | AuthError::InvalidCredentialFormat => "invalid_request",
            AuthError::InvalidCredentials => "invalid_credentials",
            AuthError::Forbidden => "insufficient_group",
            AuthError::DirectoryUnavailable(_) => "directory_unavailable",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            AuthError::MissingAuthHeader => "Basic credentials required",
            AuthError::InvalidAuthHeader => "Invalid Authorization header format",
            AuthError::InvalidBase64 => "Invalid base64 encoding in credentials",
            AuthError::InvalidCredentialFormat => {
                "Invalid credentials format (expected user:password)"
            }
            AuthError::InvalidCredentials => "Invalid username or password",
            AuthError::Forbidden => "User is not a member of a required group",
            AuthError::DirectoryUnavailable(_) => "Directory authentication is unavailable",
        }
    }
}

impl LdapAuth {
    /// Handle incoming request: authenticate against the directory.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        let (username, identity) = match self.authenticate(&req) {
            Ok(v) => v,
            Err(e) => return Action::ShortCircuit(self.error_response(&e)),
        };

        let mut modified = req;
        if self.strip_credentials {
            modified.headers.remove("authorization");
            modified.headers.remove("Authorization");
        }
        // Drop any client-supplied identity headers first, so a spoofed
        // x-auth-consumer-groups cannot survive when the directory returns no
        // groups and be trusted by acl downstream.
        modified.headers.retain(|name, _| {
            !IDENTITY_HEADERS
                .iter()
                .any(|h| name.eq_ignore_ascii_case(h))
        });
        // Identity headers for acl and upstreams, and the same identity in the
        // request context for plugins that read `context:auth.*`.
        context::set(context::AUTH_SUB, &identity.consumer);
        modified
            .headers
            .insert("x-auth-consumer".to_string(), identity.consumer);
        modified.headers.insert("x-auth-user".to_string(), username);
        modified
            .headers
            .insert("x-auth-dn".to_string(), identity.dn);
        if !identity.groups.is_empty() {
            let groups = identity.groups.join(",");
            context::set(context::AUTH_GROUPS, &groups);
            modified
                .headers
                .insert("x-auth-consumer-groups".to_string(), groups);
        }
        Action::Continue(modified)
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Extract, verify (or recall from cache) and authorize the credentials.
    fn authenticate(&mut self, req: &Request) -> Result<(String, Identity), AuthError> {
        let (username, password) = extract_credentials(req)?;
        // RFC 4513 §5.1.2: a bind with an empty password is an anonymous bind
        // and would succeed for any DN, so it can never count as verification.
        if password.is_empty() {
            return Err(AuthError::InvalidCredentials);
        }

        let now = clock::now_ms();
        let password_hash = hash_password(&password);

        let outcome = match self.cache_lookup(&username, &password_hash, now) {
            Some(cached) => cached,
            None => match self.verify(&username, &password) {
                Ok(identity) => {
                    self.cache_store(&username, password_hash, Some(identity.clone()), now);
                    Some(identity)
                }
                Err(AuthError::InvalidCredentials) => {
                    self.cache_store(&username, password_hash, None, now);
                    None
                }
                Err(e) => return Err(e),
            },
        };

        let identity = outcome.ok_or(AuthError::InvalidCredentials)?;
        if !self.required_groups.is_empty()
            && !identity
                .groups
                .iter()
                .any(|g| self.required_groups.iter().any(|r| r == g))
        {
            return Err(AuthError::Forbidden);
        }
        Ok((username, identity))
    }

    /// Locate the user, bind as it, and resolve its groups.
    fn verify(&self, username: &str, password: &str) -> Result<Identity, AuthError> {
        let service = self.service_connection();
        let filter = self
            .user_filter
            .replace("{username}", &ldap::escape_filter_value(username));
        let request = SearchRequest::new(service.clone(), &self.user_base_dn, filter)
            .attributes([self.user_attr.clone(), self.group_attr.clone()])
            .size_limit(2);
        let entries = directory::search(&request).map_err(map_search_error)?;
        let entry = match entries.as_slice() {
            [entry] => entry,
            [] => return Err(AuthError::InvalidCredentials),
            _ => {
                log::warn(&format!(
                    "ldap-auth: user filter matched several entries under {}",
                    self.user_base_dn
                ));
                return Err(AuthError::InvalidCredentials);
            }
        };

        let user = self.connection(&entry.dn, password);
        directory::bind(&user).map_err(map_bind_error)?;

        let consumer = entry
            .first(&self.user_attr)
            .map(str::to_string)
            .unwrap_or_else(|| username.to_string());
        let groups = self.resolve_groups(&service, entry, username)?;
        Ok(Identity {
            consumer,
            dn: entry.dn.clone(),
            groups,
        })
    }

    /// Groups from the user entry's membership attribute, or from a group search.
    fn resolve_groups(
        &self,
        service: &Connection,
        user: &Entry,
        username: &str,
    ) -> Result<Vec<String>, AuthError> {
        let mut groups: Vec<String> = if self.group_base_dn.is_empty() {
            user.values(&self.group_attr)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|g| self.group_name(g))
                .collect()
        } else {
            let filter = self
                .group_filter
                .replace("{dn}", &ldap::escape_filter_value(&user.dn))
                .replace("{username}", &ldap::escape_filter_value(username));
            let request = SearchRequest::new(service.clone(), &self.group_base_dn, filter)
                .attributes([self.group_name_attr.clone()]);
            let entries = directory::search(&request).map_err(map_search_error)?;
            entries
                .iter()
                .map(|e| {
                    e.first(&self.group_name_attr)
                        .map(str::to_string)
                        .unwrap_or_else(|| self.group_name(&e.dn))
                })
                .collect()
        };
        groups.retain(|g| !g.is_empty());
        groups.sort();
        groups.dedup();
        Ok(groups)
    }

    /// Reduce a group DN to its first RDN value when configured.
    fn group_name(&self, dn_or_name: &str) -> String {
        if self.group_name_from_dn {
            first_rdn_value(dn_or_name).unwrap_or_else(|| dn_or_name.to_string())
        } else {
            dn_or_name.to_string()
        }
    }

    fn service_connection(&self) -> Connection {
        self.connection(&self.bind_dn, &self.bind_password)
    }

    fn connection(&self, bind_dn: &str, password: &str) -> Connection {
        let mut conn = Connection::new(&self.url, bind_dn, password)
            .starttls(self.starttls)
            .allow_plaintext(self.allow_plaintext);
        let timeout_ms = (self.timeout * 1000.0).max(0.0) as u64;
        if timeout_ms > 0 {
            conn = conn.timeout_ms(timeout_ms);
        }
        conn
    }

    fn cache_lookup(
        &mut self,
        username: &str,
        password_hash: &[u8; 32],
        now: u64,
    ) -> Option<Option<Identity>> {
        if self.cache_ttl_seconds == 0 {
            return None;
        }
        let entry = self.cache.get(username)?;
        if entry.expires_at_ms <= now {
            self.cache.remove(username);
            return None;
        }
        if !constant_time_eq(&entry.password_hash, password_hash) {
            return None;
        }
        Some(entry.outcome.clone())
    }

    fn cache_store(
        &mut self,
        username: &str,
        password_hash: [u8; 32],
        outcome: Option<Identity>,
        now: u64,
    ) {
        if self.cache_ttl_seconds == 0 {
            return;
        }
        if self.cache.len() >= MAX_CACHE_ENTRIES && !self.cache.contains_key(username) {
            self.cache.retain(|_, e| e.expires_at_ms > now);
            if self.cache.len() >= MAX_CACHE_ENTRIES {
                return;
            }
        }
        self.cache.insert(
            username.to_string(),
            CacheEntry {
                password_hash,
                outcome,
                expires_at_ms: now.saturating_add(self.cache_ttl_seconds.saturating_mul(1000)),
            },
        );
    }

    /// Build the error response: 401 with a Basic challenge, 403, or 503.
    fn error_response(&self, error: &AuthError) -> Response {
        match error {
            AuthError::Forbidden => {
                ProblemDetails::new(403, "urn:barbacane:error:forbidden", "Forbidden")
                    .detail(error.description())
                    .into_response()
            }
            AuthError::DirectoryUnavailable(reason) => {
                log::error(&format!("ldap-auth: directory unavailable: {reason}"));
                ProblemDetails::new(
                    503,
                    "urn:barbacane:error:ldap-unavailable",
                    "Service Unavailable",
                )
                .detail(error.description())
                .into_response()
            }
            _ => {
                let mut resp = ProblemDetails::new(
                    401,
                    "urn:barbacane:error:authentication-failed",
                    "Authentication failed",
                )
                .detail(error.description())
                .into_response();
                // RFC 7617 challenge; the realm is operator-configured and quoted.
                resp.headers.insert(
                    "www-authenticate".to_string(),
                    format!(
                        "Basic realm=\"{}\", error=\"{}\", error_description=\"{}\"",
                        self.realm.replace('"', "'"),
                        error.code(),
                        error.description()
                    ),
                );
                resp
            }
        }
    }
}

/// Extract username and password from the Authorization header (RFC 7617).
fn extract_credentials(req: &Request) -> Result<(String, String), AuthError> {
    let auth_header = req
        .headers
        .get("authorization")
        .or_else(|| req.headers.get("Authorization"))
        .ok_or(AuthError::MissingAuthHeader)?;

    let encoded = auth_header
        .strip_prefix("Basic ")
        .or_else(|| auth_header.strip_prefix("basic "))
        .ok_or(AuthError::InvalidAuthHeader)?
        .trim();

    let decoded_bytes = STANDARD
        .decode(encoded)
        .map_err(|_| AuthError::InvalidBase64)?;
    let decoded = String::from_utf8(decoded_bytes).map_err(|_| AuthError::InvalidBase64)?;

    // Split on the first ':' so a password may contain colons.
    let (username, password) = decoded
        .split_once(':')
        .ok_or(AuthError::InvalidCredentialFormat)?;
    if username.is_empty() {
        return Err(AuthError::InvalidCredentialFormat);
    }
    Ok((username.to_string(), password.to_string()))
}

fn hash_password(password: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(password.as_bytes()));
    out
}

fn map_bind_error(e: LdapError) -> AuthError {
    match e {
        LdapError::InvalidCredentials => AuthError::InvalidCredentials,
        other => AuthError::DirectoryUnavailable(format!("bind: {other:?}")),
    }
}

fn map_search_error(e: LdapError) -> AuthError {
    AuthError::DirectoryUnavailable(format!("search: {e:?}"))
}

/// Value of the first RDN of `dn`, unescaped (RFC 4514). `None` when `dn` has
/// no `attribute=value` form.
fn first_rdn_value(dn: &str) -> Option<String> {
    let dn = dn.trim_start();
    let mut end = dn.len();
    let mut escaped = false;
    for (i, c) in dn.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            ',' | ';' | '+' => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let (_, value) = dn[..end].split_once('=')?;
    Some(unescape_dn_value(value.trim()))
}

/// Undo RFC 4514 escaping: `\XX` hex pairs and `\c` single-character escapes.
fn unescape_dn_value(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if i + 2 < bytes.len() {
                if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push(((h << 4) | l) as u8);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i + 1]);
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Host access: the SDK on wasm, an in-memory directory on native targets so
// on_request is unit-testable end to end.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod directory {
    use barbacane_plugin_sdk::ldap::{self, Connection, Entry, LdapError, SearchRequest};

    pub fn bind(conn: &Connection) -> Result<(), LdapError> {
        ldap::bind(conn)
    }

    pub fn search(req: &SearchRequest) -> Result<Vec<Entry>, LdapError> {
        ldap::search(req)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod directory {
    use barbacane_plugin_sdk::ldap::{Connection, Entry, LdapError, SearchRequest};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Clone)]
    pub struct MockEntry {
        pub dn: String,
        pub password: String,
        pub attrs: BTreeMap<String, Vec<String>>,
    }

    #[derive(Default)]
    pub struct MockDirectory {
        pub entries: Vec<MockEntry>,
        /// When set, every call fails with a clone of this error.
        pub failure: Option<LdapError>,
        pub bind_calls: u32,
        pub search_calls: u32,
        pub last_filter: String,
    }

    thread_local! {
        pub static DIRECTORY: RefCell<MockDirectory> = RefCell::new(MockDirectory::default());
    }

    pub fn bind(conn: &Connection) -> Result<(), LdapError> {
        DIRECTORY.with(|d| {
            let mut d = d.borrow_mut();
            d.bind_calls += 1;
            if let Some(err) = &d.failure {
                return Err(err.clone());
            }
            let ok = d
                .entries
                .iter()
                .any(|e| e.dn.eq_ignore_ascii_case(&conn.bind_dn) && e.password == conn.password);
            if ok {
                Ok(())
            } else {
                Err(LdapError::InvalidCredentials)
            }
        })
    }

    pub fn search(req: &SearchRequest) -> Result<Vec<Entry>, LdapError> {
        DIRECTORY.with(|d| {
            let mut d = d.borrow_mut();
            d.search_calls += 1;
            d.last_filter = req.filter.clone();
            if let Some(err) = &d.failure {
                return Err(err.clone());
            }
            let (attr, value) = parse_equality_filter(&req.filter).ok_or_else(|| {
                LdapError::InvalidRequest("mock supports (attr=value) only".into())
            })?;
            let base = req.base_dn.to_ascii_lowercase();
            let limit = req.size_limit.unwrap_or(u32::MAX) as usize;
            let entries = d
                .entries
                .iter()
                .filter(|e| e.dn.to_ascii_lowercase().ends_with(&base))
                .filter(|e| {
                    e.attrs.iter().any(|(k, vs)| {
                        k.eq_ignore_ascii_case(&attr) && vs.iter().any(|v| v == &value)
                    })
                })
                .take(limit)
                .map(|e| Entry {
                    dn: e.dn.clone(),
                    attrs: if req.attributes.is_empty() {
                        e.attrs.clone()
                    } else {
                        e.attrs
                            .iter()
                            .filter(|(k, _)| {
                                req.attributes.iter().any(|a| a.eq_ignore_ascii_case(k))
                            })
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    },
                })
                .collect();
            Ok(entries)
        })
    }

    /// `(attr=value)` with RFC 4515 escapes decoded in `value`.
    fn parse_equality_filter(filter: &str) -> Option<(String, String)> {
        let inner = filter.strip_prefix('(')?.strip_suffix(')')?;
        let (attr, value) = inner.split_once('=')?;
        let value = value
            .replace("\\2a", "*")
            .replace("\\28", "(")
            .replace("\\29", ")")
            .replace("\\5c", "\\")
            .replace("\\00", "\0");
        Some((attr.to_string(), value))
    }
}

#[cfg(target_arch = "wasm32")]
mod clock {
    pub fn now_ms() -> u64 {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_clock_now() -> i64;
        }
        unsafe { host_clock_now().max(0) as u64 }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod clock {
    use std::cell::Cell;

    thread_local! {
        pub static NOW_MS: Cell<u64> = const { Cell::new(1_000_000) };
    }

    pub fn now_ms() -> u64 {
        NOW_MS.with(|n| n.get())
    }
}

#[cfg(test)]
mod tests {
    use super::directory::{MockEntry, DIRECTORY};
    use super::*;
    use std::collections::BTreeMap;

    const BASE: &str = "dc=example,dc=org";

    fn attrs(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|v| v.to_string()).collect()))
            .collect()
    }

    /// Reset the mock directory with alice (admins, dev) and bob (no groups),
    /// plus two group entries for group-search mode.
    fn seed_directory() {
        DIRECTORY.with(|d| {
            let mut d = d.borrow_mut();
            *d = directory::MockDirectory::default();
            d.entries = vec![
                MockEntry {
                    dn: format!("uid=alice,ou=people,{BASE}"),
                    password: "wonderland".into(),
                    attrs: attrs(&[
                        ("uid", &["alice"]),
                        ("mail", &["alice@example.org"]),
                        (
                            "memberOf",
                            &[
                                &format!("cn=admins,ou=groups,{BASE}"),
                                &format!("cn=dev,ou=groups,{BASE}"),
                            ],
                        ),
                    ]),
                },
                MockEntry {
                    dn: format!("uid=bob,ou=people,{BASE}"),
                    password: "builder".into(),
                    attrs: attrs(&[("uid", &["bob"])]),
                },
                MockEntry {
                    dn: format!("cn=admins,ou=groups,{BASE}"),
                    password: String::new(),
                    attrs: attrs(&[
                        ("cn", &["admins"]),
                        ("member", &[&format!("uid=alice,ou=people,{BASE}")]),
                    ]),
                },
                MockEntry {
                    dn: format!("cn=dev,ou=groups,{BASE}"),
                    password: String::new(),
                    attrs: attrs(&[
                        ("cn", &["dev"]),
                        ("member", &[&format!("uid=alice,ou=people,{BASE}")]),
                    ]),
                },
            ];
        });
        clock::NOW_MS.with(|n| n.set(1_000_000));
        context::clear();
    }

    fn calls() -> (u32, u32) {
        DIRECTORY.with(|d| {
            let d = d.borrow();
            (d.search_calls, d.bind_calls)
        })
    }

    fn set_failure(err: Option<LdapError>) {
        DIRECTORY.with(|d| d.borrow_mut().failure = err);
    }

    fn plugin() -> LdapAuth {
        serde_json::from_value(serde_json::json!({
            "url": "ldap://directory.example.org:389",
            "bind_dn": format!("cn=svc,ou=services,{BASE}"),
            "bind_password": "svc-secret",
            "user_base_dn": format!("ou=people,{BASE}"),
            "realm": "test-api"
        }))
        .expect("config")
    }

    fn basic(user: &str, pass: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{user}:{pass}")))
    }

    fn request(auth: Option<&str>) -> Request {
        let mut headers = BTreeMap::new();
        if let Some(a) = auth {
            headers.insert("authorization".to_string(), a.to_string());
        }
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            query: None,
            headers,
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        }
    }

    fn expect_continue(action: Action<Request>) -> Request {
        match action {
            Action::Continue(r) => r,
            Action::ShortCircuit(resp) => panic!("expected Continue, got {}", resp.status),
        }
    }

    fn expect_short_circuit(action: Action<Request>) -> Response {
        match action {
            Action::ShortCircuit(r) => r,
            Action::Continue(_) => panic!("expected ShortCircuit"),
        }
    }

    // ==================== config ====================

    #[test]
    fn config_defaults() {
        let p: LdapAuth =
            serde_json::from_str(r#"{"url":"ldap://d","user_base_dn":"dc=x"}"#).unwrap();
        assert_eq!(p.user_filter, "(uid={username})");
        assert_eq!(p.user_attr, "uid");
        assert_eq!(p.group_attr, "memberOf");
        assert_eq!(p.group_filter, "(member={dn})");
        assert_eq!(p.group_name_attr, "cn");
        assert!(p.group_name_from_dn);
        assert!(p.required_groups.is_empty());
        assert_eq!(p.timeout, 5.0);
        assert_eq!(p.cache_ttl_seconds, 60);
        assert_eq!(p.realm, "api");
        assert!(p.strip_credentials);
        assert!(!p.starttls);
        assert!(!p.allow_plaintext);
        assert!(p.bind_dn.is_empty());
    }

    #[test]
    fn config_requires_url_and_base_dn() {
        assert!(serde_json::from_str::<LdapAuth>(r#"{"url":"ldap://d"}"#).is_err());
        assert!(serde_json::from_str::<LdapAuth>(r#"{"user_base_dn":"dc=x"}"#).is_err());
    }

    // ==================== credential extraction ====================

    #[test]
    fn missing_header_is_401_with_challenge() {
        seed_directory();
        let mut p = plugin();
        let resp = expect_short_circuit(p.on_request(request(None)));
        assert_eq!(resp.status, 401);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/problem+json"
        );
        let challenge = resp.headers.get("www-authenticate").unwrap();
        assert!(challenge.starts_with("Basic realm=\"test-api\""));
        assert!(challenge.contains("missing_credentials"));
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:authentication-failed");
        assert_eq!(calls(), (0, 0));
    }

    #[test]
    fn bearer_and_malformed_headers_are_rejected_without_directory_calls() {
        seed_directory();
        let mut p = plugin();
        for header in [
            "Bearer tok",
            "Basic !!!",
            &format!("Basic {}", STANDARD.encode("nocolon")),
        ] {
            let resp = expect_short_circuit(p.on_request(request(Some(header))));
            assert_eq!(resp.status, 401);
            assert!(resp
                .headers
                .get("www-authenticate")
                .unwrap()
                .contains("invalid_request"));
        }
        assert_eq!(calls(), (0, 0));
    }

    #[test]
    fn empty_password_is_rejected_before_any_bind() {
        seed_directory();
        let mut p = plugin();
        let resp = expect_short_circuit(p.on_request(request(Some(&basic("alice", "")))));
        assert_eq!(resp.status, 401);
        assert!(resp
            .headers
            .get("www-authenticate")
            .unwrap()
            .contains("invalid_credentials"));
        assert_eq!(calls(), (0, 0));
    }

    // ==================== success path ====================

    #[test]
    fn valid_credentials_set_identity_headers_and_strip_authorization() {
        seed_directory();
        let mut p = plugin();
        let req = expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert!(!req.headers.contains_key("authorization"));
        assert_eq!(req.headers.get("x-auth-consumer").unwrap(), "alice");
        assert_eq!(req.headers.get("x-auth-user").unwrap(), "alice");
        assert_eq!(
            req.headers.get("x-auth-dn").unwrap(),
            &format!("uid=alice,ou=people,{BASE}")
        );
        assert_eq!(
            req.headers.get("x-auth-consumer-groups").unwrap(),
            "admins,dev"
        );
        assert_eq!(context::get(context::AUTH_SUB).as_deref(), Some("alice"));
        assert_eq!(
            context::get(context::AUTH_GROUPS).as_deref(),
            Some("admins,dev")
        );
        assert_eq!(calls(), (1, 1));
    }

    #[test]
    fn user_without_groups_gets_no_groups_header() {
        seed_directory();
        let mut p = plugin();
        let req = expect_continue(p.on_request(request(Some(&basic("bob", "builder")))));
        assert_eq!(req.headers.get("x-auth-consumer").unwrap(), "bob");
        assert!(!req.headers.contains_key("x-auth-consumer-groups"));
    }

    #[test]
    fn client_supplied_identity_headers_are_replaced_not_trusted() {
        seed_directory();
        let mut p = plugin();
        // bob has no groups; a spoofed groups header must not survive, and the
        // other identity headers must reflect the directory, not the client.
        let mut req = request(Some(&basic("bob", "builder")));
        req.headers
            .insert("X-Auth-Consumer-Groups".to_string(), "admins".to_string());
        req.headers
            .insert("x-auth-consumer".to_string(), "alice".to_string());
        req.headers
            .insert("X-Auth-DN".to_string(), "uid=alice,dc=evil".to_string());
        let out = expect_continue(p.on_request(req));
        assert!(!out
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("x-auth-consumer-groups")));
        assert_eq!(out.headers.get("x-auth-consumer").unwrap(), "bob");
        assert_eq!(
            out.headers.get("x-auth-dn").unwrap(),
            &format!("uid=bob,ou=people,{BASE}")
        );
        assert!(!out.headers.contains_key("X-Auth-DN"));
    }

    #[test]
    fn authorization_is_kept_when_strip_disabled() {
        seed_directory();
        let mut p = plugin();
        p.strip_credentials = false;
        let req = expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert!(req.headers.contains_key("authorization"));
    }

    #[test]
    fn consumer_falls_back_to_username_when_attribute_missing() {
        seed_directory();
        let mut p = plugin();
        p.user_attr = "employeeNumber".into();
        let req = expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(req.headers.get("x-auth-consumer").unwrap(), "alice");
    }

    #[test]
    fn group_search_mode_reads_group_entries() {
        seed_directory();
        let mut p = plugin();
        p.group_base_dn = format!("ou=groups,{BASE}");
        let req = expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(
            req.headers.get("x-auth-consumer-groups").unwrap(),
            "admins,dev"
        );
        let filter = DIRECTORY.with(|d| d.borrow().last_filter.clone());
        assert_eq!(filter, format!("(member=uid=alice,ou=people,{BASE})"));
        assert_eq!(calls(), (2, 1));
    }

    #[test]
    fn full_group_dns_when_rdn_reduction_disabled() {
        seed_directory();
        let mut p = plugin();
        p.group_name_from_dn = false;
        let req = expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(
            req.headers.get("x-auth-consumer-groups").unwrap(),
            &format!("cn=admins,ou=groups,{BASE},cn=dev,ou=groups,{BASE}")
        );
    }

    // ==================== rejection paths ====================

    #[test]
    fn wrong_password_and_unknown_user_are_indistinguishable() {
        seed_directory();
        let mut p = plugin();
        let wrong = expect_short_circuit(p.on_request(request(Some(&basic("alice", "nope")))));
        let unknown = expect_short_circuit(p.on_request(request(Some(&basic("carol", "nope")))));
        assert_eq!(wrong.status, 401);
        assert_eq!(unknown.status, 401);
        assert_eq!(wrong.headers, unknown.headers);
        assert_eq!(wrong.body, unknown.body);
        let body: serde_json::Value = serde_json::from_slice(wrong.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["detail"], "Invalid username or password");
    }

    #[test]
    fn required_groups_enforced_with_403() {
        seed_directory();
        let mut p = plugin();
        p.required_groups = vec!["ops".into(), "admins".into()];
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));

        p.required_groups = vec!["ops".into()];
        let resp = expect_short_circuit(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(resp.status, 403);
        assert!(!resp.headers.contains_key("www-authenticate"));
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:forbidden");
    }

    #[test]
    fn directory_failure_is_503_and_not_cached() {
        seed_directory();
        set_failure(Some(LdapError::Directory(
            "timeout".into(),
            "timeout".into(),
        )));
        let mut p = plugin();
        let resp = expect_short_circuit(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(resp.status, 503);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:ldap-unavailable");
        assert!(!body["detail"].as_str().unwrap().contains("timeout"));

        set_failure(None);
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(calls(), (2, 1));
    }

    #[test]
    fn ambiguous_user_match_is_rejected() {
        seed_directory();
        DIRECTORY.with(|d| {
            let mut d = d.borrow_mut();
            let dup = MockEntry {
                dn: format!("uid=alice,ou=contractors,ou=people,{BASE}"),
                password: "other".into(),
                attrs: attrs(&[("uid", &["alice"])]),
            };
            d.entries.push(dup);
        });
        let mut p = plugin();
        let resp = expect_short_circuit(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(resp.status, 401);
        assert_eq!(calls(), (1, 0));
    }

    // ==================== injection ====================

    #[test]
    fn username_is_escaped_before_reaching_the_filter() {
        seed_directory();
        let mut p = plugin();
        let resp =
            expect_short_circuit(p.on_request(request(Some(&basic("*)(uid=*", "wonderland")))));
        assert_eq!(resp.status, 401);
        let filter = DIRECTORY.with(|d| d.borrow().last_filter.clone());
        assert_eq!(filter, "(uid=\\2a\\29\\28uid=\\2a)");
        assert_eq!(calls(), (1, 0));
    }

    // ==================== cache ====================

    #[test]
    fn verified_credentials_are_cached_until_ttl() {
        seed_directory();
        let mut p = plugin();
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(calls(), (1, 1));

        clock::NOW_MS.with(|n| n.set(n.get() + 61_000));
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(calls(), (2, 2));
    }

    #[test]
    fn rejected_credentials_are_cached_too() {
        seed_directory();
        let mut p = plugin();
        for _ in 0..3 {
            let resp = expect_short_circuit(p.on_request(request(Some(&basic("alice", "nope")))));
            assert_eq!(resp.status, 401);
        }
        assert_eq!(calls(), (1, 1));
    }

    #[test]
    fn different_password_bypasses_cached_outcome() {
        seed_directory();
        let mut p = plugin();
        expect_short_circuit(p.on_request(request(Some(&basic("alice", "nope")))));
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(calls(), (2, 2));
    }

    #[test]
    fn cache_disabled_when_ttl_is_zero() {
        seed_directory();
        let mut p = plugin();
        p.cache_ttl_seconds = 0;
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(calls(), (2, 2));
        assert!(p.cache.is_empty());
    }

    #[test]
    fn required_groups_are_checked_on_cached_identities() {
        seed_directory();
        let mut p = plugin();
        expect_continue(p.on_request(request(Some(&basic("alice", "wonderland")))));
        p.required_groups = vec!["ops".into()];
        let resp = expect_short_circuit(p.on_request(request(Some(&basic("alice", "wonderland")))));
        assert_eq!(resp.status, 403);
        assert_eq!(calls(), (1, 1));
    }

    // ==================== helpers ====================

    #[test]
    fn first_rdn_value_handles_escapes_and_case() {
        assert_eq!(
            first_rdn_value("cn=admins,ou=groups,dc=x").as_deref(),
            Some("admins")
        );
        assert_eq!(
            first_rdn_value("CN=Smith\\, John,OU=People,DC=x").as_deref(),
            Some("Smith, John")
        );
        assert_eq!(
            first_rdn_value("ou=superheros,dc=glauth,dc=com").as_deref(),
            Some("superheros")
        );
        assert_eq!(first_rdn_value("cn=a\\2bb,dc=x").as_deref(), Some("a+b"));
        assert_eq!(
            first_rdn_value("cn=multi+sn=valued,dc=x").as_deref(),
            Some("multi")
        );
        assert_eq!(first_rdn_value("no-equals-here"), None);
    }

    #[test]
    fn on_response_passes_through() {
        let mut p = plugin();
        let resp = Response {
            status: 204,
            headers: BTreeMap::new(),
            body: None,
        };
        assert_eq!(p.on_response(resp).status, 204);
    }
}
