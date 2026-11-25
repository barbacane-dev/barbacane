//! `barbacane dev`: it serves the project's specs, reloads on a change, and
//! keeps serving the previous version when a change does not compile.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT, STARTUP_TIMEOUT};

/// A spec whose `GET /hello` answers `{"version": "<version>"}`.
fn hello_spec(version: &str) -> String {
    format!(
        r#"openapi: "3.1.0"
info: {{ title: Dev, version: "1.0.0" }}
paths:
  /hello:
    get:
      operationId: hello
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"version":"{version}"}}'
      responses:
        "200": {{ description: ok }}
"#
    )
}

/// The `version` `GET /hello` answers with.
async fn version(base: &str) -> String {
    let body: serde_json::Value = reqwest::get(format!("{base}/hello"))
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    body["version"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn dev_reloads_a_changed_spec_and_keeps_the_last_one_that_compiles() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/mock/mock.wasm");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "specs: ./specs/\nplugins:\n  mock:\n    path: {}\n",
            mock.display()
        ),
    )
    .expect("manifest");
    let specs = dir.path().join("specs");
    std::fs::create_dir(&specs).expect("specs dir");
    let spec = specs.join("api.yaml");
    std::fs::write(&spec, hello_spec("v1")).expect("spec");

    let mut cmd = Command::new(barbacane_test::find_workspace_binary("barbacane").expect("built"));
    cmd.arg("dev")
        .arg("--manifest")
        .arg(dir.path().join("barbacane.yaml"))
        .args([
            "--listen",
            LISTEN_ANY_PORT,
            "--admin-bind",
            "off",
            "--debounce-ms",
            "50",
        ]);
    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn barbacane dev");
    let port = process
        .announced_port("barbacane dev: listening on", STARTUP_TIMEOUT)
        .unwrap_or_else(|| panic!("dev did not start:\n{}", process.log().text()));
    let base = format!("http://127.0.0.1:{port}");
    assert_eq!(version(&base).await, "v1");

    std::fs::write(&spec, hello_spec("v2")).expect("spec");
    assert!(
        process.log().wait_for("reloaded", Duration::from_secs(15)),
        "{}",
        process.log().text()
    );
    assert_eq!(version(&base).await, "v2");

    std::fs::write(&spec, "openapi: 3.1.0\npaths: {").expect("spec");
    assert!(
        process
            .log()
            .wait_for("serving previous version", Duration::from_secs(15)),
        "{}",
        process.log().text()
    );
    assert_eq!(version(&base).await, "v2", "the last version that compiled");
    process.kill_and_reap();
}
