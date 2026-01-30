//! TestGateway: full-stack integration test harness.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;
use thiserror::Error;

use barbacane_compiler::{compile_with_manifest, CompileOptions, ProjectManifest};

/// Errors from TestGateway operations.
#[derive(Debug, Error)]
pub enum TestError {
    #[error("compilation failed: {0}")]
    Compile(#[from] barbacane_compiler::CompileError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("gateway failed to start: {0}")]
    StartupFailed(String),

    #[error("gateway binary not found at {0}")]
    BinaryNotFound(String),
}

/// Full-stack test harness.
///
/// Compiles a spec into an in-memory artifact, boots the data plane
/// on a random port, and provides HTTP request helpers.
pub struct TestGateway {
    /// The child process running the gateway.
    child: Child,
    /// The port the gateway is listening on.
    port: u16,
    /// HTTP client for making requests.
    client: reqwest::Client,
    /// Temp directory holding the artifact (kept alive for the test duration).
    _temp_dir: TempDir,
}

impl TestGateway {
    /// Create a TestGateway from a spec YAML/JSON file.
    pub async fn from_spec(spec_path: &str) -> Result<Self, TestError> {
        Self::from_specs(&[spec_path]).await
    }

    /// Create a TestGateway from multiple spec files.
    pub async fn from_specs(spec_paths: &[&str]) -> Result<Self, TestError> {
        // Create temp directory for the artifact
        let temp_dir = TempDir::new()?;
        let artifact_path = temp_dir.path().join("test.bca");

        // Find the barbacane.yaml manifest (look in the spec's directory)
        let first_spec = Path::new(spec_paths[0]);
        let spec_dir = first_spec.parent().unwrap_or(Path::new("."));
        let manifest_path = spec_dir.join("barbacane.yaml");

        if !manifest_path.exists() {
            return Err(TestError::StartupFailed(format!(
                "barbacane.yaml manifest not found in {}",
                spec_dir.display()
            )));
        }

        // Load the project manifest
        let project_manifest = ProjectManifest::load(&manifest_path)?;

        // Compile the specs with manifest
        let paths: Vec<&Path> = spec_paths.iter().map(|s| Path::new(*s)).collect();
        let options = CompileOptions::default();
        compile_with_manifest(
            &paths,
            &project_manifest,
            spec_dir,
            &artifact_path,
            &options,
        )?;

        // Find the barbacane binary
        let binary_path = find_barbacane_binary()?;

        // Find an available port
        let port = find_available_port()?;

        // Start the gateway process
        let child = Command::new(&binary_path)
            .arg("serve")
            .arg("--artifact")
            .arg(&artifact_path)
            .arg("--listen")
            .arg(format!("127.0.0.1:{}", port))
            .arg("--dev")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let client = reqwest::Client::new();

        let mut gateway = TestGateway {
            child,
            port,
            client,
            _temp_dir: temp_dir,
        };

        // Wait for the gateway to be ready
        gateway.wait_for_ready().await?;

        Ok(gateway)
    }

    /// Wait for the gateway to be ready by polling the health endpoint.
    async fn wait_for_ready(&mut self) -> Result<(), TestError> {
        let health_url = format!("http://127.0.0.1:{}/__barbacane/health", self.port);
        let max_attempts = 50;
        let delay = Duration::from_millis(100);

        for _ in 0..max_attempts {
            if let Ok(resp) = self.client.get(&health_url).send().await {
                if resp.status().is_success() {
                    return Ok(());
                }
            }

            // Check if the process has exited
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(TestError::StartupFailed(format!(
                    "gateway exited with status: {}",
                    status
                )));
            }

            tokio::time::sleep(delay).await;
        }

        Err(TestError::StartupFailed(
            "gateway did not become ready in time".to_string(),
        ))
    }

    /// Get the port the gateway is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the base URL of the gateway.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Make a GET request to the given path.
    pub async fn get(&self, path: &str) -> Result<reqwest::Response, TestError> {
        let url = format!("{}{}", self.base_url(), path);
        Ok(self.client.get(&url).send().await?)
    }

    /// Make a POST request to the given path.
    pub async fn post(&self, path: &str, body: &str) -> Result<reqwest::Response, TestError> {
        let url = format!("{}{}", self.base_url(), path);
        Ok(self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await?)
    }

    /// Make a request with any method.
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::Response, TestError> {
        let url = format!("{}{}", self.base_url(), path);
        Ok(self.client.request(method, &url).send().await?)
    }

    /// Make a PUT request to the given path.
    pub async fn put(&self, path: &str, body: &str) -> Result<reqwest::Response, TestError> {
        let url = format!("{}{}", self.base_url(), path);
        Ok(self
            .client
            .put(&url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await?)
    }

    /// Make a PUT request with custom headers.
    pub async fn put_with_headers(
        &self,
        path: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Result<reqwest::Response, TestError> {
        let url = format!("{}{}", self.base_url(), path);
        let mut req = self
            .client
            .put(&url)
            .header("content-type", "application/json")
            .body(body.to_string());

        for (key, value) in headers {
            req = req.header(*key, *value);
        }

        Ok(req.send().await?)
    }

    /// Make a POST request with custom content type.
    pub async fn post_with_content_type(
        &self,
        path: &str,
        body: &str,
        content_type: &str,
    ) -> Result<reqwest::Response, TestError> {
        let url = format!("{}{}", self.base_url(), path);
        Ok(self
            .client
            .post(&url)
            .header("content-type", content_type)
            .body(body.to_string())
            .send()
            .await?)
    }
}

impl Drop for TestGateway {
    fn drop(&mut self) {
        // Kill the child process
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Find the barbacane binary in the target directory.
fn find_barbacane_binary() -> Result<String, TestError> {
    // Try debug build first, then release
    let candidates = [
        "target/debug/barbacane",
        "target/release/barbacane",
        "../target/debug/barbacane",
        "../target/release/barbacane",
        "../../target/debug/barbacane",
        "../../target/release/barbacane",
    ];

    for path in candidates {
        if Path::new(path).exists() {
            return Ok(path.to_string());
        }
    }

    // Try using cargo to find the binary
    if let Ok(output) = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()
    {
        if output.status.success() {
            if let Ok(meta) = String::from_utf8(output.stdout) {
                if let Some(target_dir) = meta.split("\"target_directory\":\"").nth(1) {
                    if let Some(dir) = target_dir.split('"').next() {
                        let debug_path = format!("{}/debug/barbacane", dir);
                        if Path::new(&debug_path).exists() {
                            return Ok(debug_path);
                        }
                        let release_path = format!("{}/release/barbacane", dir);
                        if Path::new(&release_path).exists() {
                            return Ok(release_path);
                        }
                    }
                }
            }
        }
    }

    Err(TestError::BinaryNotFound(
        "target/debug/barbacane or target/release/barbacane".to_string(),
    ))
}

/// Find an available TCP port.
fn find_available_port() -> Result<u16, TestError> {
    // Bind to port 0 to get an OS-assigned port
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gateway_health() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "healthy");
    }

    #[tokio::test]
    async fn test_gateway_mock_response() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/health").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn test_gateway_404() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/nonexistent").await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn test_gateway_405() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request(reqwest::Method::DELETE, "/health")
            .await
            .unwrap();
        assert_eq!(resp.status(), 405);

        // Check Allow header
        let allow = resp.headers().get("allow").unwrap().to_str().unwrap();
        assert!(allow.contains("GET"));
    }

    #[tokio::test]
    async fn test_gateway_path_params() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/users/123").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["name"], "Alice");
    }

    // ========================
    // M2: Validation Tests
    // ========================

    #[tokio::test]
    async fn test_validation_missing_required_body() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
            .await
            .expect("failed to start gateway");

        // GET /health has no validation requirements
        let resp = gateway.get("/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_validation_optional_query_params() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
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

    // ========================
    // M4: HTTP Upstream Tests
    // ========================

    #[tokio::test]
    async fn test_http_upstream_get() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/http-upstream.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/http-upstream.yaml")
            .await
            .expect("failed to start gateway");

        // Proxy POST request to httpbin.org/post
        let resp = gateway
            .post("/proxy/post", r#"{"test":"data"}"#)
            .await
            .unwrap();

        // httpbin.org/post returns 200 with JSON containing request details
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body.get("json").is_some(),
            "response should contain json field"
        );
        assert_eq!(body["json"]["test"], "data");
    }

    #[tokio::test]
    async fn test_http_upstream_headers_forwarded() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/http-upstream.yaml")
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
}
