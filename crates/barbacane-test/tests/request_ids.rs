//! The request id and trace id a response carries: a client's well-formed
//! ids are kept, anything else is replaced.

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

async fn ids(gateway: &TestGateway, request_id: &str, traceparent: &str) -> (String, String) {
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/health")
        .header("x-request-id", request_id)
        .header("traceparent", traceparent)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_else(|| panic!("no {name} header"))
            .to_string()
    };
    (header("x-request-id"), header("x-trace-id"))
}

#[tokio::test]
async fn well_formed_client_ids_are_kept() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");
    let (request_id, trace_id) = ids(
        &gateway,
        "client-req-42",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    )
    .await;
    assert_eq!(request_id, "client-req-42");
    assert_eq!(trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
}

#[tokio::test]
async fn an_oversized_request_id_is_replaced() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");
    let huge = "r".repeat(1000);
    let (request_id, _) = ids(&gateway, &huge, "").await;
    assert_ne!(request_id, huge);
    assert!(uuid::Uuid::parse_str(&request_id).is_ok(), "{request_id}");
}

#[tokio::test]
async fn a_malformed_traceparent_gives_a_new_trace_id() {
    let gateway = TestGateway::from_spec(&fixture("minimal.yaml"))
        .await
        .expect("failed to start gateway");
    let (_, trace_id) = ids(&gateway, "req", "00-not-a-trace-id-00f067aa0ba902b7-01").await;
    assert_ne!(trace_id, "not");
    assert_eq!(trace_id.len(), 32, "{trace_id}");
    assert!(
        trace_id.bytes().all(|b| b.is_ascii_hexdigit()),
        "{trace_id}"
    );
}
