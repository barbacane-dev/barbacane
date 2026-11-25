//! BARB-SEC-002 — Server-Side Request Forgery via the WASM host HTTP client.
//!
//! Threat: a plugin (or plugin *config*, which a spec author or a compromised
//! control plane controls) can make the host issue outbound HTTP to an
//! arbitrary URL. Without an egress policy, a plugin can reach the cloud
//! instance-metadata service (169.254.169.254), loopback services, or internal
//! RFC1918 hosts — classic SSRF.
//!
//! The fix: the host HTTP client
//! (`crates/barbacane-wasm/src/http_client.rs`) must resolve the target host
//! and refuse to connect when the resolved IP is loopback / link-local /
//! private / metadata, INCLUDING after following redirects (so a public URL
//! that 30x-redirects to 169.254.169.254 is still blocked).
//!
//! ## How these tests observe SSRF
//!
//! We drive the `opa-authz` middleware, whose `opa_url` is a plugin-controlled
//! outbound call. The fixture mock dispatcher returns HTTP 200 only if the
//! middleware *allows* the request, which only happens if the host call
//! succeeds and the (non-existent) OPA endpoint returns an allow decision.
//!
//! The security invariant is therefore: **a request whose `opa_url` points at a
//! blocked address must never return 200** (the protected upstream must never be
//! reached): the host refuses the connection and the middleware fails closed
//! (403/503). The fixture's targets do not answer, so a failed connection looks
//! the same with or without the guard; `a_live_loopback_target_is_refused_by_the_guard_alone`
//! points at a target that answers and allows, and checks the guard is what
//! refuses it.

use barbacane_test::TestGateway;

use crate::security_fixture;

/// Boot a gateway from the SSRF fixture with the egress guard ACTIVE, so the
/// block is what we actually observe (the default test harness allows internal
/// egress for loopback mocks).
async fn ssrf_gateway() -> TestGateway {
    TestGateway::from_spec_blocked_egress(&security_fixture("ssrf.yaml"))
        .await
        .expect("ssrf fixture failed to compile/start")
}

/// The protected response (200 "reached") must never be returned for an
/// `opa_url` pointing at the instance-metadata service.
#[tokio::test]
async fn metadata_endpoint_is_blocked() {
    let gw = ssrf_gateway().await;
    let resp = gw.get("/ssrf-metadata").await.unwrap();
    assert_ne!(
        resp.status(),
        200,
        "host must refuse outbound HTTP to 169.254.169.254 (metadata SSRF); \
         the protected upstream must never be reached"
    );
}

/// Loopback targets must be blocked.
#[tokio::test]
async fn loopback_is_blocked() {
    let gw = ssrf_gateway().await;
    let resp = gw.get("/ssrf-loopback").await.unwrap();
    assert_ne!(
        resp.status(),
        200,
        "host must refuse outbound HTTP to 127.0.0.0/8 (loopback SSRF)"
    );
}

/// RFC1918 private addresses must be blocked.
#[tokio::test]
async fn private_rfc1918_is_blocked() {
    let gw = ssrf_gateway().await;
    let resp = gw.get("/ssrf-private").await.unwrap();
    assert_ne!(
        resp.status(),
        200,
        "host must refuse outbound HTTP to 10.0.0.0/8 (private-network SSRF)"
    );
}

/// A hostname that resolves to a metadata/link-local IP must be blocked after
/// DNS resolution (the denylist must operate on the *resolved* IP, not the
/// literal string). `metadata.google.internal` resolves to 169.254.169.254 in
/// GCP; in CI it typically does not resolve, so this also covers the
/// redirect-to-metadata shape: in either case the protected upstream must not
/// be reached.
#[tokio::test]
async fn dns_resolved_metadata_is_blocked() {
    let gw = ssrf_gateway().await;
    let resp = gw.get("/ssrf-dns-metadata").await.unwrap();
    assert_ne!(
        resp.status(),
        200,
        "host must resolve the hostname and refuse link-local / metadata IPs, \
         including via redirects"
    );
}

/// A project in a temporary directory whose `GET /live-loopback` asks the OPA
/// at `opa_url` before answering 200 from the mock dispatcher.
fn live_target_spec(opa_url: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  mock:\n    path: {}\n  opa-authz:\n    path: {}\n",
            plugins.join("mock/mock.wasm").display(),
            plugins.join("opa-authz/opa-authz.wasm").display()
        ),
    )
    .expect("manifest");
    let spec = dir.path().join("api.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: "3.1.0"
info: {{ title: Live loopback target, version: "1.0.0" }}
paths:
  /live-loopback:
    get:
      operationId: liveLoopback
      x-barbacane-middlewares:
        - name: opa-authz
          config:
            opa_url: "{opa_url}"
            timeout: 2
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"reached":"loopback"}}'
      responses:
        "200": {{ description: reached }}
"#
        ),
    )
    .expect("spec");
    (dir, spec.display().to_string())
}

/// The loopback block is the guard's: the target answers and allows the
/// request. With internal egress allowed it is queried and the upstream
/// reached; with the guard on it is never queried.
#[tokio::test]
async fn a_live_loopback_target_is_refused_by_the_guard_alone() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let opa = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": true})))
        .mount(&opa)
        .await;
    let (_dir, spec) = live_target_spec(&format!("{}/v1/data/authz/allow", opa.uri()));

    let open = TestGateway::from_spec(&spec)
        .await
        .expect("the gateway starts");
    let resp = open.get("/live-loopback").await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "internal egress allowed: the target allows it"
    );
    assert_eq!(opa.received_requests().await.unwrap().len(), 1);

    let guarded = TestGateway::from_spec_blocked_egress(&spec)
        .await
        .expect("the gateway starts");
    let resp = guarded.get("/live-loopback").await.unwrap();
    assert_ne!(resp.status(), 200, "the guard refuses the loopback target");
    assert_eq!(
        opa.received_requests().await.unwrap().len(),
        1,
        "the guarded gateway never connected to it"
    );
}
