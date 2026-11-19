//! Plugins written against the 1.0 plugin API keep working across 1.x.
//!
//! `tests/fixture-plugins/compat-1.0` holds a middleware and a dispatcher
//! written against the 1.0 SDK and macros and never edited since (CI refuses a
//! change to them). The build script compiles them with the current SDK and
//! macros; these tests load them in a gateway and check what each part of the
//! API carries: the macros' exports and embedded sections, the request and
//! response a plugin sees and returns, the context, logging, hashing and a
//! middleware's short-circuit.

use std::path::PathBuf;

use barbacane_test::TestGateway;
use sha2::{Digest, Sha256};

/// The built `.wasm` of the fixture `kind` (`middleware` or `dispatcher`).
fn fixture_wasm(kind: &str) -> PathBuf {
    let wasm = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixture-plugins/compat-1.0")
        .join(kind)
        .join(format!(
            "target/wasm32-unknown-unknown/release/compat_{kind}.wasm"
        ));
    assert!(
        wasm.exists(),
        "{} is not built: barbacane-test's build script builds it, which needs \
         the wasm32-unknown-unknown target",
        wasm.display()
    );
    wasm
}

/// A spec running the fixture middleware globally in front of the fixture
/// dispatcher, written next to a manifest naming both.
fn compat_spec() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  compat-middleware:\n    path: {}\n  compat-dispatcher:\n    path: {}\n",
            fixture_wasm("middleware").display(),
            fixture_wasm("dispatcher").display(),
        ),
    )
    .expect("write manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        r#"openapi: "3.1.0"
info:
  title: Plugin API 1.0 compatibility
  version: "1.0.0"
x-barbacane-middlewares:
  - name: compat-middleware
    config:
      header: x-compat
      deny_path: /denied
paths:
  /items/{id}:
    post:
      operationId: postItem
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
        - name: q
          in: query
          schema:
            type: string
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: compat-dispatcher
        config:
          greeting: hello
      responses:
        "200":
          description: OK
  /denied:
    get:
      operationId: denied
      x-barbacane-dispatch:
        name: compat-dispatcher
        config:
          greeting: never reached
      responses:
        "200":
          description: OK
"#,
    )
    .expect("write spec");
    (dir, spec)
}

#[tokio::test]
async fn a_1_0_middleware_and_dispatcher_run_a_request() {
    let (_dir, spec) = compat_spec();
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("the 1.0 fixtures compile and load");

    let body = r#"{"name":"widget"}"#;
    let resp = gateway.post("/items/42?q=blue", body).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("x-compat-on-response")
            .and_then(|v| v.to_str().ok()),
        Some("seen"),
        "the middleware's on_response ran and read the context it set"
    );
    let echo: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(echo["greeting"], "hello", "the dispatcher's config");
    assert_eq!(echo["method"], "POST");
    assert_eq!(echo["path"], "/items/42");
    assert_eq!(echo["query"], "q=blue");
    assert_eq!(echo["id"], "42", "the path parameter");
    assert_eq!(
        echo["header"], "set-by-middleware",
        "the middleware's header"
    );
    assert_eq!(echo["context"], "seen", "the middleware's context entry");
    assert_eq!(echo["body"], body);
    assert_eq!(
        echo["body_sha256"],
        hex::encode(Sha256::digest(body.as_bytes())),
        "the host's sha256"
    );
}

#[tokio::test]
async fn a_1_0_middleware_short_circuits_with_problem_details() {
    let (_dir, spec) = compat_spec();
    let gateway = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("the 1.0 fixtures compile and load");

    let resp = gateway.get("/denied").await.unwrap();
    assert_eq!(resp.status(), 403);
    let problem: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(problem["type"], "urn:barbacane:compat:denied");
    assert_eq!(problem["title"], "Denied");
    assert_eq!(problem["status"], 403);
}
