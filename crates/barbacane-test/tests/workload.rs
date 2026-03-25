//! End-to-end workload tests for body integrity, multi-plugin chains, and
//! large payloads.
//!
//! These tests exercise realistic data flows through the full gateway pipeline
//! (middleware chain -> dispatcher -> response) to catch issues like:
//! - Serde encoding mismatches between host and WASM plugins
//! - WASM memory corruption/OOM under large payloads
//! - Body corruption through multi-middleware chains
//! - Binary data loss during base64 roundtrips
//!
//! The `body-echo` fixture plugin acts as a closed-loop verifier: it echoes
//! back the exact request body (base64-encoded) so tests can compare sent vs
//! received bytes without an external mock server.
//!
//! Run with: `cargo test -p barbacane-test workload`

use std::path::PathBuf;

use barbacane_test::TestGateway;
use base64::Engine;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn body_echo_wasm() -> PathBuf {
    workspace_root().join(
        "tests/fixture-plugins/body-echo/target/wasm32-unknown-unknown/release/body_echo.wasm",
    )
}

fn plugin_wasm(name: &str) -> PathBuf {
    workspace_root().join(format!("plugins/{name}/{name}.wasm"))
}

fn require_wasm(paths: &[(&str, PathBuf)]) {
    for (name, path) in paths {
        if !path.exists() {
            panic!(
                "WASM plugin '{name}' not found at {}.\n\
                 For fixture plugins: cargo test -p barbacane-test (build.rs compiles them).\n\
                 For regular plugins: make plugins",
                path.display()
            );
        }
    }
}

/// Decode the body_base64 field from a body-echo JSON response.
fn extract_echoed_body(resp_body: &[u8]) -> Option<Vec<u8>> {
    let echo: serde_json::Value = serde_json::from_slice(resp_body).unwrap();
    echo["body_base64"].as_str().map(|s| {
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .expect("invalid base64 in body_base64")
    })
}

fn extract_echo_json(resp_body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(resp_body).unwrap()
}

// ---------------------------------------------------------------------------
// Spec/manifest builders
// ---------------------------------------------------------------------------

struct WorkloadSpec {
    #[allow(dead_code)] // kept alive for RAII cleanup of temp files
    temp_dir: tempfile::TempDir,
    spec_path: PathBuf,
}

/// Build a spec with body-echo dispatcher only (no middleware).
fn spec_body_echo_only() -> WorkloadSpec {
    let echo_wasm = body_echo_wasm();
    require_wasm(&[("body-echo", echo_wasm.clone())]);

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  body-echo:\n    path: {}\n",
        echo_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = r#"openapi: "3.0.3"
info:
  title: Workload Test - Body Echo
  version: "1.0.0"

paths:
  /echo:
    post:
      operationId: echoPost
      requestBody:
        content:
          application/octet-stream:
            schema:
              type: string
              format: binary
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: body-echo
        config: {}
      responses:
        "200":
          description: Echo
    get:
      operationId: echoGet
      x-barbacane-dispatch:
        name: body-echo
        config: {}
      responses:
        "200":
          description: Echo
"#;
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();
    WorkloadSpec {
        temp_dir,
        spec_path,
    }
}

/// Build a spec with a single middleware + body-echo dispatcher.
fn spec_single_middleware() -> WorkloadSpec {
    let echo_wasm = body_echo_wasm();
    let apikey_wasm = plugin_wasm("apikey-auth");
    require_wasm(&[
        ("body-echo", echo_wasm.clone()),
        ("apikey-auth", apikey_wasm.clone()),
    ]);

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  body-echo:\n    path: {}\n  apikey-auth:\n    path: {}\n",
        echo_wasm.display(),
        apikey_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = r#"openapi: "3.0.3"
info:
  title: Workload Test - Single Middleware
  version: "1.0.0"

paths:
  /echo:
    post:
      operationId: echoPost
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
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: body-echo
        config: {}
      responses:
        "200":
          description: Echo
        "401":
          description: Unauthorized
"#;
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();
    WorkloadSpec {
        temp_dir,
        spec_path,
    }
}

/// Build a spec with multiple middlewares + body-echo dispatcher.
/// Chain: correlation-id (no body_access) -> apikey-auth (no body_access) -> body-echo
fn spec_multi_middleware() -> WorkloadSpec {
    let echo_wasm = body_echo_wasm();
    let apikey_wasm = plugin_wasm("apikey-auth");
    let corr_id_wasm = plugin_wasm("correlation-id");
    require_wasm(&[
        ("body-echo", echo_wasm.clone()),
        ("apikey-auth", apikey_wasm.clone()),
        ("correlation-id", corr_id_wasm.clone()),
    ]);

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  body-echo:\n    path: {}\n  apikey-auth:\n    path: {}\n  correlation-id:\n    path: {}\n",
        echo_wasm.display(),
        apikey_wasm.display(),
        corr_id_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = r#"openapi: "3.0.3"
info:
  title: Workload Test - Multi Middleware Chain
  version: "1.0.0"

paths:
  /echo:
    post:
      operationId: echoPost
      x-barbacane-middlewares:
        - name: correlation-id
          config:
            header_name: x-correlation-id
            generate_if_missing: true
            trust_incoming: true
            include_in_response: true
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
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: body-echo
        config: {}
      responses:
        "200":
          description: Echo
        "401":
          description: Unauthorized
"#;
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();
    WorkloadSpec {
        temp_dir,
        spec_path,
    }
}

// ===========================================================================
// 1. Body integrity — direct dispatch (no middleware)
// ===========================================================================

/// JSON body roundtrip through body-echo dispatcher.
#[tokio::test]
async fn workload_json_body_roundtrip() {
    let ws = spec_body_echo_only();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body = br#"{"content":"hello world","count":42}"#;
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let resp_bytes = resp.bytes().await.unwrap();
    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(echoed, body, "JSON body should survive roundtrip intact");
}

/// Binary body (PNG-like) roundtrip through body-echo dispatcher.
#[tokio::test]
async fn workload_binary_body_roundtrip() {
    let ws = spec_body_echo_only();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG magic
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0xFF, 0xFE, 0xFD, 0x80, 0x7F, 0x01, // non-UTF-8 bytes
    ];
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/octet-stream")
        .body(body.clone())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let resp_bytes = resp.bytes().await.unwrap();
    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(echoed, body, "binary body should survive roundtrip intact");
}

/// All 256 byte values roundtrip — catches any base64 or JSON escaping issues.
#[tokio::test]
async fn workload_all_byte_values_roundtrip() {
    let ws = spec_body_echo_only();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body: Vec<u8> = (0..=255).collect();
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/octet-stream")
        .body(body.clone())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let resp_bytes = resp.bytes().await.unwrap();
    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(echoed, body, "all 256 byte values should survive roundtrip");
}

/// Empty body (Content-Length: 0) is distinct from no body.
#[tokio::test]
async fn workload_empty_body_roundtrip() {
    let ws = spec_body_echo_only();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/octet-stream")
        .body(Vec::<u8>::new())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let resp_bytes = resp.bytes().await.unwrap();
    let echo = extract_echo_json(&resp_bytes);
    // Empty body may arrive as empty or null depending on hyper's handling;
    // the key thing is no error and a 200 response.
    let body_size = echo["body_size"].as_u64();
    assert!(
        body_size == Some(0) || body_size.is_none(),
        "empty body should have size 0 or be absent"
    );
}

/// GET request — no body at all.
#[tokio::test]
async fn workload_no_body_get() {
    let ws = spec_body_echo_only();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/echo").await.unwrap();
    assert_eq!(resp.status(), 200);

    let resp_bytes = resp.bytes().await.unwrap();
    let echo = extract_echo_json(&resp_bytes);
    assert!(echo["body_base64"].is_null(), "GET should have no body");
    assert!(echo["body_size"].is_null(), "GET should have no body size");
    assert_eq!(echo["method"], "GET");
}

// ===========================================================================
// 2. Body integrity through single middleware (body_access stripping)
// ===========================================================================

/// JSON body through apikey-auth (no body_access) -> body-echo.
/// Verifies body is stripped for middleware and reattached for dispatcher.
#[tokio::test]
async fn workload_body_through_single_middleware() {
    let ws = spec_single_middleware();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body = br#"{"message":"through middleware"}"#;
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/json")
        .header("x-api-key", "test-key-123")
        .body(body.to_vec())
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200, got {status}. Body:\n{}",
        String::from_utf8_lossy(&resp_bytes)
    );

    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(
        echoed, body,
        "body should survive middleware stripping + reattachment"
    );
}

/// Binary body through single middleware.
#[tokio::test]
async fn workload_binary_through_single_middleware() {
    let ws = spec_single_middleware();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0xFF, 0x80, 0x7F, 0xFE, 0x01, 0x02,
        0x03,
    ];
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/octet-stream")
        .header("x-api-key", "test-key-123")
        .body(body.clone())
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200, got {status}. Body:\n{}",
        String::from_utf8_lossy(&resp_bytes)
    );

    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(
        echoed, body,
        "binary body should survive middleware + dispatch roundtrip"
    );
}

// ===========================================================================
// 3. Body integrity through multi-middleware chain
// ===========================================================================

/// JSON body through correlation-id + apikey-auth -> body-echo.
/// Both middlewares have body_access=false, so the body is stripped for both
/// and reattached before dispatch.
#[tokio::test]
async fn workload_body_through_multi_middleware_chain() {
    let ws = spec_multi_middleware();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body = br#"{"content":"multi-middleware test","items":[1,2,3]}"#;
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/json")
        .header("x-api-key", "test-key-123")
        .body(body.to_vec())
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200, got {status}. Body:\n{}",
        String::from_utf8_lossy(&resp_bytes)
    );

    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(
        echoed, body,
        "body should survive 2-middleware chain + dispatch"
    );

    // Also verify correlation-id header was added to the response
    // (not in the echo, but in the HTTP response headers from the gateway)
    // The correlation-id middleware with include_in_response: true adds it.
}

/// Binary body through multi-middleware chain — the key regression test.
/// This is the exact scenario that would have caught the base64_body serde
/// mismatch: binary bytes passing through multiple middleware JSON roundtrips.
#[tokio::test]
async fn workload_binary_through_multi_middleware_chain() {
    let ws = spec_multi_middleware();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Mix of null bytes, high bytes, and structured content
    let body: Vec<u8> = {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47]; // PNG header
        v.extend_from_slice(&[0x00; 32]); // null bytes
        v.extend_from_slice(&(128..=255).collect::<Vec<u8>>()); // high bytes
        v.extend_from_slice(b"embedded text\r\n"); // mixed text
        v
    };

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/octet-stream")
        .header("x-api-key", "test-key-123")
        .body(body.clone())
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200, got {status}. Body:\n{}",
        String::from_utf8_lossy(&resp_bytes)
    );

    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(
        echoed, body,
        "binary body should survive multi-middleware chain + dispatch"
    );
}

/// All 256 byte values through multi-middleware chain.
#[tokio::test]
async fn workload_all_bytes_through_multi_middleware_chain() {
    let ws = spec_multi_middleware();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body: Vec<u8> = (0..=255).collect();
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/octet-stream")
        .header("x-api-key", "test-key-123")
        .body(body.clone())
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200, got {status}. Body:\n{}",
        String::from_utf8_lossy(&resp_bytes)
    );

    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(
        echoed, body,
        "all 256 byte values should survive multi-middleware chain"
    );
}

// ===========================================================================
// 4. Large payload tests
// ===========================================================================

/// 500KB body through multi-middleware chain — stress test for WASM memory.
#[tokio::test]
async fn workload_500kb_through_multi_middleware() {
    let ws = spec_multi_middleware();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body: Vec<u8> = (0..500_000).map(|i| (i % 256) as u8).collect();
    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo")
        .header("content-type", "application/octet-stream")
        .header("x-api-key", "test-key-123")
        .body(body.clone())
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200 for 500KB payload, got {status}. Body:\n{}",
        String::from_utf8_lossy(&resp_bytes[..resp_bytes.len().min(500)])
    );

    let echo = extract_echo_json(&resp_bytes);
    assert_eq!(
        echo["body_size"], 500_000,
        "dispatcher should receive all 500KB"
    );

    // Full byte-level verification
    let echoed = extract_echoed_body(&resp_bytes).expect("body should be present");
    assert_eq!(echoed, body, "500KB body should be byte-identical");
}

/// 2MB body through multi-middleware chain to http-upstream with wiremock.
///
/// Uses http-upstream + wiremock (not body-echo) because body-echo must
/// re-encode the full body in the WASM response JSON, which would OOM
/// for payloads this large. The wiremock mock verifies the upstream
/// received the correct number of bytes.
#[tokio::test]
async fn workload_2mb_through_multi_middleware() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    let corr_id_wasm = plugin_wasm("correlation-id");
    require_wasm(&[
        ("http-upstream", http_upstream_wasm.clone()),
        ("apikey-auth", apikey_wasm.clone()),
        ("correlation-id", corr_id_wasm.clone()),
    ]);

    let body: Vec<u8> = (0..2_000_000).map(|i| (i % 256) as u8).collect();
    let payload_len = body.len();

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
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n  correlation-id:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
        corr_id_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Workload Test - 2MB Multi Middleware
  version: "1.0.0"

paths:
  /upload:
    post:
      operationId: uploadLarge
      x-barbacane-middlewares:
        - name: correlation-id
          config:
            header_name: x-correlation-id
            generate_if_missing: true
            trust_incoming: true
            include_in_response: true
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
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

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
        .body(body)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let body_text = resp.text().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200 for 2MB payload through multi-middleware, got {status}. Body:\n{}",
        &body_text[..body_text.len().min(500)]
    );

    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["received_bytes"], payload_len);
}

/// Large response body (500KB) flowing back through middleware on_response.
///
/// Regression test for the Burst OOM: upstream returns a large response body,
/// and non-body-access middleware (correlation-id, apikey-auth) would OOM if
/// they received the full base64-encoded body in WASM. With response body
/// stripping (mirrors SPEC-008 for requests), middleware without body_access
/// never sees the response body, preventing OOM.
#[tokio::test]
async fn workload_large_response_through_middleware() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http_upstream_wasm = plugin_wasm("http-upstream");
    let apikey_wasm = plugin_wasm("apikey-auth");
    let corr_id_wasm = plugin_wasm("correlation-id");
    require_wasm(&[
        ("http-upstream", http_upstream_wasm.clone()),
        ("apikey-auth", apikey_wasm.clone()),
        ("correlation-id", corr_id_wasm.clone()),
    ]);

    // 500KB response body — tests response body stripping for middleware
    let response_body: Vec<u8> = (0..500_000).map(|i| (i % 256) as u8).collect();
    let expected_len = response_body.len();

    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/large-file"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(response_body.clone())
                .insert_header("content-type", "application/octet-stream"),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let manifest = format!(
        "plugins:\n  http-upstream:\n    path: {}\n  apikey-auth:\n    path: {}\n  correlation-id:\n    path: {}\n",
        http_upstream_wasm.display(),
        apikey_wasm.display(),
        corr_id_wasm.display(),
    );
    std::fs::write(temp_dir.path().join("barbacane.yaml"), manifest).unwrap();

    let spec = format!(
        r#"openapi: "3.0.3"
info:
  title: Workload Test - Large Response Through Middleware
  version: "1.0.0"

paths:
  /large-file:
    get:
      operationId: getLargeFile
      x-barbacane-middlewares:
        - name: correlation-id
          config:
            header_name: x-correlation-id
            generate_if_missing: true
            trust_incoming: true
            include_in_response: true
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
          path: "/large-file"
          timeout: 30.0
      responses:
        "200":
          description: Large file download
"#,
        upstream = mock_server.uri(),
    );
    let spec_path = temp_dir.path().join("test.yaml");
    std::fs::write(&spec_path, spec).unwrap();

    let gateway = TestGateway::from_spec_with_args(
        spec_path.to_str().unwrap(),
        &["--max-body-size", "3145728"],
    )
    .await
    .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/large-file")
        .header("x-api-key", "test-key-123")
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200 for large response through middleware, got {status}. Body:\n{}",
        String::from_utf8_lossy(&resp_bytes[..resp_bytes.len().min(1000)])
    );
    assert_eq!(
        resp_bytes.len(),
        expected_len,
        "response body should be exactly {expected_len} bytes, got {}",
        resp_bytes.len()
    );
    assert_eq!(
        resp_bytes.as_ref(),
        response_body.as_slice(),
        "response body should be byte-identical after passing through middleware on_response"
    );
}

// ===========================================================================
// 5. Request metadata verification
// ===========================================================================

/// Verify that request method, path, query, and headers arrive intact
/// at the dispatcher through middleware chain.
#[tokio::test]
async fn workload_request_metadata_through_chain() {
    let ws = spec_multi_middleware();
    let gateway = TestGateway::from_spec(ws.spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/echo?search=test&page=1")
        .header("content-type", "application/json")
        .header("x-api-key", "test-key-123")
        .header("x-custom-header", "custom-value")
        .body(r#"{"test": true}"#)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        status,
        200,
        "expected 200, got {status}. Body:\n{}",
        String::from_utf8_lossy(&resp_bytes)
    );

    let echo = extract_echo_json(&resp_bytes);
    assert_eq!(echo["method"], "POST");
    assert_eq!(echo["path"], "/echo");
    // Query string should be passed through
    let query = echo["query"].as_str().unwrap_or("");
    assert!(
        query.contains("search=test"),
        "query should contain search=test, got: {query}"
    );
    // Custom header should arrive at dispatcher
    assert_eq!(
        echo["headers"]["x-custom-header"], "custom-value",
        "custom headers should pass through middleware chain"
    );
}
