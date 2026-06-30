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
//! reached). Today the host does not enforce an egress denylist, so a request
//! pointed at, e.g., a co-located metadata mock could succeed — hence RED. Once
//! the denylist lands, the host refuses the connection and the middleware
//! fails closed (403/503), which is what we assert.

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
    // EXPECTED TO FAIL until BARB-SEC-002 is fixed (host HTTP client has no
    // resolved-IP denylist for link-local / metadata addresses).
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
    // EXPECTED TO FAIL until BARB-SEC-002 is fixed.
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
    // EXPECTED TO FAIL until BARB-SEC-002 is fixed.
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
    // EXPECTED TO FAIL until BARB-SEC-002 is fixed (resolve-then-check, incl.
    // redirect targets).
    let gw = ssrf_gateway().await;
    let resp = gw.get("/ssrf-dns-metadata").await.unwrap();
    assert_ne!(
        resp.status(),
        200,
        "host must resolve the hostname and refuse link-local / metadata IPs, \
         including via redirects"
    );
}
