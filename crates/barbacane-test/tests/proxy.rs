//! Integration tests for HTTP upstream proxy and TLS termination.
//!
//! Run with: `cargo test -p barbacane-test`

use std::path::PathBuf;

use barbacane_test::TestGateway;

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn plugin_wasm(name: &str) -> PathBuf {
    workspace_root().join(format!("plugins/{name}/{name}.wasm"))
}

// ========================
// M4: HTTP Upstream Tests
// ========================

/// Build a temp spec whose http-upstream routes proxy to a local wiremock,
/// keeping these tests hermetic. (They previously hit the real httpbin.org,
/// which is non-deterministic in CI and, when slow, trips the plugin's
/// wall-clock deadline and surfaces as a 500.)
fn http_upstream_spec(upstream_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let spec = format!(
        r#"openapi: "3.1.0"
info:
  title: HTTP Upstream Test API
  version: "1.0.0"
paths:
  /proxy/get:
    get:
      operationId: proxyGet
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{url}"
          path: "/get"
          timeout: 10.0
      responses:
        "200":
          description: OK
  /proxy/post:
    post:
      operationId: proxyPost
      requestBody:
        required: false
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{url}"
          path: "/post"
          timeout: 10.0
      responses:
        "200":
          description: OK
  /proxy/headers:
    get:
      operationId: proxyHeaders
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{url}"
          path: "/headers"
          timeout: 10.0
      responses:
        "200":
          description: OK
"#,
        url = upstream_url
    );
    std::fs::write(temp.path().join("http-upstream.yaml"), spec).expect("write spec");
    std::fs::write(
        temp.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  http-upstream:\n    path: {}\n",
            plugin_wasm("http-upstream").display()
        ),
    )
    .expect("write manifest");
    let path = temp.path().join("http-upstream.yaml");
    (temp, path)
}

#[tokio::test]
async fn test_http_upstream_get() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "url": format!("{}/get", upstream.uri()),
        })))
        .mount(&upstream)
        .await;

    let (_tmp, spec) = http_upstream_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/proxy/get").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("url").is_some(),
        "response should contain url field"
    );
}

/// Header fields the upstream repeats reach the client as separate fields,
/// each with its value: two cookies stay two cookies.
#[tokio::test]
async fn test_http_upstream_keeps_repeated_response_headers() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/get"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("set-cookie", "a=1; Path=/")
                .append_header("set-cookie", "b=2; Path=/")
                .append_header("link", "</next>; rel=next")
                .set_body_string("ok"),
        )
        .mount(&upstream)
        .await;

    let (_tmp, spec) = http_upstream_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/proxy/get").await.unwrap();
    assert_eq!(resp.status(), 200);
    let cookies: Vec<&str> = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    assert_eq!(cookies, ["a=1; Path=/", "b=2; Path=/"]);
    assert_eq!(resp.headers().get("link").unwrap(), "</next>; rel=next");
}

#[tokio::test]
async fn test_http_upstream_post() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    // Echo the posted JSON back under `json`, like httpbin's /post.
    Mock::given(method("POST"))
        .and(path("/post"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "json": {"test": "data"},
        })))
        .mount(&upstream)
        .await;

    let (_tmp, spec) = http_upstream_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post("/proxy/post", r#"{"test":"data"}"#)
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200, got {status}. Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert!(
        body.get("json").is_some(),
        "response should contain json field"
    );
    assert_eq!(body["json"]["test"], "data");
}

/// An HTTP/1.1 upstream that keeps connections open and answers `200 ok` to
/// every request, `delay` after reading it. Returns its base URL and the
/// number of connections it has accepted.
async fn start_keep_alive_upstream(
    delay: std::time::Duration,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut pending = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    // Answer each request whose head has arrived; GETs carry no body.
                    while let Some(end) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                        pending.drain(..end + 4);
                        tokio::time::sleep(delay).await;
                        let answer = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
                        if socket.write_all(answer).await.is_err() {
                            return;
                        }
                    }
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => pending.extend_from_slice(&chunk[..n]),
                    }
                }
            });
        }
    });
    (url, accepted)
}

/// A client that leaves before a slow upstream answers holds its plugin
/// instance until the upstream answers, no longer: then no instance is
/// alive, and the next request finds its slot in the pool.
#[tokio::test]
async fn test_a_client_leaving_a_slow_request_gives_its_instance_back() {
    use std::time::Duration;

    let (upstream, _) = start_keep_alive_upstream(Duration::from_secs(2)).await;
    let (_tmp, spec) = http_upstream_spec(&upstream);
    let gateway =
        TestGateway::from_spec_with_args(spec.to_str().unwrap(), &["--plugin-instance-pool", "1"])
            .await
            .expect("failed to start gateway");

    let left = tokio::time::timeout(Duration::from_millis(300), gateway.get("/proxy/get")).await;
    assert!(
        left.is_err(),
        "the client left before the upstream answered"
    );
    assert_eq!(
        gateway.metric("barbacane_wasm_instances").await.unwrap(),
        Some(1.0),
        "the dispatcher waits for the upstream"
    );

    let alive = gateway
        .wait_for_metric("barbacane_wasm_instances", 0.0, Duration::from_secs(6))
        .await;
    assert_eq!(
        alive,
        Some(0.0),
        "instances alive after the upstream answered"
    );

    assert_eq!(gateway.get("/proxy/get").await.unwrap().status(), 200);
    assert_eq!(
        gateway
            .metric("barbacane_wasm_instances_unpooled_total")
            .await
            .unwrap(),
        Some(0.0),
        "every instance came from the pool"
    );
}

/// Sequential proxied requests reuse one upstream connection: the connection
/// outlives the plugin call that opened it.
#[tokio::test]
async fn test_http_upstream_reuses_its_connection() {
    let (upstream, accepted) = start_keep_alive_upstream(std::time::Duration::ZERO).await;
    let (_tmp, spec) = http_upstream_spec(&upstream);
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    for _ in 0..10 {
        let resp = gateway.get("/proxy/get").await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok");
    }
    assert_eq!(
        accepted.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "ten sequential requests should share one upstream connection"
    );
}

/// The connections a burst of concurrent requests opens stay open for the
/// next burst: the client keeps more idle connections than the burst needs.
#[tokio::test]
async fn test_http_upstream_keeps_a_burst_of_connections_for_the_next() {
    use std::sync::atomic::Ordering;

    const BURST: usize = 32;
    // Answers after 300 ms, so the burst's requests are in flight together.
    let (upstream, accepted) =
        start_keep_alive_upstream(std::time::Duration::from_millis(300)).await;
    let (_tmp, spec) = http_upstream_spec(&upstream);
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");
    let burst = || async {
        let responses =
            futures_util::future::join_all((0..BURST).map(|_| gateway.get("/proxy/get"))).await;
        for response in responses {
            assert_eq!(response.unwrap().status(), 200);
        }
    };

    burst().await;
    let opened = accepted.load(Ordering::SeqCst);
    assert!(
        opened > 10,
        "the first burst opened {opened} connections; the test needs more than 10 in flight"
    );
    burst().await;
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        opened,
        "the second burst opened new connections"
    );
}

#[tokio::test]
async fn test_http_upstream_headers_forwarded() {
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    // Only match when the gateway actually forwarded X-Forwarded-Host; if it
    // didn't, wiremock returns no match and the request 404s, failing the test.
    Mock::given(method("GET"))
        .and(path("/headers"))
        .and(header_exists("x-forwarded-host"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "headers": {"X-Forwarded-Host": "gateway"},
        })))
        .mount(&upstream)
        .await;

    let (_tmp, spec) = http_upstream_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/proxy/headers").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let headers = &body["headers"];
    assert!(
        headers.get("X-Forwarded-Host").is_some() || headers.get("x-forwarded-host").is_some(),
        "should forward X-Forwarded-Host header"
    );
}

/// The forwarding headers the upstream receives name the client the gateway
/// resolved: the peer, or behind a trusted proxy the address it reported. A
/// hop the client wrote never reaches the upstream.
#[tokio::test]
async fn test_the_upstream_learns_the_client_from_the_gateway() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// Answer with the forwarding headers the upstream received.
    struct EchoForwarding;
    impl Respond for EchoForwarding {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let get = |name: &str| {
                let values: Vec<&str> = req
                    .headers
                    .get_all(name)
                    .iter()
                    .filter_map(|v| v.to_str().ok())
                    .collect();
                values.join(" | ")
            };
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "x-forwarded-for": get("x-forwarded-for"),
                "x-real-ip": get("x-real-ip"),
                "forwarded": get("forwarded"),
            }))
        }
    }

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/headers"))
        .respond_with(EchoForwarding)
        .mount(&upstream)
        .await;
    let (_tmp, spec) = http_upstream_spec(&upstream.uri());

    async fn received(gateway: &TestGateway) -> serde_json::Value {
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/proxy/headers")
            .header("X-Forwarded-For", "6.6.6.6, 203.0.113.9")
            .header("X-Real-IP", "6.6.6.6")
            .header("Forwarded", "for=6.6.6.6")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        resp.json().await.unwrap()
    }

    let direct = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");
    assert_eq!(
        received(&direct).await,
        serde_json::json!({
            "x-forwarded-for": "127.0.0.1",
            "x-real-ip": "127.0.0.1",
            "forwarded": "",
        })
    );

    let proxied = TestGateway::from_spec_with_args(
        spec.to_str().unwrap(),
        &["--trusted-proxies", "127.0.0.1"],
    )
    .await
    .expect("failed to start gateway");
    assert_eq!(
        received(&proxied).await,
        serde_json::json!({
            "x-forwarded-for": "203.0.113.9",
            "x-real-ip": "203.0.113.9",
            "forwarded": "",
        })
    );
}

/// A spec whose `http-upstream` route (`GET /proxy/headers`) and
/// `fire-and-forget` route (`GET /notify`) both call `upstream_url`.
fn forwarding_spec(upstream_url: &str) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let spec = format!(
        r#"openapi: "3.1.0"
info:
  title: Forwarding Test API
  version: "1.0.0"
paths:
  /proxy/headers:
    get:
      operationId: proxyHeaders
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{url}"
          path: "/headers"
          timeout: 10.0
      responses:
        "200":
          description: OK
  /notify:
    get:
      operationId: notify
      x-barbacane-dispatch:
        name: fire-and-forget
        config:
          url: "{url}/notify"
      responses:
        "202":
          description: Accepted
"#,
        url = upstream_url
    );
    std::fs::write(temp.path().join("forwarding.yaml"), spec).expect("write spec");
    std::fs::write(
        temp.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  http-upstream:\n    path: {}\n  fire-and-forget:\n    path: {}\n",
            plugin_wasm("http-upstream").display(),
            plugin_wasm("fire-and-forget").display()
        ),
    )
    .expect("write manifest");
    let path = temp.path().join("forwarding.yaml");
    (temp, path)
}

/// `X-Forwarded-Proto` and `X-Forwarded-Host` name the scheme and host the
/// client used: the listener's and the request's, or behind a trusted proxy
/// the ones it reported. What the client wrote in them never reaches the
/// upstream.
#[tokio::test]
async fn the_upstream_learns_the_scheme_and_host_from_the_gateway() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    struct EchoOrigin;
    impl Respond for EchoOrigin {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let get = |name: &str| {
                req.headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string()
            };
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "proto": get("x-forwarded-proto"),
                "host": get("x-forwarded-host"),
            }))
        }
    }

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/headers"))
        .respond_with(EchoOrigin)
        .mount(&upstream)
        .await;
    let (_tmp, spec) = forwarding_spec(&upstream.uri());

    async fn received(gateway: &TestGateway) -> serde_json::Value {
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/proxy/headers")
            .header("X-Forwarded-Proto", "https")
            .header("X-Forwarded-Host", "public.example")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        resp.json().await.unwrap()
    }

    let direct = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");
    assert_eq!(
        received(&direct).await,
        serde_json::json!({"proto": "http", "host": format!("127.0.0.1:{}", direct.port())})
    );

    let tls = TestGateway::from_spec_with_tls(spec.to_str().unwrap())
        .await
        .expect("failed to start TLS gateway");
    assert_eq!(
        received(&tls).await,
        serde_json::json!({"proto": "https", "host": format!("127.0.0.1:{}", tls.port())})
    );

    let proxied = TestGateway::from_spec_with_args(
        spec.to_str().unwrap(),
        &["--trusted-proxies", "127.0.0.1"],
    )
    .await
    .expect("failed to start gateway");
    assert_eq!(
        received(&proxied).await,
        serde_json::json!({"proto": "https", "host": "public.example"})
    );
}

/// A dispatcher that passes the request's headers on sends the scheme and host
/// the gateway resolved, not the client's own `X-Forwarded-Proto` and
/// `X-Forwarded-Host`.
#[tokio::test]
async fn a_passthrough_dispatcher_does_not_forward_the_clients_scheme() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/notify"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&upstream)
        .await;
    let (_tmp, spec) = forwarding_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/notify")
        .header("X-Forwarded-Proto", "https")
        .header("X-Forwarded-Host", "public.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    let sent = upstream.received_requests().await.expect("recording");
    let notify = sent
        .iter()
        .find(|r| r.url.path() == "/notify")
        .expect("fire-and-forget called the upstream before answering");
    let get = |name: &str| {
        notify
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    assert_eq!(get("x-forwarded-proto").as_deref(), Some("http"));
    assert_eq!(
        get("x-forwarded-host"),
        Some(format!("127.0.0.1:{}", gateway.port()))
    );
}

/// An upstream that accepts each connection, reads the request and closes the
/// connection without answering. Returns its base URL.
async fn start_hang_up_upstream() -> String {
    use tokio::io::AsyncReadExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
        }
    });
    url
}

/// An upstream that hangs up answers `502`, and the response says nothing of
/// the upstream's address or of the client error: the cause goes to the
/// gateway's log.
#[tokio::test]
async fn test_http_upstream_failure_does_not_reveal_the_upstream() {
    let upstream = start_hang_up_upstream().await;
    let (_tmp, spec) = http_upstream_spec(&upstream);
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/proxy/get").await.unwrap();
    assert_eq!(resp.status(), 502);
    let body = resp.text().await.unwrap();
    let address = upstream.trim_start_matches("http://");
    assert!(
        !body.contains(address) && !body.contains("error sending request"),
        "the response reveals the upstream: {body}"
    );
    let body: serde_json::Value = serde_json::from_str(&body).expect("problem+json");
    assert_eq!(body["type"], "urn:barbacane:error:upstream-unavailable");
    assert_eq!(body["status"], 502);

    let line = gateway
        .log()
        .wait_for_line("HTTP call failed", std::time::Duration::from_secs(5))
        .expect("the failure is logged");
    assert!(line.contains(address), "the log names the upstream: {line}");
    assert!(
        line.contains("connection closed before message completed"),
        "the log gives the cause: {line}"
    );
}

// ========================
// Request header allowlist (ADR-0033)
// ========================

/// A spec whose operation declares one header parameter and nothing else, so
/// the upstream should receive that header and not the rest.
fn allowlist_spec(upstream_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let spec = format!(
        r#"openapi: "3.1.0"
info:
  title: Header Allowlist Test API
  version: "1.0.0"
components:
  securitySchemes:
    Bearer:
      type: http
      scheme: bearer
paths:
  /declared:
    get:
      operationId: declaredHeader
      parameters:
        - name: X-Tenant-Id
          in: header
          schema:
            type: string
      security:
        - Bearer: []
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{url}"
          path: "/received"
          timeout: 10.0
      responses:
        "200":
          description: OK
  /anonymous:
    get:
      operationId: anonymousOperation
      security: []
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{url}"
          path: "/received"
          timeout: 10.0
      responses:
        "200":
          description: OK
"#,
        url = upstream_url
    );
    let path = temp.path().join("allowlist.yaml");
    std::fs::write(&path, spec).expect("write spec");
    std::fs::write(
        temp.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  http-upstream:\n    path: {}\n",
            plugin_wasm("http-upstream").display()
        ),
    )
    .expect("write manifest");
    (temp, path)
}

/// What the spec declares reaches the upstream, and what it does not is gone
/// before any plugin or upstream sees it.
#[tokio::test]
async fn test_undeclared_request_headers_do_not_reach_the_upstream() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// Answer with the header names the upstream actually received.
    struct EchoHeaders;
    impl Respond for EchoHeaders {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let names: Vec<String> = req
                .headers
                .iter()
                .map(|(name, _)| name.as_str().to_ascii_lowercase())
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "received": names }))
        }
    }

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/received"))
        .respond_with(EchoHeaders)
        .mount(&upstream)
        .await;

    let (_tmp, spec) = allowlist_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/declared")
        .header("X-Tenant-Id", "acme")
        .header("Authorization", "Bearer token")
        .header("Accept", "application/json")
        .header("X-Debug-Mode", "on")
        .header("X-Internal-Flag", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let received: Vec<String> = serde_json::from_value(body["received"].clone()).expect("names");
    let got = |name: &str| received.iter().any(|h| h == name);

    // Declared as a parameter.
    assert!(got("x-tenant-id"), "declared header missing: {received:?}");
    // Carried because the operation declares a security scheme.
    assert!(got("authorization"), "credential missing: {received:?}");
    // Baseline.
    assert!(got("accept"), "baseline header missing: {received:?}");
    // Declared nowhere.
    assert!(
        !got("x-debug-mode"),
        "undeclared header forwarded: {received:?}"
    );
    assert!(
        !got("x-internal-flag"),
        "undeclared header forwarded: {received:?}"
    );
}

/// An operation that opts out of the root requirement takes no credential, so
/// `authorization` does not reach its upstream.
#[tokio::test]
async fn test_anonymous_operation_does_not_forward_the_credential() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    struct EchoHeaders;
    impl Respond for EchoHeaders {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let names: Vec<String> = req
                .headers
                .iter()
                .map(|(name, _)| name.as_str().to_ascii_lowercase())
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "received": names }))
        }
    }

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/received"))
        .respond_with(EchoHeaders)
        .mount(&upstream)
        .await;

    let (_tmp, spec) = allowlist_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/anonymous")
        .header("Authorization", "Bearer token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let received: Vec<String> = serde_json::from_value(body["received"].clone()).expect("names");
    assert!(
        !received.iter().any(|h| h == "authorization"),
        "an operation declaring `security: []` must not forward a credential: {received:?}"
    );
}

// ========================
// M6a: TLS Termination Tests
// ========================

#[tokio::test]
async fn test_tls_gateway_health() {
    let gateway = TestGateway::from_spec_with_tls(&fixture("minimal.yaml"))
        .await
        .expect("failed to start TLS gateway");

    // Verify TLS is enabled
    assert!(gateway.is_tls_enabled());
    assert!(gateway.base_url().starts_with("https://"));

    // Health check over HTTPS
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
}

#[tokio::test]
async fn test_tls_gateway_mock_response() {
    let gateway = TestGateway::from_spec_with_tls(&fixture("minimal.yaml"))
        .await
        .expect("failed to start TLS gateway");

    // Mock response over HTTPS
    let resp = gateway.get("/health").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn test_tls_gateway_404() {
    let gateway = TestGateway::from_spec_with_tls(&fixture("minimal.yaml"))
        .await
        .expect("failed to start TLS gateway");

    // 404 response over HTTPS
    let resp = gateway.get("/nonexistent").await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_tls_gateway_http2() {
    let gateway = TestGateway::from_spec_with_tls(&fixture("minimal.yaml"))
        .await
        .expect("failed to start TLS gateway");

    // HTTP/2 should be negotiated via ALPN when TLS is enabled
    let resp = gateway.get("/health").await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.version(),
        reqwest::Version::HTTP_2,
        "expected HTTP/2 negotiated via ALPN over TLS"
    );
}

// ========================
// Middleware + POST body
// ========================
//
// Reproduces the Burst scenario: auth middleware → http-upstream dispatch
// with a JSON body. The body is stripped for middleware (to avoid WASM OOM
// with large payloads) and reattached before dispatch.

/// POST with a JSON body through apikey-auth middleware + http-upstream.
/// Uses wiremock to verify the upstream receives the exact body.
#[tokio::test]
async fn test_post_body_through_middleware_to_upstream() {
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    if !http_upstream_wasm.exists() || !apikey_wasm.exists() {
        panic!(
            "WASM plugins not found. Run `make plugins` first.\n  http-upstream: {}\n  apikey-auth: {}",
            http_upstream_wasm.display(),
            apikey_wasm.display()
        );
    }

    // Start wiremock upstream that expects a specific JSON body
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/messages"))
        .and(body_json(serde_json::json!({"content": "hello world"})))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({"id": "msg-1", "content": "hello world"})),
        )
        .mount(&mock_server)
        .await;

    // Build a temp spec with apikey-auth middleware + http-upstream dispatch
    let temp_dir = tempfile::TempDir::new().unwrap();

    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Body Through Middleware Test
  version: "1.0.0"

paths:
  /messages:
    post:
      operationId: postMessage
      summary: Post a message through auth middleware
      x-barbacane-middlewares:
        - name: apikey-auth
          config:
            header_name: x-api-key
            keys:
              - key: test-key-123
                id: key-1
                name: testuser
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/messages"
          timeout: 10.0
      responses:
        "201":
          description: Created
        "401":
          description: Unauthorized
security:
  - ApiKeyAuth: []
components:
  securitySchemes:
    ApiKeyAuth:
      type: apiKey
      in: header
      name: x-api-key
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // POST with API key — should pass auth and reach upstream
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/messages")
        .header("content-type", "application/json")
        .header("x-api-key", "test-key-123")
        .body(r#"{"content":"hello world"}"#)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 201,
        "expected 201, got {status}. Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["id"], "msg-1");
    assert_eq!(body["content"], "hello world");
}

/// POST binary body (application/octet-stream) through middleware to upstream.
/// Verifies that non-UTF-8 bytes survive the full gateway path.
#[tokio::test]
async fn test_post_binary_body_through_middleware_to_upstream() {
    use wiremock::matchers::{body_bytes, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    if !http_upstream_wasm.exists() || !apikey_wasm.exists() {
        panic!(
            "WASM plugins not found. Run `make plugins` first.\n  http-upstream: {}\n  apikey-auth: {}",
            http_upstream_wasm.display(),
            apikey_wasm.display()
        );
    }

    // Binary payload with null bytes, high bytes, PNG-like header
    let binary_payload: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG magic
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0xFF, 0xFE, 0xFD, 0x80, 0x7F, 0x01, // non-UTF-8 bytes
    ];

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .and(body_bytes(binary_payload.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"status": "uploaded", "size": 22})),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Binary Body Test
  version: "1.0.0"

paths:
  /upload:
    post:
      operationId: uploadFile
      x-barbacane-middlewares:
        - name: apikey-auth
          config:
            header_name: x-api-key
            keys:
              - key: test-key-123
                id: key-1
                name: testuser
      requestBody:
        required: true
        content:
          application/octet-stream:
            schema:
              type: string
              format: binary
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/upload"
          timeout: 10.0
      responses:
        "200":
          description: OK
security:
  - ApiKeyAuth: []
components:
  securitySchemes:
    ApiKeyAuth:
      type: apiKey
      in: header
      name: x-api-key
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/upload")
        .header("content-type", "application/octet-stream")
        .header("x-api-key", "test-key-123")
        .body(binary_payload)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200, got {status}. Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["status"], "uploaded");
    assert_eq!(body["size"], 22);
}

/// POST binary body directly to dispatcher (no middleware).
/// Verifies the no-middleware path handles binary correctly.
#[tokio::test]
async fn test_post_binary_body_direct_dispatch() {
    use wiremock::matchers::{body_bytes, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    if !http_upstream_wasm.exists() {
        panic!(
            "WASM plugin not found. Run `make plugins` first.\n  http-upstream: {}",
            http_upstream_wasm.display(),
        );
    }

    let binary_payload: Vec<u8> = vec![0x00, 0xFF, 0x80, 0x7F, 0xFE, 0x01, 0x02, 0x03];

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/data"))
        .and(body_bytes(binary_payload.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n",
        http_upstream_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Binary Direct Dispatch Test
  version: "1.0.0"

paths:
  /data:
    post:
      operationId: postData
      requestBody:
        content:
          application/octet-stream:
            schema:
              type: string
              format: binary
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/data"
          timeout: 10.0
      responses:
        "200":
          description: OK
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/data")
        .header("content-type", "application/octet-stream")
        .body(binary_payload)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200, got {status}. Body:\n{body_text}"
    );
    assert_eq!(body_text, "ok");
}

/// Upstream returns binary response body — verify it arrives at the client intact.
#[tokio::test]
async fn test_binary_response_body_from_upstream() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    if !http_upstream_wasm.exists() {
        panic!(
            "WASM plugin not found. Run `make plugins` first.\n  http-upstream: {}",
            http_upstream_wasm.display(),
        );
    }

    // Binary response body (simulating a small PNG)
    let binary_response: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG magic
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR
    ];

    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/image.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "image/png")
                .set_body_bytes(binary_response.clone()),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n",
        http_upstream_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Binary Response Test
  version: "1.0.0"

paths:
  /image.png:
    get:
      operationId: getImage
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/image.png"
          timeout: 10.0
      responses:
        "200":
          description: PNG image
          content:
            image/png:
              schema:
                type: string
                format: binary
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/image.png").await.unwrap();
    assert_eq!(resp.status(), 200);

    let bytes = resp.bytes().await.unwrap();
    assert_eq!(bytes.as_ref(), binary_response.as_slice());
}

/// POST without API key should be rejected by middleware (body never reaches upstream).
#[tokio::test]
async fn test_post_body_rejected_by_middleware() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    if !http_upstream_wasm.exists() || !apikey_wasm.exists() {
        panic!("WASM plugins not found. Run `make plugins` first.");
    }

    let mock_server = MockServer::start().await;
    // This mock should NOT be called — middleware should reject before dispatch
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Body Through Middleware Test
  version: "1.0.0"
paths:
  /messages:
    post:
      operationId: postMessage
      x-barbacane-middlewares:
        - name: apikey-auth
          config:
            header_name: x-api-key
            keys:
              - key: test-key-123
                id: key-1
                name: testuser
      requestBody:
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/messages"
          timeout: 10.0
      responses:
        "201":
          description: Created
        "401":
          description: Unauthorized
security:
  - ApiKeyAuth: []
components:
  securitySchemes:
    ApiKeyAuth:
      type: apiKey
      in: header
      name: x-api-key
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // POST without API key — should be rejected by middleware
    let resp = gateway
        .post("/messages", r#"{"content":"hello world"}"#)
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

// ========================================
// Large body tests (write_to_memory OOM)
// ========================================

/// POST a ~500KB body through middleware + http-upstream dispatch.
///
/// This exercises the `write_to_memory` code path with a large payload
/// that, when JSON+base64 encoded, produces a Request JSON exceeding the
/// initial WASM linear memory. The fix (memory grow) must handle this
/// without OOM.
#[tokio::test]
async fn test_large_body_through_middleware_to_upstream() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    if !http_upstream_wasm.exists() || !apikey_wasm.exists() {
        panic!(
            "WASM plugins not found. Run `make plugins` first.\n  http-upstream: {}\n  apikey-auth: {}",
            http_upstream_wasm.display(),
            apikey_wasm.display()
        );
    }

    // 500KB payload — large enough to stress WASM memory but under the
    // default 1MB body-size limit.
    let large_payload: Vec<u8> = (0..500_000).map(|i| (i % 256) as u8).collect();
    let payload_len = large_payload.len();

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"received_bytes": payload_len})),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Large Body Test
  version: "1.0.0"

paths:
  /upload:
    post:
      operationId: uploadLarge
      x-barbacane-middlewares:
        - name: apikey-auth
          config:
            header_name: x-api-key
            keys:
              - key: test-key-123
                id: key-1
                name: testuser
      requestBody:
        required: true
        content:
          application/octet-stream:
            schema:
              type: string
              format: binary
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/upload"
          timeout: 30.0
      responses:
        "200":
          description: OK
security:
  - ApiKeyAuth: []
components:
  securitySchemes:
    ApiKeyAuth:
      type: apiKey
      in: header
      name: x-api-key
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/upload")
        .header("content-type", "application/octet-stream")
        .header("x-api-key", "test-key-123")
        .body(large_payload)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200, got {status}. Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["received_bytes"], payload_len);
}

/// POST a ~2MB body through middleware + dispatch with raised body-size limit.
///
/// Uses `--max-body-size` to allow 3MB payloads. This is the exact scenario
/// that caused OOM before the `write_to_memory` fix: a 2MB body base64-encoded
/// produces ~2.7MB of Request JSON, which exceeds the initial 1MB WASM memory.
#[tokio::test]
async fn test_2mb_body_through_middleware_to_upstream() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    if !http_upstream_wasm.exists() || !apikey_wasm.exists() {
        panic!(
            "WASM plugins not found. Run `make plugins` first.\n  http-upstream: {}\n  apikey-auth: {}",
            http_upstream_wasm.display(),
            apikey_wasm.display()
        );
    }

    // 2MB payload
    let large_payload: Vec<u8> = (0..2_000_000).map(|i| (i % 256) as u8).collect();
    let payload_len = large_payload.len();

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"received_bytes": payload_len})),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Large Body 2MB Test
  version: "1.0.0"

paths:
  /upload:
    post:
      operationId: uploadLarge2mb
      x-barbacane-middlewares:
        - name: apikey-auth
          config:
            header_name: x-api-key
            keys:
              - key: test-key-123
                id: key-1
                name: testuser
      requestBody:
        required: true
        content:
          application/octet-stream:
            schema:
              type: string
              format: binary
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/upload"
          timeout: 30.0
      responses:
        "200":
          description: OK
security:
  - ApiKeyAuth: []
components:
  securitySchemes:
    ApiKeyAuth:
      type: apiKey
      in: header
      name: x-api-key
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    // Raise body-size limit to 3MB to allow the 2MB payload
    let gateway = TestGateway::from_spec_with_args(
        spec_path.to_str().unwrap(),
        &["--max-body-size", "3145728"],
    )
    .await
    .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/upload")
        .header("content-type", "application/octet-stream")
        .header("x-api-key", "test-key-123")
        .body(large_payload)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200, got {status}. Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["received_bytes"], payload_len);
}

/// POST a ~3MB body through middleware + dispatch.
///
/// This is the near-limit regression test for WASM memory pressure in
/// http-upstream. The peak memory budget inside the 16MB WASM instance
/// during dispatch is (assuming body is moved, not cloned):
///
///   input_json  ≈ body × 1.37          (base64 overhead + JSON framing)
///   deser_body  = body                  (decoded Vec<u8>)
///   — input_json freed after deser —
///   base64_tmp  ≈ body × 1.33          (re-encode for host_http_call)
///   output_json ≈ body × 1.37          (serialized HttpRequest)
///   peak        ≈ body + body×1.33 + body×1.37 ≈ body × 3.7
///
/// For 3MB: peak ≈ 11.1MB — fits in 16MB with ~5MB headroom.
/// Before the move-not-clone fix an extra `body` copy pushed a 2.4MB
/// upload to ~11.5MB peak which, with dlmalloc fragmentation, trapped.
#[tokio::test]
async fn test_3mb_body_through_middleware_to_upstream() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    if !http_upstream_wasm.exists() || !apikey_wasm.exists() {
        panic!(
            "WASM plugins not found. Run `make plugins` first.\n  http-upstream: {}\n  apikey-auth: {}",
            http_upstream_wasm.display(),
            apikey_wasm.display()
        );
    }

    // 3MB payload — exercises near-limit WASM memory budget
    let large_payload: Vec<u8> = (0..3_000_000).map(|i| (i % 256) as u8).collect();
    let payload_len = large_payload.len();

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"received_bytes": payload_len})),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Large Body 3MB Test
  version: "1.0.0"

paths:
  /upload:
    post:
      operationId: uploadLarge3mb
      x-barbacane-middlewares:
        - name: apikey-auth
          config:
            header_name: x-api-key
            keys:
              - key: test-key-123
                id: key-1
                name: testuser
      requestBody:
        content:
          application/octet-stream:
            schema:
              type: string
              format: binary
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/upload"
          timeout: 30.0
      responses:
        "200":
          description: OK
security:
  - ApiKeyAuth: []
components:
  securitySchemes:
    ApiKeyAuth:
      type: apiKey
      in: header
      name: x-api-key
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    // Raise body-size limit to 4MB to allow the 3MB payload
    let gateway = TestGateway::from_spec_with_args(
        spec_path.to_str().unwrap(),
        &["--max-body-size", "4194304"],
    )
    .await
    .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/upload")
        .header("content-type", "application/octet-stream")
        .header("x-api-key", "test-key-123")
        .body(large_payload)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200, got {status}. Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["received_bytes"], payload_len);
}

/// GET /items/{id} through http-upstream with NO explicit `path` config.
/// Verifies that the upstream receives the resolved path (`/items/42`)
/// rather than the OpenAPI template path (`/items/{id}`).
#[tokio::test]
async fn test_http_upstream_path_params_resolved_without_path_config() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    if !http_upstream_wasm.exists() {
        panic!(
            "WASM plugin not found. Run `make plugins` first.\n  http-upstream: {}",
            http_upstream_wasm.display(),
        );
    }

    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items/42"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id": 42, "name": "Widget"})),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n",
        http_upstream_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    // No `path:` in http-upstream config — the plugin should default to req.path,
    // which must be the resolved path, not the OpenAPI template.
    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Path Params Resolved Test
  version: "1.0.0"

paths:
  /items/{{id}}:
    get:
      operationId: getItem
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          timeout: 10.0
      responses:
        "200":
          description: OK
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/items/42").await.unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200 (upstream should receive /items/42, not /items/{{id}}). Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["id"], 42);
    assert_eq!(body["name"], "Widget");
}

/// GET /users/{id}/posts/{postId} through apikey-auth middleware + http-upstream
/// with NO explicit `path` config. Verifies path params are resolved through
/// the full middleware → dispatch pipeline, not just direct dispatch.
#[tokio::test]
async fn test_http_upstream_path_params_resolved_through_middleware() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    if !http_upstream_wasm.exists() || !apikey_wasm.exists() {
        panic!(
            "WASM plugins not found. Run `make plugins` first.\n  http-upstream: {}\n  apikey-auth: {}",
            http_upstream_wasm.display(),
            apikey_wasm.display()
        );
    }

    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/7/posts/99"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"userId": 7, "postId": 99, "title": "Hello"})),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Path Params Through Middleware Test
  version: "1.0.0"

paths:
  /users/{{userId}}/posts/{{postId}}:
    get:
      operationId: getUserPost
      parameters:
        - name: userId
          in: path
          required: true
          schema:
            type: string
        - name: postId
          in: path
          required: true
          schema:
            type: string
      x-barbacane-middlewares:
        - name: apikey-auth
          config:
            header_name: x-api-key
            keys:
              - key: test-key-123
                id: key-1
                name: testuser
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          timeout: 10.0
      responses:
        "200":
          description: OK
        "401":
          description: Unauthorized
security:
  - ApiKeyAuth: []
components:
  securitySchemes:
    ApiKeyAuth:
      type: apiKey
      in: header
      name: x-api-key
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/users/7/posts/99")
        .header("x-api-key", "test-key-123")
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200 (upstream should receive /users/7/posts/99). Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["userId"], 7);
    assert_eq!(body["postId"], 99);
    assert_eq!(body["title"], "Hello");
}

/// POST a large body directly to dispatcher (no middleware).
/// Verifies the dispatch-only path handles large payloads without OOM.
#[tokio::test]
async fn test_large_body_direct_dispatch() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    if !http_upstream_wasm.exists() {
        panic!(
            "WASM plugin not found. Run `make plugins` first.\n  http-upstream: {}",
            http_upstream_wasm.display(),
        );
    }

    let large_payload: Vec<u8> = (0..500_000).map(|i| (i % 256) as u8).collect();
    let payload_len = large_payload.len();

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/data"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"received_bytes": payload_len})),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n",
        http_upstream_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Large Body Direct Dispatch Test
  version: "1.0.0"

paths:
  /data:
    post:
      operationId: postLargeData
      requestBody:
        content:
          application/octet-stream:
            schema:
              type: string
              format: binary
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "{upstream}"
          path: "/data"
          timeout: 30.0
      responses:
        "200":
          description: OK
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/data")
        .header("content-type", "application/octet-stream")
        .body(large_payload)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status, 200,
        "expected 200, got {status}. Body:\n{body_text}"
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["received_bytes"], payload_len);
}

/// A spec routing `/files/{name}` to `/public/{name}`, `/wild/{path+}` to
/// `/{path}`, and `/raw/{path+}` to the request's own path, on `upstream_url`.
fn upstream_paths_spec(upstream_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let spec = r#"openapi: "3.1.0"
info:
  title: Upstream paths
  version: "1.0.0"
paths:
  /files/{name}:
    get:
      operationId: getFile
      parameters:
        - name: name
          in: path
          required: true
          schema:
            type: string
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "UPSTREAM"
          path: "/public/{name}"
          timeout: 10.0
      responses:
        "200":
          description: OK
  /wild/{path+}:
    get:
      operationId: getWild
      parameters:
        - name: path
          in: path
          required: true
          allowReserved: true
          schema:
            type: string
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "UPSTREAM"
          path: "/{path}"
          timeout: 10.0
      responses:
        "200":
          description: OK
  /raw/{path+}:
    get:
      operationId: getRaw
      parameters:
        - name: path
          in: path
          required: true
          allowReserved: true
          schema:
            type: string
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "UPSTREAM"
          timeout: 10.0
      responses:
        "200":
          description: OK
"#
    .replace("UPSTREAM", upstream_url);
    std::fs::write(temp.path().join("upstream-paths.yaml"), spec).expect("write spec");
    std::fs::write(
        temp.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  http-upstream:\n    path: {}\n",
            plugin_wasm("http-upstream").display()
        ),
    )
    .expect("write manifest");
    let path = temp.path().join("upstream-paths.yaml");
    (temp, path)
}

/// The status of `GET target`, sent as written: an HTTP client would resolve
/// the dot segments some of these targets hold before sending them.
async fn raw_get_status(port: u16, target: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let request = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    socket.write_all(request.as_bytes()).await.expect("send");
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.expect("read");
    String::from_utf8_lossy(&response)
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap_or_else(|| panic!("no status line for {target}"))
}

/// What the upstream received, as `path?query`, in order.
async fn received_targets(upstream: &wiremock::MockServer) -> Vec<String> {
    upstream
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| match r.url.query() {
            Some(query) => format!("{}?{}", r.url.path(), query),
            None => r.url.path().to_string(),
        })
        .collect()
}

/// A path parameter fills its own place in the `path` template: a decoded `/`
/// with `..` cannot climb out of it, and a decoded `?` cannot start a query.
#[tokio::test]
async fn test_a_path_parameter_stays_within_its_upstream_path() {
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;
    let (_tmp, spec) = upstream_paths_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");
    let port = gateway.port();

    assert_eq!(raw_get_status(port, "/files/report.v2.pdf").await, 200);
    assert_eq!(raw_get_status(port, "/files/x%3Famount%3D0").await, 200);
    assert_eq!(
        raw_get_status(port, "/files/..%2F..%2Fadmin%2Fusers").await,
        400
    );

    assert_eq!(
        received_targets(&upstream).await,
        ["/public/report.v2.pdf", "/public/x%3Famount=0"]
    );
}

/// A `.` or `..` segment, in any spelling, is refused before routing: it
/// reaches no upstream, whether the path is forwarded as is or a wildcard
/// fills a template. Paths without one are forwarded.
#[tokio::test]
async fn test_a_dot_segment_never_reaches_the_upstream() {
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;
    let (_tmp, spec) = upstream_paths_spec(&upstream.uri());
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");
    let port = gateway.port();

    for target in [
        "/raw/../admin/users",
        "/raw/%2e%2e/admin/users",
        "/raw/.%2E/admin/users",
        "/raw/a/..%2F..%2Fadmin",
        "/wild/a/..%2F..%2Fsecret",
        "/wild/%2e%2e/secret",
    ] {
        assert_eq!(raw_get_status(port, target).await, 400, "{target}");
    }
    assert!(
        received_targets(&upstream).await.is_empty(),
        "the upstream received {:?}",
        received_targets(&upstream).await
    );

    assert_eq!(raw_get_status(port, "/raw/x/y").await, 200);
    assert_eq!(raw_get_status(port, "/wild/a/b/c.txt").await, 200);
    assert_eq!(
        received_targets(&upstream).await,
        ["/raw/x/y", "/a/b/c.txt"]
    );
}
