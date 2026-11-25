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
// Plugins Without `config`
// =========================================================================

/// A spec whose global middleware `middleware` and whose `mock` dispatcher
/// have no `config`, written next to a manifest naming both.
fn spec_without_configs(middleware: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  mock:\n    path: {}\n  {middleware}:\n    path: {}\n",
            plugins.join("mock/mock.wasm").display(),
            plugins
                .join(format!("{middleware}/{middleware}.wasm"))
                .display(),
        ),
    )
    .expect("write manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: "3.1.0"
info:
  title: Plugins without config
  version: "1.0.0"
x-barbacane-middlewares:
  - name: {middleware}
paths:
  /test:
    get:
      operationId: getTest
      x-barbacane-dispatch:
        name: mock
      responses:
        "200":
          description: Success
"#
        ),
    )
    .expect("write spec");
    (dir, spec)
}

/// An omitted `config` reaches the plugin as `{}`, so it runs with its
/// defaults: correlation-id generates an id and returns it, mock answers 200.
#[tokio::test]
async fn a_plugin_without_config_runs_with_its_defaults() {
    let (_dir, spec) = spec_without_configs("correlation-id");
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/test").await.unwrap();

    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().contains_key("x-correlation-id"),
        "{:?}",
        resp.headers()
    );
}

/// An omitted `config` is checked against the plugin's schema as `{}`: cors
/// requires `allowed_origins`, so the spec does not compile (E1023).
#[tokio::test]
async fn a_plugin_without_config_is_checked_against_its_schema() {
    let (_dir, spec) = spec_without_configs("cors");
    let error = match TestGateway::from_spec(spec.to_str().unwrap()).await {
        Ok(_) => panic!("a cors entry without allowed_origins compiled"),
        Err(e) => e.to_string(),
    };
    assert!(error.contains("E1023"), "{error}");
    assert!(error.contains("allowed_origins"), "{error}");
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

/// A request from an origin outside the list passes through, and its response
/// carries no CORS headers; a browser refuses it to the page, a same-origin or
/// non-browser client is unaffected.
#[tokio::test]
async fn test_cors_simple_request_from_unlisted_origin_passes_without_cors_headers() {
    let gateway = TestGateway::from_spec(&fixture("cors.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/cors-specific")
        .header("Origin", "https://evil-site.com")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200, "the request reaches the dispatcher");
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "no CORS header for an unlisted origin: {:?}",
        resp.headers()
    );
    assert!(resp.headers().get("vary").is_none());
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
    // The gateway trusts the loopback peer as a proxy, so the address in
    // X-Forwarded-For is the client the plugin checks. Without the flag a
    // forged XFF is ignored; the security suite covers that case.
    let gateway = TestGateway::from_spec_with_args(
        &fixture("ip-restriction.yaml"),
        &["--trusted-proxies", "127.0.0.1,::1"],
    )
    .await
    .expect("failed to start gateway");

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
    // Behind the gateway's trusted loopback proxy, a client in the denied
    // range is refused.
    let gateway = TestGateway::from_spec_with_args(
        &fixture("ip-restriction.yaml"),
        &["--trusted-proxies", "127.0.0.1,::1"],
    )
    .await
    .expect("failed to start gateway");

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
    post:
      summary: Endpoint with HTTP logging and a request body
      operationId: postLogged
      x-barbacane-middlewares:
        - name: http-log
          config:
            endpoint: "{}"
            timeout_ms: 2000
            include_body: true
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
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
        log_endpoint, log_endpoint
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

/// With `skip_if_empty`, the cookie fills an absent `Authorization`, an
/// incoming one wins, and without a usable cookie no header is sent. The mock
/// echoes `{{headers.authorization}}` and keeps the placeholder when the header
/// is absent.
#[tokio::test]
async fn test_request_transformer_skip_if_empty_bearer_from_cookie() {
    let gateway = TestGateway::from_spec(&fixture("request-transformer.yaml"))
        .await
        .expect("failed to start gateway");

    let cases: [(Option<&str>, Option<&str>, &str); 5] = [
        (None, None, "{{headers.authorization}}"),
        (None, Some("sso_token=ck"), "Bearer ck"),
        (None, Some("sso_token="), "{{headers.authorization}}"),
        (None, Some("other=ck"), "{{headers.authorization}}"),
        (Some("Bearer tok"), Some("sso_token=ck"), "Bearer tok"),
    ];
    for (authorization, cookie, expected) in cases {
        let mut req = gateway.request_builder(reqwest::Method::GET, "/cookie-bearer");
        if let Some(value) = authorization {
            req = req.header("Authorization", value);
        }
        if let Some(value) = cookie {
            req = req.header("Cookie", value);
        }
        let resp = req.send().await.unwrap();
        assert_eq!(
            resp.status(),
            200,
            "authorization {authorization:?}, cookie {cookie:?}"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["authorization"], expected,
            "authorization {authorization:?}, cookie {cookie:?}"
        );
    }
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

/// GET a path without following redirects, so the 3xx status + Location header
/// can be asserted directly. The shared TestGateway client follows redirects,
/// which would turn a 301 -> /new-page into a 404 for the nonexistent target.
async fn get_no_redirect(gateway: &TestGateway, path: &str) -> reqwest::Response {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("failed to build no-redirect client")
        .get(format!("{}{}", gateway.base_url(), path))
        .send()
        .await
        .expect("request failed")
}

#[tokio::test]
async fn test_redirect_exact_path_301() {
    let gateway = TestGateway::from_spec(&fixture("redirect.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = get_no_redirect(&gateway, "/old-page").await;

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

    let resp = get_no_redirect(&gateway, "/api/v1/users").await;

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

    let resp = get_no_redirect(&gateway, "/catch-all").await;

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

    let resp = get_no_redirect(&gateway, "/with-query?foo=bar&page=2").await;

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

    let resp = get_no_redirect(&gateway, "/no-query?foo=bar").await;

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
    // A GET without `Upgrade: websocket` is refused by the plugin.
    let gateway = TestGateway::from_spec(&fixture("ws-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/ws/echo").await.unwrap();
    assert_eq!(resp.status(), 400);
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

    // A complete handshake to an upstream nothing listens on.
    assert_eq!(resp.status(), 502);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:upstream-unavailable");
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
                    if (msg.is_text() || msg.is_binary()) && tx.send(msg).await.is_err() {
                        break;
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

/// The upstream handshake carries the scheme and host the gateway resolved:
/// behind a trusted proxy, the ones the proxy reported.
#[tokio::test]
// The handshake callback's signature is tungstenite's `Callback`.
#[allow(clippy::result_large_err)]
async fn test_ws_relay_forwards_the_resolved_scheme_and_host() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("ws://{}", listener.local_addr().unwrap());
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let seen_tx = seen_tx.clone();
            tokio::spawn(async move {
                let record =
                    move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                        let get = |name: &str| {
                            req.headers()
                                .get(name)
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string)
                        };
                        let _ = seen_tx.send((get("x-forwarded-proto"), get("x-forwarded-host")));
                        Ok(resp)
                    };
                let _ = tokio_tungstenite::accept_hdr_async(stream, record).await;
            });
        }
    });

    let (_temp_dir, spec_path) = create_ws_spec(&upstream);
    let gateway = TestGateway::from_spec_with_args(
        spec_path.to_str().unwrap(),
        &["--trusted-proxies", "127.0.0.1"],
    )
    .await
    .expect("failed to start gateway");

    let mut request = format!("ws://127.0.0.1:{}/ws/echo", gateway.port())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("x-forwarded-proto", "https".parse().unwrap());
    request
        .headers_mut()
        .insert("x-forwarded-host", "public.example".parse().unwrap());
    let (_ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("WebSocket handshake through the gateway failed");

    let seen = tokio::time::timeout(std::time::Duration::from_secs(5), seen_rx.recv())
        .await
        .expect("the upstream saw no handshake")
        .expect("channel closed");
    assert_eq!(
        seen,
        (
            Some("https".to_string()),
            Some("public.example".to_string())
        )
    );
}

/// Whether a new connection to `port` is answered: an HTTP request on it
/// gets a response before the gateway closes it.
fn a_new_connection_is_answered(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let _ =
        stream.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    let mut buf = [0u8; 16];
    matches!(stream.read(&mut buf), Ok(n) if n > 0)
}

/// A WebSocket relay holds its connection's place under `--max-connections`
/// until it closes.
#[tokio::test]
async fn test_ws_relay_counts_against_the_connection_cap() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (_echo_handle, echo_url) = start_ws_echo_server();
    let (_temp_dir, spec_path) = create_ws_spec(&echo_url);
    let gateway =
        TestGateway::from_spec_with_args(spec_path.to_str().unwrap(), &["--max-connections", "2"])
            .await
            .expect("failed to start gateway");

    // One place: the harness's kept-alive connection.
    assert_eq!(gateway.get("/health").await.unwrap().status(), 200);
    // The other: the relay.
    let ws_url = format!("ws://127.0.0.1:{}/ws/echo", gateway.port());
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("WebSocket connection through gateway failed");
    ws.send(Message::text("hello")).await.unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("an echo in time")
        .expect("open")
        .expect("valid");
    assert_eq!(reply, Message::text("hello"));

    let port = gateway.port();
    assert!(
        !tokio::task::spawn_blocking(move || a_new_connection_is_answered(port))
            .await
            .unwrap(),
        "a connection past the cap was served while the relay was open"
    );

    ws.close(None).await.unwrap();
    let mut freed = false;
    for _ in 0..50 {
        if tokio::task::spawn_blocking(move || a_new_connection_is_answered(port))
            .await
            .unwrap()
        {
            freed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(freed, "the closed relay frees its place");
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

/// With `include_body`, the log entry carries the request and response body
/// sizes, which needs the middleware to receive the bodies.
#[tokio::test]
async fn test_http_log_includes_body_sizes() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    let log_endpoint = format!("{}/logs", mock_server.uri());
    let (_temp_dir, spec_path) = create_http_log_spec(&log_endpoint);
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let request_body = r#"{"hello":"world"}"#;
    let resp = gateway.post("/logged", request_body).await.unwrap();
    assert_eq!(resp.status(), 200);
    let response_body = resp.text().await.unwrap();

    let received = mock_server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1, "one log entry");
    let entry: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(entry["request"]["body_size"], request_body.len(), "{entry}");
    assert_eq!(
        entry["response"]["body_size"],
        response_body.len(),
        "{entry}"
    );
}

// ========================
// Bot Detection
// ========================

/// `GET path` with `user_agent` as `User-Agent`, or none: the status.
async fn with_user_agent(gateway: &TestGateway, path: &str, user_agent: Option<&str>) -> u16 {
    let mut request = gateway.request_builder(reqwest::Method::GET, path);
    if let Some(user_agent) = user_agent {
        request = request.header("user-agent", user_agent);
    }
    request.send().await.unwrap().status().as_u16()
}

#[tokio::test]
async fn bot_detection_blocks_denied_agents_and_lets_others_through() {
    let gateway = TestGateway::from_spec(&fixture("bot-detection.yaml"))
        .await
        .expect("failed to start gateway");
    let browser = Some("Mozilla/5.0 (X11; Linux x86_64) Firefox/120.0");

    for (path, user_agent, expected) in [
        ("/denylist", browser, 200),
        ("/denylist", Some("Scrapy/2.11 (+https://scrapy.org)"), 403),
        (
            "/denylist",
            Some("Mozilla/5.0 (compatible; AhrefsBot/7.0)"),
            403,
        ),
        (
            "/allow-trusted-bots",
            Some("Mozilla/5.0 (compatible; Googlebot/2.1)"),
            200,
        ),
        ("/allow-trusted-bots", Some("SomeOtherBot/1.0"), 403),
        ("/block-empty-ua", None, 403),
        ("/block-empty-ua", browser, 200),
        ("/custom-status", Some("curl/8.4.0"), 429),
        ("/public", Some("Scrapy/2.11"), 200),
    ] {
        assert_eq!(
            with_user_agent(&gateway, path, user_agent).await,
            expected,
            "{path} with {user_agent:?}"
        );
    }

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/custom-status")
        .header("user-agent", "Wget/1.21")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429);
    let body = resp.text().await.unwrap();
    assert!(body.contains("Automated access is not permitted"), "{body}");
}

// ========================
// Lambda Dispatcher
// ========================

/// A project whose `POST /invoke` is dispatched to the Lambda Function URL
/// `url`.
fn lambda_spec(url: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  lambda:\n    path: {}\n",
            plugins.join("lambda/lambda.wasm").display()
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: "3.1.0"
info: {{ title: Lambda, version: "1.0.0" }}
paths:
  /invoke:
    post:
      operationId: invoke
      parameters:
        - {{ name: x-tenant, in: header, schema: {{ type: string }} }}
      x-barbacane-dispatch:
        name: lambda
        config:
          url: "{url}"
          timeout: 5
      requestBody:
        content:
          application/json:
            schema: {{ type: object }}
      responses:
        "201": {{ description: created }}
"#
        ),
    )
    .expect("spec");
    (dir, spec.display().to_string())
}

/// The Lambda dispatcher invokes the function URL with the request's method,
/// headers and body, and relays the function's status, headers and body.
#[tokio::test]
async fn lambda_invokes_the_function_and_relays_its_response() {
    use wiremock::matchers::{body_string, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let function = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("x-tenant", "acme"))
        .and(body_string(r#"{"order":42}"#))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("x-function", "invoked")
                .set_body_string(r#"{"created":42}"#),
        )
        .expect(1)
        .mount(&function)
        .await;
    let (_dir, spec) = lambda_spec(&format!("{}/", function.uri()));
    let gateway = TestGateway::from_spec(&spec)
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/invoke")
        .header("content-type", "application/json")
        .header("x-tenant", "acme")
        .body(r#"{"order":42}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    assert_eq!(resp.headers()["x-function"], "invoked");
    assert_eq!(resp.text().await.unwrap(), r#"{"created":42}"#);
}

// ========================
// NATS publish success
// ========================

/// A NATS server on loopback that speaks enough of the protocol for a client
/// to connect and publish. Sends each published `(subject, payload)` on the
/// channel; returns its port.
async fn fake_nats() -> (u16, tokio::sync::mpsc::UnboundedReceiver<(String, Vec<u8>)>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let info = format!(
                    "INFO {{\"server_id\":\"test\",\"version\":\"2.10.0\",\"go\":\"go1.22\",\"host\":\"127.0.0.1\",\"port\":{port},\"headers\":true,\"max_payload\":1048576,\"proto\":1}}\r\n"
                );
                if write.write_all(info.as_bytes()).await.is_err() {
                    return;
                }
                let mut read = BufReader::new(read);
                let mut line = String::new();
                loop {
                    line.clear();
                    if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let words: Vec<&str> = line.split_whitespace().collect();
                    match words.first().map(|w| w.to_ascii_uppercase()).as_deref() {
                        Some("PING") => {
                            let _ = write.write_all(b"PONG\r\n").await;
                        }
                        Some("PUB") | Some("HPUB") => {
                            let hpub = words[0].eq_ignore_ascii_case("HPUB");
                            let total: usize =
                                words.last().and_then(|n| n.parse().ok()).unwrap_or(0);
                            let headers: usize = if hpub {
                                words[words.len() - 2].parse().unwrap_or(0)
                            } else {
                                0
                            };
                            let mut message = vec![0u8; total + 2];
                            if read.read_exact(&mut message).await.is_err() {
                                return;
                            }
                            let payload = message[headers..total].to_vec();
                            let _ = tx.send((words[1].to_string(), payload));
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    (port, rx)
}

/// With a broker that answers, the NATS dispatcher publishes the request body
/// to the configured subject and answers 202.
#[tokio::test]
async fn nats_publishes_the_request_body_to_the_subject() {
    let (port, mut published) = fake_nats().await;
    let dir = tempfile::TempDir::new().expect("temp dir");
    let fixture_spec = std::fs::read_to_string(fixture("nats-dispatch.yaml")).unwrap();
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  nats:\n    path: {}\n",
            plugins.join("nats/nats.wasm").display()
        ),
    )
    .unwrap();
    let spec = dir.path().join("nats.yaml");
    std::fs::write(
        &spec,
        fixture_spec.replace(
            "nats://127.0.0.1:14222",
            &format!("nats://127.0.0.1:{port}"),
        ),
    )
    .unwrap();
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body = r#"{"userId":"550e8400-e29b-41d4-a716-446655440000","email":"test@example.com"}"#;
    let resp = gateway.post("/events/users", body).await.unwrap();
    assert_eq!(resp.status(), 202);

    let (subject, payload) =
        tokio::time::timeout(std::time::Duration::from_secs(5), published.recv())
            .await
            .expect("published in time")
            .expect("a message");
    assert_eq!(subject, "events.users");
    assert_eq!(payload, body.as_bytes());
}
