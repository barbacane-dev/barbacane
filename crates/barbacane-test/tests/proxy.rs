//! Integration tests for HTTP upstream proxy and TLS termination.
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

// ========================
// M4: HTTP Upstream Tests
// ========================

#[tokio::test]
async fn test_http_upstream_get() {
    let gateway = TestGateway::from_spec(&fixture("http-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    // Proxy GET request to httpbin.org/get
    let resp = gateway.get("/proxy/get").await.unwrap();

    // httpbin.org/get returns 200 with JSON containing request details
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("url").is_some(),
        "response should contain url field"
    );
}

#[tokio::test]
async fn test_http_upstream_post() {
    let gateway = TestGateway::from_spec(&fixture("http-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    // Proxy POST request to httpbin.org/post
    let resp = gateway
        .post("/proxy/post", r#"{"test":"data"}"#)
        .await
        .unwrap();

    // httpbin.org/post returns 200 with JSON containing request details
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("json").is_some(),
        "response should contain json field"
    );
    assert_eq!(body["json"]["test"], "data");
}

#[tokio::test]
async fn test_http_upstream_headers_forwarded() {
    let gateway = TestGateway::from_spec(&fixture("http-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    // httpbin.org/headers returns the request headers
    let resp = gateway.get("/proxy/headers").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let headers = &body["headers"];

    // Should have X-Forwarded-Host header
    assert!(
        headers.get("X-Forwarded-Host").is_some() || headers.get("x-forwarded-host").is_some(),
        "should forward X-Forwarded-Host header"
    );
}

// ========================
// M6a: TLS Termination Tests
// ========================

#[tokio::test]
async fn test_tls_gateway_health() {
    let gateway = TestGateway::from_spec_with_tls(&fixture("minimal.yaml"))
        .await
        .expect("failed to start TLS gateway");

    // Verify TLS is enabled
    assert!(gateway.is_tls_enabled());
    assert!(gateway.base_url().starts_with("https://"));

    // Health check over HTTPS
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
}

#[tokio::test]
async fn test_tls_gateway_mock_response() {
    let gateway = TestGateway::from_spec_with_tls(&fixture("minimal.yaml"))
        .await
        .expect("failed to start TLS gateway");

    // Mock response over HTTPS
    let resp = gateway.get("/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn test_tls_gateway_404() {
    let gateway = TestGateway::from_spec_with_tls(&fixture("minimal.yaml"))
        .await
        .expect("failed to start TLS gateway");

    // 404 response over HTTPS
    let resp = gateway.get("/nonexistent").await.unwrap();
    assert_eq!(resp.status(), 404);
}
