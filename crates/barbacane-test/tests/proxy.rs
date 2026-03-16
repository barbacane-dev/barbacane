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

#[tokio::test]
async fn test_http_upstream_get() {
    let gateway = TestGateway::from_spec(&fixture("http-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    // Proxy GET request to httpbin.org/get
    let resp = gateway.get("/proxy/get").await.unwrap();

    // httpbin.org/get returns 200 with JSON containing request details
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("url").is_some(),
        "response should contain url field"
    );
}

#[tokio::test]
async fn test_http_upstream_post() {
    let gateway = TestGateway::from_spec(&fixture("http-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    // Proxy POST request to httpbin.org/post
    let resp = gateway
        .post("/proxy/post", r#"{"test":"data"}"#)
        .await
        .unwrap();

    // httpbin.org/post returns 200 with JSON containing request details
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

#[tokio::test]
async fn test_http_upstream_headers_forwarded() {
    let gateway = TestGateway::from_spec(&fixture("http-upstream.yaml"))
        .await
        .expect("failed to start gateway");

    // httpbin.org/headers returns the request headers
    let resp = gateway.get("/proxy/headers").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let headers = &body["headers"];

    // Should have X-Forwarded-Host header
    assert!(
        headers.get("X-Forwarded-Host").is_some() || headers.get("x-forwarded-host").is_some(),
        "should forward X-Forwarded-Host header"
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
              test-key-123:
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
              test-key-123:
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
              test-key-123:
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
              test-key-123:
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
              test-key-123:
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
              test-key-123:
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
