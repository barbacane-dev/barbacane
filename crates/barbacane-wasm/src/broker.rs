//! Message broker abstractions for event dispatch.
//!
//! Provides a unified interface for publishing messages to Kafka and NATS.
//! The actual broker implementations can be swapped out (mock for testing,
//! real clients for production).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;

/// Errors from broker operations.
#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("broker not configured")]
    NotConfigured,

    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("publish failed: {0}")]
    PublishFailed(String),

    #[error("invalid message: {0}")]
    InvalidMessage(String),

    #[error("timeout")]
    Timeout,
}

/// A message to publish to a broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerMessage {
    /// Topic (Kafka) or subject (NATS).
    pub topic: String,

    /// Message key (optional, used for partitioning in Kafka).
    #[serde(default)]
    pub key: Option<String>,

    /// Message payload (JSON serialized).
    pub payload: String,

    /// Message headers/metadata.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// Result of a publish operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishResult {
    /// Whether the publish succeeded.
    pub success: bool,

    /// Error message if publish failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Topic/subject the message was published to.
    pub topic: String,

    /// Partition the message was published to (Kafka only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<i32>,

    /// Offset of the published message (Kafka only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

impl PublishResult {
    /// Create a successful publish result.
    pub fn success(topic: String) -> Self {
        Self {
            success: true,
            error: None,
            topic,
            partition: None,
            offset: None,
        }
    }

    /// Create a failed publish result.
    pub fn failure(topic: String, error: String) -> Self {
        Self {
            success: false,
            error: Some(error),
            topic,
            partition: None,
            offset: None,
        }
    }
}

/// Trait for message brokers (Kafka, NATS).
pub trait MessageBroker: Send + Sync {
    /// Publish a message to the broker.
    fn publish(&self, message: BrokerMessage) -> Result<PublishResult, BrokerError>;

    /// Get the broker type name.
    fn broker_type(&self) -> &'static str;
}

/// Mock broker for testing.
/// Records published messages for verification.
#[derive(Clone, Default)]
pub struct MockBroker {
    messages: Arc<RwLock<Vec<BrokerMessage>>>,
    fail_on_topic: Arc<RwLock<Option<String>>>,
}

impl MockBroker {
    /// Create a new mock broker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get all published messages.
    pub fn messages(&self) -> Vec<BrokerMessage> {
        self.messages.read().unwrap().clone()
    }

    /// Clear recorded messages.
    pub fn clear(&self) {
        self.messages.write().unwrap().clear();
    }

    /// Set a topic that should fail on publish.
    pub fn fail_on(&self, topic: &str) {
        *self.fail_on_topic.write().unwrap() = Some(topic.to_string());
    }
}

impl MessageBroker for MockBroker {
    fn publish(&self, message: BrokerMessage) -> Result<PublishResult, BrokerError> {
        // Check if we should fail
        if let Some(fail_topic) = self.fail_on_topic.read().unwrap().as_ref() {
            if &message.topic == fail_topic {
                return Ok(PublishResult::failure(
                    message.topic,
                    "simulated failure".to_string(),
                ));
            }
        }

        let topic = message.topic.clone();
        self.messages.write().unwrap().push(message);

        Ok(PublishResult::success(topic))
    }

    fn broker_type(&self) -> &'static str {
        "mock"
    }
}

/// Kafka broker client.
///
/// Note: Actual Kafka implementation requires the `rdkafka` crate.
/// This is a placeholder that can be implemented when broker support is needed.
pub struct KafkaBroker {
    /// Broker addresses (comma-separated).
    _brokers: String,
}

impl KafkaBroker {
    /// Create a new Kafka broker client.
    ///
    /// # Arguments
    /// * `brokers` - Comma-separated list of broker addresses (e.g., "localhost:9092")
    pub fn new(brokers: &str) -> Self {
        Self {
            _brokers: brokers.to_string(),
        }
    }
}

impl MessageBroker for KafkaBroker {
    fn publish(&self, message: BrokerMessage) -> Result<PublishResult, BrokerError> {
        // Placeholder - actual Kafka implementation would use rdkafka
        tracing::warn!(
            topic = %message.topic,
            "Kafka publish not yet implemented, message dropped"
        );
        Ok(PublishResult::failure(
            message.topic,
            "Kafka client not yet implemented".to_string(),
        ))
    }

    fn broker_type(&self) -> &'static str {
        "kafka"
    }
}

/// NATS broker client.
///
/// Note: Actual NATS implementation requires the `async-nats` crate.
/// This is a placeholder that can be implemented when broker support is needed.
pub struct NatsBroker {
    /// Server addresses (comma-separated).
    _servers: String,
}

impl NatsBroker {
    /// Create a new NATS broker client.
    ///
    /// # Arguments
    /// * `servers` - Comma-separated list of server addresses (e.g., "localhost:4222")
    pub fn new(servers: &str) -> Self {
        Self {
            _servers: servers.to_string(),
        }
    }
}

impl MessageBroker for NatsBroker {
    fn publish(&self, message: BrokerMessage) -> Result<PublishResult, BrokerError> {
        // Placeholder - actual NATS implementation would use async-nats
        tracing::warn!(
            subject = %message.topic,
            "NATS publish not yet implemented, message dropped"
        );
        Ok(PublishResult::failure(
            message.topic,
            "NATS client not yet implemented".to_string(),
        ))
    }

    fn broker_type(&self) -> &'static str {
        "nats"
    }
}

/// Broker registry for managing multiple broker connections.
#[derive(Default)]
pub struct BrokerRegistry {
    kafka: Option<Arc<dyn MessageBroker>>,
    nats: Option<Arc<dyn MessageBroker>>,
}

impl BrokerRegistry {
    /// Create a new empty broker registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a broker registry with mock brokers for testing.
    pub fn with_mocks() -> Self {
        Self {
            kafka: Some(Arc::new(MockBroker::new())),
            nats: Some(Arc::new(MockBroker::new())),
        }
    }

    /// Set the Kafka broker.
    pub fn set_kafka(&mut self, broker: Arc<dyn MessageBroker>) {
        self.kafka = Some(broker);
    }

    /// Set the NATS broker.
    pub fn set_nats(&mut self, broker: Arc<dyn MessageBroker>) {
        self.nats = Some(broker);
    }

    /// Get the Kafka broker.
    pub fn kafka(&self) -> Option<&Arc<dyn MessageBroker>> {
        self.kafka.as_ref()
    }

    /// Get the NATS broker.
    pub fn nats(&self) -> Option<&Arc<dyn MessageBroker>> {
        self.nats.as_ref()
    }

    /// Publish to Kafka.
    pub fn publish_kafka(&self, message: BrokerMessage) -> Result<PublishResult, BrokerError> {
        match &self.kafka {
            Some(broker) => broker.publish(message),
            None => Err(BrokerError::NotConfigured),
        }
    }

    /// Publish to NATS.
    pub fn publish_nats(&self, message: BrokerMessage) -> Result<PublishResult, BrokerError> {
        match &self.nats {
            Some(broker) => broker.publish(message),
            None => Err(BrokerError::NotConfigured),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_broker_records_messages() {
        let broker = MockBroker::new();

        let message = BrokerMessage {
            topic: "test-topic".to_string(),
            key: Some("key-1".to_string()),
            payload: r#"{"event":"test"}"#.to_string(),
            headers: BTreeMap::new(),
        };

        let result = broker.publish(message).unwrap();
        assert!(result.success);
        assert_eq!(result.topic, "test-topic");

        let messages = broker.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].topic, "test-topic");
        assert_eq!(messages[0].key, Some("key-1".to_string()));
    }

    #[test]
    fn mock_broker_simulates_failure() {
        let broker = MockBroker::new();
        broker.fail_on("fail-topic");

        let message = BrokerMessage {
            topic: "fail-topic".to_string(),
            key: None,
            payload: "{}".to_string(),
            headers: BTreeMap::new(),
        };

        let result = broker.publish(message).unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn broker_registry_with_mocks() {
        let registry = BrokerRegistry::with_mocks();

        let message = BrokerMessage {
            topic: "events".to_string(),
            key: None,
            payload: "{}".to_string(),
            headers: BTreeMap::new(),
        };

        let result = registry.publish_kafka(message.clone()).unwrap();
        assert!(result.success);

        let result = registry.publish_nats(message).unwrap();
        assert!(result.success);
    }

    #[test]
    fn broker_registry_not_configured() {
        let registry = BrokerRegistry::new();

        let message = BrokerMessage {
            topic: "events".to_string(),
            key: None,
            payload: "{}".to_string(),
            headers: BTreeMap::new(),
        };

        let result = registry.publish_kafka(message);
        assert!(matches!(result, Err(BrokerError::NotConfigured)));
    }
}
