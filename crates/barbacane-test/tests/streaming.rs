//! Integration tests for streaming dispatch (ADR-0023).
//!
//! Tests the `host_http_stream` path through the gateway using the
//! `streaming-echo` fixture plugin against a wiremock SSE/chunked server.
//!
//! Run with: `cargo test -p barbacane-test streaming`
//!
//! The `streaming-echo` fixture plugin is compiled automatically by `build.rs`
//! when you run `cargo test -p barbacane-test`.

use std::path::PathBuf;

use barbacane_test::TestGateway;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Absolute path to the `streaming-echo` fixture plugin wasm binary.
fn streaming_echo_wasm() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixture-plugins/streaming-echo/target/wasm32-unknown-unknown/release/streaming_echo.wasm")
}

/// Absolute path to the `mock` plugin wasm binary (for buffered endpoint).
fn mock_wasm() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins/mock/mock.wasm")
}

/// Absolute path to the `apikey-auth` plugin wasm binary (for auth middleware test).
fn apikey_wasm() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins/apikey-auth/apikey-auth.wasm")
}

/// Create a temporary spec + manifest for streaming tests.
/// Returns `(TempDir, spec_path)` — keep `TempDir` alive for the test duration.
fn create_streaming_spec(upstream_url: &str) -> (tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let spec_path = temp_dir.path().join("streaming.yaml");

    let manifest = format!(
        "plugins:\n  streaming-echo:\n    path: {echo}\n  mock:\n    path: {mock}\n  apikey-auth:\n    path: {apikey}\n",
        echo = streaming_echo_wasm().display(),
        mock = mock_wasm().display(),
        apikey = apikey_wasm().display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).expect("write manifest");

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Streaming Test API
  version: "1.0.0"

paths:
  /stream:
    get:
      summary: Streaming SSE endpoint
      operationId: getStream
      x-barbacane-dispatch:
        name: streaming-echo
        config:
          url: "{upstream}/stream"
      responses:
        "200":
          description: SSE stream

  /buffered:
    get:
      summary: Buffered endpoint (non-regression)
      operationId: getBuffered
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"ok":true}}'
          content_type: application/json
      responses:
        "200":
          description: OK

  /stream/protected:
    get:
      summary: Streaming endpoint behind auth middleware
      operationId: getStreamProtected
      x-barbacane-middlewares:
        - name: apikey-auth
          config:
            header_name: x-api-key
            keys:
              - key: secret-key
                id: key-1
                name: test
      security:
        - ApiKeyAuth: []
      x-barbacane-dispatch:
        name: streaming-echo
        config:
          url: "{upstream}/stream"
      responses:
        "200":
          description: SSE stream
        "401":
          description: Unauthorized
components:
  securitySchemes:
    ApiKeyAuth:
      type: apiKey
      in: header
      name: x-api-key
"#,
        upstream = upstream_url,
    );
    std::fs::write(&spec_path, spec).expect("write spec");

    (temp_dir, spec_path)
}

// ---------------------------------------------------------------------------
// Happy path: streaming dispatcher forwards chunked/SSE upstream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_sse_response_is_forwarded_to_client() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    if !streaming_echo_wasm().exists() {
        panic!(
            "streaming-echo.wasm not found at {}. \
             Run `cd tests/fixture-plugins/streaming-echo && \
             cargo build --target wasm32-unknown-unknown --release`",
            streaming_echo_wasm().display()
        );
    }

    let mock_server = MockServer::start().await;
    let sse_body = "data: hello\n\ndata: world\n\n";
    Mock::given(method("GET"))
        .and(path("/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(sse_body, "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_streaming_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/stream").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("data: hello"),
        "expected SSE events in body, got: {body}"
    );
    assert!(
        body.contains("data: world"),
        "expected SSE events in body, got: {body}"
    );
}

// ---------------------------------------------------------------------------
// Non-regression: buffered dispatcher still works
// ---------------------------------------------------------------------------

#[tokio::test]
async fn buffered_dispatcher_still_works_alongside_streaming() {
    if !streaming_echo_wasm().exists() {
        panic!("streaming-echo.wasm not found — build fixture-plugins/streaming-echo first");
    }

    let (_tmp, spec_path) = create_streaming_spec("http://127.0.0.1:1");
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // /buffered uses the mock plugin (buffered), not streaming-echo
    let resp = gateway.get("/buffered").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
}

// ---------------------------------------------------------------------------
// Middleware short-circuit before streaming
// ---------------------------------------------------------------------------

#[tokio::test]
async fn middleware_short_circuit_before_streaming_returns_401() {
    if !streaming_echo_wasm().exists() {
        panic!("streaming-echo.wasm not found — build fixture-plugins/streaming-echo first");
    }

    let (_tmp, spec_path) = create_streaming_spec("http://127.0.0.1:1");
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // /stream/protected has apikey-auth middleware — no key → 401
    let resp = gateway.get("/stream/protected").await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

// ---------------------------------------------------------------------------
// Memory: a streamed body is forwarded as the client reads it, not buffered
// ---------------------------------------------------------------------------

/// A client that reads nothing holds the upstream back: the gateway keeps a
/// bounded number of chunks in flight instead of reading the whole body.
#[tokio::test]
async fn a_slow_client_holds_the_upstream_back() {
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const BODY: usize = 64 * 1024 * 1024;
    const CHUNK: usize = 64 * 1024;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let (sent_tx, sent_rx) = tokio::sync::oneshot::channel::<usize>();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
        let piece = vec![b'x'; CHUNK];
        let head = format!("{CHUNK:x}\r\n");
        let mut sent = 0;
        while sent < BODY {
            let frame = async {
                socket.write_all(head.as_bytes()).await?;
                socket.write_all(&piece).await?;
                socket.write_all(b"\r\n").await
            };
            // A write that stalls means the gateway stopped reading.
            match tokio::time::timeout(Duration::from_secs(2), frame).await {
                Ok(Ok(())) => sent += CHUNK,
                _ => break,
            }
        }
        let _ = sent_tx.send(sent);
    });

    let (_temp_dir, spec_path) = create_streaming_spec(&upstream);
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Read the headers, then nothing: the body stays unread.
    let resp = gateway.get("/stream").await.unwrap();
    assert_eq!(resp.status(), 200);

    let sent = sent_rx.await.unwrap();
    assert!(
        sent < 16 * 1024 * 1024,
        "the upstream was read {sent} bytes ahead of a client that read nothing"
    );
    drop(resp);
}

// ---------------------------------------------------------------------------
// Metrics of a streamed response
// ---------------------------------------------------------------------------

/// An upstream that streams `events` server-sent events, `gap` apart, with a
/// chunked body. Returns its base URL.
async fn slow_sse_upstream(events: usize, gap: std::time::Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
                if stream.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for i in 0..events {
                    let event = format!("data: {i}\n\n");
                    let chunk = format!("{:x}\r\n{event}\r\n", event.len());
                    if stream.write_all(chunk.as_bytes()).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(gap).await;
                }
                let _ = stream.write_all(b"0\r\n\r\n").await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The value of the first sample of `family` whose line contains every
/// fragment.
fn sample(body: &str, family: &str, fragments: &[&str]) -> Option<f64> {
    body.lines()
        .filter(|l| l.starts_with(family))
        .find(|l| fragments.iter().all(|f| l.contains(f)))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
}

/// A streamed response is measured to its end: the response size counts the
/// bytes the client received, and the duration covers the whole stream, not
/// the time to its headers.
#[tokio::test]
async fn a_streamed_response_is_measured_to_its_end() {
    let gap = std::time::Duration::from_millis(300);
    let upstream = slow_sse_upstream(4, gap).await;
    let (_tmp, spec_path) = create_streaming_spec(&upstream);
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body = gateway.get("/stream").await.unwrap().text().await.unwrap();
    assert_eq!(body, "data: 0\n\ndata: 1\n\ndata: 2\n\ndata: 3\n\n");

    // Recorded when the body ends on the gateway's side, which can follow the
    // client's last read by a moment.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let metrics = loop {
        let metrics = gateway
            .admin_get("/metrics")
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let recorded =
            sample(&metrics, "barbacane_requests_total", &["path=\"/stream\""]).is_some();
        if recorded || std::time::Instant::now() > deadline {
            break metrics;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };

    let size = sample(
        &metrics,
        "barbacane_response_size_bytes_sum",
        &["path=\"/stream\""],
    )
    .unwrap_or_else(|| panic!("no response size for /stream:\n{metrics}"));
    assert_eq!(size, body.len() as f64, "the bytes the client received");
    let duration = sample(
        &metrics,
        "barbacane_request_duration_seconds_sum",
        &["path=\"/stream\""],
    )
    .unwrap_or_else(|| panic!("no duration for /stream:\n{metrics}"));
    assert!(
        duration >= 3.0 * gap.as_secs_f64(),
        "the duration covers the stream: {duration}s"
    );
}

/// A client that leaves a stream gives its plugin instances back at once,
/// though the upstream sends nothing more for half a minute, and the next
/// request finds them in the pool.
#[tokio::test]
async fn a_client_leaving_a_quiet_stream_gives_its_instances_back() {
    use std::time::Duration;

    // One event, then 30 seconds of silence.
    let upstream = slow_sse_upstream(2, Duration::from_secs(30)).await;
    let (_tmp, spec_path) = create_streaming_spec(&upstream);
    let gateway = TestGateway::from_spec_with_args(
        spec_path.to_str().unwrap(),
        &["--plugin-instance-pool", "2"],
    )
    .await
    .expect("failed to start gateway");

    let mut stream = gateway
        .request_builder(reqwest::Method::GET, "/stream/protected")
        .header("x-api-key", "secret-key")
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), 200);
    let first = stream.chunk().await.unwrap().expect("the first event");
    assert!(first.starts_with(b"data: 0"), "{first:?}");
    assert_eq!(
        gateway.metric("barbacane_wasm_instances").await.unwrap(),
        Some(2.0),
        "apikey-auth and streaming-echo are alive while the stream is"
    );

    drop(stream);
    let alive = gateway
        .wait_for_metric("barbacane_wasm_instances", 0.0, Duration::from_secs(5))
        .await;
    assert_eq!(
        alive,
        Some(0.0),
        "instances alive 5 s after the client left"
    );

    assert_eq!(gateway.get("/buffered").await.unwrap().status(), 200);
    assert_eq!(
        gateway
            .metric("barbacane_wasm_instances_unpooled_total")
            .await
            .unwrap(),
        Some(0.0),
        "every instance came from the pool"
    );
}

// ---------------------------------------------------------------------------
// A streamed response's on_response runs off the async workers
// ---------------------------------------------------------------------------

/// A streamed response's `on_response` runs on a blocking thread: with one
/// async worker, an `http-log` middleware waiting on a slow log endpoint does
/// not hold up the next request.
#[tokio::test]
async fn streamed_on_response_does_not_hold_an_async_worker() {
    use std::time::{Duration, Instant};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_log_wasm =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/http-log/http-log.wasm");
    assert!(
        streaming_echo_wasm().exists() && http_log_wasm.exists(),
        "build fixture-plugins/streaming-echo and plugins/http-log first"
    );

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stream"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("data: hi\n\n", "text/event-stream"))
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/log"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&upstream)
        .await;

    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    std::fs::write(
        temp_dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  streaming-echo:\n    path: {}\n  mock:\n    path: {}\n  http-log:\n    path: {}\n",
            streaming_echo_wasm().display(),
            mock_wasm().display(),
            http_log_wasm.display(),
        ),
    )
    .expect("write manifest");
    let spec_path = temp_dir.path().join("logged-stream.yaml");
    std::fs::write(
        &spec_path,
        format!(
            r#"openapi: "3.0.3"
info:
  title: Logged stream
  version: "1.0.0"
paths:
  /stream:
    get:
      x-barbacane-middlewares:
        - name: http-log
          config:
            endpoint: "{upstream}/log"
            timeout_ms: 10000
      x-barbacane-dispatch:
        name: streaming-echo
        config:
          url: "{upstream}/stream"
      responses:
        "200":
          description: SSE stream
  /ping:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: pong
      responses:
        "200":
          description: OK
"#,
            upstream = upstream.uri(),
        ),
    )
    .expect("write spec");

    let gateway = TestGateway::from_spec_with_env(
        spec_path.to_str().unwrap(),
        &[("TOKIO_WORKER_THREADS", "1")],
    )
    .await
    .expect("failed to start gateway");

    let resp = gateway.get("/stream").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    resp.text().await.unwrap();

    // Wait until http-log's call reaches the log endpoint, which answers in 3 s.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !upstream
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .any(|r| r.url.path() == "/log")
    {
        assert!(
            Instant::now() < deadline,
            "http-log never called the log endpoint"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let started = Instant::now();
    let resp = gateway.get("/ping").await.unwrap();
    let waited = started.elapsed();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        waited < Duration::from_millis(1500),
        "/ping waited {waited:?} behind the streamed response's on_response"
    );
}
