//! TestGateway: full-stack integration test harness.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
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
    /// What the gateway wrote to stdout and stderr, drained as it runs.
    ///
    /// The gateway logs the cause of every error it answers with, so without
    /// this a failing assertion reports a status and nothing about why.
    log: GatewayLog,
}

/// The tail of a gateway's output, collected by reader threads.
///
/// Bounded, because a `--dev` gateway logging every dropped header on every
/// request will outrun any test that reads it. The most recent lines are the
/// ones that explain the failure.
#[derive(Clone, Default)]
pub struct GatewayLog {
    lines: Arc<Mutex<std::collections::VecDeque<String>>>,
    /// The reader threads, kept so their last lines can be waited for.
    readers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

impl GatewayLog {
    /// Keep this many lines. Enough for a panic backtrace and the lines around
    /// it, small enough to print in a test failure.
    const CAPACITY: usize = 400;

    fn push(&self, line: String) {
        let Ok(mut lines) = self.lines.lock() else {
            return;
        };
        if lines.len() == Self::CAPACITY {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// Everything kept, oldest first.
    pub fn text(&self) -> String {
        match self.lines.lock() {
            Ok(lines) => lines.iter().cloned().collect::<Vec<_>>().join("\n"),
            Err(_) => String::new(),
        }
    }

    /// Whether the gateway logged anything matching `needle`.
    pub fn contains(&self, needle: &str) -> bool {
        self.text().contains(needle)
    }

    /// Drain a pipe into this log on a thread of its own.
    ///
    /// A piped stream nothing reads fills its buffer and blocks the writer, so
    /// draining is what keeps the gateway running as much as it is what makes
    /// the output readable.
    fn drain<R: std::io::Read + Send + 'static>(&self, stream: R, stream_name: &'static str) {
        let log = self.clone();
        let handle = std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                log.push(format!("[{stream_name}] {line}"));
            }
        });
        if let Ok(mut readers) = self.readers.lock() {
            readers.push(handle);
        }
    }

    /// Wait for the reader threads to finish.
    ///
    /// A reader ends when its pipe reaches EOF, which happens when the child
    /// closes it, so this must follow the child exiting or being reaped.
    /// Without it `text()` can be read while the lines explaining a failure are
    /// still in a reader's buffer, which is the whole thing this exists to
    /// prevent.
    fn join_readers(&self) {
        let handles: Vec<_> = match self.readers.lock() {
            Ok(mut readers) => readers.drain(..).collect(),
            Err(_) => return,
        };
        for handle in handles {
            let _ = handle.join();
        }
    }

    /// Wait until the log contains `needle`, up to `limit`.
    ///
    /// The gateway writes asynchronously, so a test that reads immediately
    /// after the request races the reader thread.
    pub fn wait_for(&self, needle: &str, limit: Duration) -> bool {
        self.wait_for_line(needle, limit).is_some()
    }

    /// Wait for a line containing `needle` and return it, up to `limit`.
    ///
    /// Startup lines carry values the caller needs, the bound ports above all,
    /// so matching is not enough.
    pub fn wait_for_line(&self, needle: &str, limit: Duration) -> Option<String> {
        let deadline = std::time::Instant::now() + limit;
        loop {
            if let Some(line) = self.find_line(needle) {
                return Some(line);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The first line containing `needle`, without waiting.
    pub fn find_line(&self, needle: &str) -> Option<String> {
        let lines = self.lines.lock().ok()?;
        lines.iter().find(|l| l.contains(needle)).cloned()
    }
}

/// The port a child announced on `needle`'s line.
///
/// Anchored on the loopback host rather than the last colon, because the line
/// is not always the last thing on it: the control plane logs through `tracing`,
/// which appends its own fields after the message.
fn port_from_log_line(line: &str) -> Option<u16> {
    let after = line.rsplit_once(&format!("{LISTEN_HOST}:"))?.1;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    match digits.parse().ok()? {
        // What a child prints when it echoes the address it was asked for
        // instead of the one it bound. Nothing listens there.
        0 => None,
        port => Some(port),
    }
}

/// A spawned child whose output is drained and searchable.
///
/// Test children bind port 0 and announce what they got. Choosing a port in the
/// test means binding a socket, reading its number and closing it before the
/// child binds, which leaves the port unowned in between; a concurrent test
/// handed the same one then talks to the wrong process.
pub struct LoggedChild {
    /// `None` once handed to a caller that owns the child from then on, which
    /// is what stops `Drop` killing a process someone else is still using.
    child: Option<Child>,
    log: GatewayLog,
}

impl LoggedChild {
    /// Spawn `cmd`, piping and draining both streams.
    ///
    /// A pipe nothing reads fills and blocks the writer, so draining is what
    /// keeps the child running as much as it is what makes its output readable.
    pub fn spawn(cmd: &mut Command) -> std::io::Result<Self> {
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let log = GatewayLog::default();
        if let Some(stdout) = child.stdout.take() {
            log.drain::<ChildStdout>(stdout, "stdout");
        }
        if let Some(stderr) = child.stderr.take() {
            log.drain::<ChildStderr>(stderr, "stderr");
        }
        Ok(Self {
            child: Some(child),
            log,
        })
    }

    /// The port announced on the first line containing `needle`.
    ///
    /// Gives up as soon as the child exits, so a process that refuses to start
    /// costs what it took to fail rather than the whole of `limit`.
    pub fn announced_port(&mut self, needle: &str, limit: Duration) -> Option<u16> {
        let deadline = std::time::Instant::now() + limit;
        loop {
            if let Some(port) = self
                .log
                .find_line(needle)
                .as_deref()
                .and_then(port_from_log_line)
            {
                return Some(port);
            }
            if self.has_exited() {
                // Exited, so the pipes reach EOF. Wait for the readers before
                // the last look: the announcement may still be in their buffer.
                self.log.join_readers();
                return self
                    .log
                    .find_line(needle)
                    .as_deref()
                    .and_then(port_from_log_line);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// What the child has written so far.
    pub fn log(&self) -> &GatewayLog {
        &self.log
    }

    /// The child's process id, while this still owns it.
    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Whether the child has already exited.
    pub fn has_exited(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(Some(_))),
            None => false,
        }
    }

    /// Stop the child and collect it, so no zombie outlives the test.
    pub fn kill_and_reap(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let _ = child.kill();
        let _ = child.wait();
        self.log.join_readers();
    }

    /// Hand over the child and its log, for a caller that owns them from here.
    ///
    /// Takes the child, so dropping what is left does not kill a process the
    /// new owner is still using.
    pub fn into_parts(mut self) -> (Child, GatewayLog) {
        let child = self.child.take().expect("the child is handed over once");
        (child, self.log.clone())
    }
}

impl Drop for LoggedChild {
    /// Stop the child if this still owns it.
    ///
    /// `Child` does not kill on drop, so any early return holding one of these
    /// would otherwise leave the process running for the rest of the test
    /// binary. A test that gives up part-way through starting something is
    /// exactly where that happens.
    fn drop(&mut self) {
        if self.child.is_some() {
            self.kill_and_reap();
        }
    }
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
        Self::create_gateway_with_args(&[spec_path], false, extra_args, true, &[]).await
    }

    /// Create a TestGateway with extra environment variables set on the data-plane
    /// child process (e.g. `BARBACANE_SECRETS_DIR`).
    pub async fn from_spec_with_env(
        spec_path: &str,
        env: &[(&str, &str)],
    ) -> Result<Self, TestError> {
        Self::create_gateway_with_args(&[spec_path], false, &[], true, env).await
    }

    /// Create a TestGateway with the plugin SSRF guard ACTIVE (internal egress
    /// blocked). Use this for SSRF tests; the default constructors allow internal
    /// egress so tests can reach loopback mock upstreams.
    pub async fn from_spec_blocked_egress(spec_path: &str) -> Result<Self, TestError> {
        Self::create_gateway_with_args(&[spec_path], false, &[], false, &[]).await
    }

    /// Create a TestGateway from multiple spec files.
    pub async fn from_specs(spec_paths: &[&str]) -> Result<Self, TestError> {
        Self::create_gateway_with_args(spec_paths, false, &[], true, &[]).await
    }

    /// Create a TLS-enabled TestGateway from multiple spec files.
    pub async fn from_specs_with_tls(spec_paths: &[&str]) -> Result<Self, TestError> {
        Self::create_gateway_with_args(spec_paths, true, &[], true, &[]).await
    }

    /// Internal method to create a gateway with optional TLS and extra CLI args.
    ///
    /// `allow_internal_egress` controls the plugin SSRF guard: most tests reach
    /// loopback mock upstreams and need it `true`; SSRF tests pass `false` so the
    /// guard is active and the block is observable.
    async fn create_gateway_with_args(
        spec_paths: &[&str],
        tls_enabled: bool,
        extra_args: &[&str],
        allow_internal_egress: bool,
        env: &[(&str, &str)],
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

        // Generate TLS certificates if needed
        let tls_certs = if tls_enabled {
            Some(generate_test_certificates(temp_dir.path())?)
        } else {
            None
        };

        // Build the gateway command
        let mut cmd = Command::new(&binary_path);
        // Port 0, so the child binds whatever is free and reports it on startup.
        // Choosing a port here and passing the number would mean closing the
        // socket first, leaving the port unowned until the child binds it, and
        // a concurrent test can be handed the same one in that window.
        cmd.arg("serve")
            .arg("--artifact")
            .arg(&artifact_path)
            .arg("--listen")
            .arg(LISTEN_ANY_PORT)
            .arg("--admin-bind")
            .arg(LISTEN_ANY_PORT)
            .arg("--dev")
            .arg("--allow-plaintext-upstream") // Allow HTTP calls to test mock servers
            // Set egress policy explicitly so tests don't depend on the ambient
            // environment: most tests reach loopback mocks (allow), SSRF tests
            // exercise the guard (block).
            .env(
                "BARBACANE_ALLOW_INTERNAL_EGRESS",
                if allow_internal_egress { "1" } else { "0" },
            );

        // Add TLS arguments if enabled
        if let Some(ref certs) = tls_certs {
            cmd.arg("--tls-cert").arg(&certs.cert_path);
            cmd.arg("--tls-key").arg(&certs.key_path);
        }

        // Add any extra CLI arguments
        for arg in extra_args {
            cmd.arg(arg);
        }

        // Inject per-instance environment variables (e.g. BARBACANE_SECRETS_DIR)
        // into the child process, avoiding process-global set_var races between
        // concurrently running tests.
        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut process = LoggedChild::spawn(&mut cmd)?;

        // Read back the ports the child bound. Absence is an error rather than a
        // port of 0, which would surface later as a connection failure with
        // nothing pointing at the cause.
        let mut announced = |needle: &str| -> Result<u16, TestError> {
            match process.announced_port(needle, STARTUP_TIMEOUT) {
                Some(port) => Ok(port),
                None => {
                    process.kill_and_reap();
                    Err(TestError::StartupFailed(format!(
                        "gateway did not report {needle}\n{}",
                        process.log().text()
                    )))
                }
            }
        };
        let port = announced("listening on")?;
        let admin_port = announced("admin API on")?;
        let (child, log) = process.into_parts();

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
            log,
        };

        // Wait for the gateway to be ready
        gateway.wait_for_ready().await?;

        Ok(gateway)
    }

    /// Wait for the gateway to be ready by polling the health endpoint.
    async fn wait_for_ready(&mut self) -> Result<(), TestError> {
        let health_url = format!("{}/__barbacane/health", self.base_url());
        // A genuine boot hang still fails here rather than being masked.
        let delay = Duration::from_millis(100);
        let max_attempts = STARTUP_TIMEOUT.as_millis() / delay.as_millis();

        for _ in 0..max_attempts {
            if let Ok(resp) = self.client.get(&health_url).send().await {
                if resp.status().is_success() {
                    return Ok(());
                }
            }

            // Check if the process has exited
            if let Ok(Some(status)) = self.child.try_wait() {
                // The child is gone, so its pipes are closed and the readers
                // will reach EOF. Wait for them rather than racing the lines
                // that say why it died.
                self.log.join_readers();
                return Err(TestError::StartupFailed(format!(
                    "gateway exited with status: {}\n{}",
                    status,
                    self.log.text()
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

    /// What the gateway has logged so far.
    ///
    /// The gateway logs the cause of every error it answers with, so this is
    /// where a 500 says which middleware failed and why.
    pub fn log(&self) -> &GatewayLog {
        &self.log
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
        // Kill and reap first: the readers end at EOF, which only arrives once
        // the child has closed its pipes. Printing before that races the lines
        // that explain the failure.
        let _ = self.child.kill();
        let _ = self.child.wait();

        // A failing assertion sees a status and no reason, so hand it what the
        // gateway said before this goes out of scope with it.
        if std::thread::panicking() {
            self.log.join_readers();
            let log = self.log.text();
            if !log.is_empty() {
                eprintln!("--- gateway on port {} said ---\n{}\n---", self.port, log);
            }
        }
    }
}

/// Find the barbacane binary in the target directory.
fn find_barbacane_binary() -> Result<String, TestError> {
    // An explicit path wins, then the active target directory. Both come before
    // the fixed candidates so a build under a redirected CARGO_TARGET_DIR (which
    // is how `cargo llvm-cov` instruments the gateway) runs the binary it just
    // built rather than a stale one under ./target.
    if let Ok(path) = std::env::var("BARBACANE_TEST_BINARY") {
        if Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(TestError::BinaryNotFound(path));
    }
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        for profile in ["debug", "release"] {
            let path = format!("{dir}/{profile}/barbacane");
            if Path::new(&path).exists() {
                return Ok(path);
            }
        }
    }

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

/// The host every test child binds. Ports are read back from output anchored on
/// it, so a child told to bind elsewhere will not be understood.
pub const LISTEN_HOST: &str = "127.0.0.1";

/// What a test child is told to bind. The OS picks the port and the child
/// reports it, so no port is ever named here.
pub const LISTEN_ANY_PORT: &str = "127.0.0.1:0";

/// How long a test child may take to come up.
///
/// Larger WASM plugins (CEL is ~1.3 MB) need JIT compile time, and when the
/// integration suite runs sharded in CI two CEL-heavy gateways can cold-boot
/// simultaneously on a shared runner, so the loser of that CPU race needs a wide
/// window. Both the port announcement and the health check are bounded by this,
/// so neither can be the tighter of the two.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
mod child_tests {
    use super::*;
    use std::time::Instant;

    /// A child that writes `script` to stderr, under `sh`.
    fn sh(script: &str) -> LoggedChild {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        LoggedChild::spawn(&mut cmd).expect("spawn sh")
    }

    #[test]
    fn the_harness_never_names_a_port() {
        assert_eq!(
            LISTEN_ANY_PORT,
            format!("{LISTEN_HOST}:0"),
            "the child must bind an OS-assigned port; naming one here reopens \
             the window where the port belongs to nobody"
        );
    }

    #[test]
    fn reads_the_port_a_gateway_announces() {
        assert_eq!(
            port_from_log_line("[stderr] barbacane: listening on http://127.0.0.1:52133"),
            Some(52133)
        );
        assert_eq!(
            port_from_log_line("[stderr] barbacane: admin API on http://127.0.0.1:41999"),
            Some(41999)
        );
        assert_eq!(
            port_from_log_line("[stderr] barbacane dev: listening on http://127.0.0.1:8080"),
            Some(8080)
        );
        assert_eq!(
            port_from_log_line("[stderr] barbacane: listening on https://127.0.0.1:8443"),
            Some(8443)
        );
    }

    #[test]
    fn reads_the_port_the_control_plane_announces() {
        // `tracing`'s formatter puts the target and fields after the message, so
        // the port is not the last thing on the line.
        assert_eq!(
            port_from_log_line(
                "[stdout] 2026-09-23T12:00:00.000000Z  INFO barbacane_control::server: \
                 Control plane listening on 127.0.0.1:34567"
            ),
            Some(34567)
        );
        assert_eq!(
            port_from_log_line(
                r#"[stdout] {"timestamp":"2026-09-23T12:00:00Z","level":"INFO","#
                    .to_string()
                    .as_str()
            ),
            None
        );
        assert_eq!(
            port_from_log_line(
                r#"[stdout] {"message":"Control plane listening on 127.0.0.1:34567","target":"x"}"#
            ),
            Some(34567)
        );
    }

    #[test]
    fn a_port_of_zero_is_not_a_bound_port() {
        // What a child prints when it echoes the address it was asked for. Taking
        // it at face value produces a connection failure far from the cause.
        assert_eq!(
            port_from_log_line("[stderr] barbacane: listening on http://127.0.0.1:0"),
            None
        );
    }

    #[test]
    fn rejects_lines_without_a_loopback_address() {
        assert_eq!(port_from_log_line("[stderr] no address here"), None);
        assert_eq!(
            port_from_log_line("[stderr] listening on 0.0.0.0:8080"),
            None
        );
        assert_eq!(port_from_log_line("[stderr] listening on 127.0.0.1:"), None);
        // Above u16, so not a port at all.
        assert_eq!(
            port_from_log_line("[stderr] listening on 127.0.0.1:70000"),
            None
        );
        assert_eq!(
            port_from_log_line("[stderr] listening on 127.0.0.1:65535"),
            Some(65535)
        );
    }

    #[test]
    fn finds_the_port_a_child_announces() {
        let mut child =
            sh("echo 'barbacane: listening on http://127.0.0.1:51234' >&2; exec sleep 30");
        let port = child.announced_port("listening on", Duration::from_secs(10));
        child.kill_and_reap();
        assert_eq!(port, Some(51234));
    }

    #[test]
    fn waits_for_an_announcement_that_is_slow_to_arrive() {
        let mut child =
            sh("sleep 1; echo 'barbacane: listening on http://127.0.0.1:51235' >&2; exec sleep 30");
        let port = child.announced_port("listening on", Duration::from_secs(10));
        child.kill_and_reap();
        assert_eq!(
            port,
            Some(51235),
            "an announcement after a delay is still read"
        );
    }

    #[test]
    fn the_needle_chooses_between_two_announcements() {
        let mut child = sh(
            "echo 'barbacane: listening on http://127.0.0.1:51236' >&2; \
             echo 'barbacane: admin API on http://127.0.0.1:51237' >&2; exec sleep 30",
        );
        let listen = child.announced_port("listening on", Duration::from_secs(10));
        let admin = child.announced_port("admin API on", Duration::from_secs(10));
        child.kill_and_reap();
        assert_eq!((listen, admin), (Some(51236), Some(51237)));
    }

    #[test]
    fn reads_an_announcement_the_child_made_before_exiting() {
        let mut child = sh("echo 'barbacane: listening on http://127.0.0.1:51238' >&2");
        let port = child.announced_port("listening on", Duration::from_secs(10));
        child.kill_and_reap();
        assert_eq!(port, Some(51238));
    }

    #[test]
    fn a_backlog_of_output_does_not_hide_the_announcement() {
        // The announcement sits behind more lines than the log keeps, and behind
        // more bytes than a pipe buffer holds. Being last, it survives eviction,
        // and the reader is still draining the backlog when the child exits.
        let mut child = sh(
            "i=0; while [ $i -lt 4000 ]; do echo padding-line-$i >&2; i=$((i+1)); done; \
             echo 'barbacane: listening on http://127.0.0.1:51239' >&2",
        );
        let port = child.announced_port("listening on", Duration::from_secs(10));
        child.kill_and_reap();
        assert_eq!(port, Some(51239));
    }

    #[test]
    fn gives_up_as_soon_as_a_child_exits_without_announcing() {
        let mut child = sh("echo 'refusing to start' >&2; exit 3");
        let started = Instant::now();
        // A generous limit: the point is that it does not wait for it.
        let port = child.announced_port("listening on", Duration::from_secs(60));
        let elapsed = started.elapsed();
        child.kill_and_reap();

        assert_eq!(port, None);
        assert!(
            elapsed < Duration::from_secs(10),
            "returned after {elapsed:?}; a child that refuses to start must cost \
             what it took to fail, not the whole timeout"
        );
    }

    #[test]
    fn gives_up_at_the_limit_when_a_live_child_never_announces() {
        let mut child = sh("exec sleep 30");
        let started = Instant::now();
        let port = child.announced_port("listening on", Duration::from_millis(500));
        let elapsed = started.elapsed();
        child.kill_and_reap();

        assert_eq!(port, None);
        assert!(elapsed >= Duration::from_millis(500), "waited {elapsed:?}");
        assert!(elapsed < Duration::from_secs(10), "waited {elapsed:?}");
    }

    /// Whether a process id is still live, asked of the OS rather than of us.
    fn still_running(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn dropping_a_child_stops_it() {
        // `Child` does not kill on drop, so without an impl of our own an early
        // return holding one leaves the process running for the rest of the
        // test binary. `TestControlPlane::boot` returns exactly that way when
        // the control plane never announces a port.
        let pid = {
            let child = sh("exec sleep 300");
            child.id().expect("the child is owned here")
        };
        assert!(
            !still_running(pid),
            "pid {pid} outlived the LoggedChild that spawned it"
        );
    }

    #[test]
    fn handing_the_child_over_leaves_it_running() {
        // `into_parts` gives the child to a caller that owns it from then on,
        // so dropping what is left must not kill a process still in use.
        let child = sh("exec sleep 300");
        let pid = child.id().expect("owned");
        let (mut handed, _log) = child.into_parts();
        assert!(still_running(pid), "the new owner's process was killed");
        let _ = handed.kill();
        let _ = handed.wait();
    }

    #[test]
    fn the_log_keeps_what_the_child_wrote_on_both_streams() {
        let mut child = sh("echo to-stdout; echo to-stderr >&2");
        assert!(child.log().wait_for("to-stdout", Duration::from_secs(10)));
        assert!(child.log().wait_for("to-stderr", Duration::from_secs(10)));
        child.kill_and_reap();
    }
}
