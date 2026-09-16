//! LDAP client for the Barbacane gateway.
//!
//! Backs the `host_ldap_bind` and `host_ldap_search` host functions. Search
//! connections, bound as a service account, are cached by URL and bind
//! identity. Credential-verification binds use a fresh connection each time,
//! so a user's bind never shares a connection with another identity. A
//! dedicated tokio runtime drives the connections between calls.

use crate::ldap::{
    LdapBindRequest, LdapConnection, LdapEntry, LdapError, LdapResult, LdapScope, LdapSearchRequest,
};
use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry, SearchOptions};
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::time::Duration;

/// Default port for `ldap://` URLs.
const DEFAULT_LDAP_PORT: u16 = 389;

/// Default port for `ldaps://` URLs.
const DEFAULT_LDAPS_PORT: u16 = 636;

/// Upper bound on cached search connections, so a plugin can't force unbounded
/// connection growth through distinct URL or bind-identity strings.
const MAX_LDAP_CONNECTIONS: usize = 256;

/// Timeout for establishing a connection (TCP, TLS, StartTLS).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-operation timeout when the request does not set one.
const DEFAULT_OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound for a request-supplied operation timeout.
const MAX_OP_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on entries returned by a single search.
pub const MAX_SEARCH_ENTRIES: u32 = 1000;

/// Upper bound on the serialized size of the entries returned by a search.
const MAX_RESULT_BYTES: usize = 1024 * 1024;

/// LDAP result code for invalidCredentials (RFC 4511 Appendix A).
const RC_INVALID_CREDENTIALS: u32 = 49;

/// LDAP result code for sizeLimitExceeded; the entries received before the
/// limit are still valid.
const RC_SIZE_LIMIT_EXCEEDED: u32 = 4;

/// Cache key for a pooled search connection. The password is reduced to a
/// fingerprint so the key never holds it in clear.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConnKey {
    url: String,
    bind_dn: String,
    password_fingerprint: u64,
}

impl ConnKey {
    fn from_connection(conn: &LdapConnection) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        conn.password.hash(&mut hasher);
        conn.starttls.hash(&mut hasher);
        Self {
            url: conn.url.clone(),
            bind_dn: conn.bind_dn.clone(),
            password_fingerprint: hasher.finish(),
        }
    }
}

/// LDAP client with a bounded cache of service-account connections.
///
/// Owns a dedicated tokio runtime so that `ldap3` connection drivers stay alive
/// between calls. Search connections are created lazily and reused while they
/// stay open; a closed connection is evicted and re-established on next use.
pub struct LdapClient {
    runtime: tokio::runtime::Runtime,
    connections: Mutex<HashMap<ConnKey, ldap3::Ldap>>,
    /// When false, directory addresses resolving to internal/metadata ranges are
    /// rejected (SSRF guard). Operators opt in for trusted internal directories.
    allow_internal_egress: bool,
}

impl LdapClient {
    /// Create a new LDAP client with its own background runtime.
    pub fn new(allow_internal_egress: bool) -> Result<Self, LdapError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("ldap-runtime")
            .enable_all()
            .build()
            .map_err(|e| LdapError::ConnectionFailed(format!("failed to create runtime: {e}")))?;
        Ok(Self {
            runtime,
            connections: Mutex::new(HashMap::new()),
            allow_internal_egress,
        })
    }

    /// Blocking bind for use from sync WASM host functions.
    ///
    /// Must be called from a thread that is NOT inside a tokio runtime context
    /// (e.g. from within `std::thread::scope`).
    pub fn bind_blocking(&self, req: &LdapBindRequest) -> Result<LdapResult, LdapError> {
        self.runtime.block_on(self.bind(req))
    }

    /// Blocking search for use from sync WASM host functions.
    ///
    /// Must be called from a thread that is NOT inside a tokio runtime context
    /// (e.g. from within `std::thread::scope`).
    pub fn search_blocking(&self, req: &LdapSearchRequest) -> Result<LdapResult, LdapError> {
        self.runtime.block_on(self.search(req))
    }

    /// Verify credentials with a simple bind on a fresh connection.
    async fn bind(&self, req: &LdapBindRequest) -> Result<LdapResult, LdapError> {
        if req.conn.bind_dn.is_empty() {
            return Err(LdapError::InvalidRequest("bind_dn is required".into()));
        }
        let timeout = op_timeout(req.conn.timeout_ms);
        let mut ldap = self.connect(&req.conn).await?;
        let outcome = simple_bind(&mut ldap, &req.conn, timeout).await;
        // The connection carried a user identity; never keep it around.
        let _ = ldap.unbind().await;
        outcome.map(|()| LdapResult::bound())
    }

    /// Search on a pooled service-account connection.
    async fn search(&self, req: &LdapSearchRequest) -> Result<LdapResult, LdapError> {
        if req.base_dn.is_empty() {
            return Err(LdapError::InvalidRequest("base_dn is required".into()));
        }
        if req.filter.is_empty() {
            return Err(LdapError::InvalidRequest("filter is required".into()));
        }

        let timeout = op_timeout(req.conn.timeout_ms);
        let key = ConnKey::from_connection(&req.conn);
        let mut ldap = self.get_or_connect(&key, &req.conn).await?;

        let scope = match req.scope {
            LdapScope::Base => Scope::Base,
            LdapScope::One => Scope::OneLevel,
            LdapScope::Sub => Scope::Subtree,
        };
        let size_limit = req
            .size_limit
            .unwrap_or(MAX_SEARCH_ENTRIES)
            .min(MAX_SEARCH_ENTRIES);
        let attrs: Vec<&str> = if req.attributes.is_empty() {
            vec!["*"]
        } else {
            req.attributes.iter().map(String::as_str).collect()
        };
        let options = SearchOptions::new()
            .sizelimit(size_limit as i32)
            .timelimit(timeout.as_secs().max(1) as i32);

        let outcome = ldap
            .with_timeout(timeout)
            .with_search_options(options)
            .search(&req.base_dn, scope, &req.filter, attrs)
            .await;

        let ldap3::SearchResult(raw_entries, result) = match outcome {
            Ok(r) => r,
            Err(e) => {
                // The connection may be dead; drop it so the next call reconnects.
                self.evict(&key);
                return Err(map_search_error(e));
            }
        };
        if result.rc != 0 && result.rc != RC_SIZE_LIMIT_EXCEEDED {
            return Err(LdapError::SearchFailed(format!(
                "rc={} {}",
                result.rc, result.text
            )));
        }

        let mut entries = Vec::with_capacity(raw_entries.len());
        let mut bytes = 0usize;
        for raw in raw_entries {
            let entry = SearchEntry::construct(raw);
            let attrs: BTreeMap<String, Vec<String>> = entry.attrs.into_iter().collect();
            bytes += entry.dn.len()
                + attrs
                    .iter()
                    .map(|(k, v)| k.len() + v.iter().map(String::len).sum::<usize>())
                    .sum::<usize>();
            if bytes > MAX_RESULT_BYTES {
                return Err(LdapError::SearchFailed(format!(
                    "result exceeds {MAX_RESULT_BYTES} bytes"
                )));
            }
            entries.push(LdapEntry {
                dn: entry.dn,
                attrs,
            });
        }

        Ok(LdapResult::entries(entries))
    }

    /// Get a cached, still-open connection bound as the service account, or
    /// establish and bind a new one.
    async fn get_or_connect(
        &self,
        key: &ConnKey,
        conn: &LdapConnection,
    ) -> Result<ldap3::Ldap, LdapError> {
        // Check cache (lock is held briefly, no await while locked). A closed
        // handle is evicted so it is replaced below.
        {
            let mut conns = self.connections.lock();
            // `is_closed` needs `&mut Ldap`; take the answer and a clone in one
            // step so no borrow of the map outlives this expression.
            let cached = conns
                .get_mut(key)
                .map(|ldap| (ldap.is_closed(), ldap.clone()));
            match cached {
                Some((false, ldap)) => return Ok(ldap),
                Some((true, _)) => {
                    conns.remove(key);
                }
                None => {}
            }
        }

        let mut ldap = self.connect(conn).await?;
        if !conn.bind_dn.is_empty() {
            simple_bind(&mut ldap, conn, DEFAULT_OP_TIMEOUT).await?;
        }

        // Cache the new connection, bounding the cache size.
        {
            let mut conns = self.connections.lock();
            if conns.len() >= MAX_LDAP_CONNECTIONS && !conns.contains_key(key) {
                return Err(LdapError::ConnectionFailed(
                    "LDAP connection cache is full".to_string(),
                ));
            }
            conns.insert(key.clone(), ldap.clone());
        }

        Ok(ldap)
    }

    fn evict(&self, key: &ConnKey) {
        self.connections.lock().remove(key);
    }

    /// Open a connection (TCP, then TLS or StartTLS as requested) without binding.
    async fn connect(&self, conn: &LdapConnection) -> Result<ldap3::Ldap, LdapError> {
        let url = conn.url.trim();
        let is_tls = if url.starts_with("ldaps://") {
            true
        } else if url.starts_with("ldap://") {
            false
        } else {
            return Err(LdapError::InvalidRequest(format!(
                "url must start with ldap:// or ldaps://, got '{url}'"
            )));
        };
        let default_port = if is_tls {
            DEFAULT_LDAPS_PORT
        } else {
            DEFAULT_LDAP_PORT
        };

        // SSRF guard: resolve once and refuse internal/metadata targets unless the
        // operator has opted into internal egress. The vetted addresses are kept
        // so a plaintext connection can be pinned to them.
        let (host, port) = crate::broker::split_host_port(url, default_port);
        let addrs = match crate::http_client::resolve_permitted_addrs(
            &host,
            port,
            self.allow_internal_egress,
        )
        .await
        {
            Ok(a) => a,
            Err(crate::http_client::HostGuardError::Blocked(h)) => {
                return Err(LdapError::Blocked(h));
            }
            Err(crate::http_client::HostGuardError::Resolve(m)) => {
                return Err(LdapError::ConnectionFailed(m));
            }
        };

        // Pin plaintext `ldap://` connections to the vetted IP. For `ldaps://`
        // and StartTLS the hostname is kept so TLS SNI and certificate
        // validation work; the pre-connect resolution above already blocked
        // internal targets.
        let connect_url = if is_tls || conn.starttls {
            url.to_string()
        } else {
            let addr = addrs.first().ok_or_else(|| {
                LdapError::ConnectionFailed(format!("no address resolved for {host}"))
            })?;
            format!("ldap://{addr}")
        };

        let settings = LdapConnSettings::new()
            .set_conn_timeout(CONNECT_TIMEOUT)
            .set_starttls(conn.starttls);
        let (driver, ldap) = tokio::time::timeout(
            CONNECT_TIMEOUT,
            LdapConnAsync::with_settings(settings, &connect_url),
        )
        .await
        .map_err(|_| LdapError::Timeout)?
        .map_err(|e| LdapError::ConnectionFailed(e.to_string()))?;
        ldap3::drive!(driver);

        tracing::info!(url = %url, "established LDAP connection");
        Ok(ldap)
    }
}

/// Clamp a request-supplied timeout into `[1ms, MAX_OP_TIMEOUT]`, defaulting
/// to `DEFAULT_OP_TIMEOUT` when absent.
fn op_timeout(timeout_ms: Option<u64>) -> Duration {
    match timeout_ms {
        Some(ms) => Duration::from_millis(ms.max(1)).min(MAX_OP_TIMEOUT),
        None => DEFAULT_OP_TIMEOUT,
    }
}

/// Simple bind, mapping the directory's result code to `LdapError`.
async fn simple_bind(
    ldap: &mut ldap3::Ldap,
    conn: &LdapConnection,
    timeout: Duration,
) -> Result<(), LdapError> {
    let result = ldap
        .with_timeout(timeout)
        .simple_bind(&conn.bind_dn, &conn.password)
        .await
        .map_err(map_bind_error)?;
    match result.rc {
        0 => Ok(()),
        RC_INVALID_CREDENTIALS => Err(LdapError::InvalidCredentials),
        rc => Err(LdapError::BindFailed(format!("rc={rc} {}", result.text))),
    }
}

fn map_bind_error(e: ldap3::LdapError) -> LdapError {
    match e {
        ldap3::LdapError::Timeout { .. } => LdapError::Timeout,
        ldap3::LdapError::LdapResult { result } if result.rc == RC_INVALID_CREDENTIALS => {
            LdapError::InvalidCredentials
        }
        ldap3::LdapError::LdapResult { result } => {
            LdapError::BindFailed(format!("rc={} {}", result.rc, result.text))
        }
        other => LdapError::ConnectionFailed(other.to_string()),
    }
}

fn map_search_error(e: ldap3::LdapError) -> LdapError {
    match e {
        ldap3::LdapError::Timeout { .. } => LdapError::Timeout,
        ldap3::LdapError::FilterParsing => {
            LdapError::InvalidRequest("malformed search filter".into())
        }
        ldap3::LdapError::LdapResult { result } => {
            LdapError::SearchFailed(format!("rc={} {}", result.rc, result.text))
        }
        other => LdapError::ConnectionFailed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(url: &str) -> LdapConnection {
        LdapConnection {
            url: url.to_string(),
            bind_dn: "cn=svc,dc=example,dc=org".to_string(),
            password: "secret".to_string(),
            starttls: false,
            timeout_ms: Some(1000),
        }
    }

    fn bind(url: &str) -> LdapBindRequest {
        LdapBindRequest { conn: conn(url) }
    }

    fn search(url: &str) -> LdapSearchRequest {
        LdapSearchRequest {
            conn: conn(url),
            base_dn: "dc=example,dc=org".to_string(),
            scope: LdapScope::Sub,
            filter: "(uid=alice)".to_string(),
            attributes: vec!["mail".to_string()],
            size_limit: None,
        }
    }

    #[test]
    fn client_starts_empty() {
        let client = LdapClient::new(true).expect("ldap client");
        assert!(client.connections.lock().is_empty());
    }

    #[test]
    fn blocks_internal_directory_when_egress_disallowed() {
        let client = LdapClient::new(false).expect("ldap client");
        let result = client.bind_blocking(&bind("ldap://169.254.169.254:389"));
        assert!(matches!(result, Err(LdapError::Blocked(_))));
    }

    #[test]
    fn bind_connection_refused() {
        let client = LdapClient::new(true).expect("ldap client");
        let result = client.bind_blocking(&bind("ldap://127.0.0.1:13389"));
        assert!(matches!(result, Err(LdapError::ConnectionFailed(_))));
    }

    #[test]
    fn search_connection_refused_leaves_cache_empty() {
        let client = LdapClient::new(true).expect("ldap client");
        let result = client.search_blocking(&search("ldap://127.0.0.1:13389"));
        assert!(matches!(result, Err(LdapError::ConnectionFailed(_))));
        assert!(client.connections.lock().is_empty());
    }

    #[test]
    fn rejects_unknown_scheme() {
        let client = LdapClient::new(true).expect("ldap client");
        let result = client.bind_blocking(&bind("http://127.0.0.1:389"));
        assert!(matches!(result, Err(LdapError::InvalidRequest(_))));
    }

    #[test]
    fn rejects_empty_bind_dn_and_search_fields() {
        let client = LdapClient::new(true).expect("ldap client");

        let mut anonymous = bind("ldap://127.0.0.1:13389");
        anonymous.conn.bind_dn.clear();
        assert!(matches!(
            client.bind_blocking(&anonymous),
            Err(LdapError::InvalidRequest(_))
        ));

        let mut no_filter = search("ldap://127.0.0.1:13389");
        no_filter.filter.clear();
        assert!(matches!(
            client.search_blocking(&no_filter),
            Err(LdapError::InvalidRequest(_))
        ));

        let mut no_base = search("ldap://127.0.0.1:13389");
        no_base.base_dn.clear();
        assert!(matches!(
            client.search_blocking(&no_base),
            Err(LdapError::InvalidRequest(_))
        ));
    }

    #[test]
    fn bind_blocking_from_thread_scope() {
        let client = LdapClient::new(true).expect("ldap client");
        let result = std::thread::scope(|s| {
            s.spawn(|| client.bind_blocking(&bind("ldap://127.0.0.1:13389")))
                .join()
                .unwrap()
        });
        assert!(matches!(result, Err(LdapError::ConnectionFailed(_))));
    }

    #[test]
    fn op_timeout_is_clamped() {
        assert_eq!(op_timeout(None), DEFAULT_OP_TIMEOUT);
        assert_eq!(op_timeout(Some(0)), Duration::from_millis(1));
        assert_eq!(op_timeout(Some(250)), Duration::from_millis(250));
        assert_eq!(op_timeout(Some(600_000)), MAX_OP_TIMEOUT);
    }

    #[test]
    fn conn_key_separates_identities_and_hides_password() {
        let a = ConnKey::from_connection(&conn("ldap://h:389"));
        let mut other_pw = conn("ldap://h:389");
        other_pw.password = "different".to_string();
        let b = ConnKey::from_connection(&other_pw);
        assert_ne!(a, b);
        assert_eq!(a.url, b.url);
        assert_eq!(a.bind_dn, b.bind_dn);
        let debug = format!("{a:?}");
        assert!(!debug.contains("secret"));
    }
}
