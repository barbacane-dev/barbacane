//! `--max-headers` and `--max-header-size` are the limits a request meets on
//! HTTP/1.1 and HTTP/2 alike, and a request over either is answered 431.

use barbacane_test::TestGateway;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

fn fixture(name: &str) -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

/// `count` headers named `x-h-<n>`.
fn many_headers(count: usize) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for n in 0..count {
        headers.insert(
            HeaderName::try_from(format!("x-h-{n}")).unwrap(),
            HeaderValue::from_static("v"),
        );
    }
    headers
}

/// One header `x-big` of `size` bytes.
fn big_header(size: usize) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-big", HeaderValue::try_from("a".repeat(size)).unwrap());
    headers
}

fn http1() -> reqwest::Client {
    reqwest::Client::builder().http1_only().build().unwrap()
}

/// HTTP/2 over cleartext, without the upgrade dance (prior knowledge).
fn http2() -> reqwest::Client {
    reqwest::Client::builder()
        .http2_prior_knowledge()
        .build()
        .unwrap()
}

/// The status and problem type the gateway answers `headers` with.
async fn send(
    client: &reqwest::Client,
    gateway: &TestGateway,
    headers: HeaderMap,
) -> (u16, Option<String>) {
    let resp = client
        .get(format!("{}/health", gateway.base_url()))
        .headers(headers)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let problem_type = resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|body| body["type"].as_str().map(str::to_string));
    (status, problem_type)
}

#[tokio::test]
async fn more_headers_than_the_limit_answer_431() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    // HTTP/1.1: hyper refuses the request while parsing it, without a body.
    let (status, _) = send(&http1(), &gateway, many_headers(150)).await;
    assert_eq!(status, 431);

    // HTTP/2: the gateway counts them.
    let (status, problem) = send(&http2(), &gateway, many_headers(150)).await;
    assert_eq!(status, 431);
    assert_eq!(
        problem.as_deref(),
        Some("urn:barbacane:error:header-fields-too-large")
    );
}

#[tokio::test]
async fn max_headers_above_100_admits_more_headers() {
    let gateway =
        TestGateway::from_spec_with_args(&fixture("minimal.yaml"), &["--max-headers", "200"])
            .await
            .expect("failed to start gateway");

    for (name, client) in [("HTTP/1.1", http1()), ("HTTP/2", http2())] {
        let (status, _) = send(&client, &gateway, many_headers(150)).await;
        assert_eq!(status, 200, "{name}");
        let (status, _) = send(&client, &gateway, many_headers(250)).await;
        assert_eq!(status, 431, "{name}");
    }
}

#[tokio::test]
async fn a_header_larger_than_the_limit_answers_431() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    for (name, client) in [("HTTP/1.1", http1()), ("HTTP/2", http2())] {
        let (status, problem) = send(&client, &gateway, big_header(9000)).await;
        assert_eq!(status, 431, "{name}");
        assert_eq!(
            problem.as_deref(),
            Some("urn:barbacane:error:header-fields-too-large"),
            "{name}"
        );
    }
}

/// HTTP/2's header list makes room for the largest header `--max-header-size`
/// admits.
#[tokio::test]
async fn max_header_size_applies_to_http2() {
    let gateway =
        TestGateway::from_spec_with_args(&fixture("minimal.yaml"), &["--max-header-size", "32768"])
            .await
            .expect("failed to start gateway");

    let clients = [("HTTP/1.1", http1()), ("HTTP/2", http2())];
    for (name, client) in &clients {
        let (status, _) = send(client, &gateway, big_header(24 * 1024)).await;
        assert_eq!(status, 200, "{name}");
    }
    for (name, client) in &clients {
        let (status, problem) = send(client, &gateway, big_header(40 * 1024)).await;
        assert_eq!(status, 431, "{name}");
        assert_eq!(
            problem.as_deref(),
            Some("urn:barbacane:error:header-fields-too-large"),
            "{name}"
        );
    }
}
