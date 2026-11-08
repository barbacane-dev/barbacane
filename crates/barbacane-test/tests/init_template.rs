//! The project `barbacane init` creates serves what its example spec says.

use std::process::Command;

use barbacane_test::TestGateway;
use tempfile::TempDir;

fn barbacane_binary() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/barbacane")
}

#[tokio::test]
async fn the_basic_template_answers_with_the_requested_user_id() {
    let tmp = TempDir::new().expect("temp dir");
    let project = tmp.path().join("my-api");
    let status = Command::new(barbacane_binary())
        .args(["init"])
        .arg(&project)
        .status()
        .expect("run init");
    assert!(status.success());

    // The template dispatches to `mock`; declare the repository's build of it.
    let mock =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/mock/mock.wasm");
    std::fs::write(
        project.join("specs/barbacane.yaml"),
        format!("plugins:\n  mock:\n    path: {}\n", mock.display()),
    )
    .expect("manifest");

    let spec = project.join("specs/api.yaml");
    let gateway = TestGateway::from_spec(spec.to_str().expect("utf-8"))
        .await
        .expect("the template compiles and serves");
    let id = "0b7c6f2e-8d5a-4c1e-9f3b-2a6d8e4c1b9f";
    let response = gateway.get(&format!("/users/{id}")).await.expect("request");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("JSON body");
    assert_eq!(body["id"], id);
}
