//! Integration tests for request validation — required fields, formats, size limits.
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
async fn test_validation_missing_required_body() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // POST without body should fail (body is required)
    let resp = gateway.post("/users", "").await.unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_validation_missing_required_field() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // POST with body missing required 'name' field
    let resp = gateway
        .post("/users", r#"{"email":"test@example.com"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_validation_valid_body() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // POST with valid body
    let resp = gateway
        .post(
            "/users",
            r#"{"name":"Test User","email":"test@example.com"}"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

#[tokio::test]
async fn test_validation_missing_required_header() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // PUT without required X-Request-ID header
    let resp = gateway
        .put("/users/123", r#"{"name":"Updated"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_validation_with_required_header() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // PUT with required X-Request-ID header
    let resp = gateway
        .put_with_headers(
            "/users/123",
            r#"{"name":"Updated"}"#,
            &[("X-Request-ID", "abc-123")],
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_validation_unsupported_content_type() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // POST with wrong content type
    let resp = gateway
        .post_with_content_type("/users", r#"name=Test"#, "text/plain")
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_validation_no_validation_needed() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // GET /health has no validation requirements
    let resp = gateway.get("/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_validation_optional_query_params() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // GET without optional query params - should work
    let resp = gateway.get("/users").await.unwrap();
    assert_eq!(resp.status(), 200);

    // GET with valid optional query params
    let resp = gateway.get("/users?page=1&limit=10").await.unwrap();
    assert_eq!(resp.status(), 200);
}

// ========================
// M2: Format Validation Tests
// ========================

#[tokio::test]
async fn test_format_validation_valid_email() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // Valid email format
    let resp = gateway
        .post("/users", r#"{"name":"Test","email":"user@example.com"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

#[tokio::test]
async fn test_format_validation_invalid_email() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // Invalid email format
    let resp = gateway
        .post("/users", r#"{"name":"Test","email":"not-an-email"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_format_validation_valid_uuid() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // Valid UUID format
    let resp = gateway
        .get("/users/550e8400-e29b-41d4-a716-446655440000")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_format_validation_invalid_uuid() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // Invalid UUID format (not a valid UUID)
    let resp = gateway.get("/users/not-a-uuid").await.unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
}

// ========================
// M2: Request Limits Tests
// ========================

#[tokio::test]
async fn test_limits_body_size_within_limit() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // Small body should succeed
    let resp = gateway
        .post("/users", r#"{"name":"Test","email":"test@example.com"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

#[tokio::test]
async fn test_limits_body_size_exceeds_limit() {
    let gateway = TestGateway::from_spec(&fixture("validation.yaml"))
        .await
        .expect("failed to start gateway");

    // Create a body larger than 1MB (default limit)
    let large_body = format!(
        r#"{{"name":"Test","email":"test@example.com","data":"{}"}}"#,
        "x".repeat(1024 * 1024 + 1000)
    );

    // Server may either:
    // 1. Return 400 before the client finishes sending
    // 2. Close the connection early (connection reset, broken pipe, etc.)
    // Both are valid behaviors for rejecting oversized bodies
    match gateway.post("/users", &large_body).await {
        Ok(resp) => {
            assert_eq!(resp.status(), 400);
            let body: serde_json::Value = resp.json().await.unwrap();
            assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
        }
        Err(_) => {
            // Any connection error is acceptable when the server rejects a large body
            // early. The exact error depends on timing and OS behavior.
        }
    }
}
