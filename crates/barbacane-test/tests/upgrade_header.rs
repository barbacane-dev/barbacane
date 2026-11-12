//! Only a request shaped as a WebSocket handshake skips body handling; an
//! `Upgrade: websocket` header on anything else leaves the body in place.

use barbacane_test::TestGateway;

fn create_spec() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let plugins_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    std::fs::write(
        temp_dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  mock:\n    path: {}\n  request-size-limit:\n    path: {}\n",
            plugins_dir.join("mock/mock.wasm").display(),
            plugins_dir
                .join("request-size-limit/request-size-limit.wasm")
                .display()
        ),
    )
    .expect("failed to write manifest file");
    let spec_path = temp_dir.path().join("limited.yaml");
    std::fs::write(
        &spec_path,
        r#"openapi: "3.1.0"
info:
  title: Upgrade Header Test API
  version: "1.0.0"
paths:
  /items:
    post:
      operationId: createItem
      x-barbacane-middlewares:
        - name: request-size-limit
          config:
            max_bytes: 16
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
        "413":
          description: Too large
"#,
    )
    .expect("failed to write spec file");
    (temp_dir, spec_path)
}

#[tokio::test]
async fn an_upgrade_header_on_a_post_does_not_skip_the_body() {
    let (_temp_dir, spec_path) = create_spec();
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let body = format!(r#"{{"pad":"{}"}}"#, "x".repeat(100));
    let plain = gateway.post("/items", &body).await.unwrap();
    assert_eq!(
        plain.status(),
        413,
        "the size limit applies to a plain POST"
    );

    let with_upgrade = gateway
        .request_builder(reqwest::Method::POST, "/items")
        .header("content-type", "application/json")
        .header("upgrade", "websocket")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        with_upgrade.status(),
        413,
        "the size limit applies to a POST carrying an Upgrade header"
    );
}
