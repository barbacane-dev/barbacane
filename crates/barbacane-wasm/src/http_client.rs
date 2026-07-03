//! HTTP client for outbound requests from WASM plugins.
//!
//! Provides connection pooling, TLS, timeouts, and circuit breaker support.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use reqwest::{Certificate, Client, Identity};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
use barbacane_plugin_sdk::types::base64_body;

/// TLS configuration for upstream mTLS connections.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TlsConfig {
    /// Path to PEM-encoded client certificate.
    #[serde(default)]
    pub client_cert: Option<PathBuf>,
    /// Path to PEM-encoded client private key.
    #[serde(default)]
    pub client_key: Option<PathBuf>,
    /// Path to PEM-encoded CA certificate for server verification.
    #[serde(default)]
    pub ca: Option<PathBuf>,
}

impl TlsConfig {
    /// Returns true if any TLS configuration is specified.
    pub fn is_configured(&self) -> bool {
        self.client_cert.is_some() || self.client_key.is_some() || self.ca.is_some()
    }

    /// Validate that if client_cert is set, client_key must also be set (and vice versa).
    pub fn validate(&self) -> Result<(), TlsConfigError> {
        match (&self.client_cert, &self.client_key) {
            (Some(_), None) => Err(TlsConfigError::MissingClientKey),
            (None, Some(_)) => Err(TlsConfigError::MissingClientCert),
            _ => Ok(()),
        }
    }

    /// Create a cache key for this TLS configuration.
    fn cache_key(&self) -> TlsCacheKey {
        TlsCacheKey {
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.clone(),
            ca: self.ca.clone(),
        }
    }
}

/// TLS configuration errors.
#[derive(Debug, Error)]
pub enum TlsConfigError {
    #[error("client_cert specified but client_key is missing")]
    MissingClientKey,
    #[error("client_key specified but client_cert is missing")]
    MissingClientCert,
    #[error("failed to read certificate file: {0}")]
    ReadCertificate(#[source] std::io::Error),
    #[error("failed to read key file: {0}")]
    ReadKey(#[source] std::io::Error),
    #[error("failed to read CA file: {0}")]
    ReadCa(#[source] std::io::Error),
    #[error("failed to parse PEM identity: {0}")]
    ParseIdentity(#[source] reqwest::Error),
    #[error("failed to parse CA certificate: {0}")]
    ParseCaCert(#[source] reqwest::Error),
}

/// Cache key for TLS-configured clients.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TlsCacheKey {
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    ca: Option<PathBuf>,
}

impl Hash for TlsCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.client_cert.hash(state);
        self.client_key.hash(state);
        self.ca.hash(state);
    }
}

/// HTTP client with connection pooling and circuit breaker support.
#[derive(Clone)]
pub struct HttpClient {
    /// Default client (no mTLS).
    client: Client,
    /// Cached clients with specific TLS configurations.
    tls_clients: Arc<RwLock<HashMap<TlsCacheKey, Client>>>,
    /// Base config for creating new clients.
    base_config: HttpClientConfig,
    circuit_breakers: Arc<RwLock<HashMap<String, CircuitBreaker>>>,
    default_timeout: Duration,
    allow_plaintext: bool,
}

impl HttpClient {
    /// Create a new HTTP client.
    pub fn new(config: HttpClientConfig) -> Result<Self, HttpClientError> {
        let client = Client::builder()
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .pool_idle_timeout(config.pool_idle_timeout)
            .connect_timeout(config.connect_timeout)
            .timeout(config.default_timeout)
            // Disable redirect following: a permitted host could otherwise 3xx
            // to an internal/metadata target, bypassing the SSRF guard below.
            .redirect(reqwest::redirect::Policy::none())
            // Enforce the SSRF guard at connect-time resolution to close the
            // DNS-rebinding TOCTOU window.
            .dns_resolver(Arc::new(GuardedResolver {
                allow_internal: config.allow_internal_egress,
            }))
            .build()
            .map_err(HttpClientError::BuildError)?;

        let default_timeout = config.default_timeout;
        let allow_plaintext = config.allow_plaintext;

        Ok(Self {
            client,
            tls_clients: Arc::new(RwLock::new(HashMap::new())),
            base_config: config,
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            default_timeout,
            allow_plaintext,
        })
    }

    /// Get or create a client with the specified TLS configuration.
    fn get_or_create_tls_client(&self, tls_config: &TlsConfig) -> Result<Client, HttpClientError> {
        let cache_key = tls_config.cache_key();

        // Check if we already have a client for this config
        {
            let clients = self.tls_clients.read();
            if let Some(client) = clients.get(&cache_key) {
                return Ok(client.clone());
            }
        }

        // Create a new client with TLS config
        let client = self.build_tls_client(tls_config)?;

        // Cache it
        {
            let mut clients = self.tls_clients.write();
            clients.insert(cache_key, client.clone());
        }

        Ok(client)
    }

    /// Build a new client with the specified TLS configuration.
    fn build_tls_client(&self, tls_config: &TlsConfig) -> Result<Client, HttpClientError> {
        tls_config.validate().map_err(HttpClientError::TlsConfig)?;

        let mut builder = Client::builder()
            .pool_max_idle_per_host(self.base_config.pool_max_idle_per_host)
            .pool_idle_timeout(self.base_config.pool_idle_timeout)
            .connect_timeout(self.base_config.connect_timeout)
            .timeout(self.base_config.default_timeout)
            .redirect(reqwest::redirect::Policy::none())
            // Enforce the SSRF guard at connect-time resolution (DNS-rebinding).
            .dns_resolver(Arc::new(GuardedResolver {
                allow_internal: self.base_config.allow_internal_egress,
            }));

        // Add client certificate (mTLS)
        if let (Some(cert_path), Some(key_path)) = (&tls_config.client_cert, &tls_config.client_key)
        {
            let cert_pem = std::fs::read(cert_path)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ReadCertificate(e)))?;
            let key_pem = std::fs::read(key_path)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ReadKey(e)))?;

            // Combine cert and key for Identity::from_pem
            let mut pem = cert_pem;
            pem.extend_from_slice(&key_pem);

            let identity = Identity::from_pem(&pem)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ParseIdentity(e)))?;

            builder = builder.identity(identity);
        }

        // Add custom CA certificate
        if let Some(ca_path) = &tls_config.ca {
            let ca_pem = std::fs::read(ca_path)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ReadCa(e)))?;

            let ca_cert = Certificate::from_pem(&ca_pem)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ParseCaCert(e)))?;

            builder = builder.add_root_certificate(ca_cert);
        }

        builder.build().map_err(HttpClientError::BuildError)
    }

    /// Make an HTTP request.
    pub async fn call(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.call_with_tls(request, None).await
    }

    /// Send a streaming HTTP request and return the raw upstream response.
    ///
    /// Applies the same URL validation, plaintext checks, and circuit breaker
    /// as `call`, but returns the `reqwest::Response` directly so the caller
    /// can stream the response body chunk by chunk (e.g. via `bytes_stream()`).
    ///
    /// The circuit breaker is only updated on connection-level errors; success
    /// recording is left to the caller after streaming completes.
    pub async fn stream_raw(
        &self,
        request: HttpRequest,
    ) -> Result<reqwest::Response, HttpClientError> {
        let url = request
            .url
            .parse::<reqwest::Url>()
            .map_err(|e| HttpClientError::InvalidUrl(e.to_string()))?;

        if url.scheme() == "http" && !self.allow_plaintext {
            return Err(HttpClientError::PlaintextNotAllowed);
        }

        let host = url
            .host_str()
            .ok_or_else(|| HttpClientError::InvalidUrl("missing host".into()))?
            .to_string();

        // SSRF guard: reject internal/loopback/link-local/metadata targets.
        ssrf_guard(&url, self.base_config.allow_internal_egress).await?;

        let circuit_state = self.get_circuit_state(&host);
        if circuit_state == crate::circuit_breaker::CircuitState::Open {
            return Err(HttpClientError::CircuitOpen(host));
        }

        let method = request
            .method
            .parse::<reqwest::Method>()
            .map_err(|e| HttpClientError::InvalidMethod(e.to_string()))?;

        let timeout = request.timeout.unwrap_or(self.default_timeout);

        let mut req_builder = self.client.request(method, url).timeout(timeout);

        for (key, value) in &request.headers {
            req_builder = req_builder.header(key.as_str(), value.as_str());
        }

        if let Some(body) = request.body {
            req_builder = req_builder.body(body);
        }

        match req_builder.send().await {
            Ok(response) => Ok(response),
            Err(e) => {
                self.record_failure(&host);
                if e.is_timeout() {
                    Err(HttpClientError::Timeout)
                } else if e.is_connect() {
                    Err(HttpClientError::ConnectionFailed(e.to_string()))
                } else {
                    Err(HttpClientError::RequestFailed(e.to_string()))
                }
            }
        }
    }

    /// Make an HTTP request with optional TLS configuration for mTLS.
    pub async fn call_with_tls(
        &self,
        request: HttpRequest,
        tls_config: Option<&TlsConfig>,
    ) -> Result<HttpResponse, HttpClientError> {
        // Validate URL scheme
        let url = request
            .url
            .parse::<reqwest::Url>()
            .map_err(|e| HttpClientError::InvalidUrl(e.to_string()))?;

        if url.scheme() == "http" && !self.allow_plaintext {
            return Err(HttpClientError::PlaintextNotAllowed);
        }

        // Extract host for circuit breaker
        let host = url
            .host_str()
            .ok_or_else(|| HttpClientError::InvalidUrl("missing host".into()))?
            .to_string();

        // SSRF guard: reject internal/loopback/link-local/metadata targets.
        ssrf_guard(&url, self.base_config.allow_internal_egress).await?;

        // Check circuit breaker
        let circuit_state = self.get_circuit_state(&host);
        if circuit_state == CircuitState::Open {
            return Err(HttpClientError::CircuitOpen(host));
        }

        // Get the appropriate client (default or TLS-configured)
        let client = match tls_config {
            Some(tls) if tls.is_configured() => self.get_or_create_tls_client(tls)?,
            _ => self.client.clone(),
        };

        // Build request
        let method = request
            .method
            .parse::<reqwest::Method>()
            .map_err(|e| HttpClientError::InvalidMethod(e.to_string()))?;

        let timeout = request.timeout.unwrap_or(self.default_timeout);

        let mut req_builder = client.request(method, url).timeout(timeout);

        // Add headers, dropping any a plugin must not control on the outbound
        // request. `Host` is derived from the URL (a plugin-set Host enables
        // routing/cache/SSRF confusion), and the hop-by-hop / framing headers
        // below are owned by the HTTP client (a plugin-set value enables request
        // smuggling). `Authorization` is intentionally allowed: dispatchers such
        // as ai-proxy legitimately authenticate to their upstream.
        for (key, value) in &request.headers {
            if is_forbidden_outbound_header(key) {
                continue;
            }
            req_builder = req_builder.header(key.as_str(), value.as_str());
        }

        // Add body
        if let Some(body) = request.body {
            req_builder = req_builder.body(body);
        }

        // Execute request
        let result = req_builder.send().await;

        match result {
            Ok(response) => {
                // Record success
                self.record_success(&host);

                let status = response.status().as_u16();
                let headers: HashMap<String, String> = response
                    .headers()
                    .iter()
                    .filter_map(|(k, v)| {
                        v.to_str()
                            .ok()
                            .map(|v| (k.as_str().to_lowercase(), v.to_string()))
                    })
                    .collect();

                let body = self.read_body_capped(response).await?;

                Ok(HttpResponse {
                    status,
                    headers,
                    body: Some(body),
                })
            }
            Err(e) => {
                // Record failure
                self.record_failure(&host);

                if e.is_timeout() {
                    Err(HttpClientError::Timeout)
                } else if e.is_connect() {
                    Err(HttpClientError::ConnectionFailed(e.to_string()))
                } else {
                    Err(HttpClientError::RequestFailed(e.to_string()))
                }
            }
        }
    }

    /// Read an upstream response body into memory, refusing to buffer more than
    /// `max_response_bytes`. The upstream is plugin-chosen and untrusted, so a
    /// `Content-Length` over the cap is rejected up front and the streamed body
    /// is bounded chunk-by-chunk (covers chunked encoding with no length).
    async fn read_body_capped(
        &self,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, HttpClientError> {
        let limit = self.base_config.max_response_bytes;

        if let Some(len) = response.content_length() {
            if len > limit as u64 {
                return Err(HttpClientError::ResponseTooLarge { limit });
            }
        }

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(HttpClientError::ResponseReadError)?
        {
            if body.len().saturating_add(chunk.len()) > limit {
                return Err(HttpClientError::ResponseTooLarge { limit });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Configure circuit breaker for a host.
    pub fn configure_circuit_breaker(&self, host: &str, config: CircuitBreakerConfig) {
        let mut breakers = self.circuit_breakers.write();
        breakers.insert(host.to_string(), CircuitBreaker::new(config));
    }

    fn get_circuit_state(&self, host: &str) -> CircuitState {
        let breakers = self.circuit_breakers.read();
        breakers
            .get(host)
            .map(|cb| cb.state())
            .unwrap_or(CircuitState::Closed)
    }

    fn record_success(&self, host: &str) {
        let mut breakers = self.circuit_breakers.write();
        if let Some(cb) = breakers.get_mut(host) {
            cb.record_success();
        }
    }

    fn record_failure(&self, host: &str) {
        let mut breakers = self.circuit_breakers.write();
        if let Some(cb) = breakers.get_mut(host) {
            cb.record_failure();
        }
    }
}

/// Whether an IP address points at an internal / non-routable / metadata range
/// that an untrusted plugin must not be able to reach.
pub(crate) fn ip_is_internal(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254.0.0/16, incl. 169.254.169.254 cloud metadata
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.octets()[0] == 0 // 0.0.0.0/8
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64) // 100.64.0.0/10 CGNAT
        }
        IpAddr::V6(v6) => {
            // Unwrap IPv4-in-IPv6 forms and apply the v4 rules.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return ip_is_internal(&IpAddr::V4(mapped));
            }
            if let Some(compat) = v6.to_ipv4() {
                return ip_is_internal(&IpAddr::V4(compat));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// Outcome of an SSRF host check that distinguishes a policy rejection from a
/// resolution failure so callers can map each to their own error type.
pub(crate) enum HostGuardError {
    /// The host resolves to an internal/non-routable/metadata address.
    Blocked(String),
    /// The host could not be resolved.
    Resolve(String),
}

/// Reject a target whose host resolves to an internal address. `host` may be an
/// IP literal (optionally bracketed for IPv6) or a name; `port` is used only for
/// DNS resolution. A name is rejected if *any* resolved address is internal,
/// defending against a public name that points at an internal IP.
///
/// Note: a separate connect-time resolution still occurs inside the client, so a
/// rebinding attacker could in theory return a different address; pinning the
/// vetted IP via a custom resolver is tracked as follow-up hardening.
pub(crate) async fn guard_external_host(
    host: &str,
    port: u16,
    allow_internal: bool,
) -> Result<(), HostGuardError> {
    if allow_internal {
        return Ok(());
    }

    let host_clean = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    if let Ok(ip) = host_clean.parse::<IpAddr>() {
        if ip_is_internal(&ip) {
            return Err(HostGuardError::Blocked(host.to_string()));
        }
        return Ok(());
    }

    let mut saw_any = false;
    let addrs = tokio::net::lookup_host((host_clean, port))
        .await
        .map_err(|e| HostGuardError::Resolve(e.to_string()))?;
    for addr in addrs {
        saw_any = true;
        if ip_is_internal(&addr.ip()) {
            return Err(HostGuardError::Blocked(host.to_string()));
        }
    }
    if !saw_any {
        return Err(HostGuardError::Resolve(format!(
            "no DNS records for {host}"
        )));
    }
    Ok(())
}

/// Headers a plugin may not set on an outbound HTTP request. `host` is derived
/// from the URL; the rest are hop-by-hop / message-framing headers owned by the
/// HTTP client. Allowing a plugin to override any of these enables routing
/// confusion or request smuggling. `authorization` is deliberately absent:
/// dispatchers legitimately authenticate to their upstream.
fn is_forbidden_outbound_header(name: &str) -> bool {
    const FORBIDDEN: &[&str] = &[
        "host",
        "content-length",
        "transfer-encoding",
        "connection",
        "keep-alive",
        "proxy-connection",
        "upgrade",
        "te",
        "trailer",
    ];
    FORBIDDEN.iter().any(|h| name.eq_ignore_ascii_case(h))
}

/// A reqwest DNS resolver that enforces the SSRF guard at the moment of
/// resolution. reqwest connects to exactly the addresses this returns and does
/// not resolve again, which closes the DNS-rebinding TOCTOU window: a hostile
/// resolver cannot answer with a public IP for the pre-flight [`ssrf_guard`]
/// check and then rebind to an internal IP when the connection is made, because
/// the connect-time resolution is this one and it filters internal addresses.
#[derive(Debug, Clone)]
struct GuardedResolver {
    allow_internal: bool,
}

/// Keep only the addresses a plugin is permitted to connect to. With egress
/// disallowed, internal/loopback/link-local/metadata addresses are dropped; an
/// empty result means every resolved address was internal (the caller treats
/// that as blocked).
fn permitted_addrs(
    resolved: impl Iterator<Item = SocketAddr>,
    allow_internal: bool,
) -> Vec<SocketAddr> {
    if allow_internal {
        resolved.collect()
    } else {
        resolved.filter(|a| !ip_is_internal(&a.ip())).collect()
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_internal = self.allow_internal;
        Box::pin(async move {
            let host = name.as_str().to_string();
            // The port is irrelevant to resolution; reqwest applies the real
            // target port to the returned IPs.
            let resolved = tokio::net::lookup_host((host.as_str(), 0u16)).await?;
            let addrs = permitted_addrs(resolved, allow_internal);
            if addrs.is_empty() {
                let err: Box<dyn std::error::Error + Send + Sync> = format!(
                    "no permitted address for host '{host}': all resolved \
                     addresses are internal, or none were returned"
                )
                .into();
                return Err(err);
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// SSRF guard for the HTTP client: reject requests whose target resolves to an
/// internal address.
async fn ssrf_guard(url: &reqwest::Url, allow_internal: bool) -> Result<(), HttpClientError> {
    let host = url
        .host_str()
        .ok_or_else(|| HttpClientError::InvalidUrl("missing host".into()))?;
    let port = url.port_or_known_default().unwrap_or(0);
    guard_external_host(host, port, allow_internal)
        .await
        .map_err(|e| match e {
            HostGuardError::Blocked(h) => HttpClientError::BlockedTarget(h),
            HostGuardError::Resolve(m) => HttpClientError::ConnectionFailed(m),
        })
}

/// Configuration for the HTTP client.
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// Maximum idle connections per host.
    pub pool_max_idle_per_host: usize,
    /// Idle connection timeout.
    pub pool_idle_timeout: Duration,
    /// Connection timeout.
    pub connect_timeout: Duration,
    /// Default request timeout.
    pub default_timeout: Duration,
    /// Allow plaintext HTTP (development only).
    pub allow_plaintext: bool,
    /// Allow plugin egress to internal/loopback/link-local/metadata targets,
    /// disabling the SSRF guard. Off by default; operators opt in for trusted
    /// internal upstreams.
    pub allow_internal_egress: bool,
    /// Maximum size (bytes) of a buffered upstream response body. A
    /// plugin-chosen upstream is untrusted, so the buffered `call` path caps the
    /// body it will read into host memory to avoid an OOM. Streaming dispatchers
    /// (`stream_raw`) are unaffected.
    pub max_response_bytes: usize,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            pool_max_idle_per_host: 10,
            pool_idle_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(10),
            default_timeout: Duration::from_secs(30),
            allow_plaintext: false,
            allow_internal_egress: false,
            max_response_bytes: 16 * 1024 * 1024,
        }
    }
}

/// HTTP request from WASM plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    /// HTTP method (GET, POST, etc.)
    pub method: String,
    /// Full URL including scheme and host.
    pub url: String,
    /// Request headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Request body (optional, base64-encoded in JSON for WASM transport).
    #[serde(default, with = "base64_body")]
    pub body: Option<Vec<u8>>,
    /// Request timeout (optional, uses client default).
    #[serde(default, with = "option_duration_serde")]
    pub timeout: Option<Duration>,
}

/// HTTP response to WASM plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: HashMap<String, String>,
    /// Response body (optional, base64-encoded in JSON for WASM transport).
    #[serde(default, with = "base64_body")]
    pub body: Option<Vec<u8>>,
}

impl HttpResponse {
    /// Create an error response.
    pub fn error(status: u16, error_type: &str, title: &str, detail: &str) -> Self {
        let body = serde_json::json!({
            "type": error_type,
            "title": title,
            "status": status,
            "detail": detail
        });

        let mut headers = HashMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        Self {
            status,
            headers,
            body: Some(body.to_string().into_bytes()),
        }
    }
}

/// HTTP client errors.
#[derive(Debug, Error)]
pub enum HttpClientError {
    #[error("failed to build HTTP client: {0}")]
    BuildError(#[source] reqwest::Error),

    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("invalid HTTP method: {0}")]
    InvalidMethod(String),

    #[error("plaintext HTTP not allowed")]
    PlaintextNotAllowed,

    #[error("circuit breaker open for host: {0}")]
    CircuitOpen(String),

    #[error("target blocked by SSRF policy: {0}")]
    BlockedTarget(String),

    #[error("request timeout")]
    Timeout,

    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("request failed: {0}")]
    RequestFailed(String),

    #[error("failed to read response: {0}")]
    ResponseReadError(#[source] reqwest::Error),

    #[error("upstream response exceeds the {limit}-byte buffer limit")]
    ResponseTooLarge { limit: usize },

    #[error("TLS configuration error: {0}")]
    TlsConfig(#[source] TlsConfigError),
}

/// Custom serde for Option<Duration> in seconds.
mod option_duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match duration {
            Some(d) => d.as_secs_f64().serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<f64> = Option::deserialize(deserializer)?;
        Ok(opt.map(Duration::from_secs_f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_classifier_blocks_internal_targets() {
        let blocked = [
            "127.0.0.1",
            "169.254.169.254", // cloud metadata
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "0.0.0.0",
            "100.64.0.1",       // CGNAT
            "::1",              // IPv6 loopback
            "fc00::1",          // IPv6 ULA
            "fe80::1",          // IPv6 link-local
            "::ffff:127.0.0.1", // IPv4-mapped loopback
            "::ffff:169.254.169.254",
        ];
        for s in blocked {
            let ip: IpAddr = s.parse().unwrap();
            assert!(ip_is_internal(&ip), "{s} should be classified internal");
        }

        let allowed = [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ];
        for s in allowed {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!ip_is_internal(&ip), "{s} should be classified external");
        }
    }

    #[tokio::test]
    async fn ssrf_guard_rejects_ip_literals_to_metadata_and_loopback() {
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:8081/",
            "https://[::1]/",
            "http://10.1.2.3/",
        ] {
            let parsed = url.parse::<reqwest::Url>().unwrap();
            let err = ssrf_guard(&parsed, false).await.unwrap_err();
            assert!(
                matches!(err, HttpClientError::BlockedTarget(_)),
                "{url} should be blocked, got {err:?}"
            );
        }
        // With internal egress allowed, the same targets pass the guard.
        let parsed = "http://127.0.0.1:8081/".parse::<reqwest::Url>().unwrap();
        assert!(ssrf_guard(&parsed, true).await.is_ok());
    }

    #[test]
    fn test_config_default() {
        let config = HttpClientConfig::default();
        assert_eq!(config.pool_max_idle_per_host, 10);
        assert_eq!(config.default_timeout, Duration::from_secs(30));
        assert!(!config.allow_plaintext);
    }

    #[test]
    fn test_error_response() {
        let resp = HttpResponse::error(
            502,
            "urn:barbacane:error:upstream-unavailable",
            "Bad Gateway",
            "Failed to connect to upstream",
        );

        assert_eq!(resp.status, 502);
        assert_eq!(
            resp.headers.get("content-type"),
            Some(&"application/problem+json".to_string())
        );
    }

    #[test]
    fn test_tls_config_default() {
        let tls = TlsConfig::default();
        assert!(tls.client_cert.is_none());
        assert!(tls.client_key.is_none());
        assert!(tls.ca.is_none());
        assert!(!tls.is_configured());
    }

    #[test]
    fn test_tls_config_is_configured() {
        let mut tls = TlsConfig::default();
        assert!(!tls.is_configured());

        tls.client_cert = Some(PathBuf::from("/path/to/cert.pem"));
        assert!(tls.is_configured());

        tls.client_cert = None;
        tls.ca = Some(PathBuf::from("/path/to/ca.pem"));
        assert!(tls.is_configured());
    }

    #[test]
    fn test_tls_config_validate_success() {
        // Empty config is valid
        let tls = TlsConfig::default();
        assert!(tls.validate().is_ok());

        // CA only is valid
        let tls = TlsConfig {
            client_cert: None,
            client_key: None,
            ca: Some(PathBuf::from("/path/to/ca.pem")),
        };
        assert!(tls.validate().is_ok());

        // Both cert and key is valid
        let tls = TlsConfig {
            client_cert: Some(PathBuf::from("/path/to/cert.pem")),
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };
        assert!(tls.validate().is_ok());
    }

    #[test]
    fn test_tls_config_validate_missing_key() {
        let tls = TlsConfig {
            client_cert: Some(PathBuf::from("/path/to/cert.pem")),
            client_key: None,
            ca: None,
        };
        let err = tls.validate().unwrap_err();
        assert!(matches!(err, TlsConfigError::MissingClientKey));
    }

    #[test]
    fn test_tls_config_validate_missing_cert() {
        let tls = TlsConfig {
            client_cert: None,
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };
        let err = tls.validate().unwrap_err();
        assert!(matches!(err, TlsConfigError::MissingClientCert));
    }

    #[test]
    fn test_tls_config_serde() {
        let json = r#"{
            "client_cert": "/etc/certs/client.crt",
            "client_key": "/etc/certs/client.key",
            "ca": "/etc/certs/ca.crt"
        }"#;

        let tls: TlsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            tls.client_cert,
            Some(PathBuf::from("/etc/certs/client.crt"))
        );
        assert_eq!(tls.client_key, Some(PathBuf::from("/etc/certs/client.key")));
        assert_eq!(tls.ca, Some(PathBuf::from("/etc/certs/ca.crt")));
    }

    #[test]
    fn test_tls_config_serde_partial() {
        let json = r#"{"ca": "/etc/certs/ca.crt"}"#;

        let tls: TlsConfig = serde_json::from_str(json).unwrap();
        assert!(tls.client_cert.is_none());
        assert!(tls.client_key.is_none());
        assert_eq!(tls.ca, Some(PathBuf::from("/etc/certs/ca.crt")));
    }

    // ── stream_raw validation ─────────────────────────────────────────────────

    #[tokio::test]
    async fn stream_raw_rejects_invalid_url() {
        let client = HttpClient::new(HttpClientConfig::default()).expect("client");
        let req = HttpRequest {
            method: "GET".into(),
            url: "not a url".into(),
            headers: Default::default(),
            body: None,
            timeout: None,
        };
        assert!(matches!(
            client.stream_raw(req).await,
            Err(HttpClientError::InvalidUrl(_))
        ));
    }

    #[tokio::test]
    async fn stream_raw_rejects_plaintext_when_disallowed() {
        let config = HttpClientConfig {
            allow_plaintext: false,
            ..Default::default()
        };
        let client = HttpClient::new(config).expect("client");
        let req = HttpRequest {
            method: "GET".into(),
            url: "http://example.com/api".into(),
            headers: Default::default(),
            body: None,
            timeout: None,
        };
        assert!(matches!(
            client.stream_raw(req).await,
            Err(HttpClientError::PlaintextNotAllowed)
        ));
    }

    #[tokio::test]
    async fn stream_raw_rejects_invalid_method() {
        let config = HttpClientConfig {
            allow_plaintext: true,
            allow_internal_egress: true,
            ..Default::default()
        };
        let client = HttpClient::new(config).expect("client");
        let req = HttpRequest {
            method: "NOT A METHOD!!!".into(),
            url: "http://127.0.0.1:1/".into(),
            headers: Default::default(),
            body: None,
            timeout: None,
        };
        assert!(matches!(
            client.stream_raw(req).await,
            Err(HttpClientError::InvalidMethod(_))
        ));
    }

    #[tokio::test]
    async fn stream_raw_connection_refused() {
        let config = HttpClientConfig {
            allow_plaintext: true,
            allow_internal_egress: true,
            ..Default::default()
        };
        let client = HttpClient::new(config).expect("client");
        let req = HttpRequest {
            method: "GET".into(),
            url: "http://127.0.0.1:1/".into(), // port 1: connection refused
            headers: Default::default(),
            body: None,
            timeout: None,
        };
        let err = client.stream_raw(req).await.unwrap_err();
        assert!(
            matches!(
                err,
                HttpClientError::ConnectionFailed(_) | HttpClientError::RequestFailed(_)
            ),
            "expected network error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn stream_raw_timeout() {
        use tokio::net::TcpListener;

        // Bind a listener but never accept — the client will time out.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let config = HttpClientConfig {
            allow_plaintext: true,
            allow_internal_egress: true,
            ..Default::default()
        };
        let client = HttpClient::new(config).expect("client");
        let req = HttpRequest {
            method: "GET".into(),
            url: format!("http://{addr}/slow"),
            headers: Default::default(),
            body: None,
            timeout: Some(Duration::from_millis(50)),
        };
        let err = client.stream_raw(req).await.unwrap_err();
        assert!(
            matches!(err, HttpClientError::Timeout),
            "expected Timeout, got: {err:?}"
        );

        drop(listener);
    }

    #[tokio::test]
    async fn stream_raw_successful_request() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        // Spawn a minimal HTTP server that returns a 200.
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
            let response = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
            socket.write_all(response.as_bytes()).await.expect("write");
            socket.shutdown().await.expect("shutdown");
        });

        let config = HttpClientConfig {
            allow_plaintext: true,
            allow_internal_egress: true,
            ..Default::default()
        };
        let client = HttpClient::new(config).expect("client");
        let req = HttpRequest {
            method: "GET".into(),
            url: format!("http://{addr}/"),
            headers: Default::default(),
            body: None,
            timeout: None,
        };
        let resp = client
            .stream_raw(req)
            .await
            .expect("stream_raw should succeed");
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.expect("body");
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn call_rejects_oversized_response_body() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        // Server returns a 10-byte body but advertises it; the client cap is 4.
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
            let response = "HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n0123456789";
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });

        let config = HttpClientConfig {
            allow_plaintext: true,
            allow_internal_egress: true,
            max_response_bytes: 4,
            ..Default::default()
        };
        let client = HttpClient::new(config).expect("client");
        let req = HttpRequest {
            method: "GET".into(),
            url: format!("http://{addr}/"),
            headers: Default::default(),
            body: None,
            timeout: None,
        };
        let err = client.call(req).await.unwrap_err();
        assert!(
            matches!(err, HttpClientError::ResponseTooLarge { limit: 4 }),
            "expected ResponseTooLarge, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn call_allows_response_within_limit() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
            let response = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });

        let config = HttpClientConfig {
            allow_plaintext: true,
            allow_internal_egress: true,
            max_response_bytes: 1024,
            ..Default::default()
        };
        let client = HttpClient::new(config).expect("client");
        let req = HttpRequest {
            method: "GET".into(),
            url: format!("http://{addr}/"),
            headers: Default::default(),
            body: None,
            timeout: None,
        };
        let resp = client.call(req).await.expect("call should succeed");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body.as_deref(), Some(b"ok".as_slice()));
    }

    #[test]
    fn test_tls_cache_key_equality() {
        let tls1 = TlsConfig {
            client_cert: Some(PathBuf::from("/path/to/cert.pem")),
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };
        let tls2 = TlsConfig {
            client_cert: Some(PathBuf::from("/path/to/cert.pem")),
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };
        let tls3 = TlsConfig {
            client_cert: Some(PathBuf::from("/other/cert.pem")),
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };

        assert_eq!(tls1.cache_key(), tls2.cache_key());
        assert_ne!(tls1.cache_key(), tls3.cache_key());
    }

    #[test]
    fn permitted_addrs_filters_internal_unless_egress_allowed() {
        let internal: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let link_local: SocketAddr = "169.254.169.254:0".parse().unwrap();
        let external: SocketAddr = "8.8.8.8:0".parse().unwrap();

        // Egress disallowed: internal + link-local (cloud metadata) dropped.
        assert_eq!(
            permitted_addrs(vec![internal, link_local, external].into_iter(), false),
            vec![external]
        );

        // Disallowed and every address internal -> empty (caller blocks).
        assert!(permitted_addrs(vec![internal, link_local].into_iter(), false).is_empty());

        // Egress allowed: everything passes through.
        assert_eq!(
            permitted_addrs(vec![internal, external].into_iter(), true),
            vec![internal, external]
        );
    }

    #[test]
    fn forbidden_outbound_headers_are_denied_case_insensitively() {
        for h in [
            "host",
            "Host",
            "HOST",
            "content-length",
            "Transfer-Encoding",
            "connection",
            "keep-alive",
            "proxy-connection",
            "upgrade",
            "te",
            "trailer",
        ] {
            assert!(is_forbidden_outbound_header(h), "{h} should be denied");
        }
    }

    #[test]
    fn legitimate_outbound_headers_are_allowed() {
        // Authorization is intentionally allowed (dispatchers authenticate to
        // their upstream); custom and content-type headers pass through.
        for h in [
            "authorization",
            "Authorization",
            "content-type",
            "x-api-key",
            "x-custom-header",
        ] {
            assert!(!is_forbidden_outbound_header(h), "{h} should be allowed");
        }
    }

    // ── base64 body serde (host ↔ WASM plugin compatibility) ─────────────

    /// Verify that HttpRequest serialized by a WASM plugin (base64 body)
    /// deserializes correctly on the host side.
    #[test]
    fn http_request_base64_body_roundtrip() {
        let binary_body: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0xFF, 0xFE, 0x00, 0x01];
        let req = HttpRequest {
            method: "POST".into(),
            url: "https://example.com/upload".into(),
            headers: Default::default(),
            body: Some(binary_body.clone()),
            timeout: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        // Body must be base64-encoded in JSON, not raw bytes
        assert!(
            !json.contains("\\u0089"),
            "body should be base64-encoded, not escaped unicode"
        );

        let decoded: HttpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body.unwrap(), binary_body);
    }

    /// Verify that HttpResponse serialized by the host (base64 body)
    /// deserializes correctly on the plugin side.
    #[test]
    fn http_response_base64_body_roundtrip() {
        let binary_body: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0xFF, 0xFE, 0x00, 0x01];
        let resp = HttpResponse {
            status: 200,
            headers: Default::default(),
            body: Some(binary_body.clone()),
        };

        let json = serde_json::to_string(&resp).unwrap();
        let decoded: HttpResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.body.unwrap(), binary_body);
    }

    /// Verify None body serializes as null and deserializes back.
    #[test]
    fn http_request_null_body_roundtrip() {
        let req = HttpRequest {
            method: "GET".into(),
            url: "https://example.com".into(),
            headers: Default::default(),
            body: None,
            timeout: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""body":null"#));

        let decoded: HttpRequest = serde_json::from_str(&json).unwrap();
        assert!(decoded.body.is_none());
    }

    /// Simulate what a WASM plugin sends: manually construct the JSON with
    /// a base64 string body and verify the host deserializes it correctly.
    #[test]
    fn http_request_deserialize_from_plugin_json() {
        use base64::Engine;
        let raw_bytes: Vec<u8> = vec![0x00, 0x01, 0x80, 0xFF];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw_bytes);

        let json = format!(
            r#"{{
                "method": "POST",
                "url": "https://example.com/api",
                "body": "{b64}"
            }}"#
        );

        let req: HttpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req.body.unwrap(), raw_bytes);
    }

    /// Simulate what the host sends back: manually construct JSON with
    /// base64 body and verify plugin-side deserialization.
    #[test]
    fn http_response_deserialize_from_host_json() {
        use base64::Engine;
        let raw_bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw_bytes);

        let json = format!(
            r#"{{
                "status": 200,
                "headers": {{}},
                "body": "{b64}"
            }}"#
        );

        let resp: HttpResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.body.unwrap(), raw_bytes);
    }
}
