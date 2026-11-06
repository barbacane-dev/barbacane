//! A plugin that fails mid-request, in any phase: the caller gets a 500, and
//! the gateway logs why, naming the plugin and the request.
//!
//! Run with: `cargo test -p barbacane-test --test plugin_failures`

use std::time::Duration;

use barbacane_test::TestGateway;

/// A dispatcher whose `dispatch` traps on `unreachable`.
const TRAPPING_DISPATCHER: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32) unreachable)
)"#;

fn spec_with_trapping_plugin() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugin_dir = dir.path().join("trapper");
    std::fs::create_dir(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("trapper.wasm"),
        wat::parse_str(TRAPPING_DISPATCHER).expect("valid WAT"),
    )
    .expect("wasm");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        r#"[plugin]
name = "trapper"
version = "0.1.0"
type = "dispatcher"
wasm = "trapper.wasm"

[capabilities]
host_functions = []
"#,
    )
    .expect("plugin.toml");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  trapper:\n    path: {}\n",
            plugin_dir.join("trapper.wasm").display()
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        r#"openapi: "3.1.0"
info: { title: Failing plugin, version: "1.0.0" }
paths:
  /fails:
    get:
      operationId: fails
      x-barbacane-dispatch:
        name: trapper
      responses:
        "200": { description: never }
"#,
    )
    .expect("spec");
    (dir, spec)
}

#[tokio::test]
async fn a_trapping_plugin_is_answered_with_500_and_logged_with_its_name() {
    let (_dir, spec) = spec_with_trapping_plugin();
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("starts");

    let response = gateway.get("/fails").await.expect("request");
    assert_eq!(response.status(), 500);
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("the response carries a request ID")
        .to_string();

    let log = gateway.log();
    assert!(
        log.wait_for("request failed inside the gateway", Duration::from_secs(5)),
        "the failure is logged: {}",
        log.text()
    );
    let line = log
        .find_line("request failed inside the gateway")
        .expect("the logged line");
    assert!(
        line.contains("plugin 'trapper' dispatch failed"),
        "names the plugin: {line}"
    );
    assert!(line.contains("unreachable"), "names the trap: {line}");
    assert!(
        line.contains(&request_id),
        "carries the request ID the caller got back ({request_id}): {line}"
    );
}

/// Sends a request with its own request and trace IDs, and checks both reach
/// the failure log.
async fn assert_failure_log_carries_callers_ids(env: &[(&str, &str)]) {
    let (_dir, spec) = spec_with_trapping_plugin();
    let gateway = TestGateway::from_spec_with_env(spec.to_str().expect("utf-8"), env)
        .await
        .expect("starts");

    let request_id = "caller-chosen-request-id-4711";
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let response = gateway
        .request_builder(reqwest::Method::GET, "/fails")
        .header("x-request-id", request_id)
        .header("traceparent", format!("00-{trace_id}-00f067aa0ba902b7-01"))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), 500);
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok()),
        Some(request_id)
    );

    let log = gateway.log();
    let line = log
        .wait_for_line("request failed inside the gateway", Duration::from_secs(5))
        .unwrap_or_else(|| panic!("the failure is logged: {}", log.text()));
    assert!(line.contains(request_id), "carries the request ID: {line}");
    assert!(line.contains(trace_id), "carries the trace ID: {line}");
}

#[tokio::test]
async fn the_failure_log_carries_the_callers_request_and_trace_ids() {
    assert_failure_log_carries_callers_ids(&[]).await;
}

#[tokio::test]
async fn the_ids_stay_on_the_failure_log_when_only_errors_are_logged() {
    assert_failure_log_carries_callers_ids(&[("RUST_LOG", "error")]).await;
}

/// A middleware that lets every request through and traps in `on_response`.
const RESPONSE_TRAPPING_MIDDLEWARE: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_request") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_response") (param i32 i32) (result i32) unreachable)
)"#;

/// `/served` runs the response trapper on a mock response; `/refused` runs it
/// after `ip-restriction` short-circuits.
fn spec_with_response_trapper() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugin_dir = dir.path().join("response-trapper");
    std::fs::create_dir(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("response-trapper.wasm"),
        wat::parse_str(RESPONSE_TRAPPING_MIDDLEWARE).expect("valid WAT"),
    )
    .expect("wasm");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        r#"[plugin]
name = "response-trapper"
version = "0.1.0"
type = "middleware"
wasm = "response-trapper.wasm"

[capabilities]
host_functions = []
"#,
    )
    .expect("plugin.toml");
    let official = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  response-trapper:\n    path: {}\n  mock:\n    path: {}\n  ip-restriction:\n    path: {}\n",
            plugin_dir.join("response-trapper.wasm").display(),
            official.join("mock/mock.wasm").display(),
            official.join("ip-restriction/ip-restriction.wasm").display(),
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        r#"openapi: "3.1.0"
info: { title: Failing response middleware, version: "1.0.0" }
paths:
  /served:
    get:
      operationId: served
      x-barbacane-middlewares:
        - name: response-trapper
      x-barbacane-dispatch:
        name: mock
        config: { status: 200, body: '{"marker":"upstream-body"}' }
      responses:
        "200": { description: served }
  /refused:
    get:
      operationId: refused
      x-barbacane-middlewares:
        - name: response-trapper
        - name: ip-restriction
          config: { allow: ["192.0.2.0/24"] }
      x-barbacane-dispatch:
        name: mock
        config: { status: 200, body: '{"marker":"upstream-body"}' }
      responses:
        "200": { description: served }
"#,
    )
    .expect("spec");
    (dir, spec)
}

async fn assert_response_trap_is_answered_with_500(path: &str) {
    let (_dir, spec) = spec_with_response_trapper();
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("starts");

    let response = gateway.get(path).await.expect("request");
    assert_eq!(response.status(), 500);
    let body = response.text().await.expect("body");
    assert!(!body.contains("upstream-body"), "{body}");

    let log = gateway.log();
    let line = log
        .wait_for_line("request failed inside the gateway", Duration::from_secs(5))
        .unwrap_or_else(|| panic!("the failure is logged: {}", log.text()));
    assert!(
        line.contains("plugin 'response-trapper' on_response failed"),
        "names the plugin: {line}"
    );
}

#[tokio::test]
async fn a_response_middleware_that_traps_is_answered_with_500() {
    assert_response_trap_is_answered_with_500("/served").await;
}

#[tokio::test]
async fn a_response_middleware_that_traps_after_a_short_circuit_is_answered_with_500() {
    assert_response_trap_is_answered_with_500("/refused").await;
}
