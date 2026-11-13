//! A request cannot observe what a previous request left in a plugin.

use std::path::PathBuf;

use barbacane_test::TestGateway;
use tempfile::TempDir;

/// A middleware that answers 204 when it finds the state its `init` left and
/// nothing a request wrote. It answers 409 when an earlier request's mark is
/// still in memory or in its mutable global, and 412 when `init`'s value is
/// missing. Each request leaves a mark in both.
const MARKS_EACH_REQUEST: &str = r#"(module
  (@custom "barbacane_abi" "\01\00\00\00")
  (import "barbacane" "host_set_output" (func $set_output (param i32 i32)))
  (memory (export "memory") 1)
  (global $seen (mut i32) (i32.const 0))
  (data (i32.const 50000) "{\"action\":1,\"data\":{\"status\":204,\"headers\":{}}}")
  (data (i32.const 50100) "{\"action\":1,\"data\":{\"status\":409,\"headers\":{}}}")
  (data (i32.const 50200) "{\"action\":1,\"data\":{\"status\":412,\"headers\":{}}}")
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32)
    (i32.store8 (i32.const 60000) (i32.const 7))
    (i32.const 0))
  (func (export "on_request") (param i32 i32) (result i32)
    (if (i32.or (i32.load8_u (i32.const 60001)) (global.get $seen))
      (then
        (call $set_output (i32.const 50100) (i32.const 47))
        (return (i32.const 1))))
    (if (i32.ne (i32.load8_u (i32.const 60000)) (i32.const 7))
      (then
        (call $set_output (i32.const 50200) (i32.const 47))
        (return (i32.const 1))))
    (i32.store8 (i32.const 60001) (i32.const 1))
    (global.set $seen (i32.const 1))
    (call $set_output (i32.const 50000) (i32.const 47))
    (i32.const 1))
  (func (export "on_response") (param i32 i32) (result i32) (i32.const 0)))"#;

/// A dispatcher the middleware's answer keeps from running.
const UNREACHED_DISPATCHER: &str = r#"(module
  (@custom "barbacane_abi" "\01\00\00\00")
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32) unreachable))"#;

fn write_plugin(dir: &std::path::Path, name: &str, kind: &str, wat: &str) {
    let plugin_dir = dir.join(name);
    std::fs::create_dir(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join(format!("{name}.wasm")),
        wat::parse_str(wat).expect("valid WAT"),
    )
    .expect("wasm");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        format!(
            "[plugin]\nname = \"{name}\"\nversion = \"0.1.0\"\ntype = \"{kind}\"\n\
             wasm = \"{name}.wasm\"\n\n[capabilities]\nhost_functions = []\n"
        ),
    )
    .expect("plugin.toml");
}

fn project() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    write_plugin(dir.path(), "marker", "middleware", MARKS_EACH_REQUEST);
    write_plugin(dir.path(), "unreached", "dispatcher", UNREACHED_DISPATCHER);
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  marker:\n    path: {}\n  unreached:\n    path: {}\n",
            dir.path().join("marker/marker.wasm").display(),
            dir.path().join("unreached/unreached.wasm").display()
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        "openapi: \"3.1.0\"\ninfo: { title: Isolation, version: \"1.0.0\" }\npaths:\n  /x:\n    get:\n      operationId: x\n      x-barbacane-middlewares:\n        - name: marker\n      x-barbacane-dispatch:\n        name: unreached\n      responses:\n        \"204\": { description: answered by the middleware }\n",
    )
    .expect("spec");
    (dir, spec)
}

#[tokio::test]
async fn a_request_does_not_see_what_an_earlier_request_left_in_a_plugin() {
    let (_dir, spec) = project();
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("starts");
    for attempt in 1..=5 {
        let response = gateway.get("/x").await.expect("request");
        assert_eq!(
            response.status(),
            204,
            "request {attempt}: 409 means it saw an earlier request's mark, 412 that init's state was lost"
        );
    }
}
