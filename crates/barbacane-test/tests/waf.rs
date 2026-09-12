//! Integration tests for the WAF pipeline stage.
//!
//! These cover the gateway wiring rather than detection quality: header and
//! body collection, anomaly-score accumulation, the block response shape and
//! the metrics. Rule-language conformance is measured by parapet's go-ftw
//! suite, and the compile-time sealing and integrity checks are unit-tested in
//! `barbacane-compiler`.
//!
//! The fixture rule set is `tests/fixtures/waf-rules` (see its README for the
//! rules and their scores). The gateway boots with `--dev`, so the block
//! response carries `rule_id` and the tests can assert which rule fired.
//!
//! Run with: `cargo test -p barbacane-test --test waf`

use barbacane_test::TestGateway;
use reqwest::header::{HeaderName, HeaderValue};

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

/// The rule that makes the blocking decision on the accumulated inbound score.
const ANOMALY_RULE: u64 = 1_009_110;

/// The rule that makes the blocking decision on the accumulated outbound score.
const OUTBOUND_ANOMALY_RULE: u64 = 1_009_120;

async fn blocking_gateway() -> TestGateway {
    TestGateway::from_spec(&fixture("waf.yaml"))
        .await
        .expect("failed to start gateway")
}

/// Assert a request was blocked as an RFC 9457 problem document, and return the
/// id of the rule that blocked it.
async fn assert_blocked(resp: reqwest::Response, expected_status: u16) -> u64 {
    assert_eq!(
        resp.status().as_u16(),
        expected_status,
        "expected the WAF to block with {expected_status}"
    );
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.contains("application/problem+json"),
        "block response must be a problem document, got content-type {content_type:?}"
    );
    let body: serde_json::Value = resp.json().await.expect("problem document must be JSON");
    assert_eq!(body["type"], "urn:barbacane:error:waf-blocked");
    assert_eq!(body["status"], expected_status);
    body["rule_id"]
        .as_u64()
        .expect("dev mode must report the blocking rule id")
}

async fn metrics(gateway: &TestGateway) -> String {
    let resp = gateway.admin_get("/metrics").await.unwrap();
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap()
}

/// Find the value of a metric sample whose line contains every fragment given.
fn sample(body: &str, family: &str, fragments: &[&str]) -> Option<f64> {
    body.lines()
        .filter(|l| l.starts_with(family))
        .find(|l| fragments.iter().all(|f| l.contains(f)))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
}

// ---------------------------------------------------------------------------
// Attack classes reach the rules and are refused
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sql_injection_in_a_query_argument_is_blocked() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .get("/waf/search?q=1'%20or%20'1'%3D'1")
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

#[tokio::test]
async fn cross_site_scripting_in_a_query_argument_is_blocked() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .get("/waf/search?q=%3Cscript%3Ealert(1)%3C%2Fscript%3E")
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

#[tokio::test]
async fn path_traversal_in_a_query_argument_is_blocked() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .get("/waf/search?file=..%2F..%2F..%2Fetc%2Fpasswd")
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

#[tokio::test]
async fn command_injection_in_a_query_argument_is_blocked() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .get("/waf/search?q=x%3B%20cat%20%2Fetc%2Fpasswd")
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

#[tokio::test]
async fn an_attack_in_a_json_body_is_blocked() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .post("/waf/submit", r#"{"comment":"1' or '1'='1"}"#)
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

/// Exercises `@pmFromFile`: the phrase list is sealed into the artifact at
/// compile time and loaded from it at boot, with no filesystem access at
/// request time.
#[tokio::test]
async fn a_scanner_user_agent_from_a_sealed_phrase_list_is_blocked() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/waf/search?q=hello")
        .header("user-agent", "sqlmap/1.7")
        .send()
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

#[tokio::test]
async fn a_benign_request_passes_through() {
    let gateway = blocking_gateway().await;
    let resp = gateway.get("/waf/search?q=hello%20world").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn a_benign_json_body_passes_through() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .post("/waf/submit", r#"{"comment":"looks fine to me"}"#)
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ---------------------------------------------------------------------------
// Response-phase inspection (phases 3, 4 and 5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_leaking_response_body_is_blocked() {
    let gateway = blocking_gateway().await;
    let resp = gateway.get("/waf/leak-body").await.unwrap();
    assert_eq!(assert_blocked(resp, 403).await, OUTBOUND_ANOMALY_RULE);

    let body = metrics(&gateway).await;
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_matched_total",
            &["rule_id=\"1004100\"", "path=\"/waf/leak-body\""]
        ),
        Some(1.0),
        "the phase-4 body rule must be counted"
    );
}

#[tokio::test]
async fn a_leaking_response_header_is_blocked() {
    let gateway = blocking_gateway().await;
    let resp = gateway.get("/waf/leak-header").await.unwrap();
    assert_eq!(assert_blocked(resp, 403).await, OUTBOUND_ANOMALY_RULE);

    let body = metrics(&gateway).await;
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_matched_total",
            &["rule_id=\"1003100\"", "path=\"/waf/leak-header\""]
        ),
        Some(1.0),
        "the phase-3 header rule must be counted"
    );
}

#[tokio::test]
async fn a_clean_response_passes_through() {
    let gateway = blocking_gateway().await;
    let resp = gateway.get("/waf/clean-response").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "all clear");
}

/// A body over `max_response_body` is not collected, so phase-4 body rules do
/// not see it and the skip is counted. The response is not blocked on its body.
#[tokio::test]
async fn an_oversized_response_body_is_not_inspected_and_the_skip_is_counted() {
    let gateway = TestGateway::from_spec(&fixture("waf-response-cap.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/waf/big-body-leak").await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "the leaking body is over the cap, so it is not inspected or blocked"
    );

    let body = metrics(&gateway).await;
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_response_body_skipped_total",
            &["path=\"/waf/big-body-leak\""]
        ),
        Some(1.0),
        "skipping the body must be counted, not silent"
    );
    assert!(
        sample(
            &body,
            "barbacane_waf_matched_total",
            &["rule_id=\"1004100\""]
        )
        .is_none(),
        "the phase-4 body rule must not match a body that was never inspected"
    );
}

/// Response headers are inspected (phase 3) even when the body is skipped, so a
/// leaking header on an oversized response still blocks.
#[tokio::test]
async fn response_headers_are_inspected_even_when_the_body_is_skipped() {
    let gateway = TestGateway::from_spec(&fixture("waf-response-cap.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/waf/big-body-with-header").await.unwrap();
    assert_eq!(assert_blocked(resp, 403).await, OUTBOUND_ANOMALY_RULE);

    let body = metrics(&gateway).await;
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_response_body_skipped_total",
            &["path=\"/waf/big-body-with-header\""]
        ),
        Some(1.0),
        "the body was over the cap, so its inspection was skipped and counted"
    );
}

// ---------------------------------------------------------------------------
// Header collection
// ---------------------------------------------------------------------------

/// A header value is a byte string, not UTF-8. Dropping the ones that fail to
/// decode leaves an uninspected channel into the application, so values are
/// converted lossily and still reach the rules.
#[tokio::test]
async fn a_header_value_with_invalid_utf8_is_still_inspected() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/waf/search?q=hello")
        .header(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_bytes(b"nikto\xff").unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

/// Two headers with the same name are two values. Collapsing them to one loses
/// whichever the rules needed, so both are collected.
///
/// The first value matches rule 1001610 and the second does not, so a
/// last-value-wins collapse would score zero and let the request through.
#[tokio::test]
async fn every_value_of_a_repeated_header_is_inspected() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/waf/search?debug=1")
        .header("x-trace", "on")
        .header("x-trace", "off")
        .send()
        .await
        .unwrap();
    // 1001610 (+3) on the first value, 1001600 (+3) on `debug=1`, threshold 5.
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

// ---------------------------------------------------------------------------
// Anomaly scoring
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_score_below_the_threshold_is_recorded_and_allowed() {
    let gateway = blocking_gateway().await;
    let resp = gateway.get("/waf/search?debug=1").await.unwrap();
    assert_eq!(resp.status(), 200, "3 points is under the threshold of 5");

    let body = metrics(&gateway).await;
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_matched_total",
            &["rule_id=\"1001600\"", "path=\"/waf/search\""]
        ),
        Some(1.0),
        "a rule that matched without blocking must still be counted"
    );
    assert!(
        sample(
            &body,
            "barbacane_waf_blocked_total",
            &["path=\"/waf/search\""]
        )
        .is_none(),
        "nothing was blocked"
    );
}

#[tokio::test]
async fn scores_accumulate_across_rules_until_the_threshold() {
    let gateway = blocking_gateway().await;

    // 3 points on its own: allowed.
    let resp = gateway.get("/waf/search?debug=1").await.unwrap();
    assert_eq!(resp.status(), 200);

    // The same argument plus a second 3-point rule: 6 points, refused.
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/waf/search?debug=1")
        .header("x-trace", "on")
        .send()
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}

/// A rule can deny with its own status. The problem document's title is
/// derived from that status rather than assumed to be "Forbidden".
#[tokio::test]
async fn a_rule_status_other_than_403_shapes_the_problem_response() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/waf/search?q=hello")
        .header("x-waf-fixture-status", "406")
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    assert_eq!(status, 406);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Not Acceptable");
    assert_eq!(body["status"], 406);
    assert_eq!(body["rule_id"].as_u64(), Some(1_001_700));
}

// ---------------------------------------------------------------------------
// Detection-only mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn detection_only_records_matches_without_blocking() {
    let gateway = TestGateway::from_spec(&fixture("waf-detection-only.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .get("/waf/search?q=1'%20or%20'1'%3D'1")
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "detection-only mode must not interrupt the request"
    );

    let body = metrics(&gateway).await;
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_matched_total",
            &["rule_id=\"1001100\"", "path=\"/waf/search\""]
        ),
        Some(1.0),
        "the SQL injection rule must be reported even though nothing was blocked"
    );
    assert!(
        sample(
            &body,
            "barbacane_waf_blocked_total",
            &["path=\"/waf/search\""]
        )
        .is_none(),
        "barbacane_waf_blocked_total must stay at zero in detection-only mode"
    );
}

// ---------------------------------------------------------------------------
// Metrics surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn blocking_a_request_counts_the_rule_that_did_it() {
    let gateway = blocking_gateway().await;
    let resp = gateway
        .get("/waf/search?q=1'%20or%20'1'%3D'1")
        .await
        .unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);

    let body = metrics(&gateway).await;
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_blocked_total",
            &["rule_id=\"1009110\"", "path=\"/waf/search\""]
        ),
        Some(1.0)
    );
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_matched_total",
            &["rule_id=\"1001100\"", "path=\"/waf/search\""]
        ),
        Some(1.0),
        "the scoring rule that contributed must be counted, not only the blocking rule"
    );
}

#[tokio::test]
async fn an_allowed_request_is_counted_and_timed() {
    let gateway = blocking_gateway().await;
    assert_eq!(
        gateway.get("/waf/search?q=hello").await.unwrap().status(),
        200
    );

    let body = metrics(&gateway).await;
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_allowed_total",
            &["path=\"/waf/search\"", "method=\"GET\""]
        ),
        Some(1.0)
    );
    assert_eq!(
        sample(
            &body,
            "barbacane_waf_duration_seconds_count",
            &["path=\"/waf/search\""]
        ),
        Some(1.0),
        "inspection latency must be observed for every inspected request"
    );
}

#[tokio::test]
async fn a_spec_without_the_waf_extension_exports_no_waf_samples() {
    let gateway = TestGateway::from_spec(&fixture("mock.yaml"))
        .await
        .expect("failed to start gateway");
    let _ = gateway.get("/mock/ok").await.unwrap();

    let body = metrics(&gateway).await;
    assert!(
        sample(&body, "barbacane_waf_allowed_total", &["path="]).is_none(),
        "no rule set is configured, so nothing should be inspected"
    );
    assert!(
        sample(&body, "barbacane_waf_duration_seconds_count", &["path="]).is_none(),
        "no rule set is configured, so nothing should be timed"
    );
}

// ---------------------------------------------------------------------------
// Paranoia level
// ---------------------------------------------------------------------------

/// CRS gates whole rule files on `tx.blocking_paranoia_level`, and an unset
/// variable reads as zero, so the level the spec declares has to reach the
/// engine. The same rule set produces different verdicts at the two levels.
#[tokio::test]
async fn a_higher_paranoia_rule_does_not_fire_at_level_one() {
    let gateway = blocking_gateway().await;
    let resp = gateway.get("/waf/search?q=paranoid").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn a_higher_paranoia_rule_fires_at_level_two() {
    let gateway = TestGateway::from_spec(&fixture("waf-paranoia-2.yaml"))
        .await
        .expect("failed to start gateway");
    let resp = gateway.get("/waf/search?q=paranoid").await.unwrap();
    assert_eq!(assert_blocked(resp, 403).await, ANOMALY_RULE);
}
