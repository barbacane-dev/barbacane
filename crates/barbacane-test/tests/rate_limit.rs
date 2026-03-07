//! Integration tests for the rate-limit middleware plugin.
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
async fn test_rate_limit_allows_within_quota() {
    let gateway = TestGateway::from_spec(&fixture("rate-limit.yaml"))
        .await
        .expect("failed to start gateway");

    // First request should be allowed
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/limited")
        .header("x-client-id", "test-client-1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Check rate limit headers in response (added to request, passed through)
    // The mock dispatcher returns our configured response
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "ok");
}

#[tokio::test]
async fn test_rate_limit_blocks_over_quota() {
    let gateway = TestGateway::from_spec(&fixture("rate-limit.yaml"))
        .await
        .expect("failed to start gateway");

    let client_id = format!("test-client-quota-{}", std::process::id());

    // Send 3 requests (the quota)
    for i in 0..3 {
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/limited")
            .header("x-client-id", &client_id)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "request {} should be allowed", i + 1);
    }

    // 4th request should be rate limited
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/limited")
        .header("x-client-id", &client_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429, "request 4 should be rate limited");

    // Check rate limit headers before consuming the body
    let has_retry_after = resp.headers().contains_key("retry-after");
    let has_ratelimit_policy = resp.headers().contains_key("ratelimit-policy");

    // Check the response body
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:rate-limit-exceeded");
    assert_eq!(body["status"], 429);

    // Verify headers
    assert!(has_retry_after, "should have Retry-After header");
    assert!(has_ratelimit_policy, "should have RateLimit-Policy header");
}

#[tokio::test]
async fn test_rate_limit_different_clients_separate_quotas() {
    let gateway = TestGateway::from_spec(&fixture("rate-limit.yaml"))
        .await
        .expect("failed to start gateway");

    // Client A uses 3 requests (full quota)
    let client_a = format!("client-a-{}", std::process::id());
    for _ in 0..3 {
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/limited")
            .header("x-client-id", &client_a)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // Client A is now rate limited
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/limited")
        .header("x-client-id", &client_a)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429, "client A should be rate limited");

    // Client B should still have full quota
    let client_b = format!("client-b-{}", std::process::id());
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/limited")
        .header("x-client-id", &client_b)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "client B should not be rate limited");
}

#[tokio::test]
async fn test_rate_limit_unlimited_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("rate-limit.yaml"))
        .await
        .expect("failed to start gateway");

    // Unlimited endpoint should always work
    for _ in 0..10 {
        let resp = gateway.get("/unlimited").await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}
