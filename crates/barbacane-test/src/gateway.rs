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
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["message"], "Access granted");
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
}
