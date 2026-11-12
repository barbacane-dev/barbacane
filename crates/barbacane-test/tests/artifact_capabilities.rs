//! `serve` loads an artifact only when it carries each plugin's capabilities,
//! which the import check at load enforces.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use barbacane_compiler::{compile, CompileOptions, DeclaredLimits, PluginBundle};
use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// An artifact compiled without a manifest: its plugin capabilities are not
/// authoritative.
fn artifact_without_capabilities(dir: &Path) -> PathBuf {
    let output = dir.join("no-capabilities.bca");
    let mock = PluginBundle {
        name: "mock".into(),
        version: "0.1.0".into(),
        plugin_type: "dispatcher".into(),
        category: None,
        implements: vec![],
        wasm_bytes: std::fs::read(root().join("plugins/mock/mock.wasm")).expect("mock.wasm"),
        body_access: false,
        host_functions: vec![],
        limits: DeclaredLimits::default(),
        secret_fields: vec![],
        config_schema: None,
    };
    let spec = root().join("tests/fixtures/minimal.yaml");
    compile(
        &[spec.as_path()],
        &[mock],
        &output,
        &CompileOptions {
            allow_plaintext: true,
            ..CompileOptions::default()
        },
    )
    .expect("compiles");
    output
}

#[tokio::test]
async fn an_artifact_without_authoritative_capabilities_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let artifact = artifact_without_capabilities(dir.path());
    let binary = [
        "target/debug/barbacane",
        "../target/debug/barbacane",
        "../../target/debug/barbacane",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
    .expect("barbacane is built");
    let mut cmd = Command::new(binary);
    cmd.args(["serve", "--artifact"]).arg(&artifact).args([
        "--listen",
        LISTEN_ANY_PORT,
        "--admin-bind",
        "off",
        "--dev",
    ]);
    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn");

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !process.has_exited() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let exited = process.has_exited();
    let log = process.log().text();
    process.kill_and_reap();
    assert!(
        exited,
        "serve must refuse the artifact instead of starting:\n{log}"
    );
    assert!(log.contains("capabilities"), "{log}");
}
