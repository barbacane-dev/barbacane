//! Tests of the test harness itself.
//!
//! Every other file here asserts what the gateway does. This one asserts that
//! when the gateway does something unexpected, the harness can say why: the
//! gateway logs the cause of every error it answers with, and a test that
//! cannot see those logs reports a status and nothing else.

use barbacane_test::TestGateway;

fn fixture(name: &str) -> String {
    format!(
        "{}/../../tests/fixtures/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    )
}

/// The harness drains the gateway's stdout and stderr as it runs, so whatever
/// it printed is readable from the test.
#[tokio::test]
async fn the_harness_captures_what_the_gateway_logs() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");

    let log = gateway.log().text();
    assert!(
        !log.is_empty(),
        "the gateway printed nothing, or nothing was drained"
    );
    assert!(
        gateway.log().contains("route(s)"),
        "the startup line should be there, got:\n{log}"
    );
}

/// The case this exists for: an error response and the reason behind it.
///
/// `opa-authz.yaml` points the plugin at a port nothing listens on, so the
/// request fails and the gateway logs why. Reading a status alone leaves a test
/// unable to tell an expected refusal from a broken gateway.
#[tokio::test]
async fn an_error_response_is_explained_by_the_log() {
    let gateway = TestGateway::from_spec(&fixture("opa-authz.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/opa-protected").await.expect("request");
    let status = resp.status();

    // Give the reader threads the last lines.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let log = gateway.log().text();

    assert!(
        !log.is_empty(),
        "status {status} arrived with no gateway output to explain it"
    );
    assert!(
        log.contains("19999") || log.to_lowercase().contains("connect"),
        "the log should name the failure behind status {status}, got:\n{log}"
    );
}
