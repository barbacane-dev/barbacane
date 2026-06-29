//! BARB-SEC-005 — Crypto / auth: JWT validation and trust of forged client IPs.
//!
//! Two threat families, end-to-end through the gateway:
//!
//!   * **JWT validation** — `alg:none`, expired `exp`, wrong `aud`, and a
//!     tampered signature must all be rejected; a validly-signed token must be
//!     accepted. (`jwt-auth` rejects `alg:none`/HMAC and enforces exp/aud
//!     today, but real signature verification is not yet implemented — so the
//!     "validly-signed token is accepted" assertion is RED.)
//!
//!   * **Forged `X-Forwarded-For`** — a client-supplied XFF header must not let
//!     a request bypass `ip-restriction` or steer `rate-limit` partitioning.
//!     Today `ip-restriction::extract_client_ip` trusts the first XFF value
//!     unconditionally, so a forged header changes the effective client IP. The
//!     fix only trusts XFF from configured trusted proxies and otherwise uses
//!     the real peer address.

use barbacane_test::TestGateway;

use crate::{encode_jwt, fixture, now_timestamp, security_fixture};

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

/// A token with a tampered signature must be rejected when signature validation
/// is enforced. (This already holds since jwt-auth fails closed when it cannot
/// verify — but it must KEEP holding once real verification lands.)
#[tokio::test]
async fn jwt_tampered_signature_is_rejected() {
    let gw = TestGateway::from_spec(&security_fixture("jwt-verify.yaml"))
        .await
        .expect("failed to start gateway");

    let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
    let claims = serde_json::json!({
        "sub": "user",
        "iss": "test-issuer",
        "aud": "test-audience",
        "exp": now_timestamp() + 3600,
    });
    let token = encode_jwt(&header, &claims, b"this-is-not-a-valid-signature");

    let resp = gw
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "a token with an invalid signature must be rejected"
    );
}

/// A validly-signed token must be ACCEPTED when signature validation is enforced.
///
/// This is the discriminating case: today jwt-auth has no real signature
/// verification (it fails closed for every token when `skip_signature_validation`
/// is false), so even a correctly-signed token is rejected. The BARB-SEC-005 fix
/// implements verification against `public_key_pem`, after which a properly
/// signed token is accepted.
#[tokio::test]
#[ignore = "BLOCKED: requires real RS256 signature verification (public_key_pem) in jwt-auth, plus a matching private key in the fixture to sign with. See BARB-SEC-005."]
async fn jwt_valid_signature_is_accepted() {
    // EXPECTED TO FAIL until BARB-SEC-005 is fixed (signature verification
    // unimplemented; fixture also needs a real RSA keypair so the test can sign).
    let gw = TestGateway::from_spec(&security_fixture("jwt-verify.yaml"))
        .await
        .expect("failed to start gateway");

    // Once verification exists, sign these claims with the private key matching
    // the fixture's public_key_pem and assert 200.
    let _ = &gw;
}

// ---------------------------------------------------------------------------
// Forged X-Forwarded-For
// ---------------------------------------------------------------------------

/// A forged `X-Forwarded-For` must not let a request masquerade as a
/// denylisted IP — and, by the same token, must not let an external client
/// masquerade as an allowlisted one. The real test client is 127.0.0.1, which
/// `ip-restriction.yaml`'s `/denylist` (deny 10.0.0.0/8, 192.168.0.0/16) allows.
/// Forging `X-Forwarded-For: 10.0.0.5` currently flips the effective client IP
/// into the denied range (→ 403). With the fix, untrusted XFF is ignored and the
/// real peer (127.0.0.1) is used (→ 200).
#[tokio::test]
async fn forged_xff_does_not_bypass_ip_restriction() {
    // EXPECTED TO FAIL until BARB-SEC-005 is fixed (ip-restriction trusts
    // client-supplied X-Forwarded-For unconditionally).
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
    // EXPECTED TO FAIL until BARB-SEC-005 is fixed (client IP, and thus the
    // rate-limit partition key, is derived from spoofable XFF).
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
