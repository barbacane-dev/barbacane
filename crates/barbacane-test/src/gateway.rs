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
    /// The admin API port.
    admin_port: u16,
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

    /// Create a TestGateway from a spec with extra CLI args for the data plane.
    pub async fn from_spec_with_args(
        spec_path: &str,
        extra_args: &[&str],
    ) -> Result<Self, TestError> {
        Self::create_gateway_with_args(&[spec_path], false, extra_args).await
    }

    /// Create a TestGateway from multiple spec files.
    pub async fn from_specs(spec_paths: &[&str]) -> Result<Self, TestError> {
        Self::create_gateway_with_args(spec_paths, false, &[]).await
    }

    /// Create a TLS-enabled TestGateway from multiple spec files.
    pub async fn from_specs_with_tls(spec_paths: &[&str]) -> Result<Self, TestError> {
        Self::create_gateway_with_args(spec_paths, true, &[]).await
    }

    /// Internal method to create a gateway with optional TLS and extra CLI args.
    async fn create_gateway_with_args(
        spec_paths: &[&str],
        tls_enabled: bool,
        extra_args: &[&str],
    ) -> Result<Self, TestError> {
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
        let options = CompileOptions {
            allow_plaintext: true,
            ..CompileOptions::default()
        };
        compile_with_manifest(
            &paths,
            &project_manifest,
            spec_dir,
            &artifact_path,
            &options,
        )?;

        // Find the barbacane binary
        let binary_path = find_barbacane_binary()?;

        // Find available ports for main and admin
        let port = find_available_port()?;
        let admin_port = find_available_port()?;

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
            .arg("--admin-bind")
            .arg(format!("127.0.0.1:{}", admin_port))
            .arg("--dev")
            .arg("--allow-plaintext-upstream") // Allow HTTP calls to test mock servers
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Add TLS arguments if enabled
        if let Some(ref certs) = tls_certs {
            cmd.arg("--tls-cert").arg(&certs.cert_path);
            cmd.arg("--tls-key").arg(&certs.key_path);
        }

        // Add any extra CLI arguments
        for arg in extra_args {
            cmd.arg(arg);
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

            let mut tls_config = rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth();

            // Enable ALPN so the client can negotiate HTTP/2 over TLS
            tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

            reqwest::Client::builder()
                .use_preconfigured_tls(tls_config)
                .build()?
        } else {
            reqwest::Client::new()
        };

        let mut gateway = TestGateway {
            child,
            port,
            admin_port,
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
        // 60-second timeout — larger WASM plugins (e.g. CEL ~1.3 MB) need
        // more JIT compile time, especially under heavy parallel test load.
        let max_attempts = 600;
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

    /// Get the base URL of the admin API.
    pub fn admin_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.admin_port)
    }

    /// Make a GET request to the admin API at the given path.
    pub async fn admin_get(&self, path: &str) -> Result<reqwest::Response, TestError> {
        let url = format!("{}{}", self.admin_base_url(), path);
        Ok(self.client.get(&url).send().await?)
    }

    /// Make a POST request to the admin API at the given path.
    pub async fn admin_post(&self, path: &str) -> Result<reqwest::Response, TestError> {
        let url = format!("{}{}", self.admin_base_url(), path);
        Ok(self.client.post(&url).send().await?)
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
