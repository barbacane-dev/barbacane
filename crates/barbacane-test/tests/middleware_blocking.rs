//! A middleware waiting on a slow backend holds its own request, not the
//! gateway: other requests are served meanwhile.

use std::time::{Duration, Instant};

use barbacane_test::TestGateway;

/// A spec with `/guarded` behind opa-authz pointed at `opa_url`, and `/open`
/// with no middleware.
fn create_spec(opa_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
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
            "plugins:\n  mock:\n    path: {}\n  opa-authz:\n    path: {}\n",
            plugins_dir.join("mock/mock.wasm").display(),
            plugins_dir.join("opa-authz/opa-authz.wasm").display()
        ),
    )
    .expect("failed to write manifest file");
    let spec_path = temp_dir.path().join("blocking.yaml");
    std::fs::write(
        &spec_path,
        format!(
            r#"openapi: "3.1.0"
info:
  title: Blocking Middleware Test API
  version: "1.0.0"
paths:
  /guarded:
    get:
      operationId: getGuarded
      x-barbacane-middlewares:
        - name: opa-authz
          config:
            opa_url: "{opa_url}"
            timeout: 10
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"guarded":true}}'
      responses:
        "200":
          description: OK
  /open:
    get:
      operationId: getOpen
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"open":true}}'
      responses:
        "200":
          description: OK
"#
        ),
    )
    .expect("failed to write spec file");
    (temp_dir, spec_path)
}

#[tokio::test]
async fn a_slow_middleware_backend_does_not_stall_other_requests() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let opa = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/data/authz/allow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"result": true}))
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&opa)
        .await;

    let (_temp_dir, spec_path) = create_spec(&format!("{}/v1/data/authz/allow", opa.uri()));
    // One worker thread: whatever blocks it blocks every request.
    let gateway = TestGateway::from_spec_with_env(
        spec_path.to_str().unwrap(),
        &[("TOKIO_WORKER_THREADS", "1")],
    )
    .await
    .expect("failed to start gateway");

    let guarded = {
        let client = reqwest::Client::new();
        let url = format!("{}/guarded", gateway.base_url());
        tokio::spawn(async move { client.get(url).send().await.unwrap().status() })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;

    let started = Instant::now();
    let resp = reqwest::Client::new()
        .get(format!("{}/open", gateway.base_url()))
        .send()
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(resp.status(), 200);
    assert!(
        elapsed < Duration::from_millis(1500),
        "the open route waited {elapsed:?} behind the guarded one"
    );
    assert_eq!(guarded.await.unwrap(), 200);
}
