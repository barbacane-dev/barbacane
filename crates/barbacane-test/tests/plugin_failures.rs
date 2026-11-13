//! A plugin that fails mid-request, in any phase: the caller gets a 500, and
//! the gateway logs why, naming the plugin and the request.
//!
//! Run with: `cargo test -p barbacane-test --test plugin_failures`

use std::time::Duration;

use barbacane_test::TestGateway;

/// A dispatcher whose `dispatch` traps on `unreachable`.
const TRAPPING_DISPATCHER: &str = r#"(module
  (@custom "barbacane_abi" "\01\00\00\00")
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
  (@custom "barbacane_abi" "\01\00\00\00")
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_request") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_response") (param i32 i32) (result i32) unreachable)
)"#;

/// A middleware whose `on_response` returns 1 without an output, as the SDK
/// macros do when the response cannot be parsed.
const RESPONSE_REFUSING_MIDDLEWARE: &str = r#"(module
  (@custom "barbacane_abi" "\01\00\00\00")
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_request") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_response") (param i32 i32) (result i32) (i32.const 1))
)"#;

/// `/served` runs the response middleware built from `wat` on a mock response;
/// `/refused` runs it after `ip-restriction` short-circuits.
fn spec_with_response_trapper(wat: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugin_dir = dir.path().join("response-trapper");
    std::fs::create_dir(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("response-trapper.wasm"),
        wat::parse_str(wat).expect("valid WAT"),
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
    assert_response_failure_is_answered_with_500(RESPONSE_TRAPPING_MIDDLEWARE, path).await;
}

async fn assert_response_failure_is_answered_with_500(wat: &str, path: &str) {
    let (_dir, spec) = spec_with_response_trapper(wat);
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

/// A non-zero `on_response` result fails the request as a trap does; the
/// upstream body is not served in its place.
#[tokio::test]
async fn a_response_middleware_that_returns_an_error_is_answered_with_500() {
    assert_response_failure_is_answered_with_500(RESPONSE_REFUSING_MIDDLEWARE, "/served").await;
}

/// A dispatcher importing a WASI function the gateway does not provide.
const UNSUPPORTED_WASI_DISPATCHER: &str = r#"(module
  (@custom "barbacane_abi" "\01\00\00\00")
  (import "wasi_snapshot_preview1" "fd_close" (func (param i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32) (i32.const 0))
)"#;

/// A dispatcher built without the SDK macros: no `barbacane_abi` section.
const DISPATCHER_WITHOUT_ABI_VERSION: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32) (i32.const 0))
)"#;

/// Compile a spec dispatching to a plugin built from `wat`.
async fn compile_dispatcher(wat: &str) -> Result<TestGateway, String> {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugin_dir = dir.path().join("probe");
    std::fs::create_dir(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("probe.wasm"),
        wat::parse_str(wat).expect("valid WAT"),
    )
    .expect("wasm");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "[plugin]\nname = \"probe\"\nversion = \"0.1.0\"\ntype = \"dispatcher\"\nwasm = \"probe.wasm\"\n\n[capabilities]\nhost_functions = []\n",
    )
    .expect("plugin.toml");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  probe:\n    path: {}\n",
            plugin_dir.join("probe.wasm").display()
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        "openapi: \"3.1.0\"\ninfo: { title: Probe, version: \"1.0.0\" }\npaths:\n  /x:\n    get:\n      operationId: x\n      x-barbacane-dispatch:\n        name: probe\n      responses:\n        \"200\": { description: never }\n",
    )
    .expect("spec");
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .map_err(|e| e.to_string())?;
    // Keep the directory alive for the gateway's lifetime.
    std::mem::forget(dir);
    Ok(gateway)
}

/// The compiler refuses a plugin without an ABI version (E1045), and says to
/// rebuild it; the same module with the section compiles.
#[tokio::test]
async fn a_plugin_without_an_abi_version_does_not_compile() {
    let error = match compile_dispatcher(DISPATCHER_WITHOUT_ABI_VERSION).await {
        Ok(_) => panic!("compiled a plugin without an ABI version"),
        Err(e) => e,
    };
    assert!(
        error.contains("E1045")
            && error.contains("no plugin ABI version")
            && error.contains("barbacane-plugin-sdk 1.x"),
        "{error}"
    );

    let with_version = DISPATCHER_WITHOUT_ABI_VERSION.replacen(
        "(module\n",
        "(module\n  (@custom \"barbacane_abi\" \"\\01\\00\\00\\00\")\n",
        1,
    );
    let gateway = compile_dispatcher(&with_version)
        .await
        .expect("positive control: the same module with the section compiles");
    assert!(gateway.get("/x").await.is_ok(), "the gateway serves");
}

/// A plugin built for a later ABI is refused with the version it names.
#[tokio::test]
async fn a_plugin_for_another_abi_version_does_not_compile() {
    let other = DISPATCHER_WITHOUT_ABI_VERSION.replacen(
        "(module\n",
        "(module\n  (@custom \"barbacane_abi\" \"\\02\\00\\00\\00\")\n",
        1,
    );
    let error = match compile_dispatcher(&other).await {
        Ok(_) => panic!("compiled a plugin for ABI 2"),
        Err(e) => e,
    };
    assert!(
        error.contains("E1045") && error.contains("ABI 2") && error.contains("supports ABI 1"),
        "{error}"
    );
}

#[tokio::test]
async fn a_plugin_importing_an_unsupported_wasi_function_does_not_compile() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugin_dir = dir.path().join("wasi-user");
    std::fs::create_dir(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("wasi-user.wasm"),
        wat::parse_str(UNSUPPORTED_WASI_DISPATCHER).expect("valid WAT"),
    )
    .expect("wasm");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "[plugin]\nname = \"wasi-user\"\nversion = \"0.1.0\"\ntype = \"dispatcher\"\nwasm = \"wasi-user.wasm\"\n\n[capabilities]\nhost_functions = []\n",
    )
    .expect("plugin.toml");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  wasi-user:\n    path: {}\n",
            plugin_dir.join("wasi-user.wasm").display()
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        "openapi: \"3.1.0\"\ninfo: { title: WASI, version: \"1.0.0\" }\npaths:\n  /x:\n    get:\n      operationId: x\n      x-barbacane-dispatch:\n        name: wasi-user\n      responses:\n        \"200\": { description: never }\n",
    )
    .expect("spec");

    let error = match TestGateway::from_spec(spec.to_str().expect("utf-8")).await {
        Ok(_) => panic!("compiled a plugin importing fd_close"),
        Err(e) => e.to_string(),
    };
    assert!(
        error.contains("E1043") && error.contains("wasi_snapshot_preview1::fd_close"),
        "{error}"
    );
    assert!(
        error.contains("random_get"),
        "lists what is provided: {error}"
    );
}
