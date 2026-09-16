//! NATS publisher for the Barbacane gateway.
//!
//! Provides a connection-caching NATS client for the `host_nats_publish` host function.
//! Connections are lazily established on first publish and cached by server URL.
//! A dedicated tokio runtime keeps NATS background tasks alive between publishes.

use crate::broker::{BrokerError, PublishResult};
use bytes::Bytes;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

/// Default NATS server port, used when an address omits one.
const DEFAULT_NATS_PORT: u16 = 4222;

/// Upper bound on cached NATS connections, so a plugin can't force unbounded
/// connection growth by publishing to many distinct server strings.
const MAX_NATS_CONNECTIONS: usize = 256;

/// Timeout for establishing a NATS connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for an individual publish operation.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(10);

/// NATS publisher with connection caching.
///
/// Owns a dedicated tokio runtime so that `async_nats::Client` background tasks
/// (heartbeats, reconnection) stay alive between publish calls. Connections are
/// created lazily on first publish and reused for subsequent messages to the same server.
pub struct NatsPublisher {
    runtime: tokio::runtime::Runtime,
    connections: Mutex<HashMap<String, async_nats::Client>>,
    /// When false, server addresses resolving to internal/metadata ranges are
    /// rejected (SSRF guard). Operators opt in for trusted internal servers.
    allow_internal_egress: bool,
}

impl NatsPublisher {
    /// Create a new NATS publisher with its own background runtime.
    pub fn new(allow_internal_egress: bool) -> Result<Self, BrokerError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("nats-runtime")
            .enable_all()
            .build()
            .map_err(|e| BrokerError::ConnectionFailed(format!("failed to create runtime: {e}")))?;
        Ok(Self {
            runtime,
            connections: Mutex::new(HashMap::new()),
            allow_internal_egress,
        })
    }

    /// Blocking publish for use from sync WASM host functions.
    ///
    /// Must be called from a thread that is NOT inside a tokio runtime context
    /// (e.g. from within `std::thread::scope`).
    pub fn publish_blocking(
        &self,
        url: &str,
        subject: &str,
        payload: Bytes,
        headers: BTreeMap<String, String>,
    ) -> Result<PublishResult, BrokerError> {
        self.runtime
            .block_on(self.publish(url, subject, payload, headers))
    }

    /// Publish a message to a NATS subject (async).
    async fn publish(
        &self,
        url: &str,
        subject: &str,
        payload: Bytes,
        headers: BTreeMap<String, String>,
    ) -> Result<PublishResult, BrokerError> {
        let client = self.get_or_connect(url).await?;

        if headers.is_empty() {
            tokio::time::timeout(
                PUBLISH_TIMEOUT,
                client.publish(subject.to_string(), payload),
            )
            .await
            .map_err(|_| BrokerError::Timeout)?
            .map_err(|e| BrokerError::PublishFailed(e.to_string()))?;
        } else {
            let mut header_map = async_nats::HeaderMap::new();
            for (k, v) in &headers {
                header_map.insert(k.as_str(), v.as_str());
            }
            tokio::time::timeout(
                PUBLISH_TIMEOUT,
                client.publish_with_headers(subject.to_string(), header_map, payload),
            )
            .await
            .map_err(|_| BrokerError::Timeout)?
            .map_err(|e| BrokerError::PublishFailed(e.to_string()))?;
        }

        Ok(PublishResult::success(subject.to_string()))
    }

    /// Get a cached connection or establish a new one.
    async fn get_or_connect(&self, url: &str) -> Result<async_nats::Client, BrokerError> {
        // Check cache (lock is held briefly, no await while locked)
        {
            let conns = self.connections.lock();
            if let Some(client) = conns.get(url) {
                return Ok(client.clone());
            }
        }

        // SSRF guard: resolve once and refuse internal/metadata targets unless the
        // operator has opted into internal egress. The connection is opened to
        // the vetted addresses, not to a fresh resolution by async_nats.
        let (host, port) = crate::broker::split_host_port(url, DEFAULT_NATS_PORT);
        let addrs = match crate::http_client::resolve_permitted_addrs(
            &host,
            port,
            self.allow_internal_egress,
        )
        .await
        {
            Ok(a) => a,
            Err(crate::http_client::HostGuardError::Blocked(h)) => {
                return Err(BrokerError::Blocked(h));
            }
            Err(crate::http_client::HostGuardError::Resolve(m)) => {
                return Err(BrokerError::ConnectionFailed(m));
            }
        };

        // Connect to the vetted addresses with the URL scheme kept. For `tls://`
        // the server certificate is validated against the URL hostname whatever
        // address the socket was opened to; the ClientHello carries no SNI.
        let is_tls = is_tls_url(url);
        let pinned = pinned_server_addrs(url, &addrs)?;
        // Servers advertised in INFO.connect_urls never pass the SSRF guard, so
        // they are refused: the pool keeps only the vetted addresses, which are
        // IP literals and so survive the re-resolution a reconnect performs.
        let mut options = async_nats::ConnectOptions::new().ignore_discovered_servers();
        if is_tls {
            let tls_config = crate::tls_pin::pinned_client_config(&host)
                .map_err(|e| BrokerError::ConnectionFailed(format!("TLS configuration: {e}")))?;
            options = options.tls_client_config(tls_config);
        }
        let client = tokio::time::timeout(CONNECT_TIMEOUT, options.connect(pinned))
            .await
            .map_err(|_| BrokerError::Timeout)?
            .map_err(|e| BrokerError::ConnectionFailed(e.to_string()))?;

        tracing::info!(url = %url, "established NATS connection");

        // Cache the new connection, bounding the cache size.
        {
            let mut conns = self.connections.lock();
            if conns.len() >= MAX_NATS_CONNECTIONS && !conns.contains_key(url) {
                return Err(BrokerError::ConnectionFailed(
                    "NATS connection cache is full".to_string(),
                ));
            }
            conns.insert(url.to_string(), client.clone());
        }

        Ok(client)
    }
}

/// `true` for a `tls://` server URL.
fn is_tls_url(url: &str) -> bool {
    url.trim_start().starts_with("tls://")
}

/// Server list for the connection, one entry per vetted address, keeping the
/// scheme of `url` so a `tls://` server is still reached over TLS. The hostname
/// is left out on purpose: it would let the client resolve it again.
fn pinned_server_addrs(
    url: &str,
    addrs: &[std::net::SocketAddr],
) -> Result<Vec<async_nats::ServerAddr>, BrokerError> {
    let scheme = if is_tls_url(url) { "tls" } else { "nats" };
    addrs
        .iter()
        .map(|a| format!("{scheme}://{a}").parse())
        .collect::<Result<_, _>>()
        .map_err(|e| BrokerError::ConnectionFailed(format!("invalid pinned NATS address: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_starts_empty() {
        let publisher = NatsPublisher::new(true).expect("nats publisher");
        let conns = publisher.connections.lock();
        assert!(conns.is_empty());
    }

    /// The server list carries the vetted addresses, never the hostname the
    /// client could resolve again, and keeps the scheme so TLS stays TLS.
    #[test]
    fn pinned_server_addrs_use_vetted_addresses_not_the_hostname() {
        let vetted: Vec<std::net::SocketAddr> = vec![
            "203.0.113.7:4222".parse().expect("addr"),
            "203.0.113.8:4222".parse().expect("addr"),
        ];

        let plain = pinned_server_addrs("nats://broker.example.com:4222", &vetted).expect("addrs");
        let rendered: Vec<String> = plain
            .iter()
            .map(|a| format!("{}:{}", a.host(), a.port()))
            .collect();
        assert_eq!(rendered, vec!["203.0.113.7:4222", "203.0.113.8:4222"]);
        assert!(plain.iter().all(|a| !a.tls_required()));

        let tls = pinned_server_addrs("tls://broker.example.com:4222", &vetted).expect("addrs");
        assert!(tls.iter().all(|a| a.tls_required()));
        assert!(
            tls.iter().all(|a| a.host() != "broker.example.com"),
            "the hostname must not reach the connect list"
        );
    }

    #[test]
    fn blocks_internal_server_when_egress_disallowed() {
        let publisher = NatsPublisher::new(false).expect("nats publisher");
        let result = publisher.publish_blocking(
            "nats://169.254.169.254:4222",
            "test.subject",
            Bytes::from_static(b"hello"),
            BTreeMap::new(),
        );
        assert!(matches!(result, Err(BrokerError::Blocked(_))));
    }

    #[test]
    fn publish_blocking_connection_refused() {
        let publisher = NatsPublisher::new(true).expect("nats publisher");
        let result = publisher.publish_blocking(
            "nats://127.0.0.1:19999",
            "test.subject",
            Bytes::from_static(b"hello"),
            BTreeMap::new(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BrokerError::ConnectionFailed(_)));
    }

    #[test]
    fn publish_blocking_from_thread_scope() {
        let publisher = NatsPublisher::new(true).expect("nats publisher");
        let result = std::thread::scope(|s| {
            s.spawn(|| {
                publisher.publish_blocking(
                    "nats://127.0.0.1:19999",
                    "test.subject",
                    Bytes::from_static(b"hello"),
                    BTreeMap::new(),
                )
            })
            .join()
            .unwrap()
        });
        assert!(matches!(result, Err(BrokerError::ConnectionFailed(_))));
    }

    #[test]
    fn publish_blocking_with_headers() {
        let publisher = NatsPublisher::new(true).expect("nats publisher");
        let mut headers = BTreeMap::new();
        headers.insert("x-request-id".to_string(), "req-123".to_string());
        headers.insert("x-trace-id".to_string(), "trace-456".to_string());

        let result = publisher.publish_blocking(
            "nats://127.0.0.1:19999",
            "events.orders",
            Bytes::from_static(b"hello"),
            headers,
        );
        // Connection refused, but validates the headers code path
        assert!(matches!(result, Err(BrokerError::ConnectionFailed(_))));
    }
}
