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

/// A group whose name contains commas (an LDAP DN) reaches `acl` as one
/// group, so a deny rule naming it applies.
#[tokio::test]
async fn test_acl_denies_a_group_named_by_a_dn() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/deny-dn-group")
        .header("Authorization", acl_basic_auth("ops", "ops123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

/// A piece of a group name between commas is not a group the consumer has.
#[tokio::test]
async fn test_acl_part_of_a_group_name_does_not_satisfy_allow() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/allow-dn-fragment")
        .header("Authorization", acl_basic_auth("ops", "ops123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

/// `x-auth-*` request headers are dropped at ingress: a client cannot forge
/// groups for a user the auth plugin gives none.
#[tokio::test]
async fn test_acl_client_supplied_identity_headers_are_dropped() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/admin-only")
        .header("Authorization", acl_basic_auth("guest", "guest123"))
        .header("X-Auth-Consumer-Groups", "admin")
        .header("x-auth-consumer", "admin")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_acl_guest_without_groups_denied_admin_only() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/admin-only")
        .header("Authorization", acl_basic_auth("guest", "guest123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_acl_admin_with_spoofed_headers_still_allowed() {
    let gateway = TestGateway::from_spec(&fixture("acl.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/admin-only")
        .header("Authorization", acl_basic_auth("admin", "admin123"))
        .header("x-auth-consumer-groups", "nobody")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
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

// -----------------------------------------------------------------------
// CEL routing mode tests (on_match)
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_cel_routing_match_continues() {
    // on_match + true expression → sets context key and continues (no 403)
    let gateway = TestGateway::from_spec(&fixture("cel.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway.get("/cel-routing-match").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["area"], "routing-match");
}

#[tokio::test]
async fn test_cel_routing_no_match_still_continues() {
    // on_match + false expression → no 403; request passes through unchanged
    let gateway = TestGateway::from_spec(&fixture("cel.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway.get("/cel-routing-no-match").await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "routing mode must not return 403 on false"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["area"], "routing-no-match");
}

#[tokio::test]
async fn test_cel_routing_stacked_instances_both_continue() {
    // Two stacked CEL routing instances: first matches (GET), second doesn't (POST).
    // Both must continue — request should reach the mock dispatcher.
    let gateway = TestGateway::from_spec(&fixture("cel.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway.get("/cel-routing-stacked").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["area"], "routing-stacked");
}

/// A spec with one POST route behind opa-authz pointed at `opa_url`, with
/// `include_body` on.
fn create_opa_body_spec(opa_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
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
            "plugins:\n  mock:\n    path: {}\n  opa-authz:\n    path: {}\n",
            plugins_dir.join("mock/mock.wasm").display(),
            plugins_dir.join("opa-authz/opa-authz.wasm").display()
        ),
    )
    .expect("failed to write manifest file");
    let spec_path = temp_dir.path().join("opa-body.yaml");
    std::fs::write(
        &spec_path,
        format!(
            r#"openapi: "3.1.0"
info:
  title: OPA Body Test API
  version: "1.0.0"
paths:
  /orders:
    post:
      operationId: createOrder
      x-barbacane-middlewares:
        - name: opa-authz
          config:
            opa_url: "{opa_url}"
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
          status: 201
          body: '{{"created":true}}'
      responses:
        "201":
          description: Created
"#
        ),
    )
    .expect("failed to write spec file");
    (temp_dir, spec_path)
}

/// With `include_body`, the OPA input carries the request body, which needs
/// the middleware to receive it.
#[tokio::test]
async fn opa_input_includes_the_request_body_when_enabled() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let opa = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/data/authz/allow"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": true})))
        .mount(&opa)
        .await;

    let (_temp_dir, spec_path) =
        create_opa_body_spec(&format!("{}/v1/data/authz/allow", opa.uri()));
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body = r#"{"item":"book","quantity":2}"#;
    let resp = gateway.post("/orders", body).await.unwrap();
    assert_eq!(resp.status(), 201);

    let received = opa.received_requests().await.unwrap();
    assert_eq!(received.len(), 1, "one policy query");
    let query: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(query["input"]["method"], "POST", "{query}");
    assert_eq!(query["input"]["body"], body, "{query}");
}
