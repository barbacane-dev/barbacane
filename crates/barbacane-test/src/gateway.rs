//! TestGateway: full-stack integration test harness.

use std::io::Write;
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
    /// Whether TLS is enabled.
    tls_enabled: bool,
}

/// Generated TLS certificates for testing.
pub struct TestCertificates {
    /// Path to the certificate file.
    pub cert_path: std::path::PathBuf,
    /// Path to the private key file.
    pub key_path: std::path::PathBuf,
    /// Root CA certificate for client verification.
    pub root_cert: rustls::pki_types::CertificateDer<'static>,
}

/// Generate self-signed test certificates.
pub fn generate_test_certificates(temp_dir: &Path) -> Result<TestCertificates, TestError> {
    use rcgen::{generate_simple_self_signed, CertifiedKey};

    // Install the default crypto provider for rustls (required before any TLS operations).
    // This may fail if already installed, which is fine - we ignore the error.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // Generate self-signed certificate for localhost
    let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];

    let CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)
        .map_err(|e| TestError::StartupFailed(format!("failed to generate certificate: {}", e)))?;

    // Write certificate
    let cert_path = temp_dir.join("server.crt");
    let mut cert_file = std::fs::File::create(&cert_path)?;
    cert_file.write_all(cert.pem().as_bytes())?;

    // Write private key
    let key_path = temp_dir.join("server.key");
    let mut key_file = std::fs::File::create(&key_path)?;
    key_file.write_all(key_pair.serialize_pem().as_bytes())?;

    // Get the DER-encoded certificate for client trust
    let root_cert = rustls::pki_types::CertificateDer::from(cert.der().to_vec());

    Ok(TestCertificates {
        cert_path,
        key_path,
        root_cert,
    })
}

impl TestGateway {
    /// Create a TestGateway from a spec YAML/JSON file.
    pub async fn from_spec(spec_path: &str) -> Result<Self, TestError> {
        Self::from_specs(&[spec_path]).await
    }

    /// Create a TLS-enabled TestGateway from a spec YAML/JSON file.
    pub async fn from_spec_with_tls(spec_path: &str) -> Result<Self, TestError> {
        Self::from_specs_with_tls(&[spec_path]).await
    }

    /// Create a TestGateway from multiple spec files.
    pub async fn from_specs(spec_paths: &[&str]) -> Result<Self, TestError> {
        Self::create_gateway(spec_paths, false).await
    }

    /// Create a TLS-enabled TestGateway from multiple spec files.
    pub async fn from_specs_with_tls(spec_paths: &[&str]) -> Result<Self, TestError> {
        Self::create_gateway(spec_paths, true).await
    }

    /// Internal method to create a gateway with optional TLS.
    async fn create_gateway(spec_paths: &[&str], tls_enabled: bool) -> Result<Self, TestError> {
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

        // Generate TLS certificates if needed
        let tls_certs = if tls_enabled {
            Some(generate_test_certificates(temp_dir.path())?)
        } else {
            None
        };

        // Build the gateway command
        let mut cmd = Command::new(&binary_path);
        cmd.arg("serve")
            .arg("--artifact")
            .arg(&artifact_path)
            .arg("--listen")
            .arg(format!("127.0.0.1:{}", port))
            .arg("--dev")
            .arg("--allow-plaintext-upstream") // Allow HTTP calls to test mock servers
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Add TLS arguments if enabled
        if let Some(ref certs) = tls_certs {
            cmd.arg("--tls-cert").arg(&certs.cert_path);
            cmd.arg("--tls-key").arg(&certs.key_path);
        }

        // Start the gateway process
        let child = cmd.spawn()?;

        // Create HTTP client (with custom TLS config if needed)
        let client = if let Some(ref certs) = tls_certs {
            // Create a client that trusts our self-signed certificate
            let mut root_store = rustls::RootCertStore::empty();
            root_store.add(certs.root_cert.clone()).map_err(|e| {
                TestError::StartupFailed(format!("failed to add root cert: {:?}", e))
            })?;

            let tls_config = rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth();

            reqwest::Client::builder()
                .use_preconfigured_tls(tls_config)
                .build()?
        } else {
            reqwest::Client::new()
        };

        let mut gateway = TestGateway {
            child,
            port,
            client,
            _temp_dir: temp_dir,
            tls_enabled,
        };

        // Wait for the gateway to be ready
        gateway.wait_for_ready().await?;

        Ok(gateway)
    }

    /// Wait for the gateway to be ready by polling the health endpoint.
    async fn wait_for_ready(&mut self) -> Result<(), TestError> {
        let health_url = format!("{}/__barbacane/health", self.base_url());
        // Increase timeout for CI environments (15 seconds instead of 5)
        let max_attempts = 150;
        let delay = Duration::from_millis(100);

        for _ in 0..max_attempts {
            if let Ok(resp) = self.client.get(&health_url).send().await {
                if resp.status().is_success() {
                    return Ok(());
                }
            }

            // Check if the process has exited
            if let Ok(Some(status)) = self.child.try_wait() {
                // Try to read stderr to get the error message
                let stderr = self
                    .child
                    .stderr
                    .take()
                    .map(|mut s| {
                        let mut buf = String::new();
                        use std::io::Read;
                        let _ = s.read_to_string(&mut buf);
                        buf
                    })
                    .unwrap_or_default();

                return Err(TestError::StartupFailed(format!(
                    "gateway exited with status: {}\nstderr: {}",
                    status,
                    stderr.trim()
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
        let scheme = if self.tls_enabled { "https" } else { "http" };
        format!("{}://127.0.0.1:{}", scheme, self.port)
    }

    /// Check if TLS is enabled.
    pub fn is_tls_enabled(&self) -> bool {
        self.tls_enabled
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

    /// Create a request builder for customizing headers etc.
    pub fn request_builder(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url(), path);
        self.client.request(method, &url)
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

/// Assert response status matches expected, printing body on failure for debugging.
#[allow(clippy::panic)]
pub async fn assert_status(resp: reqwest::Response, expected: u16) {
    let status = resp.status().as_u16();
    if status != expected {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<error reading body: {}>", e));
        panic!(
            "Expected status {} but got {}. Response body:\n{}",
            expected, status, body
        );
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
        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            panic!("Expected 200 but got {}. Body: {}", status, body);
        }
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

    // ========================
    // M6a: TLS Termination Tests
    // ========================

    #[tokio::test]
    async fn test_tls_gateway_health() {
        let gateway = TestGateway::from_spec_with_tls("../../tests/fixtures/minimal.yaml")
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
        let gateway = TestGateway::from_spec_with_tls("../../tests/fixtures/minimal.yaml")
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
        let gateway = TestGateway::from_spec_with_tls("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start TLS gateway");

        // 404 response over HTTPS
        let resp = gateway.get("/nonexistent").await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    // JWT Authentication Tests

    /// Helper to create a JWT token for testing.
    /// Creates unsigned tokens since we use skip_signature_validation: true in tests.
    fn create_test_jwt(sub: &str, iss: &str, aud: &str, exp: u64, nbf: Option<u64>) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

        let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
        let mut claims = serde_json::json!({
            "sub": sub,
            "iss": iss,
            "aud": aud,
            "exp": exp
        });
        if let Some(nbf_val) = nbf {
            claims["nbf"] = serde_json::json!(nbf_val);
        }

        let header_b64 = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());
        let claims_b64 = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
        // Signature is just filler since we skip validation
        let sig_b64 = URL_SAFE_NO_PAD.encode(b"test_signature");

        format!("{}.{}.{}", header_b64, claims_b64, sig_b64)
    }

    /// Get current Unix timestamp.
    fn now_timestamp() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[tokio::test]
    async fn test_jwt_auth_missing_token() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Request without Authorization header should get 401
        let resp = gateway.get("/protected").await.unwrap();
        assert_eq!(resp.status(), 401);

        // Check WWW-Authenticate header
        let www_auth = resp.headers().get("www-authenticate");
        assert!(www_auth.is_some(), "expected WWW-Authenticate header");
        let www_auth_val = www_auth.unwrap().to_str().unwrap();
        assert!(
            www_auth_val.contains("Bearer"),
            "expected Bearer scheme in WWW-Authenticate"
        );
    }

    #[tokio::test]
    async fn test_jwt_auth_invalid_header_format() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Request with invalid Authorization format should get 401
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", "Basic dXNlcjpwYXNz")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn test_jwt_auth_malformed_token() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Request with malformed token should get 401
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", "Bearer not.a.valid.jwt")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn test_jwt_auth_valid_token() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Create a valid token
        let exp = now_timestamp() + 3600; // Expires in 1 hour
        let token = create_test_jwt("user-123", "test-issuer", "test-audience", exp, None);

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "Access granted");
    }

    #[tokio::test]
    async fn test_jwt_auth_expired_token() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Create an expired token (expired 2 minutes ago, beyond 60s clock skew)
        let exp = now_timestamp() - 120;
        let token = create_test_jwt("user-123", "test-issuer", "test-audience", exp, None);

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        // Check error response body
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap_or("").contains("expired"));
    }

    #[tokio::test]
    async fn test_jwt_auth_not_yet_valid() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Create a token that's not valid yet (starts 2 minutes from now)
        let exp = now_timestamp() + 3600;
        let nbf = now_timestamp() + 120;
        let token = create_test_jwt("user-123", "test-issuer", "test-audience", exp, Some(nbf));

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"]
            .as_str()
            .unwrap_or("")
            .contains("not yet valid"));
    }

    #[tokio::test]
    async fn test_jwt_auth_invalid_issuer() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Create a token with wrong issuer
        let exp = now_timestamp() + 3600;
        let token = create_test_jwt("user-123", "wrong-issuer", "test-audience", exp, None);

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap_or("").contains("issuer"));
    }

    #[tokio::test]
    async fn test_jwt_auth_invalid_audience() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Create a token with wrong audience
        let exp = now_timestamp() + 3600;
        let token = create_test_jwt("user-123", "test-issuer", "wrong-audience", exp, None);

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap_or("").contains("audience"));
    }

    #[tokio::test]
    async fn test_jwt_auth_public_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/jwt-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Public endpoint should work without a token
        let resp = gateway.get("/public").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "Public access");
    }

    // ==================== API Key Auth Tests ====================

    #[tokio::test]
    async fn test_apikey_auth_valid_key() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/apikey-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("X-API-Key", "test-key-123")
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            panic!("Expected 200 but got {}. Body: {}", status, body);
        }
    }

    #[tokio::test]
    async fn test_apikey_auth_missing_key() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/apikey-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Request without API key
        let resp = gateway.get("/protected").await.unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap_or("").contains("required"));
    }

    #[tokio::test]
    async fn test_apikey_auth_invalid_key() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/apikey-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("X-API-Key", "wrong-key")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap_or("").contains("Invalid"));
    }

    #[tokio::test]
    async fn test_apikey_auth_public_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/apikey-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Public endpoint should work without a key
        let resp = gateway.get("/public").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "Public access");
    }

    #[tokio::test]
    async fn test_apikey_auth_query_param_valid() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/apikey-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .get("/query-auth?api_key=query-key-789")
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "Query auth granted");
    }

    #[tokio::test]
    async fn test_apikey_auth_query_param_missing() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/apikey-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Missing query param
        let resp = gateway.get("/query-auth").await.unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap_or("").contains("required"));
    }

    // ==================== Basic Auth Tests ====================

    /// Encode username:password as base64 for Basic auth header.
    fn basic_auth_header(username: &str, password: &str) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let encoded = STANDARD.encode(format!("{}:{}", username, password));
        format!("Basic {}", encoded)
    }

    #[tokio::test]
    async fn test_basic_auth_valid_credentials() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/basic-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", basic_auth_header("admin", "secret123"))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            panic!("Expected 200 but got {}. Body: {}", status, body);
        }
    }

    #[tokio::test]
    async fn test_basic_auth_valid_credentials_reader() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/basic-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", basic_auth_header("reader", "readonly456"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_basic_auth_missing_header() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/basic-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/protected").await.unwrap();
        assert_eq!(resp.status(), 401);

        // Verify WWW-Authenticate header
        let www_auth = resp
            .headers()
            .get("www-authenticate")
            .expect("missing WWW-Authenticate");
        let www_auth_str = www_auth.to_str().unwrap();
        assert!(www_auth_str.starts_with("Basic realm=\"test-api\""));

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:authentication-failed");
        assert!(body["detail"]
            .as_str()
            .unwrap_or("")
            .contains("credentials required"));
    }

    #[tokio::test]
    async fn test_basic_auth_invalid_credentials() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/basic-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", basic_auth_header("admin", "wrongpassword"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"]
            .as_str()
            .unwrap_or("")
            .contains("Invalid username or password"));
    }

    #[tokio::test]
    async fn test_basic_auth_unknown_user() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/basic-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header(
                "Authorization",
                basic_auth_header("nonexistent", "anything"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn test_basic_auth_malformed_header() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/basic-auth.yaml")
            .await
            .expect("failed to start gateway");

        // Wrong auth scheme
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", "Bearer some-token")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn test_basic_auth_public_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/basic-auth.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/public").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "Public access");
    }

    // ==================== OAuth2 Auth Tests ====================

    /// Create a temporary spec file for OAuth2 auth testing with dynamic introspection URL.
    fn create_oauth2_spec(introspection_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let spec_path = temp_dir.path().join("oauth2-auth.yaml");

        // Get absolute paths to plugins (relative to this test file's location)
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let plugins_dir = manifest_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("plugins");
        let mock_path = plugins_dir.join("mock/mock.wasm");
        let oauth2_path = plugins_dir.join("oauth2-auth/oauth2-auth.wasm");

        // Create barbacane.yaml manifest in temp dir
        let manifest_path = temp_dir.path().join("barbacane.yaml");
        let manifest_content = format!(
            r#"# Test manifest for OAuth2 auth tests

plugins:
  mock:
    path: {}
  oauth2-auth:
    path: {}
"#,
            mock_path.display(),
            oauth2_path.display()
        );
        std::fs::write(&manifest_path, manifest_content).expect("failed to write manifest file");

        let spec_content = format!(
            r#"openapi: "3.0.3"
info:
  title: OAuth2 Auth Test API
  version: "1.0.0"
  description: API for testing OAuth2 token introspection middleware

x-barbacane-middlewares:
  - name: oauth2-auth
    config:
      introspection_endpoint: "{}"
      client_id: "test-client"
      client_secret: "test-secret"
      timeout: 5

paths:
  /protected:
    get:
      summary: Protected endpoint requiring OAuth2 auth
      operationId: getProtected
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "Access granted"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
        "401":
          description: Unauthorized
        "403":
          description: Forbidden

  /scoped:
    get:
      summary: Endpoint requiring specific scopes
      operationId: getScoped
      x-barbacane-middlewares:
        - name: oauth2-auth
          config:
            introspection_endpoint: "{}"
            client_id: "test-client"
            client_secret: "test-secret"
            required_scopes: "read write"
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "Scoped access granted"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
        "403":
          description: Forbidden

  /public:
    get:
      summary: Public endpoint (no auth)
      operationId: getPublic
      x-barbacane-middlewares: []
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "Public access"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
"#,
            introspection_url, introspection_url
        );

        std::fs::write(&spec_path, spec_content).expect("failed to write spec file");
        (temp_dir, spec_path)
    }

    #[tokio::test]
    async fn test_oauth2_auth_valid_token() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Start mock introspection server
        let mock_server = MockServer::start().await;

        // Mock active token response
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .and(body_string_contains("token=valid-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "sub": "user-123",
                "scope": "read write",
                "client_id": "my-client"
            })))
            .mount(&mock_server)
            .await;

        let introspection_url = format!("{}/introspect", mock_server.uri());
        let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", "Bearer valid-token")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "Access granted");
    }

    #[tokio::test]
    async fn test_oauth2_auth_missing_token() {
        use wiremock::MockServer;

        // Start mock introspection server (won't be called)
        let mock_server = MockServer::start().await;
        let introspection_url = format!("{}/introspect", mock_server.uri());
        let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        // Request without token
        let resp = gateway.get("/protected").await.unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"]
            .as_str()
            .unwrap_or("")
            .contains("Bearer token required"));
    }

    #[tokio::test]
    async fn test_oauth2_auth_inactive_token() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Mock inactive token response
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": false
            })))
            .mount(&mock_server)
            .await;

        let introspection_url = format!("{}/introspect", mock_server.uri());
        let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", "Bearer invalid-token")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap_or("").contains("not active"));
    }

    #[tokio::test]
    async fn test_oauth2_auth_insufficient_scope() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Mock active token with only "read" scope (missing "write")
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "sub": "user-123",
                "scope": "read"  // Missing "write" scope
            })))
            .mount(&mock_server)
            .await;

        let introspection_url = format!("{}/introspect", mock_server.uri());
        let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        // Access scoped endpoint (requires "read write")
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/scoped")
            .header("Authorization", "Bearer token-with-read-only")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap_or("").contains("scope"));
    }

    #[tokio::test]
    async fn test_oauth2_auth_public_endpoint() {
        use wiremock::MockServer;

        let mock_server = MockServer::start().await;
        let introspection_url = format!("{}/introspect", mock_server.uri());
        let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        // Public endpoint should work without a token
        let resp = gateway.get("/public").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "Public access");
    }

    // ==================== Secrets Tests ====================

    /// Create a temporary spec file for secrets testing using oauth2-auth.
    /// The oauth2-auth plugin has secrets as values (client_secret), making it ideal for testing.
    fn create_oauth2_secrets_spec(
        temp_dir: &std::path::Path,
        introspection_url: &str,
        client_secret: &str,
    ) -> std::path::PathBuf {
        let spec_path = temp_dir.join("secrets-test.yaml");

        // Get absolute paths to plugins
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let plugins_dir = manifest_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("plugins");
        let mock_path = plugins_dir.join("mock/mock.wasm");
        let oauth2_path = plugins_dir.join("oauth2-auth/oauth2-auth.wasm");

        // Create barbacane.yaml manifest
        let manifest_path = temp_dir.join("barbacane.yaml");
        let manifest_content = format!(
            r#"plugins:
  mock:
    path: {}
  oauth2-auth:
    path: {}
"#,
            mock_path.display(),
            oauth2_path.display()
        );
        std::fs::write(&manifest_path, manifest_content).expect("failed to write manifest");

        // Create the spec with the provided client_secret (which may be a secret reference)
        let spec_content = format!(
            r#"openapi: "3.0.3"
info:
  title: Secrets Test API
  version: "1.0.0"

paths:
  /test:
    get:
      summary: Test endpoint
      operationId: test
      x-barbacane-middlewares:
        - name: oauth2-auth
          config:
            introspection_endpoint: "{}"
            client_id: test-client
            client_secret: "{}"
            timeout: 5.0
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "success"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
"#,
            introspection_url, client_secret
        );

        std::fs::write(&spec_path, spec_content).expect("failed to write spec");
        spec_path
    }

    #[tokio::test]
    async fn test_secrets_env_reference_resolved() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Set up environment variable for the secret
        std::env::set_var("TEST_CLIENT_SECRET", "my-secret-value");

        // Start mock introspection server
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "sub": "user-123"
            })))
            .mount(&mock_server)
            .await;

        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let introspection_url = format!("{}/introspect", mock_server.uri());
        let spec_path = create_oauth2_secrets_spec(
            temp_dir.path(),
            &introspection_url,
            "env://TEST_CLIENT_SECRET",
        );

        // Gateway should start successfully with the resolved secret
        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway with env secret");

        // Make a request with a valid token
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/test")
            .header("Authorization", "Bearer test-token")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Clean up
        std::env::remove_var("TEST_CLIENT_SECRET");
    }

    #[tokio::test]
    async fn test_secrets_file_reference_resolved() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Start mock introspection server
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "sub": "user-123"
            })))
            .mount(&mock_server)
            .await;

        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");

        // Create a secret file
        let secret_file = temp_dir.path().join("client-secret.txt");
        std::fs::write(&secret_file, "file-based-secret\n").expect("failed to write secret file");

        // Use file:// reference
        let secret_ref = format!("file://{}", secret_file.display());
        let introspection_url = format!("{}/introspect", mock_server.uri());
        let spec_path =
            create_oauth2_secrets_spec(temp_dir.path(), &introspection_url, &secret_ref);

        // Gateway should start successfully with the resolved secret
        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway with file secret");

        // Make a request with a valid token
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/test")
            .header("Authorization", "Bearer test-token")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_secrets_missing_env_var_fails_startup() {
        // Make sure the env var doesn't exist
        std::env::remove_var("NONEXISTENT_SECRET_VAR_12345");

        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let spec_path = create_oauth2_secrets_spec(
            temp_dir.path(),
            "http://localhost:9999/introspect",
            "env://NONEXISTENT_SECRET_VAR_12345",
        );

        // Gateway should fail to start
        let result = TestGateway::from_spec(spec_path.to_str().unwrap()).await;
        match result {
            Ok(_) => {
                panic!("gateway should fail with missing env var");
            }
            Err(e) => {
                let err_str = format!("{}", e);
                assert!(
                    err_str.contains("secret")
                        || err_str.contains("environment")
                        || err_str.contains("not found"),
                    "error should mention secrets or environment: {}",
                    err_str
                );
            }
        }
    }

    #[tokio::test]
    async fn test_secrets_missing_file_fails_startup() {
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let spec_path = create_oauth2_secrets_spec(
            temp_dir.path(),
            "http://localhost:9999/introspect",
            "file:///nonexistent/path/to/secret.txt",
        );

        // Gateway should fail to start
        let result = TestGateway::from_spec(spec_path.to_str().unwrap()).await;
        match result {
            Ok(_) => panic!("gateway should fail with missing file"),
            Err(e) => {
                let err_str = format!("{}", e);
                assert!(
                    err_str.contains("secret")
                        || err_str.contains("file")
                        || err_str.contains("not found"),
                    "error should mention secrets or file: {}",
                    err_str
                );
            }
        }
    }

    // ==================== Rate Limit Tests ====================

    #[tokio::test]
    async fn test_rate_limit_allows_within_quota() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/rate-limit.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/rate-limit.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/rate-limit.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/rate-limit.yaml")
            .await
            .expect("failed to start gateway");

        // Unlimited endpoint should always work
        for _ in 0..10 {
            let resp = gateway.get("/unlimited").await.unwrap();
            assert_eq!(resp.status(), 200);
        }
    }

    // ==================== Cache Tests ====================

    #[tokio::test]
    async fn test_cache_miss_then_hit() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cache.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/cache.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/cache.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/cache.yaml")
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

    // ==================== Telemetry Tests ====================

    #[tokio::test]
    async fn test_metrics_endpoint_returns_prometheus_format() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        // Make a request to generate some metrics
        let _ = gateway.get("/health").await.unwrap();

        // Get the metrics endpoint
        let resp = gateway.get("/__barbacane/metrics").await.unwrap();
        assert_eq!(resp.status(), 200);

        // Check content type is Prometheus format
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("text/plain"),
            "Expected Prometheus text format, got: {}",
            content_type
        );

        let body = resp.text().await.unwrap();

        // Should contain standard Barbacane metrics
        assert!(
            body.contains("barbacane_requests_total"),
            "Missing barbacane_requests_total metric"
        );
        assert!(
            body.contains("barbacane_request_duration_seconds"),
            "Missing barbacane_request_duration_seconds metric"
        );
        assert!(
            body.contains("barbacane_active_connections"),
            "Missing barbacane_active_connections metric"
        );
    }

    #[tokio::test]
    async fn test_metrics_records_request_counts() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        // Make several requests
        for _ in 0..3 {
            let resp = gateway.get("/health").await.unwrap();
            assert_eq!(resp.status(), 200);
        }

        // Get metrics
        let resp = gateway.get("/__barbacane/metrics").await.unwrap();
        let body = resp.text().await.unwrap();

        // Should have recorded the requests
        // The metric line format is: barbacane_requests_total{...} <count>
        assert!(
            body.contains("barbacane_requests_total"),
            "Metrics should contain request counter"
        );
    }

    #[tokio::test]
    async fn test_metrics_records_validation_failures() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/validation.yaml")
            .await
            .expect("failed to start gateway");

        // Make a request that will fail validation (missing required field)
        let _ = gateway.post("/validate", "{}").await.unwrap();

        // Get metrics
        let resp = gateway.get("/__barbacane/metrics").await.unwrap();
        let body = resp.text().await.unwrap();

        // Should have recorded validation failure
        assert!(
            body.contains("barbacane_validation_failures_total"),
            "Metrics should contain validation failure counter"
        );
    }

    #[tokio::test]
    async fn test_metrics_records_404_responses() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        // Make a request to non-existent endpoint
        let resp = gateway.get("/nonexistent").await.unwrap();
        assert_eq!(resp.status(), 404);

        // Get metrics
        let resp = gateway.get("/__barbacane/metrics").await.unwrap();
        let body = resp.text().await.unwrap();

        // Should have recorded the 404 request
        assert!(
            body.contains("barbacane_requests_total"),
            "Metrics should contain request counter"
        );
        // The metric should include status=404 label
        assert!(
            body.contains("status=\"404\""),
            "Metrics should record 404 status"
        );
    }

    #[tokio::test]
    async fn test_metrics_connection_tracking() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        // Make a request to establish connection
        let _ = gateway.get("/health").await.unwrap();

        // Get metrics
        let resp = gateway.get("/__barbacane/metrics").await.unwrap();
        let body = resp.text().await.unwrap();

        // Should have connection metrics
        assert!(
            body.contains("barbacane_connections_total"),
            "Metrics should contain connections_total counter"
        );
    }

    // ==================== API Lifecycle / Deprecation Tests ====================

    #[tokio::test]
    async fn test_deprecation_header_not_present_on_current_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/deprecated.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/deprecated.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/deprecated.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/deprecated.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
            .await
            .expect("failed to start gateway");

        // With valid pagination params
        let resp = gateway.get("/users?limit=10&offset=0").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_full_crud_list_users_invalid_limit() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
            .await
            .expect("failed to start gateway");

        // Limit exceeds maximum (100)
        let resp = gateway.get("/users?limit=200").await.unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_full_crud_create_user_valid() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
            .await
            .expect("failed to start gateway");

        // Invalid UUID format
        let resp = gateway.get("/users/not-a-uuid").await.unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_full_crud_update_user() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
        let gateway = TestGateway::from_spec("../../tests/fixtures/full-crud.yaml")
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
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
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
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
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
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
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
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
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

    // ==================== AsyncAPI / Event Dispatch Tests (M10) ====================

    #[tokio::test]
    async fn test_asyncapi_spec_compilation() {
        // Test that an AsyncAPI spec compiles and loads successfully
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway with AsyncAPI spec");

        // Health endpoint should work
        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        // Should have routes from AsyncAPI operations
        assert!(body.get("routes_count").is_some());
        let routes_count = body["routes_count"].as_u64().unwrap();
        assert!(
            routes_count >= 3,
            "expected at least 3 routes from AsyncAPI spec"
        );
    }

    #[tokio::test]
    async fn test_asyncapi_send_operation_via_post() {
        // AsyncAPI SEND operations should be accessible via HTTP POST
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        // POST to a SEND operation channel address
        let resp = gateway
            .post(
                "/events/users",
                r#"{"userId":"550e8400-e29b-41d4-a716-446655440000","email":"test@example.com"}"#,
            )
            .await
            .unwrap();

        // Should get 202 Accepted (mock dispatcher returns configured response)
        assert_eq!(resp.status(), 202);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "accepted");
    }

    #[tokio::test]
    async fn test_asyncapi_send_with_path_param() {
        // AsyncAPI channels with parameters should work with path params
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        // POST to channel with orderId path parameter
        let resp = gateway
            .post(
                "/events/orders/550e8400-e29b-41d4-a716-446655440000",
                r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000","items":[{"productId":"p1","quantity":2}]}"#,
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 202);
    }

    #[tokio::test]
    async fn test_asyncapi_message_validation_required_field() {
        // AsyncAPI message payloads should be validated against the schema
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        // Missing required 'email' field
        let resp = gateway
            .post(
                "/events/users",
                r#"{"userId":"550e8400-e29b-41d4-a716-446655440000"}"#,
            )
            .await
            .unwrap();

        // Should fail validation
        assert_eq!(resp.status(), 400);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
    }

    #[tokio::test]
    async fn test_asyncapi_message_validation_invalid_format() {
        // AsyncAPI message payloads should validate format constraints
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        // Invalid email format
        let resp = gateway
            .post(
                "/events/users",
                r#"{"userId":"550e8400-e29b-41d4-a716-446655440000","email":"not-an-email"}"#,
            )
            .await
            .unwrap();

        // Should fail validation
        assert_eq!(resp.status(), 400);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
    }

    #[tokio::test]
    async fn test_asyncapi_message_validation_invalid_uuid_path_param() {
        // AsyncAPI channel parameters should be validated
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        // Invalid UUID format for orderId path parameter
        let resp = gateway
            .post(
                "/events/orders/not-a-uuid",
                r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000","items":[{"productId":"p1","quantity":2}]}"#,
            )
            .await
            .unwrap();

        // Should fail validation
        assert_eq!(resp.status(), 400);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
    }

    #[tokio::test]
    async fn test_asyncapi_404_unknown_channel() {
        // Requests to unknown channels should return 404
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .post("/events/unknown", r#"{"data":"test"}"#)
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn test_asyncapi_405_wrong_method() {
        // GET on a SEND-only channel should return 405
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/events/users").await.unwrap();

        assert_eq!(resp.status(), 405);

        // Check Allow header indicates POST
        let allow = resp.headers().get("allow").unwrap().to_str().unwrap();
        assert!(allow.contains("POST"));
    }

    #[tokio::test]
    async fn test_asyncapi_simple_notification() {
        // Test a simple channel without path parameters
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        // Notification only requires 'title'
        let resp = gateway
            .post("/notifications", r#"{"title":"Hello World"}"#)
            .await
            .unwrap();

        assert_eq!(resp.status(), 202);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "accepted");
    }

    #[tokio::test]
    async fn test_asyncapi_notification_missing_required() {
        // Test validation failure for missing required field
        let gateway = TestGateway::from_spec("../../tests/fixtures/asyncapi-events.yaml")
            .await
            .expect("failed to start gateway");

        // Missing required 'title' field
        let resp = gateway
            .post("/notifications", r#"{"body":"Some body text"}"#)
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
    }

    // ========================
    // Specs Endpoint Tests
    // ========================

    #[tokio::test]
    async fn test_specs_index_response() {
        // Test the /__barbacane/specs endpoint returns correct JSON structure
        let gateway = TestGateway::from_specs(&[
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
        ])
        .await
        .expect("failed to start gateway");

        let resp = gateway.get("/__barbacane/specs").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();

        // Check openapi section exists with correct structure
        assert!(body.get("openapi").is_some());
        assert!(body["openapi"]["specs"].is_array());
        assert_eq!(body["openapi"]["count"], 2);
        assert_eq!(body["openapi"]["merged_url"], "/__barbacane/specs/openapi");

        // Check asyncapi section exists (should be empty for these fixtures)
        assert!(body.get("asyncapi").is_some());
        assert!(body["asyncapi"]["specs"].is_array());
        assert_eq!(body["asyncapi"]["count"], 0);
    }

    #[tokio::test]
    async fn test_specs_merged_openapi() {
        // Test merged OpenAPI from multiple specs
        let gateway = TestGateway::from_specs(&[
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
        ])
        .await
        .expect("failed to start gateway");

        let resp = gateway.get("/__barbacane/specs/openapi").await.unwrap();
        assert_eq!(resp.status(), 200);

        // Check content-type is YAML by default
        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("yaml"),
            "expected yaml content-type, got: {}",
            content_type
        );

        // Parse the YAML response
        let body = resp.text().await.unwrap();
        let spec: serde_json::Value =
            serde_yaml::from_str(&body).expect("failed to parse merged spec");

        // Check it's a valid OpenAPI spec
        assert!(
            spec.get("openapi").is_some(),
            "merged spec should have openapi field"
        );

        // Check paths from both specs are merged
        let paths = spec.get("paths").and_then(|p| p.as_object());
        assert!(paths.is_some(), "merged spec should have paths");
        let paths = paths.unwrap();

        // Check paths from users.yaml
        assert!(paths.contains_key("/users"), "should contain /users path");
        assert!(
            paths.contains_key("/users/{userId}"),
            "should contain /users/{{userId}} path"
        );

        // Check paths from orders.yaml
        assert!(paths.contains_key("/orders"), "should contain /orders path");
        assert!(
            paths.contains_key("/orders/{orderId}"),
            "should contain /orders/{{orderId}} path"
        );
    }

    #[tokio::test]
    async fn test_specs_merged_openapi_strips_extensions() {
        // Test that merged specs strip x-barbacane-* extensions
        let gateway = TestGateway::from_specs(&[
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
        ])
        .await
        .expect("failed to start gateway");

        let resp = gateway.get("/__barbacane/specs/openapi").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.text().await.unwrap();
        let spec: serde_json::Value =
            serde_yaml::from_str(&body).expect("failed to parse merged spec");

        // Check that x-barbacane-dispatch is NOT in the merged spec
        let spec_str = serde_json::to_string(&spec).unwrap();
        assert!(
            !spec_str.contains("x-barbacane-"),
            "merged spec should not contain x-barbacane-* extensions"
        );
    }

    #[tokio::test]
    async fn test_specs_individual_file() {
        // Test individual spec endpoint
        let gateway = TestGateway::from_specs(&[
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
        ])
        .await
        .expect("failed to start gateway");

        let resp = gateway.get("/__barbacane/specs/users.yaml").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.text().await.unwrap();
        let spec: serde_json::Value = serde_yaml::from_str(&body).expect("failed to parse spec");

        // Check it's the users spec
        assert_eq!(
            spec.pointer("/info/title"),
            Some(&serde_json::json!("Users Service API"))
        );

        // Check paths are present
        assert!(
            spec.pointer("/paths/~1users").is_some(),
            "should have /users path"
        );
    }

    #[tokio::test]
    async fn test_specs_individual_strips_extensions() {
        // Test that individual specs strip x-barbacane-* extensions
        let gateway = TestGateway::from_specs(&[
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
        ])
        .await
        .expect("failed to start gateway");

        let resp = gateway.get("/__barbacane/specs/users.yaml").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.text().await.unwrap();

        // x-barbacane-dispatch should be stripped
        assert!(
            !body.contains("x-barbacane-"),
            "individual spec should not contain x-barbacane-* extensions"
        );
    }

    #[tokio::test]
    async fn test_specs_preserves_sunset_extension() {
        // Test that x-sunset (RFC 8594) is preserved
        let gateway = TestGateway::from_spec("../../tests/fixtures/deprecated.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .get("/__barbacane/specs/deprecated.yaml")
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.text().await.unwrap();
        let spec: serde_json::Value = serde_yaml::from_str(&body).expect("failed to parse spec");

        // x-sunset should be preserved (it's a standard extension per RFC 8594)
        let spec_str = serde_json::to_string(&spec).unwrap();
        assert!(
            spec_str.contains("x-sunset"),
            "x-sunset extension should be preserved"
        );

        // But x-barbacane-* should be stripped
        assert!(
            !spec_str.contains("x-barbacane-"),
            "x-barbacane-* extensions should be stripped"
        );
    }

    #[tokio::test]
    async fn test_specs_format_json() {
        // Test ?format=json query parameter
        let gateway = TestGateway::from_specs(&[
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
        ])
        .await
        .expect("failed to start gateway");

        let resp = gateway
            .get("/__barbacane/specs/openapi?format=json")
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Check content-type is JSON
        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("json"),
            "expected json content-type, got: {}",
            content_type
        );

        // Parse as JSON (not YAML)
        let body: serde_json::Value = resp.json().await.expect("should parse as JSON");
        assert!(body.get("openapi").is_some());
    }

    #[tokio::test]
    async fn test_specs_format_yaml_explicit() {
        // Test ?format=yaml query parameter (explicit)
        let gateway = TestGateway::from_specs(&[
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
        ])
        .await
        .expect("failed to start gateway");

        let resp = gateway
            .get("/__barbacane/specs/openapi?format=yaml")
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Check content-type is YAML
        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("yaml"),
            "expected yaml content-type, got: {}",
            content_type
        );
    }

    #[tokio::test]
    async fn test_specs_individual_format_json() {
        // Test format=json for individual spec file
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .get("/__barbacane/specs/minimal.yaml?format=json")
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Check content-type is JSON
        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("json"),
            "expected json content-type, got: {}",
            content_type
        );

        // Should parse as JSON
        let _body: serde_json::Value = resp.json().await.expect("should parse as JSON");
    }

    #[tokio::test]
    async fn test_specs_not_found() {
        // Test 404 for non-existent spec file
        let gateway = TestGateway::from_spec("../../tests/fixtures/minimal.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .get("/__barbacane/specs/nonexistent.yaml")
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn test_specs_merged_asyncapi_empty() {
        // Test merged AsyncAPI endpoint when there are no AsyncAPI specs
        let gateway = TestGateway::from_specs(&[
            "../../tests/fixtures/multi-spec/users.yaml",
            "../../tests/fixtures/multi-spec/orders.yaml",
        ])
        .await
        .expect("failed to start gateway");

        // Should return 404 when no AsyncAPI specs exist
        let resp = gateway.get("/__barbacane/specs/asyncapi").await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    // =========================================================================
    // Correlation ID Middleware Tests
    // =========================================================================

    /// Create a temporary spec file for correlation-id testing.
    fn create_correlation_id_spec() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let spec_path = temp_dir.path().join("correlation-id.yaml");

        // Get absolute paths to plugins
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let plugins_dir = manifest_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("plugins");
        let mock_path = plugins_dir.join("mock/mock.wasm");
        let correlation_id_path = plugins_dir.join("correlation-id/correlation-id.wasm");

        // Create barbacane.yaml manifest in temp dir
        let manifest_path = temp_dir.path().join("barbacane.yaml");
        let manifest_content = format!(
            r#"# Test manifest for correlation-id tests

plugins:
  mock:
    path: {}
  correlation-id:
    path: {}
"#,
            mock_path.display(),
            correlation_id_path.display()
        );
        std::fs::write(&manifest_path, manifest_content).expect("failed to write manifest file");

        let spec_content = r#"openapi: "3.0.3"
info:
  title: Correlation ID Test API
  version: "1.0.0"
  description: API for testing correlation ID middleware

x-barbacane-middlewares:
  - name: correlation-id
    config:
      header_name: "x-correlation-id"
      generate_if_missing: true
      trust_incoming: true
      include_in_response: true

paths:
  /test:
    get:
      summary: Test endpoint
      operationId: getTest
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"message": "OK"}'
          content_type: application/json
      responses:
        "200":
          description: Success
"#;

        std::fs::write(&spec_path, spec_content).expect("failed to write spec file");
        (temp_dir, spec_path)
    }

    #[tokio::test]
    async fn test_correlation_id_generates_when_missing() {
        let (_temp_dir, spec_path) = create_correlation_id_spec();
        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        // Make request without correlation ID header
        let resp = gateway.get("/test").await.unwrap();

        let status = resp.status();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            panic!("Expected 200, got {} with body: {}", status, body);
        }

        // Should have generated a correlation ID in response
        let all_headers: Vec<_> = resp
            .headers()
            .iter()
            .map(|(k, v)| format!("{}: {:?}", k, v))
            .collect();
        let correlation_id = resp.headers().get("x-correlation-id");
        assert!(
            correlation_id.is_some(),
            "Expected x-correlation-id header in response. All headers: {:?}",
            all_headers
        );

        // Verify it's a valid UUID v7 format (36 chars, version 7)
        let id = correlation_id.unwrap().to_str().unwrap();
        assert_eq!(id.len(), 36, "UUID should be 36 characters");
        assert_eq!(
            id.chars().nth(14),
            Some('7'),
            "Should be UUID v7 (version marker at position 14)"
        );
    }

    #[tokio::test]
    async fn test_correlation_id_preserves_incoming() {
        let (_temp_dir, spec_path) = create_correlation_id_spec();
        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        // Make request with existing correlation ID
        let incoming_id = "my-custom-correlation-id-12345";
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/test")
            .header("x-correlation-id", incoming_id)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        // Should preserve the incoming correlation ID
        let correlation_id = resp.headers().get("x-correlation-id");
        assert!(
            correlation_id.is_some(),
            "Expected x-correlation-id header in response"
        );
        assert_eq!(
            correlation_id.unwrap().to_str().unwrap(),
            incoming_id,
            "Should preserve incoming correlation ID"
        );
    }

    // =========================================================================
    // CORS Middleware Tests (Automatic Preflight Handling)
    // =========================================================================

    #[tokio::test]
    async fn test_cors_preflight_any_origin() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Send preflight OPTIONS request (no OPTIONS defined in spec - auto-handled)
        let resp = gateway
            .request_builder(reqwest::Method::OPTIONS, "/cors-any")
            .header("Origin", "https://any-site.com")
            .header("Access-Control-Request-Method", "POST")
            .header("Access-Control-Request-Headers", "Content-Type")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 204, "Preflight should return 204 No Content");

        // Check CORS headers
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("*"),
            "Should allow any origin"
        );
        assert!(
            resp.headers().get("access-control-allow-methods").is_some(),
            "Should have allow-methods header"
        );
        assert!(
            resp.headers().get("access-control-max-age").is_some(),
            "Should have max-age header"
        );
    }

    #[tokio::test]
    async fn test_cors_preflight_specific_origin_allowed() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Send preflight from allowed origin
        let resp = gateway
            .request_builder(reqwest::Method::OPTIONS, "/cors-specific")
            .header("Origin", "https://example.com")
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 204, "Preflight should return 204");
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("https://example.com"),
            "Should echo back allowed origin"
        );
    }

    #[tokio::test]
    async fn test_cors_preflight_specific_origin_denied() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Send preflight from disallowed origin
        let resp = gateway
            .request_builder(reqwest::Method::OPTIONS, "/cors-specific")
            .header("Origin", "https://evil-site.com")
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 403, "Should reject disallowed origin");
    }

    #[tokio::test]
    async fn test_cors_preflight_method_not_allowed() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Request DELETE method which is not allowed on cors-specific
        let resp = gateway
            .request_builder(reqwest::Method::OPTIONS, "/cors-specific")
            .header("Origin", "https://example.com")
            .header("Access-Control-Request-Method", "DELETE")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 403, "Should reject disallowed method");
    }

    #[tokio::test]
    async fn test_cors_simple_request_with_origin() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Simple GET request with Origin header
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/cors-any")
            .header("Origin", "https://example.com")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("*"),
            "Response should include CORS header"
        );
    }

    #[tokio::test]
    async fn test_cors_request_without_origin() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Request without Origin header (same-origin or non-browser)
        let resp = gateway.get("/cors-any").await.unwrap();

        assert_eq!(resp.status(), 200, "Non-CORS request should pass through");
    }

    #[tokio::test]
    async fn test_cors_credentials_header() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Preflight for credentials endpoint
        let resp = gateway
            .request_builder(reqwest::Method::OPTIONS, "/cors-credentials")
            .header("Origin", "https://trusted.example.com")
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 204);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-credentials")
                .map(|v| v.to_str().unwrap()),
            Some("true"),
            "Should include credentials header"
        );
        // With credentials, origin should be echoed, not *
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("https://trusted.example.com"),
            "With credentials, should echo origin not *"
        );
    }

    #[tokio::test]
    async fn test_cors_preflight_no_cors_middleware_returns_405() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Preflight for endpoint without CORS middleware should return 405
        let resp = gateway
            .request_builder(reqwest::Method::OPTIONS, "/no-cors")
            .header("Origin", "https://example.com")
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            405,
            "Should return 405 when no CORS middleware configured"
        );
    }

    #[tokio::test]
    async fn test_cors_global_middleware_applies_to_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // /global-cors endpoint inherits global CORS middleware config
        // Preflight should work without operation-level middleware
        let resp = gateway
            .request_builder(reqwest::Method::OPTIONS, "/global-cors")
            .header("Origin", "https://example.com")
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            204,
            "Preflight should succeed with global CORS middleware"
        );
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("*"),
            "Should have wildcard origin from global config"
        );
        assert!(
            resp.headers()
                .get("access-control-allow-methods")
                .map(|v| v.to_str().unwrap())
                .unwrap_or("")
                .contains("GET"),
            "Should include GET in allowed methods from global config"
        );
    }

    #[tokio::test]
    async fn test_cors_global_middleware_simple_request() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cors.yaml")
            .await
            .expect("failed to start gateway");

        // Simple GET request with Origin header on endpoint using global config
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/global-cors")
            .header("Origin", "https://example.com")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("*"),
            "Response should include CORS header from global middleware"
        );
    }

    // ==================== Request Size Limit Tests ====================

    #[tokio::test]
    async fn test_request_size_limit_allows_small_body() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/request-size-limit.yaml")
            .await
            .expect("failed to start gateway");

        // Small body should be allowed (under 100 byte limit)
        let resp = gateway
            .request_builder(reqwest::Method::POST, "/limited")
            .header("Content-Type", "application/json")
            .body(r#"{"msg":"hi"}"#)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "ok");
    }

    #[tokio::test]
    async fn test_request_size_limit_blocks_large_body() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/request-size-limit.yaml")
            .await
            .expect("failed to start gateway");

        // Large body should be rejected (over 100 byte limit)
        let large_body = r#"{"data":"this is a very long message that exceeds the configured limit of 100 bytes for this endpoint"}"#;
        let resp = gateway
            .request_builder(reqwest::Method::POST, "/limited")
            .header("Content-Type", "application/json")
            .body(large_body)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 413);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:payload-too-large");
        assert_eq!(body["status"], 413);
    }

    #[tokio::test]
    async fn test_request_size_limit_unlimited_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/request-size-limit.yaml")
            .await
            .expect("failed to start gateway");

        // Unlimited endpoint should accept large bodies
        let large_body = r#"{"data":"this is a very long message that would be rejected on the limited endpoint but should pass here"}"#;
        let resp = gateway
            .request_builder(reqwest::Method::POST, "/unlimited")
            .header("Content-Type", "application/json")
            .body(large_body)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "unlimited");
    }

    // ==================== IP Restriction Tests ====================

    #[tokio::test]
    async fn test_ip_restriction_allowlist_localhost() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/ip-restriction.yaml")
            .await
            .expect("failed to start gateway");

        // Localhost should be allowed
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/allowlist")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "allowed");
    }

    #[tokio::test]
    async fn test_ip_restriction_allowlist_denied_via_xff() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/ip-restriction.yaml")
            .await
            .expect("failed to start gateway");

        // Request with X-Forwarded-For from non-allowed IP should be denied
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/allowlist")
            .header("X-Forwarded-For", "203.0.113.50")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 403);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:ip-restricted");
    }

    #[tokio::test]
    async fn test_ip_restriction_denylist_allowed() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/ip-restriction.yaml")
            .await
            .expect("failed to start gateway");

        // Request from localhost (not in denylist) should be allowed
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/denylist")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_ip_restriction_denylist_blocked() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/ip-restriction.yaml")
            .await
            .expect("failed to start gateway");

        // Request from denied CIDR range should be blocked
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/denylist")
            .header("X-Forwarded-For", "10.1.2.3")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_ip_restriction_cidr_allowlist() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/ip-restriction.yaml")
            .await
            .expect("failed to start gateway");

        // 127.0.0.1 is in 127.0.0.0/8 CIDR range
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/cidr-allowlist")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_ip_restriction_custom_message() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/ip-restriction.yaml")
            .await
            .expect("failed to start gateway");

        // Request from non-allowed IP should get custom status and message
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/custom-message")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 401);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["detail"].as_str().unwrap().contains("not authorized"));
    }

    #[tokio::test]
    async fn test_ip_restriction_public_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/ip-restriction.yaml")
            .await
            .expect("failed to start gateway");

        // Public endpoint without IP restriction
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/public")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "public");
    }

    // ========================
    // NATS Dispatcher Tests
    // ========================

    #[tokio::test]
    async fn test_nats_dispatcher_spec_compiles() {
        // Test that an AsyncAPI spec with a real NATS dispatcher compiles and boots
        let gateway = TestGateway::from_spec("../../tests/fixtures/nats-dispatch.yaml")
            .await
            .expect("failed to start gateway with NATS dispatch spec");

        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_nats_dispatcher_broker_unavailable() {
        // When NATS is not running, the dispatcher should return 502
        let gateway = TestGateway::from_spec("../../tests/fixtures/nats-dispatch.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .post(
                "/events/users",
                r#"{"userId":"550e8400-e29b-41d4-a716-446655440000","email":"test@example.com"}"#,
            )
            .await
            .unwrap();

        // Should fail with 502 (NATS connection refused)
        assert_eq!(resp.status(), 502);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:nats-publish-failed");
    }

    #[tokio::test]
    async fn test_nats_dispatcher_validates_payload() {
        // Message payload validation should still work with a real dispatcher
        let gateway = TestGateway::from_spec("../../tests/fixtures/nats-dispatch.yaml")
            .await
            .expect("failed to start gateway");

        // Missing required 'email' field
        let resp = gateway
            .post(
                "/events/users",
                r#"{"userId":"550e8400-e29b-41d4-a716-446655440000"}"#,
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
    }

    // ========================
    // Kafka Dispatcher Tests
    // ========================

    #[tokio::test]
    async fn test_kafka_dispatcher_spec_compiles() {
        // Test that an AsyncAPI spec with a real Kafka dispatcher compiles and boots
        let gateway = TestGateway::from_spec("../../tests/fixtures/kafka-dispatch.yaml")
            .await
            .expect("failed to start gateway with Kafka dispatch spec");

        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_kafka_dispatcher_broker_unavailable() {
        // When Kafka is not running, the dispatcher should return 502
        let gateway = TestGateway::from_spec("../../tests/fixtures/kafka-dispatch.yaml")
            .await
            .expect("failed to start gateway");

        let resp = gateway
            .post(
                "/events/orders",
                r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000","total":99.99}"#,
            )
            .await
            .unwrap();

        // Should fail with 502 (Kafka connection refused)
        assert_eq!(resp.status(), 502);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:kafka-publish-failed");
    }

    #[tokio::test]
    async fn test_kafka_dispatcher_validates_payload() {
        // Message payload validation should still work with a real dispatcher
        let gateway = TestGateway::from_spec("../../tests/fixtures/kafka-dispatch.yaml")
            .await
            .expect("failed to start gateway");

        // Missing required 'total' field
        let resp = gateway
            .post(
                "/events/orders",
                r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000"}"#,
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:validation-failed");
    }

    // ==================== HTTP Log Tests ====================

    /// Create a temporary spec file for http-log testing with dynamic log endpoint URL.
    fn create_http_log_spec(log_endpoint: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let spec_path = temp_dir.path().join("http-log.yaml");

        // Get absolute paths to plugins
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let plugins_dir = manifest_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("plugins");
        let mock_path = plugins_dir.join("mock/mock.wasm");
        let http_log_path = plugins_dir.join("http-log/http-log.wasm");

        // Create barbacane.yaml manifest in temp dir
        let manifest_path = temp_dir.path().join("barbacane.yaml");
        let manifest_content = format!(
            r#"# Test manifest for HTTP log tests

plugins:
  mock:
    path: {}
  http-log:
    path: {}
"#,
            mock_path.display(),
            http_log_path.display()
        );
        std::fs::write(&manifest_path, manifest_content).expect("failed to write manifest file");

        let spec_content = format!(
            r#"openapi: "3.0.3"
info:
  title: HTTP Log Test API
  version: "1.0.0"
  description: API for testing HTTP logging middleware

paths:
  /logged:
    get:
      summary: Endpoint with HTTP logging
      operationId: getLogged
      x-barbacane-middlewares:
        - name: http-log
          config:
            endpoint: "{}"
            timeout_ms: 2000
            include_headers: true
            include_body: true
            custom_fields:
              env: "test"
              service: "test-api"
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "OK"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
"#,
            log_endpoint
        );

        std::fs::write(&spec_path, spec_content).expect("failed to write spec file");
        (temp_dir, spec_path)
    }

    #[tokio::test]
    async fn test_http_log_sends_entry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Accept log entries
        Mock::given(method("POST"))
            .and(path("/logs"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        let log_endpoint = format!("{}/logs", mock_server.uri());
        let (_temp_dir, spec_path) = create_http_log_spec(&log_endpoint);

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/logged").await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "OK");

        // Verify the mock received exactly 1 log entry
        // (wiremock expect(1) will panic on drop if not satisfied)
    }

    #[tokio::test]
    async fn test_http_log_includes_duration() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/logs"))
            .and(body_string_contains("duration_ms"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        let log_endpoint = format!("{}/logs", mock_server.uri());
        let (_temp_dir, spec_path) = create_http_log_spec(&log_endpoint);

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/logged").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_http_log_includes_headers() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Verify the log entry contains request headers
        Mock::given(method("POST"))
            .and(path("/logs"))
            .and(body_string_contains("headers"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        let log_endpoint = format!("{}/logs", mock_server.uri());
        let (_temp_dir, spec_path) = create_http_log_spec(&log_endpoint);

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/logged").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_http_log_failure_does_not_break_response() {
        // Use an unreachable endpoint — the response should still be 200
        let (_temp_dir, spec_path) =
            create_http_log_spec("http://127.0.0.1:1/unreachable-log-endpoint");

        let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
            .await
            .expect("failed to start gateway");

        let resp = gateway.get("/logged").await.unwrap();
        assert_eq!(
            resp.status(),
            200,
            "Response should not be affected by log delivery failure"
        );

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "OK");
    }

    // =========================================================================
    // Fixture compilation tests
    //
    // Verify that every plugin fixture spec compiles and the gateway starts.
    // These don't test runtime behavior — just that the plugin config schemas
    // are valid and the artifact builds successfully.
    // =========================================================================

    #[tokio::test]
    async fn test_fixture_compiles_mock() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/mock.yaml")
            .await
            .expect("mock fixture failed to compile");
        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_fixture_compiles_lambda() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/lambda.yaml")
            .await
            .expect("lambda fixture failed to compile");
        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_fixture_compiles_oauth2_auth() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/oauth2-auth.yaml")
            .await
            .expect("oauth2-auth fixture failed to compile");
        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_fixture_compiles_oidc_auth() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/oidc-auth.yaml")
            .await
            .expect("oidc-auth fixture failed to compile");
        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_fixture_compiles_http_log() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/http-log.yaml")
            .await
            .expect("http-log fixture failed to compile");
        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_fixture_compiles_observability() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/observability.yaml")
            .await
            .expect("observability fixture failed to compile");
        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_fixture_compiles_correlation_id() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/correlation-id.yaml")
            .await
            .expect("correlation-id fixture failed to compile");
        let resp = gateway.get("/__barbacane/health").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    // ==================== ACL Tests ====================

    /// Helper to create Basic auth header for ACL tests.
    fn acl_basic_auth(username: &str, password: &str) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let encoded = STANDARD.encode(format!("{}:{}", username, password));
        format!("Basic {}", encoded)
    }

    #[tokio::test]
    async fn test_acl_admin_allowed_admin_only() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/admin-only")
            .header("Authorization", acl_basic_auth("admin", "admin123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_acl_editor_denied_admin_only() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/admin-only")
            .header("Authorization", acl_basic_auth("editor", "editor123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_acl_editor_allowed_editors_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/editors")
            .header("Authorization", acl_basic_auth("editor", "editor123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_acl_viewer_denied_editors_endpoint() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/editors")
            .header("Authorization", acl_basic_auth("viewer", "viewer123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_acl_banned_group_denied() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/deny-banned")
            .header("Authorization", acl_basic_auth("banned_user", "banned123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_acl_non_banned_allowed_deny_rule() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/deny-banned")
            .header("Authorization", acl_basic_auth("editor", "editor123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_acl_consumer_allow_specific_user() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/consumer-allow")
            .header("Authorization", acl_basic_auth("admin", "admin123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_acl_consumer_allow_denies_other() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/consumer-allow")
            .header("Authorization", acl_basic_auth("editor", "editor123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_acl_static_consumer_groups_premium_allowed() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        // viewer gets "premium" group via static consumer_groups config
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/premium")
            .header("Authorization", acl_basic_auth("viewer", "viewer123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_acl_static_consumer_groups_non_premium_denied() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        // admin has no "premium" group
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/premium")
            .header("Authorization", acl_basic_auth("admin", "admin123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_acl_public_endpoint_no_auth() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway.get("/public").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_acl_missing_auth_returns_401() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/acl.yaml")
            .await
            .expect("failed to start gateway");
        // No Authorization header — basic-auth should return 401 before ACL runs
        let resp = gateway.get("/admin-only").await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    // --- OPA Authorization Tests ---

    #[tokio::test]
    async fn test_opa_unreachable_returns_503() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/opa-authz.yaml")
            .await
            .expect("failed to start gateway");
        // OPA URL points to unreachable port — expect 503
        let resp = gateway.get("/opa-protected").await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn test_opa_unreachable_returns_problem_json() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/opa-authz.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway.get("/opa-protected").await.unwrap();
        assert_eq!(resp.status(), 503);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:opa-unavailable");
        assert_eq!(body["status"], 503);
    }

    #[tokio::test]
    async fn test_opa_with_auth_missing_credentials_returns_401() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/opa-authz.yaml")
            .await
            .expect("failed to start gateway");
        // No auth header — basic-auth returns 401 before OPA runs
        let resp = gateway.get("/opa-with-auth").await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn test_opa_with_auth_valid_credentials_opa_unreachable() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/opa-authz.yaml")
            .await
            .expect("failed to start gateway");
        // Valid auth but OPA unreachable — expect 503
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/opa-with-auth")
            .header("Authorization", acl_basic_auth("admin", "admin123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn test_opa_public_endpoint_bypasses_opa() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/opa-authz.yaml")
            .await
            .expect("failed to start gateway");
        // Public endpoint has no OPA middleware — should succeed
        let resp = gateway.get("/public").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    // -----------------------------------------------------------------------
    // CEL policy evaluation tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_cel_method_check_get_allowed() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cel.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway.get("/cel-method-check").await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_cel_method_check_post_denied() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cel.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::POST, "/cel-method-check")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    fn cel_basic_auth(username: &str, password: &str) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let encoded = STANDARD.encode(format!("{}:{}", username, password));
        format!("Basic {}", encoded)
    }

    #[tokio::test]
    async fn test_cel_with_auth_admin_allowed() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cel.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/cel-with-auth")
            .header("Authorization", cel_basic_auth("admin", "admin123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_cel_with_auth_viewer_denied() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cel.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway
            .request_builder(reqwest::Method::GET, "/cel-with-auth")
            .header("Authorization", cel_basic_auth("viewer", "viewer123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_cel_public_endpoint_bypasses() {
        let gateway = TestGateway::from_spec("../../tests/fixtures/cel.yaml")
            .await
            .expect("failed to start gateway");
        let resp = gateway.get("/public").await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}
