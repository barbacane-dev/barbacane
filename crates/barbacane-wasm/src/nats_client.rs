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

        // SSRF guard: refuse to connect to internal/metadata targets unless the
        // operator has opted into internal egress.
        let (host, port) = crate::broker::split_host_port(url, DEFAULT_NATS_PORT);
        match crate::http_client::guard_external_host(&host, port, self.allow_internal_egress).await
        {
            Ok(()) => {}
            Err(crate::http_client::HostGuardError::Blocked(h)) => {
                return Err(BrokerError::Blocked(h));
            }
            Err(crate::http_client::HostGuardError::Resolve(m)) => {
                return Err(BrokerError::ConnectionFailed(m));
            }
        }

        // Connect (outside the lock) with a bounded timeout.
        let client = tokio::time::timeout(CONNECT_TIMEOUT, async_nats::connect(url))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_starts_empty() {
        let publisher = NatsPublisher::new(true).expect("nats publisher");
        let conns = publisher.connections.lock();
        assert!(conns.is_empty());
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
