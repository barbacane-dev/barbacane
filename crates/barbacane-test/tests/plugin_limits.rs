//! A plugin runs with the resource limits its `plugin.toml` declares.

use std::path::PathBuf;

use barbacane_test::TestGateway;
use tempfile::TempDir;

/// A middleware that grows its memory to 3 MiB, trapping when that is refused,
/// then answers 204.
const NEEDS_3_MIB: &str = r#"(module
  (import "barbacane" "host_set_output" (func $set_output (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 49152) "{\"action\":1,\"data\":{\"status\":204,\"headers\":{}}}")
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_request") (param i32 i32) (result i32)
    (if (i32.lt_u (memory.size) (i32.const 48))
      (then
        (if (i32.eq (memory.grow (i32.sub (i32.const 48) (memory.size))) (i32.const -1))
          (then unreachable))))
    (call $set_output (i32.const 49152) (i32.const 47))
    (i32.const 1))
  (func (export "on_response") (param i32 i32) (result i32) (i32.const 0)))"#;

/// A dispatcher the middleware's answer keeps from running.
const UNREACHED_DISPATCHER: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32) unreachable))"#;

fn write_plugin(dir: &std::path::Path, name: &str, kind: &str, wat: &str, extra_toml: &str) {
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
             wasm = \"{name}.wasm\"\n\n[capabilities]\nhost_functions = []\n{extra_toml}"
        ),
    )
    .expect("plugin.toml");
}

/// A project whose operation runs the 3 MiB middleware, which declares
/// `limits` (a `[limits]` table, or nothing).
fn project(limits: &str) -> (TempDir, PathBuf) {
    project_with_override(limits, "")
}

/// [`project`], with `override_yaml` (indented `limits:` lines, or nothing)
/// under the middleware's entry in `barbacane.yaml`.
fn project_with_override(limits: &str, override_yaml: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    write_plugin(dir.path(), "grower", "middleware", NEEDS_3_MIB, limits);
    write_plugin(
        dir.path(),
        "unreached",
        "dispatcher",
        UNREACHED_DISPATCHER,
        "",
    );
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  grower:\n    path: {}\n{override_yaml}  unreached:\n    path: {}\n",
            dir.path().join("grower/grower.wasm").display(),
            dir.path().join("unreached/unreached.wasm").display()
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        "openapi: \"3.1.0\"\ninfo: { title: Limits, version: \"1.0.0\" }\npaths:\n  /x:\n    get:\n      operationId: x\n      x-barbacane-middlewares:\n        - name: grower\n      x-barbacane-dispatch:\n        name: unreached\n      responses:\n        \"204\": { description: answered by the middleware }\n",
    )
    .expect("spec");
    (dir, spec)
}

#[tokio::test]
async fn a_plugin_runs_within_the_memory_it_declares() {
    let (_dir, spec) = project("\n[limits]\nmemory_mib = 2\n");
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("starts");
    let response = gateway.get("/x").await.expect("request");
    assert_eq!(response.status(), 500, "3 MiB does not fit in 2 MiB");
}

#[tokio::test]
async fn the_same_plugin_without_limits_gets_the_default_memory() {
    let (_dir, spec) = project("");
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("starts");
    let response = gateway.get("/x").await.expect("request");
    assert_eq!(response.status(), 204, "3 MiB fits in the default");
}

#[tokio::test]
async fn limits_above_the_gateway_ceiling_do_not_compile() {
    let (_dir, spec) = project("\n[limits]\nmemory_mib = 4096\n");
    let error = match TestGateway::from_spec(spec.to_str().expect("utf-8")).await {
        Ok(_) => panic!("compiled a plugin declaring 4096 MiB"),
        Err(e) => e.to_string(),
    };
    assert!(
        error.contains("E1044") && error.contains("memory_mib"),
        "{error}"
    );
}

#[tokio::test]
async fn barbacane_yaml_raises_a_limit_the_plugin_sets_too_low() {
    let (_dir, spec) = project_with_override(
        "\n[limits]\nmemory_mib = 2\n",
        "    limits:\n      memory_mib: 4\n",
    );
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("starts");
    let response = gateway.get("/x").await.expect("request");
    assert_eq!(
        response.status(),
        204,
        "3 MiB fits in the 4 MiB barbacane.yaml sets"
    );
}

#[tokio::test]
async fn barbacane_yaml_lowers_a_limit_the_plugin_leaves_at_the_default() {
    let (_dir, spec) = project_with_override("", "    limits:\n      memory_mib: 2\n");
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("starts");
    let response = gateway.get("/x").await.expect("request");
    assert_eq!(
        response.status(),
        500,
        "3 MiB does not fit in the 2 MiB barbacane.yaml sets"
    );
}
