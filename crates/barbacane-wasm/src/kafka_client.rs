//! Kafka publisher for the Barbacane gateway.
//!
//! Provides a connection-caching Kafka client for the `host_kafka_publish` host function.
//! Connections are lazily established on first publish and cached by broker URL.
//! A dedicated tokio runtime keeps Kafka background tasks alive between publishes.

use crate::broker::{BrokerError, PublishResult};
use chrono::Utc;
use rskafka::client::partition::{Compression, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::record::Record;
use rskafka::BackoffConfig;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Kafka publisher with connection caching.
///
/// Owns a dedicated tokio runtime so that `rskafka::Client` background tasks
/// stay alive between publish calls. Connections are created lazily on first
/// publish and reused for subsequent messages to the same broker.
pub struct KafkaPublisher {
    runtime: tokio::runtime::Runtime,
    clients: Mutex<HashMap<String, Arc<Client>>>,
}

impl Default for KafkaPublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl KafkaPublisher {
    /// Create a new Kafka publisher with its own background runtime.
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("kafka-runtime")
            .enable_all()
            .build()
            .expect("failed to create Kafka runtime");
        Self {
            runtime,
            clients: Mutex::new(HashMap::new()),
        }
    }

    /// Blocking publish for use from sync WASM host functions.
    ///
    /// Must be called from a thread that is NOT inside a tokio runtime context
    /// (e.g. from within `std::thread::scope`).
    pub fn publish_blocking(
        &self,
        brokers: &str,
        topic: &str,
        key: Option<String>,
        payload: &str,
        headers: BTreeMap<String, String>,
    ) -> Result<PublishResult, BrokerError> {
        self.runtime
            .block_on(self.publish(brokers, topic, key, payload, headers))
    }

    /// Publish a message to a Kafka topic (async).
    async fn publish(
        &self,
        brokers: &str,
        topic: &str,
        key: Option<String>,
        payload: &str,
        headers: BTreeMap<String, String>,
    ) -> Result<PublishResult, BrokerError> {
        let client = self.get_or_connect(brokers).await?;

        let partition_client = client
            .partition_client(topic.to_string(), 0, UnknownTopicHandling::Retry)
            .await
            .map_err(|e| BrokerError::ConnectionFailed(e.to_string()))?;

        let record = Record {
            key: key.map(|k| k.into_bytes()),
            value: Some(payload.as_bytes().to_vec()),
            headers: headers
                .into_iter()
                .map(|(k, v)| (k, v.into_bytes()))
                .collect(),
            timestamp: Utc::now(),
        };

        let offsets = partition_client
            .produce(vec![record], Compression::NoCompression)
            .await
            .map_err(|e| BrokerError::PublishFailed(e.to_string()))?;

        let offset = offsets.first().copied();

        Ok(PublishResult {
            success: true,
            error: None,
            topic: topic.to_string(),
            partition: Some(0),
            offset,
        })
    }

    /// Get a cached client or establish a new one.
    async fn get_or_connect(&self, brokers: &str) -> Result<Arc<Client>, BrokerError> {
        // Check cache (lock is held briefly, no await while locked)
        {
            let clients = self.clients.lock().unwrap();
            if let Some(client) = clients.get(brokers) {
                return Ok(client.clone());
            }
        }

        // Parse broker addresses (comma-separated)
        let broker_list: Vec<String> = brokers.split(',').map(|s| s.trim().to_string()).collect();

        // Connect (outside the lock) with a 5s deadline to avoid blocking the host function
        let backoff = BackoffConfig {
            deadline: Some(Duration::from_secs(5)),
            ..Default::default()
        };
        let client = ClientBuilder::new(broker_list)
            .backoff_config(backoff)
            .build()
            .await
            .map_err(|e| BrokerError::ConnectionFailed(e.to_string()))?;

        let client = Arc::new(client);
        tracing::info!(brokers = %brokers, "established Kafka connection");

        // Cache the new connection
        {
            let mut clients = self.clients.lock().unwrap();
            clients.insert(brokers.to_string(), client.clone());
        }

        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_starts_empty() {
        let publisher = KafkaPublisher::new();
        let clients = publisher.clients.lock().unwrap();
        assert!(clients.is_empty());
    }

    #[test]
    fn default_impl() {
        let publisher = KafkaPublisher::default();
        let clients = publisher.clients.lock().unwrap();
        assert!(clients.is_empty());
    }

    #[test]
    fn publish_blocking_connection_refused() {
        let publisher = KafkaPublisher::new();
        let result = publisher.publish_blocking(
            "127.0.0.1:19092",
            "test-topic",
            None,
            "hello",
            BTreeMap::new(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BrokerError::ConnectionFailed(_)));
    }

    #[test]
    fn publish_blocking_from_thread_scope() {
        let publisher = KafkaPublisher::new();
        let result = std::thread::scope(|s| {
            s.spawn(|| {
                publisher.publish_blocking(
                    "127.0.0.1:19092",
                    "test-topic",
                    None,
                    "hello",
                    BTreeMap::new(),
                )
            })
            .join()
            .unwrap()
        });
        assert!(matches!(result, Err(BrokerError::ConnectionFailed(_))));
    }

    #[test]
    fn publish_blocking_with_key_and_headers() {
        let publisher = KafkaPublisher::new();
        let mut headers = BTreeMap::new();
        headers.insert("x-request-id".to_string(), "req-123".to_string());

        let result = publisher.publish_blocking(
            "127.0.0.1:19092",
            "test-topic",
            Some("order-456".to_string()),
            r#"{"orderId":"456"}"#,
            headers,
        );
        // Connection refused, but validates the key/headers path compiles and runs
        assert!(matches!(result, Err(BrokerError::ConnectionFailed(_))));
    }

    #[test]
    fn publish_blocking_comma_separated_brokers() {
        let publisher = KafkaPublisher::new();
        let result = publisher.publish_blocking(
            "127.0.0.1:19092, 127.0.0.1:19093",
            "test-topic",
            None,
            "hello",
            BTreeMap::new(),
        );
        // Connection refused, but validates comma-separated broker parsing
        assert!(matches!(result, Err(BrokerError::ConnectionFailed(_))));
    }
}
