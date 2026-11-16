//! The listener's TLS: client certificates (`--tls-client-ca`) and reloading
//! the certificate, key and client CA while serving.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT, STARTUP_TIMEOUT};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedKey, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

fn binary() -> PathBuf {
    barbacane_test::gateway::find_barbacane_binary()
        .map(PathBuf::from)
        .expect("barbacane binary not found; run `cargo build -p barbacane` first")
}

/// One mock route, `GET /ping` answering `pong`.
fn artifact(dir: &Path) -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .to_path_buf();
    let mock = repo.join("plugins/mock/mock.wasm");
    assert!(
        mock.exists(),
        "{} is not built; run `make plugins`",
        mock.display()
    );
    std::fs::write(
        dir.join("barbacane.yaml"),
        format!("plugins:\n  mock:\n    path: {}\n", mock.display()),
    )
    .expect("manifest");
    std::fs::write(
        dir.join("api.yaml"),
        r#"openapi: "3.0.3"
info: { title: tls, version: "1.0.0" }
paths:
  /ping:
    get:
      operationId: ping
      x-barbacane-dispatch: { name: mock, config: { status: 200, body: "pong" } }
      responses: { "200": { description: ok } }
"#,
    )
    .expect("spec");
    let out = dir.join("api.bca");
    let status = Command::new(binary())
        .args(["compile", "-s"])
        .arg(dir.join("api.yaml"))
        .arg("-m")
        .arg(dir.join("barbacane.yaml"))
        .arg("-o")
        .arg(&out)
        .env_remove("BARBACANE_SIGNING_KEY")
        .output()
        .expect("compile");
    assert!(status.status.success(), "{status:?}");
    out
}

/// A server certificate for `localhost`, self-signed.
struct ServerCert {
    cert: String,
    key: String,
    der: CertificateDer<'static>,
}

fn server_cert() -> ServerCert {
    let CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("server cert");
    ServerCert {
        cert: cert.pem(),
        key: key_pair.serialize_pem(),
        der: cert.der().clone(),
    }
}

/// A CA and the client certificates it signs.
struct Ca {
    params: CertificateParams,
    key: KeyPair,
    pem: String,
}

impl Ca {
    fn new(name: &str) -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let key = KeyPair::generate().expect("ca key");
        let pem = params.clone().self_signed(&key).expect("ca cert").pem();
        Self { params, key, pem }
    }

    fn client(&self, name: &str) -> Client {
        let issuer = self.params.clone().self_signed(&self.key).expect("issuer");
        let mut params = CertificateParams::new(vec![name.to_string()]).expect("params");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let key = KeyPair::generate().expect("client key");
        let cert = params
            .signed_by(&key, &issuer, &self.key)
            .expect("client cert");
        Client {
            cert: cert.der().clone(),
            key: PrivateKeyDer::try_from(key.serialize_der()).expect("key der"),
        }
    }
}

struct Client {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

/// `GET /ping` over TLS, trusting `roots`, presenting `client` when given.
/// Returns the response and the certificate the server presented.
fn ping(
    port: u16,
    roots: &[&CertificateDer<'static>],
    client: Option<&Client>,
) -> Result<(String, CertificateDer<'static>), String> {
    let mut store = rustls::RootCertStore::empty();
    for root in roots {
        store.add((*root).clone()).map_err(|e| e.to_string())?;
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(store);
    let config = match client {
        Some(client) => builder
            .with_client_auth_cert(vec![client.cert.clone()], client.key.clone_key())
            .map_err(|e| e.to_string())?,
        None => builder.with_no_client_auth(),
    };
    let mut conn = rustls::ClientConnection::new(
        Arc::new(config),
        "localhost".try_into().expect("server name"),
    )
    .map_err(|e| e.to_string())?;
    let mut socket =
        std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let mut stream = rustls::Stream::new(&mut conn, &mut socket);
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(_) if !response.is_empty() => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    let response = String::from_utf8_lossy(&response).into_owned();
    if !response.starts_with("HTTP/1.1 200") || !response.contains("pong") {
        return Err(format!("unexpected response: {response}"));
    }
    let presented = conn
        .peer_certificates()
        .and_then(|chain| chain.first())
        .ok_or("no server certificate")?
        .clone()
        .into_owned();
    Ok((response, presented))
}

struct Files {
    cert: PathBuf,
    key: PathBuf,
    ca: PathBuf,
}

impl Files {
    fn new(dir: &Path) -> Self {
        Self {
            cert: dir.join("server.crt"),
            key: dir.join("server.key"),
            ca: dir.join("clients.crt"),
        }
    }

    fn serve(&self, cert: &ServerCert) {
        std::fs::write(&self.cert, &cert.cert).expect("write cert");
        std::fs::write(&self.key, &cert.key).expect("write key");
    }
}

/// `serve` over TLS from `files`, with `args`; returns the process and its port.
fn spawn(artifact: &Path, files: &Files, args: &[&str]) -> (LoggedChild, u16) {
    let mut cmd = Command::new(binary());
    cmd.arg("serve")
        .arg("--artifact")
        .arg(artifact)
        .args([
            "--listen",
            LISTEN_ANY_PORT,
            "--admin-bind",
            "off",
            "--allow-unsigned",
        ])
        .arg("--tls-cert")
        .arg(&files.cert)
        .arg("--tls-key")
        .arg(&files.key)
        .args(args)
        .env_remove("BARBACANE_TRUSTED_PUBKEY");
    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn the gateway");
    let port = process
        .announced_port("barbacane: listening on", STARTUP_TIMEOUT)
        .unwrap_or_else(|| panic!("the gateway did not start:\n{}", process.log().text()));
    (process, port)
}

fn sighup(process: &LoggedChild) {
    let pid = process.id().expect("running") as i32;
    // SAFETY: signalling a child this test spawned and still owns.
    let sent = unsafe { libc::kill(pid, libc::SIGHUP) };
    assert_eq!(sent, 0, "send SIGHUP");
}

/// Wait until `line` has been logged `count` times.
fn wait_for_count(process: &LoggedChild, line: &str, count: usize, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if process.log().text().matches(line).count() >= count {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

const RELOADED: &str = "TLS configuration reloaded";
const NOT_RELOADED: &str = "TLS configuration not reloaded";

#[test]
fn with_a_client_ca_every_client_presents_a_certificate_from_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = artifact(dir.path());
    let files = Files::new(dir.path());
    let server = server_cert();
    files.serve(&server);
    let ca = Ca::new("clients");
    std::fs::write(&files.ca, &ca.pem).expect("write ca");
    let ca_arg = files.ca.to_string_lossy().into_owned();

    let (mut process, port) = spawn(&artifact, &files, &["--tls-client-ca", &ca_arg]);
    let roots = [&server.der];

    let trusted = ca.client("trusted");
    assert!(
        ping(port, &roots, Some(&trusted)).is_ok(),
        "a certificate from the CA is accepted: {:?}",
        ping(port, &roots, Some(&trusted))
    );
    assert!(
        ping(port, &roots, None).is_err(),
        "no certificate is refused"
    );
    let other = Ca::new("other").client("stranger");
    assert!(
        ping(port, &roots, Some(&other)).is_err(),
        "a certificate from another CA is refused"
    );
    process.kill_and_reap();

    // Without --tls-client-ca, no certificate is asked for.
    let (mut process, port) = spawn(&artifact, &files, &[]);
    assert!(ping(port, &roots, None).is_ok());
    process.kill_and_reap();
}

#[test]
fn sighup_loads_a_new_certificate_for_new_connections() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = artifact(dir.path());
    let files = Files::new(dir.path());
    let (first, second) = (server_cert(), server_cert());
    files.serve(&first);
    let (mut process, port) = spawn(&artifact, &files, &["--tls-reload-interval", "0"]);
    let roots = [&first.der, &second.der];

    let (_, presented) = ping(port, &roots, None).expect("served");
    assert_eq!(presented, first.der);

    files.serve(&second);
    sighup(&process);
    assert!(
        wait_for_count(&process, RELOADED, 1, Duration::from_secs(10)),
        "the reload is logged:\n{}",
        process.log().text()
    );
    let (_, presented) = ping(port, &roots, None).expect("served after the reload");
    assert_eq!(
        presented, second.der,
        "new connections get the new certificate"
    );
    assert!(!process.has_exited(), "SIGHUP does not stop the gateway");
    process.kill_and_reap();
}

#[test]
fn changed_files_are_loaded_without_a_signal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = artifact(dir.path());
    let files = Files::new(dir.path());
    let (first, second) = (server_cert(), server_cert());
    files.serve(&first);
    let (mut process, port) = spawn(&artifact, &files, &["--tls-reload-interval", "1"]);
    let roots = [&first.der, &second.der];
    assert_eq!(ping(port, &roots, None).expect("served").1, first.der);

    files.serve(&second);
    assert!(
        wait_for_count(&process, RELOADED, 1, Duration::from_secs(15)),
        "the change is picked up:\n{}",
        process.log().text()
    );
    assert_eq!(ping(port, &roots, None).expect("served").1, second.der);
    process.kill_and_reap();
}

#[test]
fn files_that_do_not_load_leave_the_previous_configuration() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = artifact(dir.path());
    let files = Files::new(dir.path());
    let (first, second) = (server_cert(), server_cert());
    files.serve(&first);
    let (mut process, port) = spawn(&artifact, &files, &["--tls-reload-interval", "0"]);
    let roots = [&first.der, &second.der];

    // The new certificate with the old key: they do not match.
    std::fs::write(&files.cert, &second.cert).expect("write cert");
    sighup(&process);
    assert!(
        wait_for_count(&process, NOT_RELOADED, 1, Duration::from_secs(10)),
        "the failure is logged:\n{}",
        process.log().text()
    );
    assert_eq!(
        ping(port, &roots, None).expect("still served").1,
        first.der,
        "the previous certificate is kept"
    );

    // Not a certificate at all.
    std::fs::write(&files.cert, "not a certificate").expect("write cert");
    sighup(&process);
    assert!(wait_for_count(
        &process,
        NOT_RELOADED,
        2,
        Duration::from_secs(10)
    ));
    assert_eq!(ping(port, &roots, None).expect("still served").1, first.der);

    // Fixed: the next reload takes.
    files.serve(&second);
    sighup(&process);
    assert!(wait_for_count(
        &process,
        RELOADED,
        1,
        Duration::from_secs(10)
    ));
    assert_eq!(ping(port, &roots, None).expect("served").1, second.der);
    process.kill_and_reap();
}

#[test]
fn the_client_ca_is_reloaded_too() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = artifact(dir.path());
    let files = Files::new(dir.path());
    let server = server_cert();
    files.serve(&server);
    let (old_ca, new_ca) = (Ca::new("old"), Ca::new("new"));
    std::fs::write(&files.ca, &old_ca.pem).expect("write ca");
    let ca_arg = files.ca.to_string_lossy().into_owned();
    let (mut process, port) = spawn(
        &artifact,
        &files,
        &["--tls-client-ca", &ca_arg, "--tls-reload-interval", "0"],
    );
    let roots = [&server.der];
    let (old_client, new_client) = (old_ca.client("old"), new_ca.client("new"));
    assert!(ping(port, &roots, Some(&old_client)).is_ok());
    assert!(ping(port, &roots, Some(&new_client)).is_err());

    std::fs::write(&files.ca, &new_ca.pem).expect("write ca");
    sighup(&process);
    assert!(wait_for_count(
        &process,
        RELOADED,
        1,
        Duration::from_secs(10)
    ));
    assert!(ping(port, &roots, Some(&new_client)).is_ok());
    assert!(ping(port, &roots, Some(&old_client)).is_err());
    process.kill_and_reap();
}

#[test]
fn a_client_ca_needs_tls_and_certificates() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = artifact(dir.path());
    let files = Files::new(dir.path());
    files.serve(&server_cert());
    std::fs::write(&files.ca, "").expect("write ca");

    // A gateway that starts anyway serves until killed: bounded, so that is a
    // failure rather than a hang.
    let run = |args: &[&str]| -> (Option<i32>, String) {
        let mut cmd = Command::new(binary());
        cmd.arg("serve")
            .arg("--artifact")
            .arg(&artifact)
            .args([
                "--listen",
                LISTEN_ANY_PORT,
                "--admin-bind",
                "off",
                "--allow-unsigned",
            ])
            .args(args)
            .env_remove("BARBACANE_TRUSTED_PUBKEY");
        let mut process = LoggedChild::spawn(&mut cmd).expect("spawn the gateway");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !process.has_exited() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let exited = process.has_exited();
        process.kill_and_reap();
        let log = process.log().text();
        assert!(exited, "{args:?}: the gateway started:\n{log}");
        let (mut child, _) = process.into_parts();
        (child.wait().expect("wait").code(), log)
    };
    let ca = files.ca.to_string_lossy().into_owned();
    let (code, stderr) = run(&["--tls-client-ca", &ca]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("--tls-client-ca requires --tls-cert"),
        "{stderr}"
    );

    let (cert, key) = (
        files.cert.to_string_lossy().into_owned(),
        files.key.to_string_lossy().into_owned(),
    );
    let (code, stderr) = run(&[
        "--tls-cert",
        &cert,
        "--tls-key",
        &key,
        "--tls-client-ca",
        &ca,
    ]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("no certificates found"), "{stderr}");
}
