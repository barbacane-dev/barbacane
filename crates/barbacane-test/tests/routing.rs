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
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["route"], "wildcard");
    assert_eq!(body["path"], "readme.txt");
}

#[tokio::test]
async fn test_wildcard_path_multi_segment() {
    let gateway = TestGateway::from_spec(&fixture("wildcard-path.yaml"))
        .await
        .expect("failed to start gateway");

    // Multi-segment path captured as a single value
    let resp = gateway.get("/files/docs/2024/report.pdf").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["route"], "wildcard");
    assert_eq!(body["path"], "docs/2024/report.pdf");
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
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["route"], "storage");
    assert_eq!(body["bucket"], "my-bucket");
    assert_eq!(body["key"], "folder/sub/file.txt");
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

/// A resource that answers GET answers HEAD the same way, with no body.
#[tokio::test]
async fn head_is_answered_by_the_get_route_without_a_body() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway
        .request_builder(reqwest::Method::HEAD, "/users/42")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().len(), 0);
}

/// A parameter may share its segment with literal text: `/reports/{id}.pdf`
/// matches `/reports/42.pdf` with `id` 42, and its value is validated.
#[tokio::test]
async fn test_parameter_with_literal_text_in_its_segment() {
    let gateway = TestGateway::from_spec(&fixture("path-patterns.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/reports/42.pdf").await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), r#"{"route":"pdf","id":"42"}"#);

    let resp = gateway.get("/reports/42").await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.text().await.unwrap(),
        r#"{"route":"report","id":"42"}"#
    );

    // `id` is an integer in the pdf route; `x` is not.
    let resp = gateway.get("/reports/x.pdf").await.unwrap();
    assert_eq!(resp.status(), 400);
}

/// A path parameter name may hold `-` and `.`; the router captures it and the
/// validator checks it under that name.
#[tokio::test]
async fn test_parameter_names_with_hyphen_and_dot() {
    let gateway = TestGateway::from_spec(&fixture("path-patterns.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/members/7/items/a1").await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), r#"{"member":"7","item":"a1"}"#);

    // `member-id` is an integer.
    let resp = gateway.get("/members/x/items/a1").await.unwrap();
    assert_eq!(resp.status(), 400);
}

/// An operation is served under the path of its servers' URLs: the document's
/// `https://api.example.com/v1` puts `/users` at `/v1/users`, and an
/// operation's own root server keeps `/status` at `/status`.
#[tokio::test]
async fn test_routes_are_served_under_the_server_base_path() {
    let gateway = TestGateway::from_spec(&fixture("server-base-path.yaml"))
        .await
        .expect("failed to start gateway");

    let mut wrong = Vec::new();
    for (path, expected) in [
        ("/v1/users", 200),
        ("/users", 404),
        ("/status", 200),
        ("/v1/status", 404),
    ] {
        let status = gateway.get(path).await.unwrap().status().as_u16();
        if status != expected {
            wrong.push(format!("{path}: {status}, expected {expected}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
