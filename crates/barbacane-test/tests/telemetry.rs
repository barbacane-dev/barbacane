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

    // Make a request that will fail validation (missing required field)
    let _ = gateway.post("/validate", "{}").await.unwrap();

    // Get metrics
    let resp = gateway.admin_get("/metrics").await.unwrap();
    let body = resp.text().await.unwrap();

    // Should have recorded validation failure
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
