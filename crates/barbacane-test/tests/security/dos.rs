//! BARB-SEC-003 — Denial of service / resource limits.
//!
//! Three sub-areas:
//!
//!   1. **Oversized chunked body** — a `Transfer-Encoding: chunked` request with
//!      an over-limit body must be rejected with 413, and the gateway must not
//!      buffer the whole body before deciding (it should bail as soon as the
//!      streamed size crosses the cap).
//!
//!   2. **Slowloris / missing read timeout** — documented; hard to assert
//!      deterministically without flaky wall-clock timing. See the `#[ignore]`
//!      test below for the intended shape.
//!
//!   3. **404-flood metric cardinality** — a flood of unique unmatched paths must
//!      NOT create one Prometheus series per raw path. The router currently
//!      records `record_request_metrics(&method, &path, ...)` with the *raw*
//!      request path on the `RouteMatch::NotFound` arm
//!      (`crates/barbacane/src/main.rs`), so an attacker can blow up metric
//!      cardinality (memory DoS) and exfiltrate scanned paths. The fix replaces
//!      the label with a sentinel such as `<not_found>`.

use barbacane_test::TestGateway;

use crate::{fixture, security_fixture};

// ---------------------------------------------------------------------------
// 1. Oversized chunked body -> 413
// ---------------------------------------------------------------------------

/// A chunked request whose body exceeds the configured limit must get 413.
///
/// `request-size-limit.yaml` caps `/limited` at 100 bytes. We send ~64 KiB with
/// `Transfer-Encoding: chunked` (reqwest streams a body of unknown length as
/// chunked). The hardened gateway rejects it with 413.
#[tokio::test]
async fn oversized_chunked_body_rejected_with_413() {
    // EXPECTED TO FAIL until BARB-SEC-003 is fixed (chunked bodies without a
    // Content-Length must be size-capped while streaming, not buffered then
    // checked / accepted).
    let gw = TestGateway::from_spec(&fixture("request-size-limit.yaml"))
        .await
        .expect("failed to start gateway");

    // A 64 KiB payload (limit is 100 bytes). Using a streaming body forces
    // chunked transfer-encoding (no Content-Length).
    let big = vec![b'a'; 64 * 1024];
    let body = reqwest::Body::wrap_stream(futures_util::stream::once(async move {
        Ok::<_, std::io::Error>(big)
    }));

    let resp = gw
        .request_builder(reqwest::Method::POST, "/limited")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status(),
        413,
        "oversized chunked body must be rejected with 413 Payload Too Large"
    );
}

// ---------------------------------------------------------------------------
// 2. Slowloris / missing timeout (documented, non-deterministic)
// ---------------------------------------------------------------------------

/// Slowloris: a client that opens a connection and dribbles bytes (or never
/// finishes the body) must be timed out so it cannot pin a worker indefinitely.
///
/// This is inherently timing-dependent and would require holding a socket open
/// and measuring that the server closes it within a header/body read deadline.
/// Asserting that deterministically (without sleeps that make CI flaky) needs a
/// configurable, observable read timeout we can drive from the test. Parked
/// until the gateway exposes a read/header timeout knob we can set low and a
/// signal we can assert on.
#[tokio::test]
#[ignore = "BLOCKED: needs a configurable+observable read/header timeout to assert slowloris defence without flaky wall-clock sleeps (BARB-SEC-003)"]
async fn slowloris_connection_is_timed_out() {
    // EXPECTED TO FAIL until BARB-SEC-003 (slowloris) is addressed.
    //
    // Intended shape: open a raw TCP socket to the gateway, send a partial
    // request ("GET /health HTTP/1.1\r\nHost: x\r\n") and then stop, and assert
    // the server closes the connection within N seconds rather than waiting
    // forever. Requires a deterministic timeout to assert against.
}

// ---------------------------------------------------------------------------
// 3. 404-flood metric cardinality
// ---------------------------------------------------------------------------

/// Flooding unique unmatched paths must not create a distinct Prometheus series
/// per raw path. We hit many random 404 paths, then scrape `/metrics` and
/// assert (a) none of the raw flood paths appear as label values, and (b) a
/// `<not_found>` sentinel label is present instead.
#[tokio::test]
async fn not_found_flood_does_not_explode_metric_cardinality() {
    // EXPECTED TO FAIL until BARB-SEC-003 is fixed (RouteMatch::NotFound records
    // the raw request path as a metric label; should use a `<not_found>`
    // sentinel).
    let gw = TestGateway::from_spec(&security_fixture("metrics.yaml"))
        .await
        .expect("failed to start gateway");

    // Flood unique 404 paths.
    let unique_paths: Vec<String> = (0..50)
        .map(|i| format!("/nonexistent-scan-{}-{}", std::process::id(), i))
        .collect();
    for p in &unique_paths {
        let resp = gw.get(p).await.unwrap();
        assert_eq!(resp.status(), 404, "{} should be a 404", p);
    }

    // Scrape the Prometheus metrics from the admin port.
    let metrics = gw
        .admin_get("/metrics")
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    // (a) No raw flood path may appear as a metric label value.
    for p in &unique_paths {
        assert!(
            !metrics.contains(p.as_str()),
            "raw 404 path {} leaked into Prometheus labels (unbounded cardinality DoS)",
            p
        );
    }

    // (b) Unmatched requests should collapse to a sentinel label.
    assert!(
        metrics.contains("<not_found>"),
        "expected a `<not_found>` sentinel label for unmatched requests"
    );
}
