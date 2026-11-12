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
