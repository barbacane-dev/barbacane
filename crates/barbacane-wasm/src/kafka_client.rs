//! Kafka publisher for the Barbacane gateway.
//!
//! Provides a connection-caching Kafka client for the `host_kafka_publish` host function.
//! Connections are lazily established on first publish and cached by broker URL.
//! A dedicated tokio runtime keeps Kafka background tasks alive between publishes.

use crate::broker::{BrokerError, PublishResult};
use chrono::Utc;
use parking_lot::Mutex;
use rskafka::client::partition::{Compression, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::record::Record;
use rskafka::BackoffConfig;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

/// Default Kafka broker port, used when an address omits one.
const DEFAULT_KAFKA_PORT: u16 = 9092;

/// Upper bound on cached broker connections, so a plugin can't force unbounded
/// connection growth by publishing to many distinct broker strings.
const MAX_KAFKA_CONNECTIONS: usize = 256;

/// Timeout for an individual produce operation.
const PRODUCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Kafka publisher with connection caching.
///
/// Owns a dedicated tokio runtime so that `rskafka::Client` background tasks
/// stay alive between publish calls. Connections are created lazily on first
/// publish and reused for subsequent messages to the same broker.
pub struct KafkaPublisher {
    runtime: tokio::runtime::Runtime,
    clients: Mutex<HashMap<String, Arc<Client>>>,
    /// When false, broker addresses resolving to internal/metadata ranges are
    /// rejected (SSRF guard). Operators opt in for trusted internal brokers.
    allow_internal_egress: bool,
}

impl KafkaPublisher {
    /// Create a new Kafka publisher with its own background runtime.
    pub fn new(allow_internal_egress: bool) -> Result<Self, BrokerError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("kafka-runtime")
            .enable_all()
            .build()
            .map_err(|e| BrokerError::ConnectionFailed(format!("failed to create runtime: {e}")))?;
        Ok(Self {
            runtime,
            clients: Mutex::new(HashMap::new()),
            allow_internal_egress,
        })
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

        let offsets = tokio::time::timeout(
            PRODUCE_TIMEOUT,
            partition_client.produce(vec![record], Compression::NoCompression),
        )
        .await
        .map_err(|_| BrokerError::Timeout)?
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
            let clients = self.clients.lock();
            if let Some(client) = clients.get(brokers) {
                return Ok(client.clone());
            }
        }

        // Parse broker addresses (comma-separated)
        let broker_list: Vec<String> = brokers.split(',').map(|s| s.trim().to_string()).collect();

        // SSRF guard: resolve each bootstrap broker once and pin the connection to
        // the vetted IPs, refusing internal/metadata targets unless the operator
        // opted into internal egress. Pinning (vs. letting rskafka resolve again at
        // connect time) closes the DNS-rebinding TOCTOU window for the bootstrap
        // connection. The Kafka client here is plaintext, so connecting by IP is
        // transparent.
        //
        // Residual (rskafka 0.6 limitation): after bootstrap, rskafka discovers the
        // cluster's *advertised* broker addresses from metadata and connects to
        // those directly. rskafka exposes no resolver/socket hook, so those
        // addresses cannot be filtered here; a hostile broker advertising an
        // internal listener is not fully mitigated. Tracked as follow-up.
        let mut pinned_brokers: Vec<String> = Vec::new();
        for broker in &broker_list {
            let (host, port) = crate::broker::split_host_port(broker, DEFAULT_KAFKA_PORT);
            match crate::http_client::resolve_permitted_addrs(
                &host,
                port,
                self.allow_internal_egress,
            )
            .await
            {
                Ok(addrs) => pinned_brokers.extend(addrs.iter().map(|a| a.to_string())),
                Err(crate::http_client::HostGuardError::Blocked(h)) => {
                    return Err(BrokerError::Blocked(h));
                }
                Err(crate::http_client::HostGuardError::Resolve(m)) => {
                    return Err(BrokerError::ConnectionFailed(m));
                }
            }
        }

        // Connect (outside the lock) with a 5s deadline to avoid blocking the host function
        let backoff = BackoffConfig {
            deadline: Some(Duration::from_secs(5)),
            ..Default::default()
        };
        let client = ClientBuilder::new(pinned_brokers)
            .backoff_config(backoff)
            .build()
            .await
            .map_err(|e| BrokerError::ConnectionFailed(e.to_string()))?;

        let client = Arc::new(client);
        tracing::info!(brokers = %brokers, "established Kafka connection");

        // Cache the new connection, bounding the cache size.
        {
            let mut clients = self.clients.lock();
            if clients.len() >= MAX_KAFKA_CONNECTIONS && !clients.contains_key(brokers) {
                return Err(BrokerError::ConnectionFailed(
                    "Kafka connection cache is full".to_string(),
                ));
            }
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
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
        let clients = publisher.clients.lock();
        assert!(clients.is_empty());
    }

    #[test]
    fn blocks_internal_broker_when_egress_disallowed() {
        let publisher = KafkaPublisher::new(false).expect("kafka publisher");
        let result = publisher.publish_blocking(
            "169.254.169.254:9092",
            "test-topic",
            None,
            "hello",
            BTreeMap::new(),
        );
        assert!(matches!(result, Err(BrokerError::Blocked(_))));
    }

    #[test]
    fn publish_blocking_connection_refused() {
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
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
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
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
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
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
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
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
