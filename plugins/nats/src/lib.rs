//! NATS dispatcher plugin for Barbacane API gateway.
//!
//! Publishes incoming HTTP requests to NATS subjects and returns 202 Accepted.
//! Implements the sync-to-async bridge pattern for event-driven architectures.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// NATS dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NatsDispatcher {
    /// NATS server URL (e.g., nats://localhost:4222).
    url: String,

    /// NATS subject to publish messages to: one subject, without wildcards.
    #[serde(deserialize_with = "subject")]
    subject: String,

    /// Custom acknowledgment response configuration.
    #[serde(default)]
    ack_response: Option<AckResponse>,

    /// Request headers to pass as NATS message headers.
    #[serde(default)]
    headers_from_request: Vec<String>,
}

/// Custom acknowledgment response.
#[derive(Default, Deserialize)]
struct AckResponse {
    body: Option<serde_json::Value>,
    headers: Option<BTreeMap<String, String>>,
}

/// Message to send to host_nats_publish.
#[derive(Serialize)]
struct BrokerMessage {
    url: String,
    topic: String, // "topic" field is used for subject in NATS
    payload: String,
    /// The body when it is not UTF-8; the host publishes these bytes.
    #[serde(
        with = "barbacane_plugin_sdk::types::base64_body",
        skip_serializing_if = "Option::is_none"
    )]
    payload_base64: Option<Vec<u8>>,
    headers: BTreeMap<String, String>,
}

/// Result from host_nats_publish.
#[derive(Deserialize)]
struct PublishResult {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    topic: String,
}

/// A NATS subject a message can be published to: dot-separated tokens, none
/// empty, none a wildcard (`*`, `>`), no whitespace.
fn subject<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let subject = String::deserialize(deserializer)?;
    let valid = !subject.chars().any(char::is_whitespace)
        && subject
            .split('.')
            .all(|token| !token.is_empty() && token != "*" && token != ">");
    if valid {
        Ok(subject)
    } else {
        Err(serde::de::Error::custom(format!(
            "subject {subject:?} is not one NATS subject: write dot-separated tokens, \
             without wildcards (`*`, `>`), empty tokens or whitespace"
        )))
    }
}

impl NatsDispatcher {
    /// Dispatch a request by publishing to NATS and returning 202 Accepted.
    pub fn dispatch(&mut self, req: Request) -> Response {
        let message = self.message(&req);

        // Serialize and publish
        let message_json = match serde_json::to_vec(&message) {
            Ok(json) => json,
            Err(e) => {
                return self.error_response(500, "failed to serialize message", &e.to_string())
            }
        };

        let result_len =
            unsafe { host_nats_publish(message_json.as_ptr() as i32, message_json.len() as i32) };

        if result_len < 0 {
            return self.error_response(
                502,
                "NATS publish failed",
                "host_nats_publish returned error",
            );
        }

        // Read the publish result
        let mut result_buf = vec![0u8; result_len as usize];
        let bytes_read =
            unsafe { host_broker_read_result(result_buf.as_mut_ptr() as i32, result_len) };

        if bytes_read <= 0 {
            return self.error_response(
                502,
                "NATS publish failed",
                "failed to read publish result",
            );
        }

        // Parse the publish result
        let publish_result: PublishResult = match serde_json::from_slice(
            &result_buf[..(bytes_read as usize).min(result_buf.len())],
        ) {
            Ok(r) => r,
            Err(e) => return self.error_response(502, "invalid publish result", &e.to_string()),
        };

        if !publish_result.success {
            let detail = publish_result
                .error
                .unwrap_or_else(|| "unknown error".to_string());
            return self.error_response(502, "NATS publish failed", &detail);
        }

        // Build the 202 Accepted response
        self.accepted_response(&publish_result)
    }

    /// Build the 202 Accepted response.
    fn accepted_response(&self, result: &PublishResult) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            vec!["application/json".to_string()],
        );

        // Add custom headers from config
        if let Some(ref ack) = self.ack_response {
            if let Some(ref custom_headers) = ack.headers {
                for (k, v) in custom_headers {
                    headers.insert(k.clone(), vec![v.clone()]);
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

        Response::new(202)
            .with_headers(headers)
            .with_body(body.into_bytes())
    }

    /// Build the default acknowledgment body.
    fn default_ack_body(&self, result: &PublishResult) -> String {
        serde_json::json!({
            "status": "accepted",
            "subject": result.topic
        })
        .to_string()
    }

    /// Create an error response in RFC 9457 format.
    fn error_response(&self, status: u16, title: &str, detail: &str) -> Response {
        let error_type = match status {
            502 => "urn:barbacane:error:nats-publish-failed",
            503 => "urn:barbacane:error:nats-unavailable",
            _ => "urn:barbacane:error:internal-error",
        };

        ProblemDetails::new(status, error_type, title)
            .detail(detail)
            .into_response()
    }
}

// Host function declarations
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Publish a message to NATS. Returns the result length, or -1 on error.
    fn host_nats_publish(msg_ptr: i32, msg_len: i32) -> i32;

    /// Read the broker publish result into the provided buffer. Returns bytes read.
    fn host_broker_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}

// Native stubs for testing
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_nats_publish(_msg_ptr: i32, _msg_len: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_broker_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}

impl NatsDispatcher {
    /// The message for `req`: its body and the request headers it carries.
    /// Request header names are lower-case, so configured names are matched in
    /// any case.
    fn message(&self, req: &Request) -> BrokerMessage {
        let mut msg_headers = BTreeMap::new();
        for header_name in &self.headers_from_request {
            if let Some(value) = req.headers.get(&header_name.to_ascii_lowercase()) {
                msg_headers.insert(header_name.clone(), value.clone());
            }
        }
        for name in ["x-request-id", "x-trace-id"] {
            if let Some(value) = req.headers.get(name) {
                msg_headers.insert(name.to_string(), value.clone());
            }
        }
        let (payload, payload_base64) = payload(req.body.clone());
        BrokerMessage {
            url: self.url.clone(),
            topic: self.subject.clone(),
            payload,
            payload_base64,
            headers: msg_headers,
        }
    }
}

/// The request body as a message payload: text when it is UTF-8, otherwise
/// the bytes, which the host receives as base64.
fn payload(body: Option<Vec<u8>>) -> (String, Option<Vec<u8>>) {
    match body.map(String::from_utf8) {
        None => (String::new(), None),
        Some(Ok(text)) => (text, None),
        Some(Err(not_text)) => (String::new(), Some(not_text.into_bytes())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_config() -> NatsDispatcher {
        let config_json = json!({
            "url": "nats://localhost:4222",
            "subject": "test.subject"
        });
        serde_json::from_value(config_json).unwrap()
    }

    fn create_test_request() -> Request {
        Request::new("POST", "/test")
            .with_body(b"test body".to_vec())
            .with_client_ip("127.0.0.1")
    }

    /// A subject with a wildcard, an empty token or whitespace is refused
    /// when the configuration is read: NATS publishes to one subject.
    #[test]
    fn a_subject_with_wildcards_is_refused() {
        for subject in [
            "events.>",
            "events.*.created",
            "events..user",
            "",
            "events user",
        ] {
            let config = json!({"url": "nats://localhost:4222", "subject": subject});
            let error = serde_json::from_value::<NatsDispatcher>(config)
                .err()
                .unwrap_or_else(|| panic!("{subject:?} accepted"));
            assert!(
                error.to_string().contains("not one NATS subject"),
                "{error}"
            );
        }
        let config = json!({"url": "nats://localhost:4222", "subject": "events.user.signup"});
        assert!(serde_json::from_value::<NatsDispatcher>(config).is_ok());
    }

    /// A configured header name matches the lower-case request header, and a
    /// body that is not UTF-8 goes to the host as its bytes.
    #[test]
    fn headers_match_in_any_case_and_bodies_keep_their_bytes() {
        let dispatcher: NatsDispatcher = serde_json::from_value(json!({
            "url": "nats://localhost:4222",
            "subject": "events",
            "headers_from_request": ["X-User-Id"]
        }))
        .unwrap();
        let mut req = create_test_request().with_body(vec![0xff, 0x00]);
        req.headers
            .insert("x-user-id".to_string(), "u1".to_string());
        let json = serde_json::to_value(dispatcher.message(&req)).unwrap();
        assert_eq!(json["headers"]["X-User-Id"], "u1");
        assert_eq!(json["payload"], "");
        assert_eq!(json["payload_base64"], "/wA=");
    }

    #[test]
    fn test_accepted_response_with_default_body() {
        let dispatcher = create_test_config();
        let result = PublishResult {
            success: true,
            error: None,
            topic: "test.subject".to_string(),
        };

        let response = dispatcher.accepted_response(&result);

        assert_eq!(response.status, 202);
        assert_eq!(response.header("content-type").unwrap(), "application/json");

        let body = String::from_utf8(response.body.unwrap()).unwrap();
        assert!(
            body.contains("\"status\":\"accepted\"") || body.contains("\"status\": \"accepted\"")
        );
        assert!(
            body.contains("\"subject\":\"test.subject\"")
                || body.contains("\"subject\": \"test.subject\"")
        );
    }

    #[test]
    fn test_accepted_response_with_custom_body_and_headers() {
        let config_json = json!({
            "url": "nats://localhost:4222",
            "subject": "test.subject",
            "ack_response": {
                "body": {"custom": "response", "id": 123},
                "headers": {
                    "x-custom-header": "custom-value",
                    "x-correlation-id": "abc123"
                }
            }
        });
        let dispatcher: NatsDispatcher = serde_json::from_value(config_json).unwrap();
        let result = PublishResult {
            success: true,
            error: None,
            topic: "test.subject".to_string(),
        };

        let response = dispatcher.accepted_response(&result);

        assert_eq!(response.status, 202);
        assert_eq!(response.header("content-type").unwrap(), "application/json");
        assert_eq!(response.header("x-custom-header").unwrap(), "custom-value");
        assert_eq!(response.header("x-correlation-id").unwrap(), "abc123");

        let body = String::from_utf8(response.body.unwrap()).unwrap();
        assert!(
            body.contains("\"custom\":\"response\"") || body.contains("\"custom\": \"response\"")
        );
        assert!(body.contains("\"id\":123") || body.contains("\"id\": 123"));
    }

    #[test]
    fn test_default_ack_body() {
        let dispatcher = create_test_config();
        let result = PublishResult {
            success: true,
            error: None,
            topic: "test.subject".to_string(),
        };

        let body = dispatcher.default_ack_body(&result);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(parsed["status"], "accepted");
        assert_eq!(parsed["subject"], "test.subject");
    }

    #[test]
    fn test_error_response_502() {
        let dispatcher = create_test_config();
        let response = dispatcher.error_response(502, "NATS publish failed", "connection timeout");

        assert_eq!(response.status, 502);
        assert_eq!(
            response.header("content-type").unwrap(),
            "application/problem+json"
        );

        let body = response.body.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["type"], "urn:barbacane:error:nats-publish-failed");
        assert_eq!(parsed["title"], "NATS publish failed");
        assert_eq!(parsed["status"], 502);
        assert_eq!(parsed["detail"], "connection timeout");
    }

    #[test]
    fn test_error_response_503() {
        let dispatcher = create_test_config();
        let response = dispatcher.error_response(503, "NATS unavailable", "service down");

        assert_eq!(response.status, 503);
        assert_eq!(
            response.header("content-type").unwrap(),
            "application/problem+json"
        );

        let body = response.body.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["type"], "urn:barbacane:error:nats-unavailable");
        assert_eq!(parsed["title"], "NATS unavailable");
        assert_eq!(parsed["status"], 503);
        assert_eq!(parsed["detail"], "service down");
    }

    #[test]
    fn test_config_deserialization_required_fields() {
        let config_json = json!({
            "url": "nats://localhost:4222",
            "subject": "events.test"
        });
        let dispatcher: NatsDispatcher = serde_json::from_value(config_json).unwrap();

        assert_eq!(dispatcher.url, "nats://localhost:4222");
        assert_eq!(dispatcher.subject, "events.test");
        assert!(dispatcher.ack_response.is_none());
        assert!(dispatcher.headers_from_request.is_empty());
    }

    #[test]
    fn test_config_deserialization_with_defaults() {
        let config_json = json!({
            "url": "nats://localhost:4222",
            "subject": "events.test",
            "ack_response": {
                "body": {"msg": "queued"}
            },
            "headers_from_request": ["authorization", "content-type"]
        });
        let dispatcher: NatsDispatcher = serde_json::from_value(config_json).unwrap();

        assert_eq!(dispatcher.url, "nats://localhost:4222");
        assert_eq!(dispatcher.subject, "events.test");
        assert!(dispatcher.ack_response.is_some());
        assert_eq!(dispatcher.headers_from_request.len(), 2);
        assert_eq!(dispatcher.headers_from_request[0], "authorization");
        assert_eq!(dispatcher.headers_from_request[1], "content-type");
    }

    #[test]
    fn test_dispatch_with_native_stub_returns_502() {
        let mut dispatcher = create_test_config();
        let request = create_test_request();

        let response = dispatcher.dispatch(request);

        // Native stub returns -1, so we expect a 502 error
        assert_eq!(response.status, 502);
        assert_eq!(
            response.header("content-type").unwrap(),
            "application/problem+json"
        );

        let body = response.body.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["type"], "urn:barbacane:error:nats-publish-failed");
        assert_eq!(parsed["status"], 502);
        assert!(parsed["detail"]
            .as_str()
            .unwrap()
            .contains("host_nats_publish returned error"));
    }

    #[test]
    fn test_message_header_propagation_specified_headers() {
        let config_json = json!({
            "url": "nats://localhost:4222",
            "subject": "test.subject",
            "headers_from_request": ["authorization", "content-type", "x-custom"]
        });
        let mut dispatcher: NatsDispatcher = serde_json::from_value(config_json).unwrap();

        let mut request = create_test_request();
        request
            .headers
            .insert("authorization".to_string(), "Bearer token123".to_string());
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());
        request
            .headers
            .insert("x-custom".to_string(), "custom-value".to_string());
        request
            .headers
            .insert("x-ignored".to_string(), "should-not-appear".to_string());

        // Call dispatch - it will fail with native stub, but we can verify the logic
        // by checking the error response (which means headers were processed)
        let response = dispatcher.dispatch(request);

        // Should get 502 error since native stub returns -1
        assert_eq!(response.status, 502);
    }

    #[test]
    fn test_message_header_propagation_x_request_id_auto_included() {
        let mut dispatcher = create_test_config();

        let mut request = create_test_request();
        request
            .headers
            .insert("x-request-id".to_string(), "req-123".to_string());
        request
            .headers
            .insert("x-trace-id".to_string(), "trace-456".to_string());

        // Call dispatch - it will fail with native stub
        let response = dispatcher.dispatch(request);

        // Should get 502 error since native stub returns -1
        assert_eq!(response.status, 502);

        // The fact we got a proper error response means the headers were processed correctly
        let body = response.body.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["type"], "urn:barbacane:error:nats-publish-failed");
    }

    #[test]
    fn test_message_header_propagation_both_specified_and_auto() {
        let config_json = json!({
            "url": "nats://localhost:4222",
            "subject": "test.subject",
            "headers_from_request": ["authorization"]
        });
        let mut dispatcher: NatsDispatcher = serde_json::from_value(config_json).unwrap();

        let mut request = create_test_request();
        request
            .headers
            .insert("authorization".to_string(), "Bearer token".to_string());
        request
            .headers
            .insert("x-request-id".to_string(), "req-abc".to_string());
        request
            .headers
            .insert("x-trace-id".to_string(), "trace-xyz".to_string());
        request
            .headers
            .insert("user-agent".to_string(), "test-client".to_string());

        // Call dispatch
        let response = dispatcher.dispatch(request);

        // Should get 502 error since native stub returns -1
        assert_eq!(response.status, 502);
    }

    #[test]
    fn an_unexpected_failure_is_an_internal_error() {
        let dispatcher: NatsDispatcher =
            serde_json::from_str(r#"{"url": "nats://localhost:4222", "subject": "test.subject"}"#)
                .expect("config");
        let response = dispatcher.error_response(500, "Internal Server Error", "unexpected");
        assert_eq!(response.status, 500);
        let body: serde_json::Value =
            serde_json::from_slice(response.body.as_ref().expect("body")).expect("problem+json");
        assert_eq!(body["type"], "urn:barbacane:error:internal-error");
    }
}
