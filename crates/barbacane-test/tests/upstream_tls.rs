//! `http-upstream` with `tls`: a client certificate for an upstream that
//! requires one, and a CA the upstream's certificate is verified against.
//!
//! Each test runs an HTTPS upstream that accepts only clients presenting a
//! certificate issued by the test CA, and answers `mtls-ok`.
//!
//! Run with: `cargo test -p barbacane-test --test upstream_tls`

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use barbacane_test::TestGateway;
use rcgen::{BasicConstraints, Certificate, CertificateParams, IsCa, KeyPair};

/// A certificate authority that issues the test's certificates.
struct Authority {
    cert: Certificate,
    key: KeyPair,
}

impl Authority {
    fn new() -> Self {
        let key = KeyPair::generate().expect("CA key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).expect("CA certificate");
        Self { cert, key }
    }

    fn issue(&self, names: &[&str]) -> (Certificate, KeyPair) {
        let key = KeyPair::generate().expect("key");
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let cert = CertificateParams::new(names)
            .expect("params")
            .signed_by(&key, &self.cert, &self.key)
            .expect("certificate");
        (cert, key)
    }
}

/// Serve HTTPS on a free port with `server`, accepting only clients whose
/// certificate `client_ca` issued. Returns the port.
fn mtls_upstream(server: &(Certificate, KeyPair), client_ca: &Authority) -> u16 {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(client_ca.cert.der().clone())
        .expect("client CA root");
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .expect("client verifier");
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(server.1.serialize_der().into());
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![server.0.der().clone()], key)
            .expect("server config"),
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let config = Arc::clone(&config);
            std::thread::spawn(move || {
                let Ok(conn) = rustls::ServerConnection::new(config) else {
                    return;
                };
                let mut tls = rustls::StreamOwned::new(conn, stream);
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tls.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = tls.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\nconnection: close\r\n\r\nmtls-ok",
                );
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        }
    });
    port
}

/// Write `cert` and `key` as PEM files named `{name}.pem` and `{name}.key`.
fn write_pem(dir: &Path, name: &str, cert: &Certificate, key: Option<&KeyPair>) {
    std::fs::write(dir.join(format!("{name}.pem")), cert.pem()).expect("certificate file");
    if let Some(key) = key {
        std::fs::write(dir.join(format!("{name}.key")), key.serialize_pem()).expect("key file");
    }
}

/// A spec routing `GET /mtls` to `https://localhost:{port}` through
/// `http-upstream` with `tls`, in a project whose manifest names the official
/// plugin.
fn spec_with_tls(dir: &Path, port: u16, tls_yaml: &str) -> PathBuf {
    let plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/http-upstream/http-upstream.wasm");
    std::fs::write(
        dir.join("barbacane.yaml"),
        format!(
            "plugins:\n  http-upstream:\n    path: {}\n",
            plugin.display()
        ),
    )
    .expect("manifest");
    let spec = dir.join("api.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: "3.1.0"
info: {{ title: Upstream TLS, version: "1.0.0" }}
paths:
  /mtls:
    get:
      operationId: mtls
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: https://localhost:{port}
          path: /
          timeout: 5
          tls: {tls_yaml}
      responses:
        "200": {{ description: ok }}
"#
        ),
    )
    .expect("spec");
    spec
}

async fn get_mtls(spec: &Path, secrets_dir: &Path) -> (u16, String, String) {
    let gateway = TestGateway::from_spec_with_env(
        spec.to_str().expect("utf-8"),
        &[
            (
                "BARBACANE_SECRETS_DIR",
                secrets_dir.to_str().expect("utf-8"),
            ),
            ("BARBACANE_ALLOW_INTERNAL_EGRESS", "1"),
        ],
    )
    .await
    .expect("gateway starts");
    let response = gateway.get("/mtls").await.expect("request");
    let status = response.status().as_u16();
    let body = response.text().await.expect("body");
    let log = gateway.log();
    log.wait_for("HTTP call", Duration::from_millis(500));
    (status, body, log.text())
}

#[tokio::test]
async fn a_client_certificate_reaches_an_upstream_that_requires_one() {
    let ca = Authority::new();
    let port = mtls_upstream(&ca.issue(&["localhost"]), &ca);
    let secrets = tempfile::TempDir::new().unwrap();
    write_pem(secrets.path(), "ca", &ca.cert, None);
    let (cert, key) = ca.issue(&["gateway"]);
    write_pem(secrets.path(), "client", &cert, Some(&key));

    let project = tempfile::TempDir::new().unwrap();
    let spec = spec_with_tls(
        project.path(),
        port,
        "{ client_cert: client.pem, client_key: client.key, ca: ca.pem }",
    );
    let (status, body, log) = get_mtls(&spec, secrets.path()).await;
    assert_eq!((status, body.as_str()), (200, "mtls-ok"), "{log}");
}

#[tokio::test]
async fn without_a_client_certificate_the_upstream_refuses() {
    let ca = Authority::new();
    let port = mtls_upstream(&ca.issue(&["localhost"]), &ca);
    let secrets = tempfile::TempDir::new().unwrap();
    write_pem(secrets.path(), "ca", &ca.cert, None);

    let project = tempfile::TempDir::new().unwrap();
    let spec = spec_with_tls(project.path(), port, "{ ca: ca.pem }");
    let (status, body, _) = get_mtls(&spec, secrets.path()).await;
    assert_eq!(status, 502);
    assert!(!body.contains("mtls-ok"));
}

#[tokio::test]
async fn an_upstream_certificate_from_another_ca_is_refused() {
    let ca = Authority::new();
    let other = Authority::new();
    let port = mtls_upstream(&other.issue(&["localhost"]), &ca);
    let secrets = tempfile::TempDir::new().unwrap();
    write_pem(secrets.path(), "ca", &ca.cert, None);
    let (cert, key) = ca.issue(&["gateway"]);
    write_pem(secrets.path(), "client", &cert, Some(&key));

    let project = tempfile::TempDir::new().unwrap();
    let spec = spec_with_tls(
        project.path(),
        port,
        "{ client_cert: client.pem, client_key: client.key, ca: ca.pem }",
    );
    let (status, body, _) = get_mtls(&spec, secrets.path()).await;
    assert_eq!(status, 502);
    assert!(!body.contains("mtls-ok"));
}

#[tokio::test]
async fn a_tls_file_outside_the_secrets_dir_is_not_read() {
    let ca = Authority::new();
    let port = mtls_upstream(&ca.issue(&["localhost"]), &ca);
    let secrets = tempfile::TempDir::new().unwrap();
    let elsewhere = tempfile::TempDir::new().unwrap();
    write_pem(elsewhere.path(), "ca", &ca.cert, None);
    let (cert, key) = ca.issue(&["gateway"]);
    write_pem(elsewhere.path(), "client", &cert, Some(&key));

    let project = tempfile::TempDir::new().unwrap();
    let spec = spec_with_tls(
        project.path(),
        port,
        &format!(
            "{{ client_cert: {0}/client.pem, client_key: {0}/client.key, ca: {0}/ca.pem }}",
            elsewhere.path().display()
        ),
    );
    let (status, body, log) = get_mtls(&spec, secrets.path()).await;
    assert_ne!(status, 200);
    assert!(!body.contains("mtls-ok"));
    assert!(log.contains("escapes BARBACANE_SECRETS_DIR"), "{log}");
    let shown = elsewhere.path().display().to_string();
    assert!(!body.contains(&shown), "the response names no file: {body}");
}
