//! LDAP client for the Barbacane gateway.
//!
//! Backs the `host_ldap_bind` and `host_ldap_search` host functions. Search
//! connections, bound as a service account, are cached per plugin, URL and bind
//! identity with least-recently-used eviction. Credential-verification binds
//! use a fresh connection each time, so a user's bind never shares a connection
//! with another identity. A password is sent over a plaintext `ldap://`
//! connection without StartTLS only when the request opts in. A dedicated
//! tokio runtime drives the connections between calls.

use crate::ldap::{
    LdapBindRequest, LdapConnection, LdapEntry, LdapError, LdapResult, LdapScope, LdapSearchRequest,
};
use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry, SearchOptions, StdStream};
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

/// Default port for `ldap://` URLs.
const DEFAULT_LDAP_PORT: u16 = 389;

/// Default port for `ldaps://` URLs.
const DEFAULT_LDAPS_PORT: u16 = 636;

/// Upper bound on cached search connections across all plugins.
const MAX_LDAP_CONNECTIONS: usize = 256;

/// Upper bound on cached search connections held by one plugin, so a single
/// plugin cannot crowd the others out of the cache.
const MAX_CONNECTIONS_PER_PLUGIN: usize = 32;

/// Timeout for establishing a connection (TCP, TLS, StartTLS).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-operation timeout when the request does not set one.
const DEFAULT_OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound for a request-supplied operation timeout.
const MAX_OP_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on entries returned by a single search.
pub const MAX_SEARCH_ENTRIES: u32 = 1000;

/// Upper bound on the serialized size of the entries returned by a search.
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;

/// LDAP result code for invalidCredentials (RFC 4511 Appendix A).
const RC_INVALID_CREDENTIALS: u32 = 49;

/// LDAP result code for sizeLimitExceeded; the entries received before the
/// limit are still valid.
const RC_SIZE_LIMIT_EXCEEDED: u32 = 4;

/// Cache key for a pooled search connection. The password is reduced to a
/// fingerprint so the key never holds it in clear.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConnKey {
    plugin: String,
    url: String,
    bind_dn: String,
    password_fingerprint: u64,
}

impl ConnKey {
    fn new(plugin: &str, conn: &LdapConnection) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        conn.password.hash(&mut hasher);
        conn.starttls.hash(&mut hasher);
        Self {
            plugin: plugin.to_string(),
            url: conn.url.clone(),
            bind_dn: conn.bind_dn.clone(),
            password_fingerprint: hasher.finish(),
        }
    }
}

/// A pooled connection with its last use, for least-recently-used eviction.
struct CachedConn {
    ldap: ldap3::Ldap,
    last_used: Instant,
}

/// LDAP client with a bounded, per-plugin partitioned cache of service-account
/// connections.
///
/// Owns a dedicated tokio runtime so that `ldap3` connection drivers stay alive
/// between calls. Search connections are created lazily and reused while they
/// stay open; a closed connection is evicted and re-established on next use.
pub struct LdapClient {
    runtime: tokio::runtime::Runtime,
    connections: Mutex<HashMap<ConnKey, CachedConn>>,
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

    /// Blocking search for use from sync WASM host functions. `plugin` names the
    /// calling plugin and partitions the connection cache.
    ///
    /// Must be called from a thread that is NOT inside a tokio runtime context
    /// (e.g. from within `std::thread::scope`).
    pub fn search_blocking(
        &self,
        plugin: &str,
        req: &LdapSearchRequest,
    ) -> Result<LdapResult, LdapError> {
        self.runtime.block_on(self.search(plugin, req))
    }

    /// Verify credentials with a simple bind on a fresh connection.
    async fn bind(&self, req: &LdapBindRequest) -> Result<LdapResult, LdapError> {
        if req.conn.bind_dn.is_empty() {
            return Err(LdapError::InvalidRequest("bind_dn is required".into()));
        }
        let is_tls = scheme_is_tls(&req.conn.url)?;
        check_transport(&req.conn, is_tls)?;

        let timeout = op_timeout(req.conn.timeout_ms);
        let mut ldap = self.connect(&req.conn, is_tls).await?;
        let outcome = simple_bind(&mut ldap, &req.conn, timeout).await;
        // The connection carried a user identity; never keep it around.
        let _ = ldap.unbind().await;
        outcome.map(|()| LdapResult::bound())
    }

    /// Search on a pooled service-account connection, enforcing the entry and
    /// byte caps while entries arrive.
    async fn search(&self, plugin: &str, req: &LdapSearchRequest) -> Result<LdapResult, LdapError> {
        if req.base_dn.is_empty() {
            return Err(LdapError::InvalidRequest("base_dn is required".into()));
        }
        if req.filter.is_empty() {
            return Err(LdapError::InvalidRequest("filter is required".into()));
        }
        let is_tls = scheme_is_tls(&req.conn.url)?;
        check_transport(&req.conn, is_tls)?;

        let timeout = op_timeout(req.conn.timeout_ms);
        let key = ConnKey::new(plugin, &req.conn);
        let mut ldap = self
            .get_or_connect(&key, &req.conn, is_tls, timeout)
            .await?;

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

        let mut stream = match ldap
            .with_timeout(timeout)
            .with_search_options(options)
            .streaming_search(&req.base_dn, scope, &req.filter, attrs)
            .await
        {
            Ok(stream) => stream,
            Err(e) => {
                // The connection may be dead; drop it so the next call reconnects.
                self.evict(&key);
                return Err(map_search_error(e));
            }
        };

        // Consume entries one at a time so a server that ignores the requested
        // size limit, or pads entries, cannot grow host memory past the caps.
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        loop {
            match stream.next().await {
                Ok(Some(raw)) => {
                    let entry = SearchEntry::construct(raw);
                    let attrs: BTreeMap<String, Vec<String>> = entry.attrs.into_iter().collect();
                    bytes += entry.dn.len()
                        + attrs
                            .iter()
                            .map(|(k, v)| k.len() + v.iter().map(String::len).sum::<usize>())
                            .sum::<usize>();
                    if entries.len() >= size_limit as usize || bytes > MAX_RESULT_BYTES {
                        // Unread messages remain on this connection; discard it.
                        drop(stream);
                        self.evict(&key);
                        return Err(LdapError::SearchFailed(format!(
                            "result exceeds {size_limit} entries or {MAX_RESULT_BYTES} bytes"
                        )));
                    }
                    entries.push(LdapEntry {
                        dn: entry.dn,
                        attrs,
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    drop(stream);
                    self.evict(&key);
                    return Err(map_search_error(e));
                }
            }
        }

        let result = stream.finish().await;
        if result.rc != 0 && result.rc != RC_SIZE_LIMIT_EXCEEDED {
            return Err(LdapError::SearchFailed(format!(
                "rc={} {}",
                result.rc, result.text
            )));
        }

        Ok(LdapResult::entries(entries))
    }

    /// Get a cached, still-open connection bound as the service account, or
    /// establish and bind a new one.
    async fn get_or_connect(
        &self,
        key: &ConnKey,
        conn: &LdapConnection,
        is_tls: bool,
        timeout: Duration,
    ) -> Result<ldap3::Ldap, LdapError> {
        // Check cache (lock is held briefly, no await while locked). A closed
        // handle is evicted so it is replaced below.
        {
            let mut conns = self.connections.lock();
            // `is_closed` needs `&mut Ldap`; take the answer and a clone in one
            // step so no borrow of the map outlives this expression.
            let cached = conns.get_mut(key).map(|c| {
                c.last_used = Instant::now();
                (c.ldap.is_closed(), c.ldap.clone())
            });
            match cached {
                Some((false, ldap)) => return Ok(ldap),
                Some((true, _)) => {
                    conns.remove(key);
                }
                None => {}
            }
        }

        let mut ldap = self.connect(conn, is_tls).await?;
        if !conn.bind_dn.is_empty() {
            simple_bind(&mut ldap, conn, timeout).await?;
        }

        // Cache the new connection, evicting closed and least-recently-used
        // entries to stay within the per-plugin and global bounds.
        {
            let mut conns = self.connections.lock();
            make_room(&mut conns, key);
            conns.insert(
                key.clone(),
                CachedConn {
                    ldap: ldap.clone(),
                    last_used: Instant::now(),
                },
            );
        }

        Ok(ldap)
    }

    fn evict(&self, key: &ConnKey) {
        self.connections.lock().remove(key);
    }

    /// Open a connection (TCP, then TLS or StartTLS as requested) without binding.
    async fn connect(&self, conn: &LdapConnection, is_tls: bool) -> Result<ldap3::Ldap, LdapError> {
        let url = conn.url.trim();
        let default_port = if is_tls {
            DEFAULT_LDAPS_PORT
        } else {
            DEFAULT_LDAP_PORT
        };

        // SSRF guard: resolve once and refuse internal/metadata targets unless the
        // operator has opted into internal egress. The connection is opened to
        // one of the vetted addresses.
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

        // Open the socket to a vetted address and hand it to ldap3 with the
        // original URL: the hostname drives SNI and certificate validation for
        // `ldaps://` and StartTLS, while the bytes flow to the checked address.
        let (driver, ldap) = tokio::time::timeout(CONNECT_TIMEOUT, async {
            let tcp = crate::http_client::connect_pinned_tcp(&addrs)
                .await
                .map_err(LdapError::ConnectionFailed)?;
            let tcp = tcp
                .into_std()
                .map_err(|e| LdapError::ConnectionFailed(e.to_string()))?;
            let settings = LdapConnSettings::new()
                .set_starttls(conn.starttls)
                .set_std_stream(StdStream::Tcp(tcp));
            LdapConnAsync::with_settings(settings, url)
                .await
                .map_err(|e| LdapError::ConnectionFailed(e.to_string()))
        })
        .await
        .map_err(|_| LdapError::Timeout)??;
        ldap3::drive!(driver);

        tracing::info!(url = %url, "established LDAP connection");
        Ok(ldap)
    }
}

/// `true` for `ldaps://`, `false` for `ldap://`, error for anything else.
fn scheme_is_tls(url: &str) -> Result<bool, LdapError> {
    let url = url.trim();
    if url.starts_with("ldaps://") {
        Ok(true)
    } else if url.starts_with("ldap://") {
        Ok(false)
    } else {
        Err(LdapError::InvalidRequest(format!(
            "url must start with ldap:// or ldaps://, got '{url}'"
        )))
    }
}

/// Refuse to send a password over a connection that is neither `ldaps://` nor
/// StartTLS unless the request sets `allow_plaintext`.
fn check_transport(conn: &LdapConnection, is_tls: bool) -> Result<(), LdapError> {
    if conn.password.is_empty() || is_tls || conn.starttls || conn.allow_plaintext {
        Ok(())
    } else {
        Err(LdapError::PlaintextRefused)
    }
}

/// Drop closed connections, then evict least-recently-used entries until `key`
/// fits within the per-plugin and global bounds.
fn make_room(conns: &mut HashMap<ConnKey, CachedConn>, key: &ConnKey) {
    if conns.contains_key(key) {
        return;
    }
    conns.retain(|_, c| !c.ldap.is_closed());
    loop {
        let victim = eviction_victim(conns.iter().map(|(k, c)| (k, c.last_used)), &key.plugin);
        match victim {
            Some(v) => {
                conns.remove(&v);
            }
            None => break,
        }
    }
}

/// Which entry to evict so that `plugin` can insert one more connection, or
/// `None` when there is room. The plugin's own least-recently-used entry goes
/// first when the plugin is at its cap; otherwise the global least-recently-used
/// entry when the cache is full.
fn eviction_victim<'a>(
    entries: impl Iterator<Item = (&'a ConnKey, Instant)>,
    plugin: &str,
) -> Option<ConnKey> {
    let mut total = 0usize;
    let mut plugin_count = 0usize;
    let mut oldest_global: Option<(&ConnKey, Instant)> = None;
    let mut oldest_plugin: Option<(&ConnKey, Instant)> = None;
    for (key, last_used) in entries {
        total += 1;
        if oldest_global.is_none_or(|(_, t)| last_used < t) {
            oldest_global = Some((key, last_used));
        }
        if key.plugin == plugin {
            plugin_count += 1;
            if oldest_plugin.is_none_or(|(_, t)| last_used < t) {
                oldest_plugin = Some((key, last_used));
            }
        }
    }
    if plugin_count >= MAX_CONNECTIONS_PER_PLUGIN {
        return oldest_plugin.map(|(k, _)| k.clone());
    }
    if total >= MAX_LDAP_CONNECTIONS {
        return oldest_global.map(|(k, _)| k.clone());
    }
    None
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

    const PLUGIN: &str = "ldap-auth";

    fn conn(url: &str) -> LdapConnection {
        LdapConnection {
            url: url.to_string(),
            bind_dn: "cn=svc,dc=example,dc=org".to_string(),
            password: "secret".to_string(),
            starttls: false,
            allow_plaintext: true,
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
        let result = client.search_blocking(PLUGIN, &search("ldap://127.0.0.1:13389"));
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
            client.search_blocking(PLUGIN, &no_filter),
            Err(LdapError::InvalidRequest(_))
        ));

        let mut no_base = search("ldap://127.0.0.1:13389");
        no_base.base_dn.clear();
        assert!(matches!(
            client.search_blocking(PLUGIN, &no_base),
            Err(LdapError::InvalidRequest(_))
        ));
    }

    #[test]
    fn plaintext_credentials_refused_without_opt_in() {
        let client = LdapClient::new(true).expect("ldap client");

        let mut req = bind("ldap://127.0.0.1:13389");
        req.conn.allow_plaintext = false;
        assert!(matches!(
            client.bind_blocking(&req),
            Err(LdapError::PlaintextRefused)
        ));

        let mut req = search("ldap://127.0.0.1:13389");
        req.conn.allow_plaintext = false;
        assert!(matches!(
            client.search_blocking(PLUGIN, &req),
            Err(LdapError::PlaintextRefused)
        ));
    }

    #[test]
    fn transport_policy_admits_tls_starttls_anonymous_and_opt_in() {
        let mut plain = conn("ldap://127.0.0.1:13389");
        plain.allow_plaintext = false;
        assert!(matches!(
            check_transport(&plain, false),
            Err(LdapError::PlaintextRefused)
        ));

        let mut anonymous = plain.clone();
        anonymous.password.clear();
        assert!(check_transport(&anonymous, false).is_ok());

        let mut starttls = plain.clone();
        starttls.starttls = true;
        assert!(check_transport(&starttls, false).is_ok());

        assert!(check_transport(&plain, true).is_ok());

        let mut opted_in = plain.clone();
        opted_in.allow_plaintext = true;
        assert!(check_transport(&opted_in, false).is_ok());
    }

    #[test]
    fn ldaps_url_reaches_the_connection_attempt() {
        let client = LdapClient::new(true).expect("ldap client");
        let mut req = bind("ldaps://127.0.0.1:13636");
        req.conn.allow_plaintext = false;
        let result = client.bind_blocking(&req);
        assert!(matches!(
            result,
            Err(LdapError::ConnectionFailed(_)) | Err(LdapError::Timeout)
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
    fn conn_key_separates_plugins_and_identities_and_hides_password() {
        let a = ConnKey::new(PLUGIN, &conn("ldap://h:389"));
        let mut other_pw = conn("ldap://h:389");
        other_pw.password = "different".to_string();
        let b = ConnKey::new(PLUGIN, &other_pw);
        assert_ne!(a, b);
        assert_eq!(a.url, b.url);
        assert_eq!(a.bind_dn, b.bind_dn);
        let c = ConnKey::new("other-plugin", &conn("ldap://h:389"));
        assert_ne!(a, c);
        let debug = format!("{a:?}");
        assert!(!debug.contains("secret"));
    }

    fn key(plugin: &str, n: usize) -> ConnKey {
        ConnKey {
            plugin: plugin.to_string(),
            url: format!("ldap://d{n}:389"),
            bind_dn: "cn=svc".to_string(),
            password_fingerprint: n as u64,
        }
    }

    #[test]
    fn eviction_victim_is_none_while_there_is_room() {
        let now = Instant::now();
        let keys: Vec<ConnKey> = (0..3).map(|n| key("a", n)).collect();
        let victim = eviction_victim(keys.iter().map(|k| (k, now)), "a");
        assert_eq!(victim, None);
    }

    #[test]
    fn eviction_victim_is_the_plugins_oldest_when_the_plugin_is_at_cap() {
        let now = Instant::now();
        let keys: Vec<ConnKey> = (0..MAX_CONNECTIONS_PER_PLUGIN)
            .map(|n| key("a", n))
            .collect();
        let other = key("b", 999);
        let entries = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k, now + Duration::from_secs(i as u64 + 1)))
            .chain(std::iter::once((&other, now)));
        // "b" holds the globally oldest entry, but "a" is the plugin at its cap,
        // so "a"'s own oldest entry (index 0) is evicted.
        let victim = eviction_victim(entries, "a");
        assert_eq!(victim, Some(keys[0].clone()));
    }

    #[test]
    fn eviction_victim_is_the_global_oldest_when_the_cache_is_full() {
        let now = Instant::now();
        // 8 plugins with 32 connections each = 256, none over its own cap.
        let keys: Vec<ConnKey> = (0..MAX_LDAP_CONNECTIONS)
            .map(|n| key(&format!("p{}", n / MAX_CONNECTIONS_PER_PLUGIN), n))
            .collect();
        let entries = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k, now + Duration::from_secs(i as u64)));
        let victim = eviction_victim(entries, "newcomer");
        assert_eq!(victim, Some(keys[0].clone()));
    }
}
