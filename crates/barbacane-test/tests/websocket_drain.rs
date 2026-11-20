//! Work that outlives its request, at SIGTERM: a relayed WebSocket gets a
//! Close 1001 (going away) on both sides, the middleware `on_response` after a
//! streamed response runs to its end, and the gateway exits 0 after both.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT, STARTUP_TIMEOUT};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;

fn binary() -> PathBuf {
    barbacane_test::gateway::find_barbacane_binary()
        .map(PathBuf::from)
        .expect("barbacane binary not found; run `cargo build -p barbacane` first")
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .to_path_buf()
}

fn plugin(name: &str) -> PathBuf {
    let path = repo().join(format!("plugins/{name}/{name}.wasm"));
    assert!(
        path.exists(),
        "{} is not built; run `make plugins`",
        path.display()
    );
    path
}

/// Compile `spec` with `plugins` (name, wasm), allowing plaintext upstreams.
fn compile(dir: &Path, plugins: &[(&str, PathBuf)], spec: &str) -> PathBuf {
    let manifest: String = plugins
        .iter()
        .map(|(name, wasm)| format!("  {name}:\n    path: {}\n", wasm.display()))
        .collect();
    std::fs::write(dir.join("barbacane.yaml"), format!("plugins:\n{manifest}")).expect("manifest");
    std::fs::write(dir.join("api.yaml"), spec).expect("spec");
    let out = dir.join("api.bca");
    let output = Command::new(binary())
        .args(["compile", "--allow-plaintext", "-s"])
        .arg(dir.join("api.yaml"))
        .arg("-m")
        .arg(dir.join("barbacane.yaml"))
        .arg("-o")
        .arg(&out)
        .env_remove("BARBACANE_SIGNING_KEY")
        .output()
        .expect("compile");
    assert!(output.status.success(), "{output:?}");
    out
}

/// `serve` on `artifact` with loopback upstreams allowed; returns the process
/// and its port.
fn serve(artifact: &Path) -> (LoggedChild, u16) {
    let mut cmd = Command::new(binary());
    cmd.arg("serve")
        .arg("--artifact")
        .arg(artifact)
        .args(["--listen", LISTEN_ANY_PORT, "--admin-bind", "off"])
        .args(["--allow-unsigned", "--allow-plaintext-upstream"])
        .env_remove("BARBACANE_TRUSTED_PUBKEY")
        .env("BARBACANE_ALLOW_INTERNAL_EGRESS", "1");
    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn the gateway");
    let port = process
        .announced_port("barbacane: listening on", STARTUP_TIMEOUT)
        .unwrap_or_else(|| panic!("the gateway did not start:\n{}", process.log().text()));
    (process, port)
}

fn sigterm(process: &LoggedChild) {
    // SAFETY: signalling a child this test spawned and still owns.
    unsafe {
        libc::kill(process.id().expect("running") as i32, libc::SIGTERM);
    }
}

/// Wait up to `limit` for the gateway to exit; its exit code.
async fn exit_code(mut process: LoggedChild, limit: Duration) -> Option<i32> {
    let deadline = Instant::now() + limit;
    while !process.has_exited() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        process.has_exited(),
        "the gateway did not exit:\n{}",
        process.log().text()
    );
    let (mut child, _) = process.into_parts();
    child.wait().expect("wait").code()
}

/// An artifact relaying `GET /ws` to `ws://127.0.0.1:<upstream>`.
fn ws_artifact(dir: &Path, upstream: u16) -> PathBuf {
    compile(
        dir,
        &[("ws-upstream", plugin("ws-upstream"))],
        &format!(
            r#"openapi: "3.0.3"
info: {{ title: ws, version: "1.0.0" }}
paths:
  /ws:
    get:
      operationId: ws
      x-barbacane-dispatch:
        name: ws-upstream
        config: {{ url: "ws://127.0.0.1:{upstream}", connect_timeout: 5 }}
      responses: {{ "101": {{ description: Switching Protocols }} }}
"#
        ),
    )
}

/// An echo server that reports the code of each Close frame it receives.
async fn echo_upstream() -> (u16, tokio::sync::mpsc::UnboundedReceiver<Option<CloseCode>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (closes, received) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let closes = closes.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                while let Some(Ok(message)) = ws.next().await {
                    match message {
                        Message::Close(frame) => {
                            let _ = closes.send(frame.map(|f| f.code));
                            break;
                        }
                        m if m.is_text() || m.is_binary() => {
                            if ws.send(m).await.is_err() {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    (port, received)
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_closes_a_relayed_websocket_with_going_away() {
    let (upstream, mut upstream_closes) = echo_upstream().await;
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = ws_artifact(dir.path(), upstream);
    let (process, port) = serve(&artifact);

    let (mut client, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws"))
        .await
        .unwrap_or_else(|e| panic!("connect through the gateway: {e}\n{}", process.log().text()));
    client.send(Message::text("hello")).await.expect("send");
    let echoed = tokio::time::timeout(Duration::from_secs(5), client.next())
        .await
        .expect("an echo in time");
    assert!(
        matches!(&echoed, Some(Ok(m)) if m.to_text().ok() == Some("hello")),
        "the relay works: {echoed:?}"
    );

    sigterm(&process);

    let closed = tokio::time::timeout(Duration::from_secs(5), client.next())
        .await
        .unwrap_or_else(|_| panic!("no frame after SIGTERM:\n{}", process.log().text()));
    match closed {
        Some(Ok(Message::Close(Some(frame)))) => assert_eq!(frame.code, CloseCode::Away),
        other => panic!(
            "the client gets Close 1001, got {other:?}\n{}",
            process.log().text()
        ),
    }
    let upstream_close = tokio::time::timeout(Duration::from_secs(5), upstream_closes.recv())
        .await
        .expect("the upstream gets a Close in time");
    assert_eq!(upstream_close, Some(Some(CloseCode::Away)));

    assert_eq!(exit_code(process, Duration::from_secs(10)).await, Some(0));
}

/// An upstream that streams one server-sent event every 2 seconds, three in all.
async fn slow_stream_upstream() -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
                if stream.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for i in 0..3 {
                    let event = format!("data: {i}\n\n");
                    let chunk = format!("{:x}\r\n{event}\r\n", event.len());
                    if stream.write_all(chunk.as_bytes()).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let _ = stream.write_all(b"0\r\n\r\n").await;
            });
        }
    });
    port
}

/// A client that leaves a streamed response early ends the dispatcher's read of
/// the upstream, and the middleware `on_response` (here `http-log`) runs after
/// it. At SIGTERM the gateway waits for both before exiting.
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_waits_for_the_middleware_after_a_streamed_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = slow_stream_upstream().await;
    let sink = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/log"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink)
        .await;

    let echo = repo().join(
        "tests/fixture-plugins/streaming-echo/target/wasm32-unknown-unknown/release/streaming_echo.wasm",
    );
    assert!(echo.exists(), "{} is not built", echo.display());
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = compile(
        dir.path(),
        &[("streaming-echo", echo), ("http-log", plugin("http-log"))],
        &format!(
            r#"openapi: "3.0.3"
info: {{ title: stream, version: "1.0.0" }}
paths:
  /stream:
    get:
      operationId: stream
      x-barbacane-middlewares:
        - name: http-log
          config: {{ endpoint: "{sink}/log", timeout_ms: 5000 }}
      x-barbacane-dispatch:
        name: streaming-echo
        config: {{ url: "http://127.0.0.1:{upstream}/stream" }}
      responses: {{ "200": {{ description: ok }} }}
"#,
            sink = sink.uri(),
        ),
    );
    let (process, port) = serve(&artifact);

    // Read the first event, then leave.
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    client
        .write_all(b"GET /stream HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("send");
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    while !seen.windows(7).any(|w| w == b"data: 0") {
        let n = tokio::time::timeout(Duration::from_secs(10), client.read(&mut buf))
            .await
            .expect("the first event in time")
            .expect("read");
        assert!(
            n > 0,
            "the stream started: {}",
            String::from_utf8_lossy(&seen)
        );
        seen.extend_from_slice(&buf[..n]);
    }
    drop(client);

    sigterm(&process);
    assert_eq!(exit_code(process, Duration::from_secs(15)).await, Some(0));
    assert_eq!(
        sink.received_requests().await.map_or(0, |r| r.len()),
        1,
        "http-log ran after the stream ended, before the gateway exited"
    );
}
