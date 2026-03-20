//! Integration tests for utility middleware plugins — CORS, correlation-id, request-size-limit, IP restriction, NATS, Kafka, HTTP log, request-transformer, response-transformer, ws-upstream.
//!
//! Run with: `cargo test -p barbacane-test`

use barbacane_test::TestGateway;

fn fixture(name: &str) -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

// =========================================================================
// Correlation ID Middleware Tests
// =========================================================================

/// Create a temporary spec file for correlation-id testing.
fn create_correlation_id_spec() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let spec_path = temp_dir.path().join("correlation-id.yaml");

    // Get absolute paths to plugins
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugins_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    let mock_path = plugins_dir.join("mock/mock.wasm");
    let correlation_id_path = plugins_dir.join("correlation-id/correlation-id.wasm");

    // Create barbacane.yaml manifest in temp dir
    let manifest_path = temp_dir.path().join("barbacane.yaml");
    let manifest_content = format!(
        r#"# Test manifest for correlation-id tests

plugins:
  mock:
    path: {}
  correlation-id:
    path: {}
"#,
        mock_path.display(),
        correlation_id_path.display()
    );
    std::fs::write(&manifest_path, manifest_content).expect("failed to write manifest file");

    let spec_content = r#"openapi: "3.0.3"
info:
  title: Correlation ID Test API
  version: "1.0.0"
  description: API for testing correlation ID middleware

x-barbacane-middlewares:
  - name: correlation-id
    config:
      header_name: "x-correlation-id"
      generate_if_missing: true
      trust_incoming: true
      include_in_response: true

paths:
  /test:
    get:
      summary: Test endpoint
      operationId: getTest
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"message": "OK"}'
          content_type: application/json
      responses:
        "200":
          description: Success
"#;

    std::fs::write(&spec_path, spec_content).expect("failed to write spec file");
    (temp_dir, spec_path)
}

#[tokio::test]
async fn test_correlation_id_generates_when_missing() {
    let (_temp_dir, spec_path) = create_correlation_id_spec();
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Make request without correlation ID header
    let resp = gateway.get("/test").await.unwrap();

    let status = resp.status();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        panic!("Expected 200, got {} with body: {}", status, body);
    }

    // Should have generated a correlation ID in response
    let all_headers: Vec<_> = resp
        .headers()
        .iter()
        .map(|(k, v)| format!("{}: {:?}", k, v))
        .collect();
    let correlation_id = resp.headers().get("x-correlation-id");
    assert!(
        correlation_id.is_some(),
        "Expected x-correlation-id header in response. All headers: {:?}",
        all_headers
    );

    // Verify it's a valid UUID v7 format (36 chars, version 7)
    let id = correlation_id.unwrap().to_str().unwrap();
    assert_eq!(id.len(), 36, "UUID should be 36 characters");
    assert_eq!(
        id.chars().nth(14),
        Some('7'),
        "Should be UUID v7 (version marker at position 14)"
    );
}

#[tokio::test]
async fn test_correlation_id_preserves_incoming() {
    let (_temp_dir, spec_path) = create_correlation_id_spec();
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Make request with existing correlation ID
    let incoming_id = "my-custom-correlation-id-12345";
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/test")
        .header("x-correlation-id", incoming_id)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // Should preserve the incoming correlation ID
    let correlation_id = resp.headers().get("x-correlation-id");
    assert!(
        correlation_id.is_some(),
        "Expected x-correlation-id header in response"
    );
    assert_eq!(
        correlation_id.unwrap().to_str().unwrap(),
        incoming_id,
        "Should preserve incoming correlation ID"
    );
}

// =========================================================================
// CORS Middleware Tests (Automatic Preflight Handling)
// =========================================================================

#[tokio::test]
async fn test_cors_preflight_any_origin() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Send preflight OPTIONS request (no OPTIONS defined in spec - auto-handled)
    let resp = gateway
        .request_builder(reqwest::Method::OPTIONS, "/cors-any")
        .header("Origin", "https://any-site.com")
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "Content-Type")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 204, "Preflight should return 204 No Content");

    // Check CORS headers
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("*"),
        "Should allow any origin"
    );
    assert!(
        resp.headers().get("access-control-allow-methods").is_some(),
        "Should have allow-methods header"
    );
    assert!(
        resp.headers().get("access-control-max-age").is_some(),
        "Should have max-age header"
    );
}

#[tokio::test]
async fn test_cors_preflight_specific_origin_allowed() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Send preflight from allowed origin
    let resp = gateway
        .request_builder(reqwest::Method::OPTIONS, "/cors-specific")
        .header("Origin", "https://example.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 204, "Preflight should return 204");
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("https://example.com"),
        "Should echo back allowed origin"
    );
}

#[tokio::test]
async fn test_cors_preflight_specific_origin_denied() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Send preflight from disallowed origin
    let resp = gateway
        .request_builder(reqwest::Method::OPTIONS, "/cors-specific")
        .header("Origin", "https://evil-site.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 403, "Should reject disallowed origin");
}

#[tokio::test]
async fn test_cors_preflight_method_not_allowed() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Request DELETE method which is not allowed on cors-specific
    let resp = gateway
        .request_builder(reqwest::Method::OPTIONS, "/cors-specific")
        .header("Origin", "https://example.com")
        .header("Access-Control-Request-Method", "DELETE")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 403, "Should reject disallowed method");
}

#[tokio::test]
async fn test_cors_simple_request_with_origin() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Simple GET request with Origin header
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/cors-any")
        .header("Origin", "https://example.com")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("*"),
        "Response should include CORS header"
    );
}

#[tokio::test]
async fn test_cors_request_without_origin() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Request without Origin header (same-origin or non-browser)
    let resp = gateway.get("/cors-any").await.unwrap();

    assert_eq!(resp.status(), 200, "Non-CORS request should pass through");
}

#[tokio::test]
async fn test_cors_credentials_header() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Preflight for credentials endpoint
    let resp = gateway
        .request_builder(reqwest::Method::OPTIONS, "/cors-credentials")
        .header("Origin", "https://trusted.example.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 204);
    assert_eq!(
        resp.headers()
            .get("access-control-allow-credentials")
            .map(|v| v.to_str().unwrap()),
        Some("true"),
        "Should include credentials header"
    );
    // With credentials, origin should be echoed, not *
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("https://trusted.example.com"),
        "With credentials, should echo origin not *"
    );
}

#[tokio::test]
async fn test_cors_preflight_no_cors_middleware_returns_405() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Preflight for endpoint without CORS middleware should return 405
    let resp = gateway
        .request_builder(reqwest::Method::OPTIONS, "/no-cors")
        .header("Origin", "https://example.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        405,
        "Should return 405 when no CORS middleware configured"
    );
}

#[tokio::test]
async fn test_cors_global_middleware_applies_to_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // /global-cors endpoint inherits global CORS middleware config
    // Preflight should work without operation-level middleware
    let resp = gateway
        .request_builder(reqwest::Method::OPTIONS, "/global-cors")
        .header("Origin", "https://example.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        204,
        "Preflight should succeed with global CORS middleware"
    );
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("*"),
        "Should have wildcard origin from global config"
    );
    assert!(
        resp.headers()
            .get("access-control-allow-methods")
            .map(|v| v.to_str().unwrap())
            .unwrap_or("")
            .contains("GET"),
        "Should include GET in allowed methods from global config"
    );
}

#[tokio::test]
async fn test_cors_global_middleware_simple_request() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    // Simple GET request with Origin header on endpoint using global config
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/global-cors")
        .header("Origin", "https://example.com")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("*"),
        "Response should include CORS header from global middleware"
    );
}

// ==================== Request Size Limit Tests ====================

#[tokio::test]
async fn test_request_size_limit_allows_small_body() {
    let gateway = TestGateway::from_spec(&fixture("request-size-limit.yaml"))
        .await
        .expect("failed to start gateway");

    // Small body should be allowed (under 100 byte limit)
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/limited")
        .header("Content-Type", "application/json")
        .body(r#"{"msg":"hi"}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "ok");
}

#[tokio::test]
async fn test_request_size_limit_blocks_large_body() {
    let gateway = TestGateway::from_spec(&fixture("request-size-limit.yaml"))
        .await
        .expect("failed to start gateway");

    // Large body should be rejected (over 100 byte limit)
    let large_body = r#"{"data":"this is a very long message that exceeds the configured limit of 100 bytes for this endpoint"}"#;
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/limited")
        .header("Content-Type", "application/json")
        .body(large_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 413);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:payload-too-large");
    assert_eq!(body["status"], 413);
}

#[tokio::test]
async fn test_request_size_limit_unlimited_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("request-size-limit.yaml"))
        .await
        .expect("failed to start gateway");

    // Unlimited endpoint should accept large bodies
    let large_body = r#"{"data":"this is a very long message that would be rejected on the limited endpoint but should pass here"}"#;
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/unlimited")
        .header("Content-Type", "application/json")
        .body(large_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "unlimited");
}

// ==================== IP Restriction Tests ====================

#[tokio::test]
async fn test_ip_restriction_allowlist_localhost() {
    let gateway = TestGateway::from_spec(&fixture("ip-restriction.yaml"))
        .await
        .expect("failed to start gateway");

    // Localhost should be allowed
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/allowlist")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "allowed");
}

#[tokio::test]
async fn test_ip_restriction_allowlist_denied_via_xff() {
    let gateway = TestGateway::from_spec(&fixture("ip-restriction.yaml"))
        .await
        .expect("failed to start gateway");

    // Request with X-Forwarded-For from non-allowed IP should be denied
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/allowlist")
        .header("X-Forwarded-For", "203.0.113.50")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:ip-restricted");
}

#[tokio::test]
async fn test_ip_restriction_denylist_allowed() {
    let gateway = TestGateway::from_spec(&fixture("ip-restriction.yaml"))
        .await
        .expect("failed to start gateway");

    // Request from localhost (not in denylist) should be allowed
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/denylist")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_ip_restriction_denylist_blocked() {
    let gateway = TestGateway::from_spec(&fixture("ip-restriction.yaml"))
        .await
        .expect("failed to start gateway");

    // Request from denied CIDR range should be blocked
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/denylist")
        .header("X-Forwarded-For", "10.1.2.3")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_ip_restriction_cidr_allowlist() {
    let gateway = TestGateway::from_spec(&fixture("ip-restriction.yaml"))
        .await
        .expect("failed to start gateway");

    // 127.0.0.1 is in 127.0.0.0/8 CIDR range
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/cidr-allowlist")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_ip_restriction_custom_message() {
    let gateway = TestGateway::from_spec(&fixture("ip-restriction.yaml"))
        .await
        .expect("failed to start gateway");

    // Request from non-allowed IP should get custom status and message
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/custom-message")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap().contains("not authorized"));
}

#[tokio::test]
async fn test_ip_restriction_public_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("ip-restriction.yaml"))
        .await
        .expect("failed to start gateway");

    // Public endpoint without IP restriction
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/public")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "public");
}

// ========================
// NATS Dispatcher Tests
// ========================

#[tokio::test]
async fn test_nats_dispatcher_spec_compiles() {
    // Test that an AsyncAPI spec with a real NATS dispatcher compiles and boots
    let gateway = TestGateway::from_spec(&fixture("nats-dispatch.yaml"))
        .await
        .expect("failed to start gateway with NATS dispatch spec");

    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_nats_dispatcher_broker_unavailable() {
    // When NATS is not running, the dispatcher should return 502
    let gateway = TestGateway::from_spec(&fixture("nats-dispatch.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post(
            "/events/users",
            r#"{"userId":"550e8400-e29b-41d4-a716-446655440000","email":"test@example.com"}"#,
        )
        .await
        .unwrap();

    // Should fail with 502 (NATS connection refused)
    assert_eq!(resp.status(), 502);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:nats-publish-failed");
}

#[tokio::test]
async fn test_nats_dispatcher_validates_payload() {
    // Message payload validation should still work with a real dispatcher
    let gateway = TestGateway::from_spec(&fixture("nats-dispatch.yaml"))
        .await
        .expect("failed to start gateway");

    // Missing required 'email' field
    let resp = gateway
        .post(
            "/events/users",
            r#"{"userId":"550e8400-e29b-41d4-a716-446655440000"}"#,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

// ========================
// Kafka Dispatcher Tests
// ========================

#[tokio::test]
async fn test_kafka_dispatcher_spec_compiles() {
    // Test that an AsyncAPI spec with a real Kafka dispatcher compiles and boots
    let gateway = TestGateway::from_spec(&fixture("kafka-dispatch.yaml"))
        .await
        .expect("failed to start gateway with Kafka dispatch spec");

    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_kafka_dispatcher_broker_unavailable() {
    // When Kafka is not running, the dispatcher should return 502
    let gateway = TestGateway::from_spec(&fixture("kafka-dispatch.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post(
            "/events/orders",
            r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000","total":99.99}"#,
        )
        .await
        .unwrap();

    // Should fail with 502 (Kafka connection refused)
    assert_eq!(resp.status(), 502);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:kafka-publish-failed");
}

#[tokio::test]
async fn test_kafka_dispatcher_validates_payload() {
    // Message payload validation should still work with a real dispatcher
    let gateway = TestGateway::from_spec(&fixture("kafka-dispatch.yaml"))
        .await
        .expect("failed to start gateway");

    // Missing required 'total' field
    let resp = gateway
        .post(
            "/events/orders",
            r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000"}"#,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

// ==================== HTTP Log Tests ====================

/// Create a temporary spec file for http-log testing with dynamic log endpoint URL.
fn create_http_log_spec(log_endpoint: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let spec_path = temp_dir.path().join("http-log.yaml");

    // Get absolute paths to plugins
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugins_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    let mock_path = plugins_dir.join("mock/mock.wasm");
    let http_log_path = plugins_dir.join("http-log/http-log.wasm");

    // Create barbacane.yaml manifest in temp dir
    let manifest_path = temp_dir.path().join("barbacane.yaml");
    let manifest_content = format!(
        r#"# Test manifest for HTTP log tests

plugins:
  mock:
    path: {}
  http-log:
    path: {}
"#,
        mock_path.display(),
        http_log_path.display()
    );
    std::fs::write(&manifest_path, manifest_content).expect("failed to write manifest file");

    let spec_content = format!(
        r#"openapi: "3.0.3"
info:
  title: HTTP Log Test API
  version: "1.0.0"
  description: API for testing HTTP logging middleware

paths:
  /logged:
    get:
      summary: Endpoint with HTTP logging
      operationId: getLogged
      x-barbacane-middlewares:
        - name: http-log
          config:
            endpoint: "{}"
            timeout_ms: 2000
            include_headers: true
            include_body: true
            custom_fields:
              env: "test"
              service: "test-api"
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "OK"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
"#,
        log_endpoint
    );

    std::fs::write(&spec_path, spec_content).expect("failed to write spec file");
    (temp_dir, spec_path)
}

#[tokio::test]
async fn test_http_log_sends_entry() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Accept log entries
    Mock::given(method("POST"))
        .and(path("/logs"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let log_endpoint = format!("{}/logs", mock_server.uri());
    let (_temp_dir, spec_path) = create_http_log_spec(&log_endpoint);

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/logged").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "OK");

    // Verify the mock received exactly 1 log entry
    // (wiremock expect(1) will panic on drop if not satisfied)
}

#[tokio::test]
async fn test_http_log_includes_duration() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/logs"))
        .and(body_string_contains("duration_ms"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let log_endpoint = format!("{}/logs", mock_server.uri());
    let (_temp_dir, spec_path) = create_http_log_spec(&log_endpoint);

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/logged").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_http_log_includes_headers() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Verify the log entry contains request headers
    Mock::given(method("POST"))
        .and(path("/logs"))
        .and(body_string_contains("headers"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let log_endpoint = format!("{}/logs", mock_server.uri());
    let (_temp_dir, spec_path) = create_http_log_spec(&log_endpoint);

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/logged").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_http_log_failure_does_not_break_response() {
    // Use an unreachable endpoint — the response should still be 200
    let (_temp_dir, spec_path) =
        create_http_log_spec("http://127.0.0.1:1/unreachable-log-endpoint");

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/logged").await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "Response should not be affected by log delivery failure"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "OK");
}

// =========================================================================
// Request Transformer middleware integration tests
// =========================================================================

#[tokio::test]
async fn test_request_transformer_header_transformations() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/headers")
        .header("content-type", "application/json")
        .header("X-Old-Name", "rename-me")
        .header("Authorization", "Bearer secret")
        .body("{}")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["received"], "headers");
}

#[tokio::test]
async fn test_request_transformer_query_transformations() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .get("/query?oldParam=value&internal_token=secret")
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["received"], "query");
}

#[tokio::test]
async fn test_request_transformer_path_strip_prefix() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/path/strip").await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["path"], "stripped");
}

#[tokio::test]
async fn test_request_transformer_path_add_prefix() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/api/add").await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["path"], "prefixed");
}

#[tokio::test]
async fn test_request_transformer_path_regex_replace() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/replace/test").await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["path"], "replaced");
}

#[tokio::test]
async fn test_request_transformer_body_transformations() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post(
            "/body",
            r#"{"userName":"alice","userId":"42","password":"secret","internal_flags":true}"#,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["received"], "body");
}

#[tokio::test]
async fn test_request_transformer_variable_interpolation() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/users/123/interpolation?page=5")
        .header("content-type", "application/json")
        .body(r#"{"existing":"data"}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["interpolated"], "values");
}

#[tokio::test]
async fn test_request_transformer_query_to_body() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    // ADR-0020 showcase: move userId from query to body
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/query-to-body?userId=42")
        .header("content-type", "application/json")
        .body(r#"{}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["transformed"], "query-to-body");
}

#[tokio::test]
async fn test_request_transformer_combined_transformations() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/combined?internal=secret")
        .header("content-type", "application/json")
        .header("Authorization", "Bearer token")
        .body(r#"{"password":"secret","data":"keep"}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["all"], "transformations");
}

#[tokio::test]
async fn test_request_transformer_passthrough_no_plugin() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/passthrough").await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["passthrough"], "no-transformations");
}

// =========================================================================
// Response Transformer middleware integration tests
// =========================================================================

#[tokio::test]
async fn test_response_transformer_status_mapping() {
    let gateway = TestGateway::from_spec(&fixture("response-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/status").await.unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"], "status-mapped");
}

#[tokio::test]
async fn test_response_transformer_status_mapping_multi() {
    let gateway = TestGateway::from_spec(&fixture("response-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/status-multi").await.unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"], "status-multi");
}

#[tokio::test]
async fn test_response_transformer_header_transformations() {
    let gateway = TestGateway::from_spec(&fixture("response-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/headers").await.unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers().get("x-gateway").unwrap(), "barbacane");
    assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
    // "Server: upstream/1.0" is stripped by the middleware; the gateway's own
    // Server header is still present (add_standard_headers always sets it).
    assert_ne!(
        resp.headers().get("server").and_then(|v| v.to_str().ok()),
        Some("upstream/1.0")
    );
    assert!(resp.headers().get("x-powered-by").is_none());

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"], "headers");
}

#[tokio::test]
async fn test_response_transformer_body_transformations() {
    let gateway = TestGateway::from_spec(&fixture("response-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/body").await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["gateway"], "barbacane");
    assert_eq!(body["metadata"]["version"], "1.0");
    assert_eq!(body["user_name"], "alice");
    assert_eq!(body["data"], "keep");
    assert!(body.get("internal_flags").is_none());
    assert!(body.get("debug_info").is_none());
    assert!(body.get("userName").is_none());
}

#[tokio::test]
async fn test_response_transformer_combined() {
    let gateway = TestGateway::from_spec(&fixture("response-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/combined").await.unwrap();

    assert_eq!(resp.status(), 201);
    assert_eq!(resp.headers().get("x-gateway").unwrap(), "barbacane");
    // "Server: nginx" is stripped by the middleware; the gateway's own
    // Server header is still present (add_standard_headers always sets it).
    assert_ne!(
        resp.headers().get("server").and_then(|v| v.to_str().ok()),
        Some("nginx")
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["data"], "value");
    assert_eq!(body["gateway"], "barbacane");
    assert!(body.get("internal").is_none());
}

#[tokio::test]
async fn test_response_transformer_passthrough() {
    let gateway = TestGateway::from_spec(&fixture("response-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/passthrough").await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["passthrough"], "no-transformations");
}

// =========================================================================
// Redirect middleware integration tests
// =========================================================================

#[tokio::test]
async fn test_redirect_exact_path_301() {
    let gateway = TestGateway::from_spec(&fixture("redirect.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/old-page")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 301);
    assert_eq!(
        resp.headers().get("location").map(|v| v.to_str().unwrap()),
        Some("/new-page")
    );
}

#[tokio::test]
async fn test_redirect_prefix_strips_and_appends() {
    let gateway = TestGateway::from_spec(&fixture("redirect.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/api/v1/users")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 302);
    assert_eq!(
        resp.headers().get("location").map(|v| v.to_str().unwrap()),
        Some("/api/v2/users")
    );
}

#[tokio::test]
async fn test_redirect_catch_all_308() {
    let gateway = TestGateway::from_spec(&fixture("redirect.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/catch-all")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 308);
    assert_eq!(
        resp.headers().get("location").map(|v| v.to_str().unwrap()),
        Some("https://example.com")
    );
}

#[tokio::test]
async fn test_redirect_preserves_query_string() {
    let gateway = TestGateway::from_spec(&fixture("redirect.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/with-query?foo=bar&page=2")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 302);
    assert_eq!(
        resp.headers().get("location").map(|v| v.to_str().unwrap()),
        Some("/new-location?foo=bar&page=2")
    );
}

#[tokio::test]
async fn test_redirect_strips_query_when_disabled() {
    let gateway = TestGateway::from_spec(&fixture("redirect.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/no-query?foo=bar")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 302);
    assert_eq!(
        resp.headers().get("location").map(|v| v.to_str().unwrap()),
        Some("/new-location")
    );
}

#[tokio::test]
async fn test_redirect_no_redirect_endpoint_passes_through() {
    let gateway = TestGateway::from_spec(&fixture("redirect.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/no-redirect").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "no redirect");
}

// =========================================================================
// WebSocket Upstream dispatcher integration tests
// =========================================================================

#[tokio::test]
async fn test_ws_upstream_spec_compiles() {
    // Test that an OpenAPI spec with a ws-upstream dispatcher compiles and boots
    let gateway = TestGateway::from_spec(&fixture("ws-upstream.yaml"))
        .await
        .expect("failed to start gateway with ws-upstream spec");

    // Health endpoint should work (proves the spec compiled and gateway started)
    let resp = gateway.get("/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_ws_upstream_rejects_non_websocket_request() {
    // A plain HTTP GET to a WebSocket endpoint should return an error
    // The WASM plugin returns 400 but the data plane may wrap it as 500
    // depending on the upgrade path handling
    let gateway = TestGateway::from_spec(&fixture("ws-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/ws/echo").await.unwrap();
    // Without a real WebSocket upgrade, the request fails at the dispatch level
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "expected error status, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn test_ws_upstream_upstream_unavailable() {
    // When the upstream WS server is not running, the dispatcher should return an error
    let gateway = TestGateway::from_spec(&fixture("ws-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    // Send a request with Upgrade: websocket header but no real WS handshake
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/ws/echo")
        .header("upgrade", "websocket")
        .header("connection", "upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .send()
        .await
        .unwrap();

    // Without a real WebSocket client, reqwest sends a normal HTTP request.
    // The gateway dispatch path may return 400, 502, or 500 depending on
    // how far the upgrade handshake progresses before failing.
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "expected error status, got {}",
        resp.status()
    );
}

/// Start a simple WebSocket echo server on a random port.
/// Returns the `(join_handle, "ws://127.0.0.1:PORT")`.
fn start_ws_echo_server() -> (tokio::task::JoinHandle<()>, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("ws://127.0.0.1:{}", port);

    // Convert to a tokio TcpListener inside the spawned task
    listener.set_nonblocking(true).unwrap();

    let handle = tokio::spawn(async move {
        use futures_util::{SinkExt, StreamExt};

        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut tx, mut rx) = ws.split();
                while let Some(Ok(msg)) = rx.next().await {
                    if msg.is_close() {
                        break;
                    }
                    if msg.is_text() || msg.is_binary() {
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                }
            });
        }
    });

    (handle, url)
}

/// Create a temporary spec + manifest for ws-upstream relay tests.
fn create_ws_spec(upstream_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let spec_path = temp_dir.path().join("ws-relay.yaml");

    let ws_wasm = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins/ws-upstream/ws-upstream.wasm");
    let mock_wasm = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins/mock/mock.wasm");

    let manifest = format!(
        "plugins:\n  ws-upstream:\n    path: {ws}\n  mock:\n    path: {mock}\n",
        ws = ws_wasm.display(),
        mock = mock_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).expect("write manifest");

    let spec = format!(
        r#"openapi: "3.1.0"
info:
  title: WS Relay Test
  version: "1.0.0"

paths:
  /health:
    get:
      operationId: health
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"ok":true}}'
      responses:
        "200":
          description: OK

  /ws/echo:
    get:
      operationId: wsEcho
      x-barbacane-dispatch:
        name: ws-upstream
        config:
          url: "{upstream}"
      responses:
        "101":
          description: Switching Protocols
"#,
        upstream = upstream_url,
    );
    std::fs::write(&spec_path, spec).expect("write spec");

    (temp_dir, spec_path)
}

#[tokio::test]
async fn test_ws_relay_stays_alive_and_echoes_frames() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    // Start a WS echo server
    let (_echo_handle, echo_url) = start_ws_echo_server();

    // Create spec pointing to the echo server and boot the gateway
    let (_temp_dir, spec_path) = create_ws_spec(&echo_url);
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Connect through the gateway
    let ws_url = format!("ws://127.0.0.1:{}/ws/echo", gateway.port());
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("WebSocket connection through gateway failed");

    // Send multiple text frames and verify each echoes back
    for i in 0..5 {
        let msg = format!("hello {}", i);
        ws.send(Message::Text(msg.clone().into()))
            .await
            .expect("send failed");

        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for echo")
            .expect("stream ended")
            .expect("read error");

        assert_eq!(reply, Message::Text(msg.into()), "frame {} did not echo", i);
    }

    // Send a binary frame too
    ws.send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .expect("binary send failed");

    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for binary echo")
        .expect("stream ended")
        .expect("read error");

    assert_eq!(reply, Message::Binary(vec![1, 2, 3].into()));

    // Clean close
    ws.close(None).await.expect("close failed");
}

#[tokio::test]
async fn test_ws_relay_upstream_close_propagates_to_client() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (_echo_handle, echo_url) = start_ws_echo_server();
    let (_temp_dir, spec_path) = create_ws_spec(&echo_url);
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let ws_url = format!("ws://127.0.0.1:{}/ws/echo", gateway.port());
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("WebSocket connection through gateway failed");

    // Verify the connection works
    ws.send(Message::Text("ping".into()))
        .await
        .expect("send failed");

    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("timed out")
        .expect("stream ended")
        .expect("read error");

    assert_eq!(reply, Message::Text("ping".into()));

    // Send close and verify the stream ends cleanly
    ws.send(Message::Close(None)).await.expect("close failed");

    // The next read should be a close frame, end-of-stream, or a reset
    // (the proxy may drop the connection without a close handshake).
    let final_msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await;
    match final_msg {
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {} // Clean close
        Ok(Some(Err(_))) => {}                           // Reset (proxy dropped connection)
        other => panic!("expected close or end-of-stream, got {:?}", other),
    }
}
