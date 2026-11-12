//! A request body must arrive within the keep-alive deadline; a client that
//! trickles it is answered 408 and disconnected.

use std::time::Duration;

use barbacane_test::TestGateway;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn create_spec() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let mock = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins/mock/mock.wasm");
    std::fs::write(
        temp_dir.path().join("barbacane.yaml"),
        format!("plugins:\n  mock:\n    path: {}\n", mock.display()),
    )
    .expect("failed to write manifest file");
    let spec_path = temp_dir.path().join("items.yaml");
    std::fs::write(
        &spec_path,
        r#"openapi: "3.1.0"
info:
  title: Body Deadline Test API
  version: "1.0.0"
paths:
  /items:
    post:
      operationId: createItem
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: mock
        config:
          status: 201
          body: '{"created":true}'
      responses:
        "201":
          description: Created
"#,
    )
    .expect("failed to write spec file");
    (temp_dir, spec_path)
}

#[tokio::test]
async fn a_body_that_does_not_arrive_in_time_is_answered_408() {
    let (_temp_dir, spec_path) = create_spec();
    let gateway = TestGateway::from_spec_with_args(
        spec_path.to_str().unwrap(),
        &["--keepalive-timeout", "1"],
    )
    .await
    .expect("failed to start gateway");

    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", gateway.port()))
        .await
        .expect("connect");
    socket
        .write_all(
            b"POST /items HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"partial\":",
        )
        .await
        .expect("send the head and a few bytes");

    let mut reply = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let mut buf = [0u8; 4096];
        loop {
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => reply.extend_from_slice(&buf[..n]),
            }
        }
    })
    .await;
    assert!(
        read.is_ok(),
        "no answer within five seconds for a body that never completes"
    );
    let reply = String::from_utf8_lossy(&reply);
    assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
    assert!(
        reply.contains("urn:barbacane:error:request-timeout"),
        "{reply}"
    );
}
