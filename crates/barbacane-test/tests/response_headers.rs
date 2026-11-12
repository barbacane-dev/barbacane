//! What every response and the public health endpoint disclose.

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
async fn the_server_header_names_no_version() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");
    for path in ["/health", "/missing"] {
        let resp = gateway.get(path).await.unwrap();
        assert_eq!(
            resp.headers().get("server").and_then(|v| v.to_str().ok()),
            Some("barbacane"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn the_public_health_body_says_only_that_the_gateway_is_up() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");
    let body: serde_json::Value = gateway
        .get("/__barbacane/health")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body, serde_json::json!({"status": "healthy"}), "{body}");
}
