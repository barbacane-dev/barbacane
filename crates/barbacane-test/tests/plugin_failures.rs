//! A plugin that fails mid-request: the caller gets a 500, and the gateway
//! logs why, naming the plugin.
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
}
