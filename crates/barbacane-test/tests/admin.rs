//! Integration tests for the admin API (ADR-0022).
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

// ==================== Admin API Tests (ADR-0022) ====================

#[tokio::test]
async fn test_admin_health_returns_200() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.admin_get("/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
    assert!(body["uptime_secs"].is_u64());
    assert!(body["artifact_version"].is_u64());
    assert!(body["compiler_version"].is_string());
    assert!(body["routes_count"].is_u64());
}

#[tokio::test]
async fn test_admin_provenance_returns_artifact_hash() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.admin_get("/provenance").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();

    // Artifact hash is always present
    let hash = body["artifact_hash"].as_str().unwrap();
    assert!(
        hash.starts_with("sha256:"),
        "artifact_hash should start with sha256:, got: {}",
        hash,
    );

    // Source specs are present
    let source_specs = body["source_specs"].as_array().unwrap();
    assert!(!source_specs.is_empty());
    assert!(source_specs[0]["file"].is_string());
    assert!(source_specs[0]["sha256"].is_string());
    assert!(source_specs[0]["type"].is_string());

    // drift_detected is present (false by default)
    assert_eq!(body["drift_detected"], false);
}

#[tokio::test]
async fn test_admin_metrics_returns_prometheus_format() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.admin_get("/metrics").await.unwrap();
    assert_eq!(resp.status(), 200);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/plain"),
        "Expected Prometheus text format, got: {}",
        content_type,
    );
}

#[tokio::test]
async fn test_admin_unknown_path_returns_404() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.admin_get("/unknown").await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_admin_post_returns_405() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.admin_post("/health").await.unwrap();
    assert_eq!(resp.status(), 405);
}

#[tokio::test]
async fn test_metrics_not_on_main_port() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    // Metrics should NOT be available on the main port
    let resp = gateway.get("/__barbacane/metrics").await.unwrap();
    assert_eq!(
        resp.status(),
        404,
        "Metrics should not be served on the main traffic port"
    );
}
