//! Integration tests for routing — health check, 404/405, path parameters, wildcard paths.
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
async fn test_gateway_health() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
}

#[tokio::test]
async fn test_gateway_mock_response() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn test_gateway_404() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/nonexistent").await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_gateway_405() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request(reqwest::Method::DELETE, "/health")
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);

    // Check Allow header
    let allow = resp.headers().get("allow").unwrap().to_str().unwrap();
    assert!(allow.contains("GET"));
}

#[tokio::test]
async fn test_gateway_path_params() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/users/123").await.unwrap();
    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        panic!("Expected 200 but got {}. Body: {}", status, body);
    }
}

#[tokio::test]
async fn test_wildcard_path_single_segment() {
    let gateway = TestGateway::from_spec(&fixture("wildcard-path.yaml"))
        .await
        .expect("failed to start gateway");

    // Single segment captured by {path+}
    let resp = gateway.get("/files/readme.txt").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn test_wildcard_path_multi_segment() {
    let gateway = TestGateway::from_spec(&fixture("wildcard-path.yaml"))
        .await
        .expect("failed to start gateway");

    // Multi-segment path captured as a single value
    let resp = gateway.get("/files/docs/2024/report.pdf").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn test_wildcard_path_static_takes_precedence() {
    let gateway = TestGateway::from_spec(&fixture("wildcard-path.yaml"))
        .await
        .expect("failed to start gateway");

    // Static route /files/special must win over /files/{path+}
    let resp = gateway.get("/files/special").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["route"], "static");
}

#[tokio::test]
async fn test_wildcard_path_with_prefix_param() {
    let gateway = TestGateway::from_spec(&fixture("wildcard-path.yaml"))
        .await
        .expect("failed to start gateway");

    // S3-style: {bucket} is a regular param, {key+} captures the rest
    let resp = gateway
        .get("/storage/my-bucket/folder/sub/file.txt")
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn test_wildcard_path_missing_key_segment() {
    let gateway = TestGateway::from_spec(&fixture("wildcard-path.yaml"))
        .await
        .expect("failed to start gateway");

    // /storage/{bucket} with no key — {key+} requires at least one segment
    let resp = gateway.get("/storage/my-bucket").await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}
