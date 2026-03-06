//! Integration tests for API lifecycle — deprecation headers, full CRUD, multi-spec compilation.
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

// ==================== API Lifecycle / Deprecation Tests ====================

#[tokio::test]
async fn test_deprecation_header_not_present_on_current_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("deprecated.yaml"))
        .await
        .expect("failed to start gateway");

    // Current (non-deprecated) endpoint should not have deprecation headers
    let resp = gateway.get("/v2/users").await.unwrap();
    assert_eq!(resp.status(), 200);

    assert!(
        !resp.headers().contains_key("deprecation"),
        "Current endpoint should not have Deprecation header"
    );
    assert!(
        !resp.headers().contains_key("sunset"),
        "Current endpoint should not have Sunset header"
    );
}

#[tokio::test]
async fn test_deprecation_header_present_on_deprecated_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("deprecated.yaml"))
        .await
        .expect("failed to start gateway");

    // Deprecated endpoint (without sunset) should have Deprecation header only
    let resp = gateway.get("/v1/users").await.unwrap();
    assert_eq!(resp.status(), 200);

    let deprecation = resp.headers().get("deprecation");
    assert!(
        deprecation.is_some(),
        "Deprecated endpoint should have Deprecation header"
    );
    assert_eq!(
        deprecation.unwrap().to_str().unwrap(),
        "true",
        "Deprecation header should be 'true'"
    );

    // This endpoint has no sunset date configured
    assert!(
        !resp.headers().contains_key("sunset"),
        "Endpoint without x-sunset should not have Sunset header"
    );
}

#[tokio::test]
async fn test_deprecation_and_sunset_headers_present() {
    let gateway = TestGateway::from_spec(&fixture("deprecated.yaml"))
        .await
        .expect("failed to start gateway");

    // Deprecated endpoint with sunset date should have both headers
    let resp = gateway.get("/v1/users/123").await.unwrap();
    assert_eq!(resp.status(), 200);

    let deprecation = resp.headers().get("deprecation");
    assert!(
        deprecation.is_some(),
        "Deprecated endpoint should have Deprecation header"
    );
    assert_eq!(deprecation.unwrap().to_str().unwrap(), "true");

    let sunset = resp.headers().get("sunset");
    assert!(
        sunset.is_some(),
        "Endpoint with x-sunset should have Sunset header"
    );
    let sunset_val = sunset.unwrap().to_str().unwrap();
    assert!(
        sunset_val.contains("2025"),
        "Sunset header should contain the configured date: {}",
        sunset_val
    );
}

#[tokio::test]
async fn test_legacy_endpoint_with_far_future_sunset() {
    let gateway = TestGateway::from_spec(&fixture("deprecated.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/legacy/status").await.unwrap();
    assert_eq!(resp.status(), 200);

    // Check both headers
    assert!(resp.headers().contains_key("deprecation"));
    let sunset = resp.headers().get("sunset");
    assert!(sunset.is_some());
    let sunset_val = sunset.unwrap().to_str().unwrap();
    assert!(
        sunset_val.contains("2030"),
        "Sunset header should contain 2030: {}",
        sunset_val
    );
}

// ==================== Full CRUD Tests ====================

#[tokio::test]
async fn test_full_crud_list_users() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/users").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("users").is_some());
    assert!(body.get("total").is_some());
}

#[tokio::test]
async fn test_full_crud_list_users_with_pagination() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    // With valid pagination params
    let resp = gateway.get("/users?limit=10&offset=0").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_full_crud_list_users_invalid_limit() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    // Limit exceeds maximum (100)
    let resp = gateway.get("/users?limit=200").await.unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_full_crud_create_user_valid() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post(
            "/users",
            r#"{"email":"test@example.com","name":"Test User"}"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("id").is_some());
}

#[tokio::test]
async fn test_full_crud_create_user_missing_required() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    // Missing required 'name' field
    let resp = gateway
        .post("/users", r#"{"email":"test@example.com"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_full_crud_create_user_invalid_email() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    // Invalid email format
    let resp = gateway
        .post("/users", r#"{"email":"not-an-email","name":"Test"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_full_crud_get_user() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    // Valid UUID
    let resp = gateway
        .get("/users/550e8400-e29b-41d4-a716-446655440000")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_full_crud_get_user_invalid_uuid() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    // Invalid UUID format
    let resp = gateway.get("/users/not-a-uuid").await.unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_full_crud_update_user() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .put(
            "/users/550e8400-e29b-41d4-a716-446655440000",
            r#"{"name":"Updated Name"}"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_full_crud_delete_user() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request(
            reqwest::Method::DELETE,
            "/users/550e8400-e29b-41d4-a716-446655440000",
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn test_full_crud_nested_resource() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    // Get orders for a user
    let resp = gateway
        .get("/users/550e8400-e29b-41d4-a716-446655440000/orders")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("orders").is_some());
}

#[tokio::test]
async fn test_full_crud_nested_resource_with_filter() {
    let gateway = TestGateway::from_spec(&fixture("full-crud.yaml"))
        .await
        .expect("failed to start gateway");

    // Get orders with status filter
    let resp = gateway
        .get("/users/550e8400-e29b-41d4-a716-446655440000/orders?status=pending")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ==================== Multi-Spec Compilation Tests ====================

#[tokio::test]
async fn test_multi_spec_routes_from_both_specs() {
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway with multiple specs");

    // Routes from users.yaml
    let resp = gateway.get("/users").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("users").is_some());

    // Routes from orders.yaml
    let resp = gateway.get("/orders").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("orders").is_some());
}

#[tokio::test]
async fn test_multi_spec_user_crud() {
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    // Create user
    let resp = gateway
        .post("/users", r#"{"name":"Test","email":"test@example.com"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Get user
    let resp = gateway.get("/users/123").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_multi_spec_order_crud() {
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    // Create order
    let resp = gateway
        .post(
            "/orders",
            r#"{"userId":"123","items":[{"productId":"p1","quantity":2}]}"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Get order
    let resp = gateway.get("/orders/order-1").await.unwrap();
    assert_eq!(resp.status(), 200);

    // Update order status
    let resp = gateway
        .put("/orders/order-1/status", r#"{"status":"shipped"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_multi_spec_health_from_both() {
    let gateway = TestGateway::from_specs(&[
        &fixture("multi-spec/users.yaml"),
        &fixture("multi-spec/orders.yaml"),
    ])
    .await
    .expect("failed to start gateway");

    // Built-in health endpoint should work
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    // Should show combined routes count
    assert!(body.get("routes_count").is_some());
}
