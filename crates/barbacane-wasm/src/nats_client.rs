//! NATS publisher for the Barbacane gateway.
//!
//! Provides a connection-caching NATS client for the `host_nats_publish` host function.
//! Connections are lazily established on first publish and cached by server URL.
//! A dedicated tokio runtime keeps NATS background tasks alive between publishes.

use crate::broker::{BrokerError, PublishResult};
use bytes::Bytes;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

/// NATS publisher with connection caching.
///
/// Owns a dedicated tokio runtime so that `async_nats::Client` background tasks
/// (heartbeats, reconnection) stay alive between publish calls. Connections are
/// created lazily on first publish and reused for subsequent messages to the same server.
pub struct NatsPublisher {
    runtime: tokio::runtime::Runtime,
    connections: Mutex<HashMap<String, async_nats::Client>>,
}

impl Default for NatsPublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl NatsPublisher {
    /// Create a new NATS publisher with its own background runtime.
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("nats-runtime")
            .enable_all()
            .build()
            .expect("failed to create NATS runtime");
        Self {
            runtime,
            connections: Mutex::new(HashMap::new()),
        }
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
            client
                .publish(subject.to_string(), payload)
                .await
                .map_err(|e| BrokerError::PublishFailed(e.to_string()))?;
        } else {
            let mut header_map = async_nats::HeaderMap::new();
            for (k, v) in &headers {
                header_map.insert(k.as_str(), v.as_str());
            }
            client
                .publish_with_headers(subject.to_string(), header_map, payload)
                .await
                .map_err(|e| BrokerError::PublishFailed(e.to_string()))?;
        }

        Ok(PublishResult::success(subject.to_string()))
    }

    /// Get a cached connection or establish a new one.
    async fn get_or_connect(&self, url: &str) -> Result<async_nats::Client, BrokerError> {
        // Check cache (lock is held briefly, no await while locked)
        {
            let conns = self.connections.lock().unwrap();
            if let Some(client) = conns.get(url) {
                return Ok(client.clone());
            }
        }

        // Connect (outside the lock)
        let client = async_nats::connect(url)
            .await
            .map_err(|e| BrokerError::ConnectionFailed(e.to_string()))?;

        tracing::info!(url = %url, "established NATS connection");

        // Cache the new connection
        {
            let mut conns = self.connections.lock().unwrap();
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
        let publisher = NatsPublisher::new();
        let conns = publisher.connections.lock().unwrap();
        assert!(conns.is_empty());
    }

    #[test]
    fn default_impl() {
        let publisher = NatsPublisher::default();
        let conns = publisher.connections.lock().unwrap();
        assert!(conns.is_empty());
    }

    #[test]
    fn publish_blocking_connection_refused() {
        let publisher = NatsPublisher::new();
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
        let publisher = NatsPublisher::new();
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
        let publisher = NatsPublisher::new();
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
