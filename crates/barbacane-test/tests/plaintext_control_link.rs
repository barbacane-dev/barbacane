//! A plaintext `ws://` control plane link to a host other than loopback is
//! refused at startup unless allowed explicitly.

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
async fn a_plaintext_link_to_a_remote_control_plane_is_refused() {
    let result = TestGateway::from_spec_with_args(
        &fixture("minimal.yaml"),
        &[
            "--control-plane",
            "ws://192.0.2.10:9090/ws/data-plane",
            "--project-id",
            "880e8400-e29b-41d4-a716-446655440003",
            "--api-key",
            "bbk_test",
        ],
    )
    .await;
    let error = match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("the data plane started on a plaintext link to a remote control plane"),
    };
    assert!(error.contains("plaintext"), "{error}");
}
