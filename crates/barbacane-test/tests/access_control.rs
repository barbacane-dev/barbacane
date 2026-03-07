//! Integration tests for authorization plugins — ACL, OPA, CEL.
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

// ==================== ACL Tests ====================

/// Helper to create Basic auth header for ACL tests.
fn acl_basic_auth(username: &str, password: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let encoded = STANDARD.encode(format!("{}:{}", username, password));
    format!("Basic {}", encoded)
}

#[tokio::test]
async fn test_acl_admin_allowed_admin_only() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/admin-only")
        .header("Authorization", acl_basic_auth("admin", "admin123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_acl_editor_denied_admin_only() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/admin-only")
        .header("Authorization", acl_basic_auth("editor", "editor123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_acl_editor_allowed_editors_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/editors")
        .header("Authorization", acl_basic_auth("editor", "editor123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_acl_viewer_denied_editors_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/editors")
        .header("Authorization", acl_basic_auth("viewer", "viewer123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_acl_banned_group_denied() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/deny-banned")
        .header("Authorization", acl_basic_auth("banned_user", "banned123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_acl_non_banned_allowed_deny_rule() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/deny-banned")
        .header("Authorization", acl_basic_auth("editor", "editor123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_acl_consumer_allow_specific_user() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/consumer-allow")
        .header("Authorization", acl_basic_auth("admin", "admin123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_acl_consumer_allow_denies_other() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/consumer-allow")
        .header("Authorization", acl_basic_auth("editor", "editor123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_acl_static_consumer_groups_premium_allowed() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    // viewer gets "premium" group via static consumer_groups config
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/premium")
        .header("Authorization", acl_basic_auth("viewer", "viewer123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_acl_static_consumer_groups_non_premium_denied() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    // admin has no "premium" group
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/premium")
        .header("Authorization", acl_basic_auth("admin", "admin123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_acl_public_endpoint_no_auth() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway.get("/public").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_acl_missing_auth_returns_401() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    // No Authorization header — basic-auth should return 401 before ACL runs
    let resp = gateway.get("/admin-only").await.unwrap();
    assert_eq!(resp.status(), 401);
}

// --- OPA Authorization Tests ---

#[tokio::test]
async fn test_opa_unreachable_returns_503() {
    let gateway = TestGateway::from_spec(&fixture("opa-authz.yaml"))
        .await
        .expect("failed to start gateway");
    // OPA URL points to unreachable port — expect 503
    let resp = gateway.get("/opa-protected").await.unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn test_opa_unreachable_returns_problem_json() {
    let gateway = TestGateway::from_spec(&fixture("opa-authz.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway.get("/opa-protected").await.unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:opa-unavailable");
    assert_eq!(body["status"], 503);
}

#[tokio::test]
async fn test_opa_with_auth_missing_credentials_returns_401() {
    let gateway = TestGateway::from_spec(&fixture("opa-authz.yaml"))
        .await
        .expect("failed to start gateway");
    // No auth header — basic-auth returns 401 before OPA runs
    let resp = gateway.get("/opa-with-auth").await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_opa_with_auth_valid_credentials_opa_unreachable() {
    let gateway = TestGateway::from_spec(&fixture("opa-authz.yaml"))
        .await
        .expect("failed to start gateway");
    // Valid auth but OPA unreachable — expect 503
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/opa-with-auth")
        .header("Authorization", acl_basic_auth("admin", "admin123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn test_opa_public_endpoint_bypasses_opa() {
    let gateway = TestGateway::from_spec(&fixture("opa-authz.yaml"))
        .await
        .expect("failed to start gateway");
    // Public endpoint has no OPA middleware — should succeed
    let resp = gateway.get("/public").await.unwrap();
    assert_eq!(resp.status(), 200);
}

// -----------------------------------------------------------------------
// CEL policy evaluation tests
// -----------------------------------------------------------------------

fn cel_basic_auth(username: &str, password: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let encoded = STANDARD.encode(format!("{}:{}", username, password));
    format!("Basic {}", encoded)
}

#[tokio::test]
async fn test_cel_method_check_get_allowed() {
    let gateway = TestGateway::from_spec(&fixture("cel.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway.get("/cel-method-check").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_cel_method_check_post_denied() {
    let gateway = TestGateway::from_spec(&fixture("cel.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/cel-method-check")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_cel_with_auth_admin_allowed() {
    let gateway = TestGateway::from_spec(&fixture("cel.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/cel-with-auth")
        .header("Authorization", cel_basic_auth("admin", "admin123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_cel_with_auth_viewer_denied() {
    let gateway = TestGateway::from_spec(&fixture("cel.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/cel-with-auth")
        .header("Authorization", cel_basic_auth("viewer", "viewer123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_cel_public_endpoint_bypasses() {
    let gateway = TestGateway::from_spec(&fixture("cel.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway.get("/public").await.unwrap();
    assert_eq!(resp.status(), 200);
}
