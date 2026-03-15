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
              secret-key:
                id: key-1
                name: test
      x-barbacane-dispatch:
        name: streaming-echo
        config:
          url: "{upstream}/stream"
      responses:
        "200":
          description: SSE stream
        "401":
          description: Unauthorized
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
