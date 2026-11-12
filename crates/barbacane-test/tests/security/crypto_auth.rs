//! Crypto and auth through the gateway: JWT validation (`alg:none`, expired
//! `exp`, wrong `aud` and a tampered signature are rejected; a validly signed
//! token is accepted) and forged `X-Forwarded-For` headers (ignored unless
//! the peer is a configured trusted proxy).

use barbacane_test::TestGateway;

use crate::{encode_jwt, fixture, now_timestamp, security_fixture};

/// Fixed P-256 signing key for the signature tests. Deterministic, so the
/// public JWK embedded in the spec is stable.
fn jwt_signing_key() -> p256::ecdsa::SigningKey {
    p256::ecdsa::SigningKey::from_slice(&[0x11u8; 32]).expect("valid P-256 scalar")
}

/// A temp spec whose `/protected` route requires jwt-auth verifying ES256
/// tokens against the public key of [`jwt_signing_key`].
fn jwt_verify_spec() -> (tempfile::TempDir, std::path::PathBuf) {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    let point = jwt_signing_key().verifying_key().to_sec1_point(false);
    let x = URL_SAFE_NO_PAD.encode(point.x().expect("x").as_slice());
    let y = URL_SAFE_NO_PAD.encode(point.y().expect("y").as_slice());
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    let temp = tempfile::TempDir::new().expect("temp dir");
    std::fs::write(
        temp.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  mock:\n    path: {}\n  jwt-auth:\n    path: {}\n",
            plugins.join("mock/mock.wasm").display(),
            plugins.join("jwt-auth/jwt-auth.wasm").display()
        ),
    )
    .expect("write manifest");
    let spec = temp.path().join("jwt-verify.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: "3.1.0"
info:
  title: JWT Signature Verification Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      issuer: "test-issuer"
      audience: "test-audience"
      clock_skew_seconds: 60
      public_key_jwk:
        kty: "EC"
        crv: "P-256"
        alg: "ES256"
        x: "{x}"
        y: "{y}"
paths:
  /protected:
    get:
      operationId: getProtected
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message":"Access granted"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
        "401":
          description: Unauthorized
security:
  - BearerAuth: []
components:
  securitySchemes:
    BearerAuth:
      type: http
      scheme: bearer
      bearerFormat: JWT
"#
        ),
    )
    .expect("write spec");
    (temp, spec)
}

/// Claims accepted by [`jwt_verify_spec`], valid for an hour.
fn verify_claims() -> serde_json::Value {
    serde_json::json!({
        "sub": "user",
        "iss": "test-issuer",
        "aud": "test-audience",
        "exp": now_timestamp() + 3600,
    })
}

/// `header.claims` signed with [`jwt_signing_key`] (ES256, raw `r || s`).
fn sign_es256(header: &serde_json::Value, claims: &serde_json::Value) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use p256::ecdsa::{signature::Signer, Signature};

    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string().as_bytes()),
        URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes())
    );
    let signature: Signature = jwt_signing_key().sign(signing_input.as_bytes());
    format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

// ---------------------------------------------------------------------------
// JWT validation — regression locks (should pass today) + the RED signature case
// ---------------------------------------------------------------------------

/// A token with `alg: none` must be rejected. (Regression lock: jwt-auth
/// already rejects `none`; this keeps it that way.)
#[tokio::test]
async fn jwt_alg_none_is_rejected() {
    let gw = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let header = serde_json::json!({"alg": "none", "typ": "JWT"});
    let claims = serde_json::json!({
        "sub": "attacker",
        "iss": "test-issuer",
        "aud": "test-audience",
        "exp": now_timestamp() + 3600,
    });
    // alg:none tokens conventionally carry an empty signature.
    let token = encode_jwt(&header, &claims, b"");

    let resp = gw
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "alg:none must be rejected");
}

/// An expired token must be rejected. (Regression lock.)
#[tokio::test]
async fn jwt_expired_is_rejected() {
    let gw = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
    let claims = serde_json::json!({
        "sub": "user",
        "iss": "test-issuer",
        "aud": "test-audience",
        "exp": now_timestamp() - 120, // 2 min ago, beyond 60s skew
    });
    let token = encode_jwt(&header, &claims, b"sig");

    let resp = gw
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "expired exp must be rejected");
}

/// A token with the wrong audience must be rejected. (Regression lock.)
#[tokio::test]
async fn jwt_wrong_audience_is_rejected() {
    let gw = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
    let claims = serde_json::json!({
        "sub": "user",
        "iss": "test-issuer",
        "aud": "WRONG-audience",
        "exp": now_timestamp() + 3600,
    });
    let token = encode_jwt(&header, &claims, b"sig");

    let resp = gw
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "wrong aud must be rejected");
}

/// A token whose signature does not verify under the configured key is
/// rejected: a wrong signature, and a valid signature on altered claims.
#[tokio::test]
async fn jwt_tampered_signature_is_rejected() {
    let (_tmp, spec) = jwt_verify_spec();
    let gw = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");
    let header = serde_json::json!({"alg": "ES256", "typ": "JWT"});

    let forged = encode_jwt(&header, &verify_claims(), &[7u8; 64]);
    let signed = sign_es256(&header, &verify_claims());
    let (signing_input, signature) = signed.rsplit_once('.').unwrap();
    let (header_b64, _) = signing_input.split_once('.').unwrap();
    let altered_claims = {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let mut claims = verify_claims();
        claims["sub"] = serde_json::json!("admin");
        URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes())
    };
    let altered = format!("{header_b64}.{altered_claims}.{signature}");

    for (name, token) in [("forged signature", forged), ("altered claims", altered)] {
        let resp = gw
            .request_builder(reqwest::Method::GET, "/protected")
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "{name} must be rejected");
    }
}

/// A token validly signed under the configured key is accepted.
#[tokio::test]
async fn jwt_valid_signature_is_accepted() {
    let (_tmp, spec) = jwt_verify_spec();
    let gw = TestGateway::from_spec(spec.to_str().unwrap())
        .await
        .expect("failed to start gateway");
    let token = sign_es256(
        &serde_json::json!({"alg": "ES256", "typ": "JWT"}),
        &verify_claims(),
    );
    let resp = gw
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "a validly signed token is accepted");
}

// ---------------------------------------------------------------------------
// Forged X-Forwarded-For
// ---------------------------------------------------------------------------

/// A forged `X-Forwarded-For` must not let a request masquerade as a
/// denylisted IP — and, by the same token, must not let an external client
/// masquerade as an allowlisted one. The real test client is 127.0.0.1, which
/// `ip-restriction.yaml`'s `/denylist` (deny 10.0.0.0/8, 192.168.0.0/16) allows.
/// A forged `X-Forwarded-For: 10.0.0.5` from an untrusted peer is ignored and
/// the real peer (127.0.0.1) is used (200).
#[tokio::test]
async fn forged_xff_does_not_bypass_ip_restriction() {
    let gw = TestGateway::from_spec(&fixture("ip-restriction.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gw
        .request_builder(reqwest::Method::GET, "/denylist")
        .header("X-Forwarded-For", "10.0.0.5")
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "a forged X-Forwarded-For must be ignored; the real peer (127.0.0.1) is \
         not denylisted so the request must succeed"
    );
}

/// A forged `X-Forwarded-For` must not partition the rate-limit buckets — an
/// attacker must not be able to dodge their own rate limit by rotating the XFF
/// header. With `partition_key: client_ip` and untrusted XFF ignored, all the
/// requests below share one bucket and the (n+1)th is limited regardless of XFF.
#[tokio::test]
async fn forged_xff_does_not_reset_rate_limit_bucket() {
    let gw = TestGateway::from_spec(&security_fixture("xff-rate-limit.yaml"))
        .await
        .expect("failed to start gateway");

    // Exhaust the quota (3) while rotating XFF on every request.
    let mut last_status = 0u16;
    for i in 0..5 {
        let resp = gw
            .request_builder(reqwest::Method::GET, "/limited")
            .header("X-Forwarded-For", format!("203.0.113.{}", i))
            .send()
            .await
            .unwrap();
        last_status = resp.status().as_u16();
    }

    assert_eq!(
        last_status, 429,
        "rotating X-Forwarded-For must not create fresh rate-limit buckets; \
         the 4th+ request must still be limited"
    );
}
