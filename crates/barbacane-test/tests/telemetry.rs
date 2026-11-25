//! Integration tests for Prometheus metrics and telemetry.
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

#[tokio::test]
async fn test_metrics_endpoint_returns_prometheus_format() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    // Make a request to generate some metrics
    let _ = gateway.get("/health").await.unwrap();

    // Get the metrics endpoint
    let resp = gateway.admin_get("/metrics").await.unwrap();
    assert_eq!(resp.status(), 200);

    // Check content type is Prometheus format
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/plain"),
        "Expected Prometheus text format, got: {}",
        content_type
    );

    let body = resp.text().await.unwrap();

    // Should contain standard Barbacane metrics
    assert!(
        body.contains("barbacane_requests_total"),
        "Missing barbacane_requests_total metric"
    );
    assert!(
        body.contains("barbacane_request_duration_seconds"),
        "Missing barbacane_request_duration_seconds metric"
    );
    assert!(
        body.contains("barbacane_active_connections"),
        "Missing barbacane_active_connections metric"
    );
}

#[tokio::test]
async fn test_metrics_records_request_counts() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    // Make several requests
    for _ in 0..3 {
        let resp = gateway.get("/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    // Get metrics
    let resp = gateway.admin_get("/metrics").await.unwrap();
    let body = resp.text().await.unwrap();

    // Should have recorded the requests
    // The metric line format is: barbacane_requests_total{...} <count>
    assert!(
        body.contains("barbacane_requests_total"),
        "Metrics should contain request counter"
    );
}

#[tokio::test]
async fn test_metrics_records_validation_failures() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // POST /users with an empty body fails request validation (missing required
    // fields), which records a validation-failure counter sample.
    let resp = gateway.post("/users", "{}").await.unwrap();
    assert_eq!(
        resp.status(),
        400,
        "empty body must fail request validation"
    );

    let resp = gateway.admin_get("/metrics").await.unwrap();
    let body = resp.text().await.unwrap();

    assert!(
        body.contains("barbacane_validation_failures_total"),
        "Metrics should contain validation failure counter"
    );
}

#[tokio::test]
async fn test_metrics_records_404_responses() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    // Make a request to non-existent endpoint
    let resp = gateway.get("/nonexistent").await.unwrap();
    assert_eq!(resp.status(), 404);

    // Get metrics
    let resp = gateway.admin_get("/metrics").await.unwrap();
    let body = resp.text().await.unwrap();

    // Should have recorded the 404 request
    assert!(
        body.contains("barbacane_requests_total"),
        "Metrics should contain request counter"
    );
    // The metric should include status=404 label
    assert!(
        body.contains("status=\"404\""),
        "Metrics should record 404 status"
    );
}

#[tokio::test]
async fn test_metrics_connection_tracking() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    // Make a request to establish connection
    let _ = gateway.get("/health").await.unwrap();

    // Get metrics
    let resp = gateway.admin_get("/metrics").await.unwrap();
    let body = resp.text().await.unwrap();

    // Should have connection metrics
    assert!(
        body.contains("barbacane_connections_total"),
        "Metrics should contain connections_total counter"
    );
}

#[tokio::test]
async fn a_plugin_metric_is_exported_as_its_own_family_with_real_labels() {
    let gateway = TestGateway::from_spec(&fixture("observability.yaml"))
        .await
        .expect("failed to start gateway");
    for _ in 0..3 {
        assert_eq!(gateway.get("/timed").await.unwrap().status(), 200);
    }

    let body = gateway
        .admin_get("/metrics")
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        body.contains("# TYPE barbacane_plugin_observability_latency_seconds histogram"),
        "{body}"
    );
    assert!(
        body.contains(r#"barbacane_plugin_observability_latency_seconds_count{status="200"} 3"#),
        "{body}"
    );
    assert!(!body.contains("labels_json"), "{body}");
}

/// With `detailed_request_logs`, the observability middleware logs the
/// request body size, which needs it to receive the body.
#[tokio::test]
async fn observability_detailed_request_log_reports_the_body_size() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let plugins_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    std::fs::write(
        temp_dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  mock:\n    path: {}\n  observability:\n    path: {}\n",
            plugins_dir.join("mock/mock.wasm").display(),
            plugins_dir
                .join("observability/observability.wasm")
                .display()
        ),
    )
    .expect("failed to write manifest file");
    let spec_path = temp_dir.path().join("observability-body.yaml");
    std::fs::write(
        &spec_path,
        r#"openapi: "3.1.0"
info:
  title: Observability Body Test API
  version: "1.0.0"
paths:
  /events:
    post:
      operationId: postEvent
      x-barbacane-middlewares:
        - name: observability
          config:
            detailed_request_logs: true
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: mock
        config:
          status: 202
          body: '{"accepted":true}'
      responses:
        "202":
          description: Accepted
"#,
    )
    .expect("failed to write spec file");
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body = r#"{"event":"signup"}"#;
    let resp = gateway.post("/events", body).await.unwrap();
    assert_eq!(resp.status(), 202);

    let needle = format!("body_size={}", body.len());
    assert!(
        gateway
            .log()
            .wait_for(&needle, std::time::Duration::from_secs(5)),
        "no request log with {needle}:\n{}",
        gateway
            .log()
            .find_line("observability: request")
            .unwrap_or_default()
    );
}

// ==================== Distributed tracing ====================

/// A gateway exporting spans over OTLP/HTTP to `collector`, proxying `/items`
/// to `upstream` with `http-upstream`. Batches leave every 100 ms.
async fn traced_gateway(collector: &str, upstream: &str) -> (tempfile::TempDir, TestGateway) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let http_upstream = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/http-upstream/http-upstream.wasm");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  http-upstream:\n    path: {}\n",
            http_upstream.display()
        ),
    )
    .expect("write manifest");
    let spec = dir.path().join("traced.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: "3.1.0"
info: {{ title: Traced, version: "1" }}
paths:
  /items:
    get:
      operationId: listItems
      x-barbacane-dispatch:
        name: http-upstream
        config: {{ url: "{upstream}", path: /items }}
      responses: {{ "200": {{ description: ok }} }}
"#
        ),
    )
    .expect("write spec");
    let gateway = TestGateway::from_spec_with_env(
        spec.to_str().unwrap(),
        &[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", collector),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
            ("OTEL_BSP_SCHEDULE_DELAY", "100"),
        ],
    )
    .await
    .expect("failed to start gateway");
    (dir, gateway)
}

/// The OTLP export bodies the collector received, waiting up to 10 s for
/// one that holds `needle`.
async fn exported_with(collector: &wiremock::MockServer, needle: &[u8]) -> Vec<u8> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let bodies: Vec<u8> = collector
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.url.path() == "/v1/traces")
            .flat_map(|r| r.body)
            .collect();
        if bodies.windows(needle.len()).any(|w| w == needle) {
            return bodies;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no exported span holds {:?}",
            String::from_utf8_lossy(needle)
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// A request carrying a `traceparent` continues that trace: the gateway's
/// span is its child, the upstream receives the gateway's span as parent and
/// the gateway's request id, and the spans reach the OTLP collector.
#[tokio::test]
async fn a_traced_request_continues_its_trace_upstream_and_is_exported() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&collector)
        .await;
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&upstream)
        .await;
    let (_dir, gateway) = traced_gateway(&collector.uri(), &upstream.uri()).await;

    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/items")
        .header("traceparent", format!("00-{trace_id}-00f067aa0ba902b7-01"))
        .header("x-request-id", "client-request-1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-trace-id"], trace_id);

    let sent = &upstream.received_requests().await.unwrap()[0];
    let traceparent = sent.headers["traceparent"].to_str().unwrap();
    let parts: Vec<&str> = traceparent.split('-').collect();
    assert_eq!(parts[1], trace_id, "{traceparent}");
    assert_ne!(
        parts[2], "00f067aa0ba902b7",
        "the gateway's span is the parent"
    );
    assert_eq!(sent.headers["x-request-id"], "client-request-1");

    let trace_bytes: Vec<u8> = (0..trace_id.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&trace_id[i..i + 2], 16).unwrap())
        .collect();
    let exported = exported_with(&collector, b"barbacane.dispatch.http-upstream").await;
    assert!(exported.windows(16).any(|w| w == trace_bytes.as_slice()));
    assert!(exported.windows(10).any(|w| w == b"GET /items"));
}

/// A request without a `traceparent` starts a trace; the upstream receives
/// it, and the response names it.
#[tokio::test]
async fn an_untraced_request_starts_a_trace_the_upstream_receives() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&collector)
        .await;
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&upstream)
        .await;
    let (_dir, gateway) = traced_gateway(&collector.uri(), &upstream.uri()).await;

    let resp = gateway.get("/items").await.unwrap();
    assert_eq!(resp.status(), 200);
    let trace_id = resp.headers()["x-trace-id"].to_str().unwrap().to_string();
    let request_id = resp.headers()["x-request-id"].to_str().unwrap().to_string();

    let sent = &upstream.received_requests().await.unwrap()[0];
    let traceparent = sent.headers["traceparent"].to_str().unwrap();
    assert_eq!(
        traceparent.split('-').nth(1),
        Some(trace_id.as_str()),
        "{traceparent}"
    );
    assert_eq!(sent.headers["x-request-id"], request_id.as_str());
}

/// The gRPC exporter runs on the gateway's runtime: with no collector to
/// reach, requests are answered as usual.
#[tokio::test]
async fn a_grpc_exporter_without_a_collector_does_not_hold_up_requests() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&upstream)
        .await;
    let dir = tempfile::TempDir::new().expect("temp dir");
    let http_upstream = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/http-upstream/http-upstream.wasm");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  http-upstream:\n    path: {}\n",
            http_upstream.display()
        ),
    )
    .expect("write manifest");
    let spec = dir.path().join("traced.yaml");
    std::fs::write(
        &spec,
        format!(
            "openapi: \"3.1.0\"\ninfo: {{ title: T, version: \"1\" }}\npaths:\n  /items:\n    get:\n      x-barbacane-dispatch:\n        name: http-upstream\n        config: {{ url: \"{}\", path: /items }}\n      responses: {{ \"200\": {{ description: ok }} }}\n",
            upstream.uri()
        ),
    )
    .expect("write spec");
    let gateway = TestGateway::from_spec_with_env(
        spec.to_str().unwrap(),
        &[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1"),
            ("OTEL_BSP_SCHEDULE_DELAY", "50"),
        ],
    )
    .await
    .expect("failed to start gateway");
    for _ in 0..5 {
        assert_eq!(gateway.get("/items").await.unwrap().status(), 200);
    }
}

// ==================== Runtime metrics ====================

/// The admin endpoint's metrics text, once it holds `needle`.
async fn metrics_with(gateway: &TestGateway, needle: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let body = gateway
            .admin_get("/metrics")
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        if body.contains(needle) || std::time::Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A dispatch is measured, and so is each plugin call.
#[tokio::test]
async fn dispatches_and_plugin_calls_are_measured() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");
    assert_eq!(gateway.get("/health").await.unwrap().status(), 200);

    let dispatch = r#"barbacane_dispatch_duration_seconds_count{dispatcher="mock"} 1"#;
    let body = metrics_with(&gateway, dispatch).await;
    assert!(body.contains(dispatch), "{body}");
    assert!(
        body.contains(
            r#"barbacane_wasm_execution_duration_seconds_count{plugin="mock",function="dispatch"} 1"#
        ),
        "{body}"
    );
}

/// A request to a deprecated operation is counted by route and spec.
#[tokio::test]
async fn requests_to_deprecated_routes_are_counted() {
    let gateway = TestGateway::from_spec(&fixture("deprecated.yaml"))
        .await
        .expect("failed to start gateway");
    assert_eq!(gateway.get("/v1/users").await.unwrap().status(), 200);
    assert_eq!(gateway.get("/v2/users").await.unwrap().status(), 200);

    let deprecated = r#"barbacane_deprecated_route_requests_total{method="GET",path="/v1/users",api="deprecated"} 1"#;
    let body = metrics_with(&gateway, deprecated).await;
    assert!(body.contains(deprecated), "{body}");
    assert!(!body
        .contains(r#"barbacane_deprecated_route_requests_total{method="GET",path="/v2/users""#));
}

/// The `api` label names the spec each operation comes from.
#[tokio::test]
async fn the_api_label_is_the_operations_spec() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/multi-spec");
    let gateway = TestGateway::from_specs(&[
        dir.join("users.yaml").to_str().unwrap(),
        dir.join("orders.yaml").to_str().unwrap(),
    ])
    .await
    .expect("failed to start gateway");
    assert_eq!(gateway.get("/users").await.unwrap().status(), 200);
    assert_eq!(gateway.get("/orders").await.unwrap().status(), 200);

    let orders =
        r#"barbacane_requests_total{method="GET",path="/orders",status="200",api="orders"} 1"#;
    let body = metrics_with(&gateway, orders).await;
    assert!(body.contains(orders), "{body}");
    assert!(
        body.contains(
            r#"barbacane_requests_total{method="GET",path="/users",status="200",api="users"} 1"#
        ),
        "{body}"
    );
}
