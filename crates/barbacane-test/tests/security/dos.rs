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
//!      the label with an `<unmatched>` sentinel.

use barbacane_test::TestGateway;

use crate::{fixture, security_fixture};

// ---------------------------------------------------------------------------
// 1. Oversized chunked body -> 413
// ---------------------------------------------------------------------------

/// A chunked request whose body exceeds the gateway's body-size limit must be
/// rejected with 413 while streaming — not buffered in full and then checked.
///
/// We boot the gateway with `--max-body-size 100` and POST ~64 KiB with
/// `Transfer-Encoding: chunked` (reqwest streams a body of unknown length as
/// chunked, so there is no Content-Length). The hardened gateway caps the read
/// (`http_body_util::Limited`) and returns 413 before the body is fully
/// buffered or schema validation runs.
#[tokio::test]
async fn oversized_chunked_body_rejected_with_413() {
    let gw = TestGateway::from_spec_with_args(
        &fixture("request-size-limit.yaml"),
        &["--max-body-size", "100"],
    )
    .await
    .expect("failed to start gateway");

    // A 64 KiB payload (gateway limit is 100 bytes). A streaming body forces
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
/// finishes the request headers) must be timed out so it cannot pin a worker
/// indefinitely.
///
/// We boot the gateway with `--keepalive-timeout 2`, which drives the
/// per-request header-read deadline. Then we open a raw TCP socket, send a
/// partial request (request line + one header, but never the terminating blank
/// line) and assert the server closes the connection well within a generous
/// window rather than waiting forever.
#[tokio::test]
async fn slowloris_connection_is_timed_out() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let gw = TestGateway::from_spec_with_args(
        &fixture("request-size-limit.yaml"),
        &["--keepalive-timeout", "2"],
    )
    .await
    .expect("failed to start gateway");

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", gw.port()))
        .await
        .expect("connect");

    // Partial request: never send the blank line that ends the header block.
    // A vulnerable server would wait on this read forever.
    stream
        .write_all(b"GET /limited HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("write");
    stream.flush().await.expect("flush");

    // A hardened server hits the header-read deadline and closes the connection
    // (EOF / reset), optionally after a 408/400. The 15s window is far larger
    // than the 2s deadline, so this is not timing-sensitive.
    let mut buf = [0u8; 1024];
    match tokio::time::timeout(std::time::Duration::from_secs(15), stream.read(&mut buf)).await {
        Ok(Ok(0)) => { /* EOF: server closed the slow connection — good */ }
        Ok(Ok(n)) => {
            let resp = String::from_utf8_lossy(&buf[..n]);
            assert!(
                resp.contains("408") || resp.contains("400"),
                "expected the server to time out the slow connection, got: {resp}"
            );
        }
        Ok(Err(_)) => { /* connection reset by peer — server closed it, good */ }
        Err(_) => panic!("server did not time out the slowloris connection within 15s"),
    }
}

// ---------------------------------------------------------------------------
// 3. 404-flood metric cardinality
// ---------------------------------------------------------------------------

/// Flooding unique unmatched paths must not create a distinct Prometheus series
/// per raw path. We hit many random 404 paths, then scrape `/metrics` and
/// assert (a) none of the raw flood paths appear as label values, and (b) an
/// `<unmatched>` sentinel label is present instead.
#[tokio::test]
async fn not_found_flood_does_not_explode_metric_cardinality() {
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
        metrics.contains("<unmatched>"),
        "expected an `<unmatched>` sentinel label for unmatched requests"
    );
}
