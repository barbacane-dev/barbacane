//! Kafka dispatcher plugin for Barbacane API gateway.
//!
//! Publishes incoming HTTP requests to Kafka topics and returns 202 Accepted.
//! Implements the sync-to-async bridge pattern for event-driven architectures.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Kafka dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct KafkaDispatcher {
    /// Kafka broker addresses (comma-separated, e.g. "kafka:9092").
    brokers: String,

    /// Kafka topic to publish messages to.
    topic: String,

    /// Message key expression (supports $request.header.X-Key, $request.path.id, or literal).
    #[serde(default)]
    key: Option<String>,

    /// Custom acknowledgment response configuration.
    #[serde(default)]
    ack_response: Option<AckResponse>,

    /// Include publish metadata (partition, offset) in response.
    #[serde(default)]
    include_metadata: bool,

    /// Request headers to pass as Kafka message headers.
    #[serde(default)]
    headers_from_request: Vec<String>,
}

/// Custom acknowledgment response.
#[derive(Default, Deserialize)]
struct AckResponse {
    body: Option<serde_json::Value>,
    headers: Option<BTreeMap<String, String>>,
}

/// Message to send to host_kafka_publish.
#[derive(Serialize)]
struct BrokerMessage {
    url: String,
    topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    payload: String,
    headers: BTreeMap<String, String>,
}

/// Result from host_kafka_publish.
#[derive(Deserialize)]
struct PublishResult {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    topic: String,
    #[serde(default)]
    partition: Option<i32>,
    #[serde(default)]
    offset: Option<i64>,
}

impl KafkaDispatcher {
    /// Dispatch a request by publishing to Kafka and returning 202 Accepted.
    pub fn dispatch(&mut self, req: Request) -> Response {
        // Resolve the message key
        let key = self.resolve_key(&req);

        // Build message headers from request headers
        let mut msg_headers = BTreeMap::new();
        for header_name in &self.headers_from_request {
            if let Some(value) = req.headers.get(header_name) {
                msg_headers.insert(header_name.clone(), value.clone());
            }
        }

        // Add standard metadata headers
        if let Some(request_id) = req.headers.get("x-request-id") {
            msg_headers.insert("x-request-id".to_string(), request_id.clone());
        }
        if let Some(trace_id) = req.headers.get("x-trace-id") {
            msg_headers.insert("x-trace-id".to_string(), trace_id.clone());
        }

        // Build the broker message
        let message = BrokerMessage {
            url: self.brokers.clone(),
            topic: self.topic.clone(),
            key,
            payload: req.body.clone().unwrap_or_default(),
            headers: msg_headers,
        };

        // Serialize and publish
        let message_json = match serde_json::to_vec(&message) {
            Ok(json) => json,
            Err(e) => {
                return self.error_response(500, "failed to serialize message", &e.to_string())
            }
        };

        let result_len =
            unsafe { host_kafka_publish(message_json.as_ptr() as i32, message_json.len() as i32) };

        if result_len < 0 {
            return self.error_response(
                502,
                "Kafka publish failed",
                "host_kafka_publish returned error",
            );
        }

        // Read the publish result
        let mut result_buf = vec![0u8; result_len as usize];
        let bytes_read =
            unsafe { host_broker_read_result(result_buf.as_mut_ptr() as i32, result_len) };

        if bytes_read <= 0 {
            return self.error_response(
                502,
                "Kafka publish failed",
                "failed to read publish result",
            );
        }

        // Parse the publish result
        let publish_result: PublishResult =
            match serde_json::from_slice(&result_buf[..bytes_read as usize]) {
                Ok(r) => r,
                Err(e) => {
                    return self.error_response(502, "invalid publish result", &e.to_string())
                }
            };

        if !publish_result.success {
            let detail = publish_result
                .error
                .unwrap_or_else(|| "unknown error".to_string());
            return self.error_response(502, "Kafka publish failed", &detail);
        }

        // Build the 202 Accepted response
        self.accepted_response(&publish_result)
    }

    /// Resolve the message key from config and request.
    fn resolve_key(&self, req: &Request) -> Option<String> {
        let key_expr = self.key.as_ref()?;

        if key_expr.starts_with("$request.header.") {
            let header_name = key_expr.strip_prefix("$request.header.")?;
            req.headers.get(header_name).cloned()
        } else if key_expr.starts_with("$request.path.") {
            let param_name = key_expr.strip_prefix("$request.path.")?;
            req.path_params.get(param_name).cloned()
        } else {
            // Literal value
            Some(key_expr.clone())
        }
    }

    /// Build the 202 Accepted response.
    fn accepted_response(&self, result: &PublishResult) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());

        // Add custom headers from config
        if let Some(ref ack) = self.ack_response {
            if let Some(ref custom_headers) = ack.headers {
                for (k, v) in custom_headers {
                    headers.insert(k.clone(), v.clone());
                }
            }
        }

        // Build response body
        let body = if let Some(ref ack) = self.ack_response {
            if let Some(ref custom_body) = ack.body {
                custom_body.to_string()
            } else {
                self.default_ack_body(result)
            }
        } else {
            self.default_ack_body(result)
        };

        Response {
            status: 202,
            headers,
            body: Some(body),
        }
    }

    /// Build the default acknowledgment body.
    fn default_ack_body(&self, result: &PublishResult) -> String {
        if self.include_metadata {
            serde_json::json!({
                "status": "accepted",
                "topic": result.topic,
                "partition": result.partition,
                "offset": result.offset
            })
            .to_string()
        } else {
            serde_json::json!({
                "status": "accepted"
            })
            .to_string()
        }
    }

    /// Create an error response in RFC 9457 format.
    fn error_response(&self, status: u16, title: &str, detail: &str) -> Response {
        let error_type = match status {
            502 => "urn:barbacane:error:kafka-publish-failed",
            503 => "urn:barbacane:error:kafka-unavailable",
            _ => "urn:barbacane:error:internal",
        };

        let body = serde_json::json!({
            "type": error_type,
            "title": title,
            "status": status,
            "detail": detail
        });

        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        Response {
            status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

// Host function declarations
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Publish a message to Kafka. Returns the result length, or -1 on error.
    fn host_kafka_publish(msg_ptr: i32, msg_len: i32) -> i32;

    /// Read the broker publish result into the provided buffer. Returns bytes read.
    fn host_broker_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}

// Native stubs for testing
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_kafka_publish(_msg_ptr: i32, _msg_len: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_broker_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_request() -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("x-request-id".to_string(), "req-123".to_string());
        headers.insert("x-trace-id".to_string(), "trace-456".to_string());
        headers.insert("x-custom-header".to_string(), "custom-value".to_string());

        let mut path_params = BTreeMap::new();
        path_params.insert("id".to_string(), "user-789".to_string());

        Request {
            method: "POST".to_string(),
            path: "/api/users".to_string(),
            headers,
            body: Some(r#"{"name":"test"}"#.to_string()),
            query: None,
            path_params,
            client_ip: "127.0.0.1".to_string(),
        }
    }

    fn create_publish_result(success: bool) -> PublishResult {
        PublishResult {
            success,
            error: if success {
                None
            } else {
                Some("publish error".to_string())
            },
            topic: "test-topic".to_string(),
            partition: Some(0),
            offset: Some(12345),
        }
    }

    #[test]
    fn test_resolve_key_from_header() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "key": "$request.header.x-custom-header"
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let req = create_test_request();

        let key = dispatcher.resolve_key(&req);
        assert_eq!(key, Some("custom-value".to_string()));
    }

    #[test]
    fn test_resolve_key_from_path_param() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "key": "$request.path.id"
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let req = create_test_request();

        let key = dispatcher.resolve_key(&req);
        assert_eq!(key, Some("user-789".to_string()));
    }

    #[test]
    fn test_resolve_key_literal_value() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "key": "static-key"
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let req = create_test_request();

        let key = dispatcher.resolve_key(&req);
        assert_eq!(key, Some("static-key".to_string()));
    }

    #[test]
    fn test_resolve_key_none_when_no_key_configured() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic"
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let req = create_test_request();

        let key = dispatcher.resolve_key(&req);
        assert_eq!(key, None);
    }

    #[test]
    fn test_resolve_key_none_when_header_not_found() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "key": "$request.header.nonexistent"
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let req = create_test_request();

        let key = dispatcher.resolve_key(&req);
        assert_eq!(key, None);
    }

    #[test]
    fn test_accepted_response_without_metadata() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "include_metadata": false
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let result = create_publish_result(true);

        let response = dispatcher.accepted_response(&result);
        assert_eq!(response.status, 202);
        assert_eq!(
            response.headers.get("content-type"),
            Some(&"application/json".to_string())
        );

        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["status"], "accepted");
        assert!(body.get("topic").is_none());
        assert!(body.get("partition").is_none());
        assert!(body.get("offset").is_none());
    }

    #[test]
    fn test_accepted_response_with_metadata() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "include_metadata": true
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let result = create_publish_result(true);

        let response = dispatcher.accepted_response(&result);
        assert_eq!(response.status, 202);

        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["status"], "accepted");
        assert_eq!(body["topic"], "test-topic");
        assert_eq!(body["partition"], 0);
        assert_eq!(body["offset"], 12345);
    }

    #[test]
    fn test_default_ack_body_without_metadata() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "include_metadata": false
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let result = create_publish_result(true);

        let body_str = dispatcher.default_ack_body(&result);
        let body: serde_json::Value = serde_json::from_str(&body_str).unwrap();

        assert_eq!(body["status"], "accepted");
        assert!(body.get("topic").is_none());
    }

    #[test]
    fn test_default_ack_body_with_metadata() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "include_metadata": true
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let result = create_publish_result(true);

        let body_str = dispatcher.default_ack_body(&result);
        let body: serde_json::Value = serde_json::from_str(&body_str).unwrap();

        assert_eq!(body["status"], "accepted");
        assert_eq!(body["topic"], "test-topic");
        assert_eq!(body["partition"], 0);
        assert_eq!(body["offset"], 12345);
    }

    #[test]
    fn test_error_response_502() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic"
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();

        let response = dispatcher.error_response(502, "Kafka publish failed", "connection timeout");
        assert_eq!(response.status, 502);
        assert_eq!(
            response.headers.get("content-type"),
            Some(&"application/problem+json".to_string())
        );

        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:kafka-publish-failed");
        assert_eq!(body["title"], "Kafka publish failed");
        assert_eq!(body["status"], 502);
        assert_eq!(body["detail"], "connection timeout");
    }

    #[test]
    fn test_custom_ack_response_body() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "ack_response": {
                "body": {"message": "queued", "id": 123}
            }
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let result = create_publish_result(true);

        let response = dispatcher.accepted_response(&result);
        assert_eq!(response.status, 202);

        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["message"], "queued");
        assert_eq!(body["id"], 123);
    }

    #[test]
    fn test_custom_ack_response_headers() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "ack_response": {
                "headers": {
                    "x-custom-response": "custom-value",
                    "x-tracking-id": "track-123"
                }
            }
        });
        let dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let result = create_publish_result(true);

        let response = dispatcher.accepted_response(&result);
        assert_eq!(
            response.headers.get("x-custom-response"),
            Some(&"custom-value".to_string())
        );
        assert_eq!(
            response.headers.get("x-tracking-id"),
            Some(&"track-123".to_string())
        );
        assert_eq!(
            response.headers.get("content-type"),
            Some(&"application/json".to_string())
        );
    }

    #[test]
    fn test_config_deserialization_minimal() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic"
        });
        let dispatcher: Result<KafkaDispatcher, _> = serde_json::from_value(config);
        assert!(dispatcher.is_ok());

        let dispatcher = dispatcher.unwrap();
        assert_eq!(dispatcher.brokers, "kafka:9092");
        assert_eq!(dispatcher.topic, "test-topic");
        assert_eq!(dispatcher.key, None);
        assert!(!dispatcher.include_metadata);
        assert!(dispatcher.headers_from_request.is_empty());
    }

    #[test]
    fn test_config_deserialization_full() {
        let config = serde_json::json!({
            "brokers": "kafka1:9092,kafka2:9092",
            "topic": "events",
            "key": "$request.header.x-key",
            "include_metadata": true,
            "headers_from_request": ["x-correlation-id", "x-tenant-id"],
            "ack_response": {
                "body": {"status": "ok"},
                "headers": {"x-custom": "value"}
            }
        });
        let dispatcher: Result<KafkaDispatcher, _> = serde_json::from_value(config);
        assert!(dispatcher.is_ok());

        let dispatcher = dispatcher.unwrap();
        assert_eq!(dispatcher.brokers, "kafka1:9092,kafka2:9092");
        assert_eq!(dispatcher.topic, "events");
        assert_eq!(dispatcher.key, Some("$request.header.x-key".to_string()));
        assert!(dispatcher.include_metadata);
        assert_eq!(
            dispatcher.headers_from_request,
            vec!["x-correlation-id", "x-tenant-id"]
        );
    }

    #[test]
    fn test_dispatch_with_native_stub_returns_502() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic"
        });
        let mut dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let req = create_test_request();

        // Native stub returns -1, so dispatch should return 502
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
        assert_eq!(
            response.headers.get("content-type"),
            Some(&"application/problem+json".to_string())
        );

        let body: serde_json::Value =
            serde_json::from_str(response.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:kafka-publish-failed");
        assert_eq!(body["title"], "Kafka publish failed");
    }

    #[test]
    fn test_message_header_propagation_specified_headers() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "headers_from_request": ["x-custom-header"]
        });
        let mut dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let req = create_test_request();

        // We can't easily test the actual message content without mocking the host function,
        // but we can verify the dispatch logic runs without panicking
        let response = dispatcher.dispatch(req);
        // Should be 502 because native stub returns -1
        assert_eq!(response.status, 502);
    }

    #[test]
    fn test_message_header_propagation_auto_includes_request_id_and_trace_id() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "headers_from_request": []
        });
        let mut dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();

        let mut req = create_test_request();
        req.headers
            .insert("x-request-id".to_string(), "req-abc".to_string());
        req.headers
            .insert("x-trace-id".to_string(), "trace-xyz".to_string());

        // The dispatch method should include x-request-id and x-trace-id automatically
        let response = dispatcher.dispatch(req);
        // Should be 502 because native stub returns -1
        assert_eq!(response.status, 502);
    }

    #[test]
    fn test_message_header_propagation_missing_header() {
        let config = serde_json::json!({
            "brokers": "kafka:9092",
            "topic": "test-topic",
            "headers_from_request": ["x-nonexistent-header"]
        });
        let mut dispatcher: KafkaDispatcher = serde_json::from_value(config).unwrap();
        let req = create_test_request();

        // Should handle missing headers gracefully
        let response = dispatcher.dispatch(req);
        assert_eq!(response.status, 502);
    }
}
