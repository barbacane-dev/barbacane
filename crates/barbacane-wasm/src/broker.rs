//! Shared types for message broker dispatch (Kafka, NATS).
//!
//! Defines the common message, result, and error types used by both
//! `KafkaPublisher` and `NatsPublisher` host functions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// Broker URL (e.g., nats://localhost:4222 or kafka:9092).
    /// Provided per-message by the plugin from its dispatcher config.
    #[serde(default)]
    pub url: Option<String>,

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_message_url_roundtrip() {
        let message = BrokerMessage {
            url: Some("nats://localhost:4222".to_string()),
            topic: "test.subject".to_string(),
            key: None,
            payload: r#"{"data":"hello"}"#.to_string(),
            headers: BTreeMap::new(),
        };

        let json = serde_json::to_string(&message).unwrap();
        let deserialized: BrokerMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.url, Some("nats://localhost:4222".to_string()));
        assert_eq!(deserialized.topic, "test.subject");
        assert_eq!(deserialized.payload, r#"{"data":"hello"}"#);
    }

    #[test]
    fn broker_message_url_absent() {
        let json = r#"{"topic":"events","payload":"{}"}"#;
        let message: BrokerMessage = serde_json::from_str(json).unwrap();
        assert!(message.url.is_none());
        assert_eq!(message.topic, "events");
    }

    #[test]
    fn broker_message_with_key_and_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("x-request-id".to_string(), "req-123".to_string());
        headers.insert("x-trace-id".to_string(), "trace-456".to_string());

        let message = BrokerMessage {
            url: Some("kafka:9092".to_string()),
            topic: "orders.placed".to_string(),
            key: Some("order-789".to_string()),
            payload: r#"{"orderId":"789"}"#.to_string(),
            headers,
        };

        let json = serde_json::to_string(&message).unwrap();
        let deserialized: BrokerMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.key, Some("order-789".to_string()));
        assert_eq!(deserialized.headers.len(), 2);
        assert_eq!(deserialized.headers["x-request-id"], "req-123");
        assert_eq!(deserialized.headers["x-trace-id"], "trace-456");
    }

    #[test]
    fn broker_message_key_defaults_to_none() {
        let json = r#"{"topic":"events","payload":"{}"}"#;
        let message: BrokerMessage = serde_json::from_str(json).unwrap();
        assert!(message.key.is_none());
        assert!(message.headers.is_empty());
    }

    #[test]
    fn publish_result_success_constructor() {
        let result = PublishResult::success("orders.placed".to_string());
        assert!(result.success);
        assert!(result.error.is_none());
        assert_eq!(result.topic, "orders.placed");
        assert!(result.partition.is_none());
        assert!(result.offset.is_none());
    }

    #[test]
    fn publish_result_failure_constructor() {
        let result = PublishResult::failure(
            "orders.placed".to_string(),
            "connection refused".to_string(),
        );
        assert!(!result.success);
        assert_eq!(result.error, Some("connection refused".to_string()));
        assert_eq!(result.topic, "orders.placed");
    }

    #[test]
    fn publish_result_success_serialization_skips_optional() {
        let result = PublishResult::success("events".to_string());
        let json = serde_json::to_string(&result).unwrap();

        // skip_serializing_if should omit error, partition, and offset
        assert!(!json.contains("error"));
        assert!(!json.contains("partition"));
        assert!(!json.contains("offset"));
        assert!(json.contains(r#""success":true"#));
    }

    #[test]
    fn publish_result_failure_serialization_includes_error() {
        let result = PublishResult::failure("events".to_string(), "timeout".to_string());
        let json = serde_json::to_string(&result).unwrap();

        assert!(json.contains(r#""error":"timeout""#));
        assert!(json.contains(r#""success":false"#));
    }

    #[test]
    fn publish_result_with_kafka_metadata_roundtrip() {
        let result = PublishResult {
            success: true,
            error: None,
            topic: "orders".to_string(),
            partition: Some(3),
            offset: Some(42),
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: PublishResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.partition, Some(3));
        assert_eq!(deserialized.offset, Some(42));
    }

    #[test]
    fn broker_error_display() {
        assert_eq!(
            BrokerError::NotConfigured.to_string(),
            "broker not configured"
        );
        assert_eq!(BrokerError::Timeout.to_string(), "timeout");
        assert_eq!(
            BrokerError::ConnectionFailed("refused".to_string()).to_string(),
            "connection failed: refused"
        );
        assert_eq!(
            BrokerError::PublishFailed("full".to_string()).to_string(),
            "publish failed: full"
        );
        assert_eq!(
            BrokerError::InvalidMessage("bad json".to_string()).to_string(),
            "invalid message: bad json"
        );
    }
}
