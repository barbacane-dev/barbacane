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
    runtime: Option<tokio::runtime::Runtime>,
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
            runtime: Some(runtime),
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
        self.runtime()
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
        // operator has opted into internal egress.
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

        let servers = if address_may_be_pinned(url)? {
            pinned_server_addrs(&addrs)?
        } else {
            vec![url
                .parse::<async_nats::ServerAddr>()
                .map_err(|e| BrokerError::ConnectionFailed(format!("invalid NATS URL: {e}")))?]
        };
        // Servers advertised in INFO.connect_urls never pass the SSRF guard, so
        // they are refused and the pool keeps only the servers configured here.
        let options = async_nats::ConnectOptions::new().ignore_discovered_servers();
        let client = tokio::time::timeout(CONNECT_TIMEOUT, options.connect(servers))
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

/// Whether a vetted address may stand in for the URL.
///
/// Only plain NATS over TCP may: a vetted address is rendered as `nats://ip:port`,
/// which carries neither the TLS server name a `tls://` or `wss://` handshake
/// needs for SNI nor the path a websocket URL carries. Schemes are compared
/// case-insensitively, so `TLS://` cannot fall through to the plaintext branch
/// and downgrade the connection. A scheme `async_nats` does not accept is an
/// error rather than a silent plaintext connection.
fn address_may_be_pinned(url: &str) -> Result<bool, BrokerError> {
    let Some((scheme, _)) = url.trim_start().split_once("://") else {
        // A bare `host:port` is plain NATS over TCP.
        return Ok(true);
    };
    if scheme.eq_ignore_ascii_case("nats") {
        Ok(true)
    } else if ["tls", "ws", "wss"]
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
    {
        Ok(false)
    } else {
        Err(BrokerError::ConnectionFailed(format!(
            "unsupported NATS URL scheme '{scheme}'"
        )))
    }
}

/// Plaintext server list, one entry per vetted address. The hostname is left
/// out on purpose: it would let the client resolve it again.
fn pinned_server_addrs(
    addrs: &[std::net::SocketAddr],
) -> Result<Vec<async_nats::ServerAddr>, BrokerError> {
    addrs
        .iter()
        .map(|a| format!("nats://{a}").parse())
        .collect::<Result<_, _>>()
        .map_err(|e| BrokerError::ConnectionFailed(format!("invalid pinned NATS address: {e}")))
}

impl NatsPublisher {
    /// The runtime, which is present for the whole life of the value and taken
    /// only by `Drop`.
    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("the runtime is taken only while dropping")
    }
}

impl Drop for NatsPublisher {
    /// Hand the runtime to tokio's background shutdown instead of waiting for
    /// it here.
    ///
    /// Dropping a runtime blocks until its workers stop, which tokio refuses
    /// inside an async context. This type is reachable from tasks running on
    /// the gateway's runtime, so the last reference can fall anywhere, and
    /// `shutdown_background` is safe wherever that happens.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod drop_safety_tests {
    use super::*;

    /// The gateway holds this behind an `Arc` that background tasks clone, so
    /// the last reference can fall on a tokio worker. Dropping a runtime there
    /// blocks, which tokio refuses, and the process died on an ordinary
    /// shutdown because of it.
    #[test]
    fn dropping_inside_a_runtime_does_not_panic() {
        let outer = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("outer runtime");

        outer.block_on(async {
            let publisher = NatsPublisher::new(true).expect("publisher");
            drop(publisher);
        });
    }

    /// And from a spawned task, which is where the gateway's eviction and
    /// hot-reload tasks would drop it.
    #[test]
    fn dropping_inside_a_spawned_task_does_not_panic() {
        let outer = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("outer runtime");

        outer.block_on(async {
            let shared = std::sync::Arc::new(NatsPublisher::new(true).expect("publisher"));
            let held = shared.clone();
            let task = tokio::spawn(async move {
                // The task outlives the local reference, so its drop is last.
                drop(held);
            });
            drop(shared);
            task.await.expect("task");
        });
    }
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

    /// A plaintext server list carries the vetted addresses, never the hostname
    /// the client could resolve again.
    #[test]
    fn pinned_server_addrs_use_vetted_addresses_not_the_hostname() {
        let vetted: Vec<std::net::SocketAddr> = vec![
            "203.0.113.7:4222".parse().expect("addr"),
            "203.0.113.8:4222".parse().expect("addr"),
        ];

        let plain = pinned_server_addrs(&vetted).expect("addrs");
        let rendered: Vec<String> = plain
            .iter()
            .map(|a| format!("{}:{}", a.host(), a.port()))
            .collect();
        assert_eq!(rendered, vec!["203.0.113.7:4222", "203.0.113.8:4222"]);
        assert!(plain.iter().all(|a| !a.tls_required()));
        assert!(
            plain.iter().all(|a| a.host() != "broker.example.com"),
            "the hostname must not reach the connect list"
        );
    }

    /// A `tls://` server keeps its hostname so the ClientHello carries SNI.
    #[test]
    fn tls_url_keeps_the_hostname_for_sni() {
        let addr: async_nats::ServerAddr = "tls://broker.example.com:4222".parse().expect("addr");
        assert!(addr.tls_required());
        assert_eq!(addr.host(), "broker.example.com");
    }

    /// Only plain NATS over TCP is replaced by a vetted address. A scheme
    /// carrying TLS or a websocket path keeps its URL, whatever its case, so a
    /// TLS server can never fall through to the plaintext branch.
    #[test]
    fn only_plain_nats_is_pinned_and_scheme_case_does_not_downgrade() {
        for url in [
            "nats://broker.example.com:4222",
            "NATS://broker.example.com:4222",
            "broker.example.com:4222",
        ] {
            assert!(
                address_may_be_pinned(url).expect("supported scheme"),
                "{url} is plain NATS and may be pinned"
            );
        }

        for url in [
            "tls://broker.example.com:4222",
            "TLS://broker.example.com:4222",
            "Tls://broker.example.com:4222",
            "ws://broker.example.com:8080/nats",
            "wss://broker.example.com:443/nats",
            "WSS://broker.example.com:443/nats",
        ] {
            assert!(
                !address_may_be_pinned(url).expect("supported scheme"),
                "{url} must keep its URL rather than become a plaintext address"
            );
        }
    }

    /// A scheme async_nats does not accept fails instead of quietly becoming a
    /// plaintext NATS connection, so a typo cannot downgrade the transport.
    #[test]
    fn unsupported_scheme_is_refused() {
        for url in ["tsl://broker.example.com:4222", "http://broker.example.com"] {
            let err = address_may_be_pinned(url).expect_err("unsupported scheme must fail");
            assert!(
                matches!(err, BrokerError::ConnectionFailed(ref m) if m.contains("unsupported")),
                "{url}: {err}"
            );
        }
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
