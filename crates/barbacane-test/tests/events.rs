//! Integration tests for AsyncAPI event dispatch.
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
async fn test_asyncapi_spec_compilation() {
    // Test that an AsyncAPI spec compiles and loads successfully
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway with AsyncAPI spec");

    // Health endpoint should work
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    // Should have routes from AsyncAPI operations
    assert!(body.get("routes_count").is_some());
    let routes_count = body["routes_count"].as_u64().unwrap();
    assert!(
        routes_count >= 3,
        "expected at least 3 routes from AsyncAPI spec"
    );
}

#[tokio::test]
async fn test_asyncapi_send_operation_via_post() {
    // AsyncAPI SEND operations should be accessible via HTTP POST
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    // POST to a SEND operation channel address
    let resp = gateway
        .post(
            "/events/users",
            r#"{"userId":"550e8400-e29b-41d4-a716-446655440000","email":"test@example.com"}"#,
        )
        .await
        .unwrap();

    // Should get 202 Accepted (mock dispatcher returns configured response)
    assert_eq!(resp.status(), 202);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");
}

#[tokio::test]
async fn test_asyncapi_send_with_path_param() {
    // AsyncAPI channels with parameters should work with path params
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    // POST to channel with orderId path parameter
    let resp = gateway
        .post(
            "/events/orders/550e8400-e29b-41d4-a716-446655440000",
            r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000","items":[{"productId":"p1","quantity":2}]}"#,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 202);
}

#[tokio::test]
async fn test_asyncapi_message_validation_required_field() {
    // AsyncAPI message payloads should be validated against the schema
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    // Missing required 'email' field
    let resp = gateway
        .post(
            "/events/users",
            r#"{"userId":"550e8400-e29b-41d4-a716-446655440000"}"#,
        )
        .await
        .unwrap();

    // Should fail validation
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_asyncapi_message_validation_invalid_format() {
    // AsyncAPI message payloads should validate format constraints
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    // Invalid email format
    let resp = gateway
        .post(
            "/events/users",
            r#"{"userId":"550e8400-e29b-41d4-a716-446655440000","email":"not-an-email"}"#,
        )
        .await
        .unwrap();

    // Should fail validation
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_asyncapi_message_validation_invalid_uuid_path_param() {
    // AsyncAPI channel parameters should be validated
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    // Invalid UUID format for orderId path parameter
    let resp = gateway
        .post(
            "/events/orders/not-a-uuid",
            r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000","items":[{"productId":"p1","quantity":2}]}"#,
        )
        .await
        .unwrap();

    // Should fail validation
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_asyncapi_404_unknown_channel() {
    // Requests to unknown channels should return 404
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post("/events/unknown", r#"{"data":"test"}"#)
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_asyncapi_405_wrong_method() {
    // GET on a SEND-only channel should return 405
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/events/users").await.unwrap();

    assert_eq!(resp.status(), 405);

    // Check Allow header indicates POST
    let allow = resp.headers().get("allow").unwrap().to_str().unwrap();
    assert!(allow.contains("POST"));
}

#[tokio::test]
async fn test_asyncapi_simple_notification() {
    // Test a simple channel without path parameters
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    // Notification only requires 'title'
    let resp = gateway
        .post("/notifications", r#"{"title":"Hello World"}"#)
        .await
        .unwrap();

    assert_eq!(resp.status(), 202);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");
}

#[tokio::test]
async fn test_asyncapi_notification_missing_required() {
    // Test validation failure for missing required field
    let gateway = TestGateway::from_spec(&fixture("asyncapi-events.yaml"))
        .await
        .expect("failed to start gateway");

    // Missing required 'title' field
    let resp = gateway
        .post("/notifications", r#"{"body":"Some body text"}"#)
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}
