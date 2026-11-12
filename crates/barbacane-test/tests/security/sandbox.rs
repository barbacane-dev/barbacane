//! BARB-SEC-004: plugin sandbox and capability confinement.
//!
//! A plugin may import only the host functions its declared capabilities
//! cover. The data plane checks each module's imports against the capabilities
//! recorded in the artifact before the module can run, and a module can call
//! only what it imports, so a plugin granted only `log` cannot reach
//! `host_http_call`.

use std::path::PathBuf;

use barbacane_test::TestGateway;
use tempfile::TempDir;

/// A dispatcher importing `host_log` and `host_http_call`.
const LOGS_AND_CALLS_OUT: &str = r#"(module
  (@custom "barbacane_abi" "\01\00\00\00")
  (import "barbacane" "host_log" (func (param i32 i32 i32)))
  (import "barbacane" "host_http_call" (func (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32) (i32.const 0)))"#;

/// A project whose only operation dispatches to that plugin, granted
/// `capabilities` (a TOML array of capability names).
fn project(capabilities: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let plugin_dir = dir.path().join("caller");
    std::fs::create_dir(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("caller.wasm"),
        wat::parse_str(LOGS_AND_CALLS_OUT).expect("valid WAT"),
    )
    .expect("wasm");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        format!(
            "[plugin]\nname = \"caller\"\nversion = \"0.1.0\"\ntype = \"dispatcher\"\n\
             wasm = \"caller.wasm\"\n\n[capabilities]\nhost_functions = {capabilities}\n"
        ),
    )
    .expect("plugin.toml");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  caller:\n    path: {}\n",
            plugin_dir.join("caller.wasm").display()
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        "openapi: \"3.1.0\"\ninfo: { title: Sandbox, version: \"1.0.0\" }\npaths:\n  /x:\n    get:\n      operationId: x\n      x-barbacane-dispatch:\n        name: caller\n      responses:\n        \"200\": { description: never }\n",
    )
    .expect("spec");
    (dir, spec)
}

#[tokio::test]
async fn a_plugin_granted_only_log_cannot_import_host_http_call() {
    let (_dir, spec) = project(r#"["log"]"#);
    let error = match TestGateway::from_spec(spec.to_str().expect("utf-8")).await {
        Ok(_) => panic!("loaded a plugin importing host_http_call with only `log` granted"),
        Err(e) => e.to_string(),
    };
    assert!(
        error.contains("plugin 'caller' violates its declared capabilities")
            && error.contains("host_http_call"),
        "{error}"
    );
}

#[tokio::test]
async fn the_same_plugin_granted_http_call_loads() {
    let (_dir, spec) = project(r#"["log", "http_call"]"#);
    TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("a plugin importing only what it was granted loads");
}
