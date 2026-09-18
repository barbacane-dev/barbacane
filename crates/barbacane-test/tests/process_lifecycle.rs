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

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The gateway binary built alongside these tests.
fn gateway_binary() -> std::path::PathBuf {
    let mut dir = std::env::current_exe().expect("test binary path");
    dir.pop(); // deps/
    dir.pop(); // debug/ or release/
    dir.join("barbacane")
}

/// The smallest artifact the repository can build: one mock route.
fn build_artifact(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    let mock = repo.join("plugins/mock/mock.wasm");
    if !mock.exists() {
        return None;
    }

    let manifest = dir.join("barbacane.yaml");
    std::fs::write(
        &manifest,
        format!("plugins:\n  mock:\n    path: {}\n", mock.display()),
    )
    .ok()?;

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
    .ok()?;

    let out = dir.join("api.bca");
    let status = Command::new(gateway_binary())
        .args(["compile", "-s"])
        .arg(&spec)
        .arg("-m")
        .arg(&manifest)
        .arg("-o")
        .arg(&out)
        .output()
        .ok()?;
    if !status.status.success() {
        eprintln!(
            "skipping: could not compile the fixture: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        return None;
    }
    Some(out)
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
    if !binary.exists() {
        eprintln!("skipping: {} is not built", binary.display());
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let Some(artifact) = build_artifact(dir.path()) else {
        return;
    };

    let mut child = Command::new(&binary)
        .arg("serve")
        .arg("--artifact")
        .arg(&artifact)
        .args(["--listen", "127.0.0.1:34201"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the gateway");

    assert!(
        wait_for_port(34201, Duration::from_secs(30)),
        "the gateway never accepted a connection"
    );

    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }

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

    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut stderr);
    }

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
    if !binary.exists() {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let Some(artifact) = build_artifact(dir.path()) else {
        return;
    };

    let held = std::net::TcpListener::bind("127.0.0.1:34202").expect("hold the port");

    let output = Command::new(&binary)
        .arg("serve")
        .arg("--artifact")
        .arg(&artifact)
        .args(["--listen", "127.0.0.1:34202"])
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
