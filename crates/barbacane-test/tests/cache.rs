//! Integration tests for the response cache middleware plugin.
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
async fn test_cache_miss_then_hit() {
    let gateway = TestGateway::from_spec(&fixture("cache.yaml"))
        .await
        .expect("failed to start gateway");

    // First request should be a cache miss
    let resp1 = gateway.get("/cached").await.unwrap();
    assert_eq!(resp1.status(), 200);
    let x_cache1 = resp1
        .headers()
        .get("x-cache")
        .map(|v| v.to_str().unwrap().to_string());
    let body1: serde_json::Value = resp1.json().await.unwrap();
    assert_eq!(body1["message"], "cached response");
    assert_eq!(x_cache1, Some("MISS".to_string()));

    // Second request should be a cache hit
    let resp2 = gateway.get("/cached").await.unwrap();
    assert_eq!(resp2.status(), 200);
    let x_cache2 = resp2
        .headers()
        .get("x-cache")
        .map(|v| v.to_str().unwrap().to_string());
    assert_eq!(x_cache2, Some("HIT".to_string()));
}

#[tokio::test]
async fn test_cache_vary_header() {
    let gateway = TestGateway::from_spec(&fixture("cache.yaml"))
        .await
        .expect("failed to start gateway");

    // Request with Accept-Language: en
    let resp1 = gateway
        .request_builder(reqwest::Method::GET, "/cached-with-vary")
        .header("accept-language", "en")
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 200);
    let x_cache1 = resp1
        .headers()
        .get("x-cache")
        .map(|v| v.to_str().unwrap().to_string());
    assert_eq!(x_cache1, Some("MISS".to_string()));

    // Same request should hit cache
    let resp2 = gateway
        .request_builder(reqwest::Method::GET, "/cached-with-vary")
        .header("accept-language", "en")
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);
    let x_cache2 = resp2
        .headers()
        .get("x-cache")
        .map(|v| v.to_str().unwrap().to_string());
    assert_eq!(x_cache2, Some("HIT".to_string()));

    // Different Accept-Language should miss cache
    let resp3 = gateway
        .request_builder(reqwest::Method::GET, "/cached-with-vary")
        .header("accept-language", "fr")
        .send()
        .await
        .unwrap();
    assert_eq!(resp3.status(), 200);
    let x_cache3 = resp3
        .headers()
        .get("x-cache")
        .map(|v| v.to_str().unwrap().to_string());
    assert_eq!(x_cache3, Some("MISS".to_string()));
}

#[tokio::test]
async fn test_cache_post_not_cached() {
    let gateway = TestGateway::from_spec(&fixture("cache.yaml"))
        .await
        .expect("failed to start gateway");

    // POST requests should not be cached by default
    let resp1 = gateway.post("/post-not-cached", "{}").await.unwrap();
    assert_eq!(resp1.status(), 200);
    // POST shouldn't even have x-cache header since it's not cacheable
    let has_x_cache = resp1.headers().contains_key("x-cache");
    resp1.text().await.unwrap(); // consume body

    // Second POST should also not have cache header
    let resp2 = gateway.post("/post-not-cached", "{}").await.unwrap();
    assert_eq!(resp2.status(), 200);
    let has_x_cache2 = resp2.headers().contains_key("x-cache");

    // Neither should have x-cache since POSTs are not cached
    assert!(!has_x_cache, "POST should not be cached");
    assert!(!has_x_cache2, "POST should not be cached");
}

#[tokio::test]
async fn test_uncached_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("cache.yaml"))
        .await
        .expect("failed to start gateway");

    // Endpoint without cache middleware
    let resp = gateway.get("/uncached").await.unwrap();
    assert_eq!(resp.status(), 200);
    // Should not have x-cache header
    assert!(!resp.headers().contains_key("x-cache"));

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "not cached");
}
