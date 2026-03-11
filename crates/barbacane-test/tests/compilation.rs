//! Smoke tests that verify all fixture specs compile successfully.
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
// Fixture compilation tests
//
// Verify that every plugin fixture spec compiles and the gateway starts.
// These don't test runtime behavior — just that the plugin config schemas
// are valid and the artifact builds successfully.
// =========================================================================

#[tokio::test]
async fn test_fixture_compiles_mock() {
    let gateway = TestGateway::from_spec(&fixture("mock.yaml"))
        .await
        .expect("mock fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_lambda() {
    let gateway = TestGateway::from_spec(&fixture("lambda.yaml"))
        .await
        .expect("lambda fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_oauth2_auth() {
    let gateway = TestGateway::from_spec(&fixture("oauth2-auth.yaml"))
        .await
        .expect("oauth2-auth fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_oidc_auth() {
    let gateway = TestGateway::from_spec(&fixture("oidc-auth.yaml"))
        .await
        .expect("oidc-auth fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_http_log() {
    let gateway = TestGateway::from_spec(&fixture("http-log.yaml"))
        .await
        .expect("http-log fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_observability() {
    let gateway = TestGateway::from_spec(&fixture("observability.yaml"))
        .await
        .expect("observability fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_correlation_id() {
    let gateway = TestGateway::from_spec(&fixture("correlation-id.yaml"))
        .await
        .expect("correlation-id fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_ai_proxy() {
    let gateway = TestGateway::from_spec(&fixture("ai-proxy.yaml"))
        .await
        .expect("ai-proxy fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}
