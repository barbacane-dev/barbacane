//! Integration tests for the /__barbacane/specs* introspection endpoints.
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
async fn test_specs_index_response() {
    // Test the /__barbacane/specs endpoint returns correct JSON structure
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    let resp = gateway.get("/__barbacane/specs").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();

    // Check openapi section exists with correct structure
    assert!(body.get("openapi").is_some());
    assert!(body["openapi"]["specs"].is_array());
    assert_eq!(body["openapi"]["count"], 2);
    assert_eq!(body["openapi"]["merged_url"], "/__barbacane/specs/openapi");

    // Check asyncapi section exists (should be empty for these fixtures)
    assert!(body.get("asyncapi").is_some());
    assert!(body["asyncapi"]["specs"].is_array());
    assert_eq!(body["asyncapi"]["count"], 0);
}

#[tokio::test]
async fn test_specs_merged_openapi() {
    // Test merged OpenAPI from multiple specs
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    let resp = gateway.get("/__barbacane/specs/openapi").await.unwrap();
    assert_eq!(resp.status(), 200);

    // Check content-type is YAML by default
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("yaml"),
        "expected yaml content-type, got: {}",
        content_type
    );

    // Parse the YAML response
    let body = resp.text().await.unwrap();
    let spec: serde_json::Value = serde_yaml::from_str(&body).expect("failed to parse merged spec");

    // Check it's a valid OpenAPI spec
    assert!(
        spec.get("openapi").is_some(),
        "merged spec should have openapi field"
    );

    // Check paths from both specs are merged
    let paths = spec.get("paths").and_then(|p| p.as_object());
    assert!(paths.is_some(), "merged spec should have paths");
    let paths = paths.unwrap();

    // Check paths from users.yaml
    assert!(paths.contains_key("/users"), "should contain /users path");
    assert!(
        paths.contains_key("/users/{userId}"),
        "should contain /users/{{userId}} path"
    );

    // Check paths from orders.yaml
    assert!(paths.contains_key("/orders"), "should contain /orders path");
    assert!(
        paths.contains_key("/orders/{orderId}"),
        "should contain /orders/{{orderId}} path"
    );
}

#[tokio::test]
async fn test_specs_merged_openapi_strips_extensions() {
    // Test that merged specs strip x-barbacane-* extensions
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    let resp = gateway.get("/__barbacane/specs/openapi").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let spec: serde_json::Value = serde_yaml::from_str(&body).expect("failed to parse merged spec");

    // Check that x-barbacane-dispatch is NOT in the merged spec
    let spec_str = serde_json::to_string(&spec).unwrap();
    assert!(
        !spec_str.contains("x-barbacane-"),
        "merged spec should not contain x-barbacane-* extensions"
    );
}

#[tokio::test]
async fn test_specs_individual_file() {
    // Test individual spec endpoint
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    let resp = gateway.get("/__barbacane/specs/users.yaml").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let spec: serde_json::Value = serde_yaml::from_str(&body).expect("failed to parse spec");

    // Check it's the users spec
    assert_eq!(
        spec.pointer("/info/title"),
        Some(&serde_json::json!("Users Service API"))
    );

    // Check paths are present
    assert!(
        spec.pointer("/paths/~1users").is_some(),
        "should have /users path"
    );
}

#[tokio::test]
async fn test_specs_individual_strips_extensions() {
    // Test that individual specs strip x-barbacane-* extensions
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    let resp = gateway.get("/__barbacane/specs/users.yaml").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();

    // x-barbacane-dispatch should be stripped
    assert!(
        !body.contains("x-barbacane-"),
        "individual spec should not contain x-barbacane-* extensions"
    );
}

#[tokio::test]
async fn test_specs_preserves_sunset_extension() {
    // Test that x-sunset (RFC 8594) is preserved
    let gateway = TestGateway::from_spec(&fixture("deprecated.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .get("/__barbacane/specs/deprecated.yaml")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let spec: serde_json::Value = serde_yaml::from_str(&body).expect("failed to parse spec");

    // x-sunset should be preserved (it's a standard extension per RFC 8594)
    let spec_str = serde_json::to_string(&spec).unwrap();
    assert!(
        spec_str.contains("x-sunset"),
        "x-sunset extension should be preserved"
    );

    // But x-barbacane-* should be stripped
    assert!(
        !spec_str.contains("x-barbacane-"),
        "x-barbacane-* extensions should be stripped"
    );
}

#[tokio::test]
async fn test_specs_format_json() {
    // Test ?format=json query parameter
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    let resp = gateway
        .get("/__barbacane/specs/openapi?format=json")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Check content-type is JSON
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("json"),
        "expected json content-type, got: {}",
        content_type
    );

    // Parse as JSON (not YAML)
    let body: serde_json::Value = resp.json().await.expect("should parse as JSON");
    assert!(body.get("openapi").is_some());
}

#[tokio::test]
async fn test_specs_format_yaml_explicit() {
    // Test ?format=yaml query parameter (explicit)
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    let resp = gateway
        .get("/__barbacane/specs/openapi?format=yaml")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Check content-type is YAML
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("yaml"),
        "expected yaml content-type, got: {}",
        content_type
    );
}

#[tokio::test]
async fn test_specs_individual_format_json() {
    // Test format=json for individual spec file
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .get("/__barbacane/specs/minimal.yaml?format=json")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Check content-type is JSON
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("json"),
        "expected json content-type, got: {}",
        content_type
    );

    // Should parse as JSON
    let _body: serde_json::Value = resp.json().await.expect("should parse as JSON");
}

#[tokio::test]
async fn test_specs_not_found() {
    // Test 404 for non-existent spec file
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .get("/__barbacane/specs/nonexistent.yaml")
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_specs_merged_asyncapi_empty() {
    // Test merged AsyncAPI endpoint when there are no AsyncAPI specs
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    // Should return 404 when no AsyncAPI specs exist
    let resp = gateway.get("/__barbacane/specs/asyncapi").await.unwrap();
    assert_eq!(resp.status(), 404);
}
