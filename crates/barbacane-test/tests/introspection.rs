//! `/__barbacane/specs*` serves the embedded specs unless `serve` runs with
//! `--no-introspection`.

use barbacane_test::TestGateway;

fn fixture(name: &str) -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

#[tokio::test]
async fn introspection_is_on_by_default() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");
    for path in ["/__barbacane/specs", "/__barbacane/specs/openapi"] {
        assert_eq!(gateway.get(path).await.unwrap().status(), 200, "{path}");
    }
}

#[tokio::test]
async fn no_introspection_answers_404_and_keeps_health() {
    let gateway =
        TestGateway::from_spec_with_args(&fixture("minimal.yaml"), &["--no-introspection"])
            .await
            .expect("failed to start gateway");
    for path in [
        "/__barbacane/specs",
        "/__barbacane/specs/openapi",
        "/__barbacane/specs/minimal.yaml",
    ] {
        assert_eq!(gateway.get(path).await.unwrap().status(), 404, "{path}");
    }
    assert_eq!(
        gateway.get("/__barbacane/health").await.unwrap().status(),
        200
    );
    assert_eq!(gateway.get("/health").await.unwrap().status(), 200);
}
