//! Uploads through the s3 dispatcher, checked by a mock S3 that recomputes
//! what was signed.
//!
//! The mock accepts a PUT only when `x-amz-content-sha256` is the SHA-256 of
//! the body it received and the `Authorization` header is exactly the SigV4
//! signature of that request, recomputed here. Anything else gets the mock's
//! default 404, so a 200 through the gateway proves the object arrived intact
//! and correctly signed.
//!
//! Run with: `cargo test -p barbacane-test --test s3_dispatch`

use std::collections::BTreeMap;

use barbacane_sigv4 as sigv4;
use barbacane_test::TestGateway;
use sha2::{Digest, Sha256};
use wiremock::matchers::method;
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const REGION: &str = "us-east-1";
/// The gateway's body limit in these tests.
const MAX_BODY: usize = 32 * 1024 * 1024;

/// Matches a request whose payload hash and signature are both correct.
struct SignedOverItsBody;

impl Match for SignedOverItsBody {
    fn matches(&self, req: &Request) -> bool {
        let header = |name: &str| {
            req.headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let (Some(payload), Some(amz_date), Some(host), Some(authorization)) = (
            header("x-amz-content-sha256"),
            header("x-amz-date"),
            header("host"),
            header("authorization"),
        ) else {
            return false;
        };
        if payload != hex::encode(Sha256::digest(&req.body)) {
            return false;
        }

        let mut headers_to_sign = BTreeMap::new();
        headers_to_sign.insert("host".to_string(), host);
        headers_to_sign.insert("x-amz-content-sha256".to_string(), payload.clone());
        headers_to_sign.insert("x-amz-date".to_string(), amz_date.clone());
        let canonical_uri = sigv4::canonical_uri(req.url.path());
        let canonical_query = sigv4::canonical_query(req.url.query());
        let expected = sigv4::sign(
            &sigv4::SigningInput {
                method: req.method.as_str(),
                canonical_uri: &canonical_uri,
                canonical_query: &canonical_query,
                headers_to_sign: &headers_to_sign,
                body_sha256: &payload,
                datetime: &amz_date,
                date: &amz_date[..8],
            },
            &sigv4::Credentials {
                access_key_id: ACCESS_KEY.to_string(),
                secret_access_key: SECRET_KEY.to_string(),
                session_token: None,
            },
            &sigv4::SigningConfig {
                region: REGION,
                service: "s3",
            },
        );
        authorization == expected.authorization
    }
}

/// A spec routing PUT /s3/{bucket}/{key+} to the s3 dispatcher at `endpoint`.
fn spec(endpoint: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/s3/s3.wasm")
        .canonicalize()
        .expect("plugins/s3/s3.wasm is built (make plugins)");
    std::fs::write(
        dir.path().join("barbacane.yaml"),
        format!("plugins:\n  s3:\n    path: {}\n", plugins.display()),
    )
    .expect("manifest");
    let spec = dir.path().join("s3.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: "3.1.0"
info:
  title: S3 uploads
  version: "1.0.0"
paths:
  /s3/{{bucket}}/{{key+}}:
    put:
      operationId: putObject
      parameters:
        - {{ name: bucket, in: path, required: true, schema: {{ type: string }} }}
        - {{ name: key, in: path, required: true, allowReserved: true, schema: {{ type: string }} }}
      requestBody:
        required: false
        content:
          "*/*":
            schema: {{ type: string, format: binary }}
      x-barbacane-dispatch:
        name: s3
        config:
          region: {REGION}
          access_key_id: {ACCESS_KEY}
          secret_access_key: {SECRET_KEY}
          endpoint: {endpoint}
          force_path_style: true
      responses:
        "200": {{ description: Stored }}
"#
        ),
    )
    .expect("spec");
    (dir, spec)
}

async fn setup() -> (MockServer, TestGateway, tempfile::TempDir) {
    let s3 = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(SignedOverItsBody)
        .respond_with(ResponseTemplate::new(200).insert_header("etag", "\"stored\""))
        .mount(&s3)
        .await;
    let (dir, spec) = spec(&s3.uri());
    let gateway = TestGateway::from_spec_with_args(
        spec.to_str().expect("utf-8 path"),
        &["--max-body-size", &MAX_BODY.to_string()],
    )
    .await
    .expect("gateway starts");
    (s3, gateway, dir)
}

/// Bytes that differ along the body, so a truncated or shifted upload changes
/// the hash.
fn body(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect()
}

async fn put(gateway: &TestGateway, key: &str, body: Vec<u8>) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!("{}/s3/bucket/{key}", gateway.base_url()))
        .header("content-type", "application/octet-stream")
        .body(body)
        .send()
        .await
        .expect("request")
}

#[tokio::test]
async fn uploads_of_every_size_arrive_signed_over_their_exact_bytes() {
    let (s3, gateway, _dir) = setup().await;
    // Around the ~750 KB at which hashing in WASM ran out of fuel, and up to
    // past 20 MB.
    let sizes = [
        1,
        64 * 1024,
        700 * 1024,
        800 * 1024,
        1024 * 1024,
        4 * 1024 * 1024,
        20 * 1024 * 1024,
        24 * 1024 * 1024,
    ];
    for (i, len) in sizes.into_iter().enumerate() {
        let sent = body(len);
        let response = put(&gateway, &format!("object-{i}"), sent.clone()).await;
        assert_eq!(
            response.status(),
            200,
            "{len} bytes: {}",
            response.text().await.unwrap_or_default()
        );
        let received = s3.received_requests().await.expect("recording on");
        let last = received.last().expect("the mock saw the upload");
        assert_eq!(last.body.len(), len, "{len} bytes arrived whole");
        assert!(last.body == sent, "{len} bytes arrived unchanged");
    }
}

#[tokio::test]
async fn an_empty_upload_is_signed_over_the_empty_hash() {
    let (s3, gateway, _dir) = setup().await;
    let response = put(&gateway, "empty", Vec::new()).await;
    assert_eq!(response.status(), 200);
    let received = s3.received_requests().await.expect("recording on");
    assert_eq!(
        received[0].headers["x-amz-content-sha256"],
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[tokio::test]
async fn control_a_payload_hash_that_does_not_match_the_body_is_refused() {
    // Shows the mock refuses what it should, so the 200s above mean something.
    let s3 = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(SignedOverItsBody)
        .respond_with(ResponseTemplate::new(200))
        .mount(&s3)
        .await;
    let forged = reqwest::Client::new()
        .put(format!("{}/bucket/key", s3.uri()))
        .header(
            "x-amz-content-sha256",
            hex::encode(Sha256::digest(b"other bytes")),
        )
        .header("x-amz-date", "20260925T120000Z")
        .header("authorization", "AWS4-HMAC-SHA256 Credential=x")
        .body("these bytes")
        .send()
        .await
        .expect("request");
    assert_eq!(forged.status(), 404);
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused_before_the_plugin() {
    let (s3, gateway, _dir) = setup().await;
    let response = put(&gateway, "too-big", body(MAX_BODY + 1)).await;
    assert_eq!(response.status(), 413);
    let received = s3.received_requests().await.expect("recording on");
    assert!(received.is_empty(), "nothing reaches S3");
}

#[tokio::test]
async fn a_body_exactly_at_the_limit_is_stored() {
    let (_s3, gateway, _dir) = setup().await;
    let response = put(&gateway, "at-limit", body(MAX_BODY)).await;
    assert_eq!(
        response.status(),
        200,
        "{}",
        response.text().await.unwrap_or_default()
    );
}
