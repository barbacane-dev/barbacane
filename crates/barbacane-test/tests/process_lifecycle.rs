//! The gateway as a process: does it start, serve, and stop cleanly?
//!
//! Every other test drives the gateway in-process, so nothing started the real
//! binary, signalled it and looked at its exit code. That is how a panic on
//! every graceful shutdown reached a release: the drain was correct, only the
//! exit code lied about it, and a supervisor reads a non-zero exit as a crash.
//!
//! The cause was a runtime dropped inside the async context. The plugin host
//! owns broker and directory clients that each carry one, so the panic fired
//! whatever the artifact contained.

use std::process::Command;
use std::time::{Duration, Instant};

use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT, STARTUP_TIMEOUT};

/// The gateway binary these tests drive, found as the test harness finds it.
/// Where cargo puts the test executable itself varies between cargo versions,
/// so the path is not derived from it.
fn gateway_binary() -> std::path::PathBuf {
    barbacane_test::gateway::find_barbacane_binary()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("target/debug/barbacane"))
}

/// The smallest artifact the repository can build: one mock route.
///
/// Returns the reason it could not, never a bare `None`: a test that skips
/// without saying so reads as a pass, and `cargo test` hides the output of one.
fn build_artifact(dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("the repository root is not two levels above the crate")?
        .to_path_buf();
    let mock = repo.join("plugins/mock/mock.wasm");
    if !mock.exists() {
        return Err(format!(
            "{} is not built; run `make plugins`",
            mock.display()
        ));
    }

    let manifest = dir.join("barbacane.yaml");
    std::fs::write(
        &manifest,
        format!("plugins:\n  mock:\n    path: {}\n", mock.display()),
    )
    .map_err(|e| format!("could not write the manifest: {e}"))?;

    let spec = dir.join("api.yaml");
    std::fs::write(
        &spec,
        r#"openapi: "3.0.3"
info: { title: lifecycle, version: "1.0.0" }
paths:
  /ping:
    get:
      operationId: ping
      x-barbacane-dispatch: { name: mock, config: { status: 200, body: "pong" } }
      responses: { "200": { description: ok } }
"#,
    )
    .map_err(|e| format!("could not write the spec: {e}"))?;

    let out = dir.join("api.bca");
    let status = Command::new(gateway_binary())
        .args(["compile", "-s"])
        .arg(&spec)
        .arg("-m")
        .arg(&manifest)
        .arg("-o")
        .arg(&out)
        .output()
        .map_err(|e| format!("could not run the compiler: {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "compiling the fixture failed: {}",
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    Ok(out)
}

fn wait_for_port(port: u16, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// SIGTERM on a healthy gateway must drain and exit 0. A non-zero exit is how
/// a supervisor decides the process crashed, so a clean stop that exits 101
/// produces restart backoff, crash-loop counters and alerts on every deploy.
#[test]
fn sigterm_on_a_serving_gateway_exits_zero() {
    let binary = gateway_binary();
    assert!(
        binary.exists(),
        "{} is not built. Every test in this crate drives the real binary, so a \
         missing one is a broken run, not a reason to pass quietly",
        binary.display()
    );
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");

    let mut cmd = Command::new(&binary);
    cmd.arg("serve")
        .arg("--artifact")
        .arg(&artifact)
        .args(["--listen", LISTEN_ANY_PORT])
        .arg("--allow-unsigned")
        .env_remove("BARBACANE_TRUSTED_PUBKEY");
    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn the gateway");
    let port = process.announced_port("listening on", STARTUP_TIMEOUT);
    assert!(
        port.is_some_and(|port| wait_for_port(port, Duration::from_secs(30))),
        "the gateway never accepted a connection:\n{}",
        process.log().text()
    );

    unsafe {
        libc::kill(process.id().expect("running") as i32, libc::SIGTERM);
    }
    let (mut child, log) = process.into_parts();

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait().expect("wait") {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                panic!("the gateway did not exit within 30s of SIGTERM");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    let stderr = log.text();

    assert!(
        !stderr.contains("panicked"),
        "the gateway panicked while shutting down:\n{stderr}"
    );
    assert_eq!(
        status.code(),
        Some(0),
        "SIGTERM must exit 0, got {status:?}\nstderr:\n{stderr}"
    );
}

/// A taken port is a configuration error. It must report the cause and exit
/// non-zero without panicking, so a misconfiguration stays distinguishable
/// from a crash.
#[test]
fn a_taken_port_reports_the_cause_without_panicking() {
    let binary = gateway_binary();
    if !barbacane_test::require(binary.exists(), "the barbacane binary") {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");

    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let taken = held.local_addr().expect("local addr").port();

    let output = Command::new(&binary)
        .arg("serve")
        .arg("--artifact")
        .arg(&artifact)
        .args(["--listen", &format!("127.0.0.1:{taken}")])
        .arg("--allow-unsigned")
        .env_remove("BARBACANE_TRUSTED_PUBKEY")
        .output()
        .expect("run the gateway");

    drop(held);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to bind"),
        "the real cause must be reported:\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "a taken port is a configuration error, not a panic:\n{stderr}"
    );
    assert_eq!(output.status.code(), Some(1), "stderr:\n{stderr}");
}

/// An admin API that cannot bind must not stop the data plane.
///
/// `/health`, `/metrics` and `/provenance` are operational surface. Losing them
/// to a port conflict is a degradation; refusing to serve traffic over it turns
/// one misconfigured port into an outage.
#[test]
fn a_taken_admin_port_does_not_stop_the_gateway_serving() {
    let binary = gateway_binary();
    if !barbacane_test::require(binary.exists(), "the barbacane binary") {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");

    // Held for the whole of startup, so the gateway's admin bind must fail.
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let taken = held.local_addr().expect("local addr").port();

    let mut cmd = Command::new(&binary);
    cmd.arg("serve")
        .arg("--artifact")
        .arg(&artifact)
        .args(["--listen", LISTEN_ANY_PORT])
        .arg("--allow-unsigned")
        .env_remove("BARBACANE_TRUSTED_PUBKEY")
        .arg("--admin-bind")
        .arg(format!("127.0.0.1:{taken}"));

    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn the gateway");
    let port = match process.announced_port("listening on", STARTUP_TIMEOUT) {
        Some(port) => port,
        None => {
            process.kill_and_reap();
            let log = process.log().text();
            panic!("the gateway stopped instead of serving without its admin API:\n{log}");
        }
    };

    // The data plane is announced before the admin bind is attempted, so wait
    // for the conflict to be reported before asking whether it is still up.
    // Otherwise a gateway on its way out still answers.
    let reported = process
        .log()
        .wait_for("admin API unavailable", Duration::from_secs(30));

    // An HTTP response, not a TCP accept: the socket stays connectable for as
    // long as the listener is open, including while the process is dying.
    let served = serves_http(port, "/__barbacane/health", Duration::from_secs(30));

    // Reap before reading: the reader threads end at EOF, and `text()` read
    // earlier can miss the lines that explain a failure.
    process.kill_and_reap();
    let log = process.log().text();
    drop(held);

    assert!(
        reported,
        "the conflict must be reported, not swallowed:\n{log}"
    );
    assert!(served, "the data plane must still answer requests:\n{log}");
    assert!(
        !log.contains("panicked"),
        "a taken admin port is a configuration error, not a panic:\n{log}"
    );
}

/// Whether `path` answers with a 2xx within `limit`.
///
/// Raw HTTP/1.1 over a socket, to keep this test synchronous like its
/// neighbours rather than pulling in a runtime for one request.
fn serves_http(port: u16, path: &str, limit: Duration) -> bool {
    use std::io::{Read, Write};

    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut response = String::new();
                if stream.read_to_string(&mut response).is_ok() {
                    if let Some(status) = response.lines().next() {
                        if status.starts_with("HTTP/1.1 2") {
                            return true;
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Once SIGTERM arrives, the gateway finishes the requests in flight and
/// takes no new connection; its health endpoints say it is draining.
#[test]
fn a_draining_gateway_refuses_new_connections_and_reports_it() {
    use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT};
    use std::io::{Read, Write};

    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");
    let mut cmd = Command::new(gateway_binary());
    cmd.arg("serve")
        .arg("--artifact")
        .arg(&artifact)
        .args(["--listen", LISTEN_ANY_PORT, "--admin-bind", LISTEN_ANY_PORT])
        .arg("--allow-unsigned")
        .env_remove("BARBACANE_TRUSTED_PUBKEY");
    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn the gateway");
    let port = process
        .announced_port("barbacane: listening on", Duration::from_secs(30))
        .expect("the gateway announced its port");
    let admin_port = process
        .announced_port("barbacane: admin API on", Duration::from_secs(30))
        .expect("the gateway announced its admin port");

    // A request whose body never completes keeps a connection in flight. A
    // complete request first, on the same connection, shows the gateway has
    // accepted it: a connection still in the listen backlog when SIGTERM
    // arrives is never served, and the gateway rightly exits at once.
    let mut in_flight = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    in_flight
        .write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("send a request");
    assert!(
        read_until(&mut in_flight, b"pong"),
        "the gateway answered on the connection"
    );
    in_flight
        .write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\nContent-Length: 10\r\n\r\nabc")
        .expect("send a partial request");
    // Time for the gateway to read the headers and start waiting for the body.
    std::thread::sleep(Duration::from_millis(300));

    unsafe {
        libc::kill(process.id().expect("pid") as i32, libc::SIGTERM);
    }
    assert!(
        process
            .log()
            .wait_for("draining connections", Duration::from_secs(10)),
        "the gateway did not start draining"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !process.has_exited(),
        "the in-flight request keeps the gateway up"
    );

    // No new connection on the main port.
    assert!(
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(2)
        )
        .is_err(),
        "a draining gateway must refuse new connections"
    );

    // The admin health says so.
    let mut admin = std::net::TcpStream::connect(("127.0.0.1", admin_port)).expect("admin connect");
    admin
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("admin request");
    let mut reply = String::new();
    admin.read_to_string(&mut reply).expect("admin reply");
    assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
    assert!(reply.contains("draining"), "{reply}");

    // Ending the in-flight request lets the gateway exit cleanly.
    drop(in_flight);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !process.has_exited() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        process.has_exited(),
        "the gateway did not exit once drained"
    );
    let (mut child, log) = process.into_parts();
    let status = child.wait().expect("wait");
    assert_eq!(status.code(), Some(0), "{}", log.text());
}

/// Read from `stream` until `needle` has arrived, or give up after 10 seconds.
fn read_until(stream: &mut std::net::TcpStream, needle: &[u8]) -> bool {
    use std::io::Read;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    while !seen.windows(needle.len()).any(|window| window == needle) {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return false,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
        }
    }
    true
}

/// `serve` on `artifact` with `args` and `envs`, its output drained.
/// With `open_files`, the process runs under that soft open-file limit.
fn spawn_serve(
    artifact: &std::path::Path,
    args: &[&str],
    envs: &[(&str, &str)],
    open_files: Option<u32>,
) -> LoggedChild {
    let mut cmd = match open_files {
        Some(limit) => {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(format!(r#"ulimit -Sn {limit} && exec "$0" "$@""#))
                .arg(gateway_binary());
            cmd
        }
        None => Command::new(gateway_binary()),
    };
    cmd.arg("serve")
        .arg("--artifact")
        .arg(artifact)
        .args(["--admin-bind", "off", "--allow-unsigned"])
        .args(args)
        .env_remove("BARBACANE_TRUSTED_PUBKEY")
        .env_remove("BARBACANE_MAX_CONNECTIONS")
        .envs(envs.iter().copied());
    LoggedChild::spawn(&mut cmd).expect("spawn the gateway")
}

/// Arguments and environment variables for `serve`.
type ServeInputs<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);

/// The line every serving gateway logs after its listener checks.
const CAP_LINE: &str = "concurrent connections; open-file limit";

/// A gateway serving plaintext on an address other hosts reach says so at
/// start, naming the address.
#[test]
fn a_plaintext_listener_off_loopback_is_warned_about() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");

    let mut process = spawn_serve(&artifact, &["--listen", "0.0.0.0:0"], &[], None);
    let warning = process
        .log()
        .wait_for_line("listening in plaintext", STARTUP_TIMEOUT);
    process.kill_and_reap();
    let log = process.log().text();

    let warning = warning.unwrap_or_else(|| panic!("no plaintext warning:\n{log}"));
    assert!(warning.contains(r#""level":"WARN""#), "{warning}");
    assert!(warning.contains("0.0.0.0:"), "names the address: {warning}");
    assert!(warning.contains("--tls-cert"), "says what to do: {warning}");
}

/// Loopback, or TLS on any address, raises no plaintext warning.
#[test]
fn a_loopback_or_tls_listener_is_not_warned_about() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");
    let certs = barbacane_test::gateway::generate_test_certificates(dir.path())
        .expect("generate test certificates");
    let cert = certs.cert_path.to_string_lossy().into_owned();
    let key = certs.key_path.to_string_lossy().into_owned();

    for args in [
        vec!["--listen", LISTEN_ANY_PORT],
        vec!["--listen", "[::1]:0"],
        vec![
            "--listen",
            "0.0.0.0:0",
            "--tls-cert",
            &cert,
            "--tls-key",
            &key,
        ],
    ] {
        let mut process = spawn_serve(&artifact, &args, &[], None);
        // Logged after the plaintext check, on the same stream.
        let started = process.log().wait_for(CAP_LINE, STARTUP_TIMEOUT);
        process.kill_and_reap();
        let log = process.log().text();
        assert!(started, "{args:?}: the gateway did not start:\n{log}");
        assert!(
            !log.contains("listening in plaintext"),
            "{args:?}: no warning expected:\n{log}"
        );
    }
}

/// The cap set by `--max-connections` or `BARBACANE_MAX_CONNECTIONS` closes a
/// connection past it, and a closed connection frees its place.
#[test]
fn connections_past_the_cap_are_closed() {
    use std::io::Write;

    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");

    let flag: ServeInputs = (&["--max-connections", "1"], &[]);
    let env: ServeInputs = (&[], &[("BARBACANE_MAX_CONNECTIONS", "1")]);
    for (args, envs) in [flag, env] {
        let mut args = args.to_vec();
        args.extend(["--listen", LISTEN_ANY_PORT]);
        let mut process = spawn_serve(&artifact, &args, envs, None);
        let port = process
            .announced_port("barbacane: listening on", STARTUP_TIMEOUT)
            .unwrap_or_else(|| panic!("{args:?} {envs:?}: no port:\n{}", process.log().text()));
        assert!(
            process
                .log()
                .wait_for("serving at most 1 concurrent connections", STARTUP_TIMEOUT),
            "{args:?} {envs:?}: the cap is logged:\n{}",
            process.log().text()
        );

        // The one place, taken by a kept-alive connection.
        let mut held = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        held.write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("send a request");
        assert!(
            read_until(&mut held, b"pong"),
            "the first connection is served"
        );

        let mut over = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let _ = over.write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(
            closed_without_answer(&mut over),
            "{args:?} {envs:?}: a connection past the cap is closed:\n{}",
            process.log().text()
        );
        assert!(
            process
                .log()
                .wait_for("connection limit reached", Duration::from_secs(10)),
            "{args:?} {envs:?}: the shed connection is logged"
        );

        drop(held);
        assert!(
            serves_http(port, "/ping", Duration::from_secs(10)),
            "{args:?} {envs:?}: a closed connection frees its place:\n{}",
            process.log().text()
        );
        process.kill_and_reap();
    }
}

/// Whether the peer closed `stream` without sending anything. A read timeout
/// is not a close.
fn closed_without_answer(stream: &mut std::net::TcpStream) -> bool {
    use std::io::Read;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = [0u8; 64];
    match stream.read(&mut buf) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
        ),
    }
}

/// The cap is logged against the open-file limit, with a warning when the
/// limit runs out first.
#[test]
fn the_connection_cap_is_checked_against_the_open_file_limit() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");
    let listen = ["--listen", LISTEN_ANY_PORT];

    // The default cap of 10000 under a limit of 1024.
    let mut process = spawn_serve(&artifact, &listen, &[], Some(1024));
    let warning = process
        .log()
        .wait_for_line("is not above --max-connections", STARTUP_TIMEOUT);
    let logged = process.log().find_line(CAP_LINE);
    process.kill_and_reap();
    let log = process.log().text();
    let warning = warning.unwrap_or_else(|| panic!("no open-file warning:\n{log}"));
    assert!(warning.contains(r#""level":"WARN""#), "{warning}");
    assert!(
        warning.contains("open-file limit 1024 is not above --max-connections 10000"),
        "{warning}"
    );
    assert!(
        logged.is_some_and(|line| line
            .contains("serving at most 10000 concurrent connections; open-file limit 1024")),
        "the cap and the limit are logged:\n{log}"
    );

    // A cap below the limit is logged, without a warning.
    let mut args = listen.to_vec();
    args.extend(["--max-connections", "512"]);
    let mut process = spawn_serve(&artifact, &args, &[], Some(1024));
    let logged = process.log().wait_for_line(
        "serving at most 512 concurrent connections",
        STARTUP_TIMEOUT,
    );
    process.kill_and_reap();
    let log = process.log().text();
    assert!(
        logged.is_some_and(|line| line.contains("open-file limit 1024")),
        "the cap and the limit are logged:\n{log}"
    );
    assert!(!log.contains("is not above"), "no warning expected:\n{log}");
}

/// A cap that is not a whole number from 1 up, by flag or environment, stops
/// the gateway at start with a usage error; an empty variable is the default.
#[test]
fn an_invalid_connection_cap_is_refused_at_start() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = build_artifact(dir.path()).expect("build the fixture artifact");

    let cases: [ServeInputs; 5] = [
        (&["--max-connections", "0"], &[]),
        (&["--max-connections", "many"], &[]),
        (&[], &[("BARBACANE_MAX_CONNECTIONS", "0")]),
        (&[], &[("BARBACANE_MAX_CONNECTIONS", "-5")]),
        // Above what a semaphore holds.
        (
            &[],
            &[("BARBACANE_MAX_CONNECTIONS", "18446744073709551615")],
        ),
    ];
    for (args, envs) in cases {
        let mut args = args.to_vec();
        args.extend(["--listen", LISTEN_ANY_PORT]);
        let mut process = spawn_serve(&artifact, &args, envs, None);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !process.has_exited() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let exited = process.has_exited();
        process.kill_and_reap();
        let log = process.log().text();
        assert!(exited, "{args:?} {envs:?}: the gateway started:\n{log}");
        let (mut child, _) = process.into_parts();
        let code = child.wait().expect("wait").code();
        assert_eq!(code, Some(2), "{args:?} {envs:?}: a usage error:\n{log}");
        assert!(log.contains("--max-connections"), "names the flag:\n{log}");
        assert!(!log.contains("panicked"), "{args:?} {envs:?}:\n{log}");
    }

    let mut process = spawn_serve(
        &artifact,
        &["--listen", LISTEN_ANY_PORT],
        &[("BARBACANE_MAX_CONNECTIONS", "")],
        None,
    );
    let started = process.log().wait_for(
        "serving at most 10000 concurrent connections",
        STARTUP_TIMEOUT,
    );
    process.kill_and_reap();
    assert!(
        started,
        "an empty variable is the default:\n{}",
        process.log().text()
    );
}
