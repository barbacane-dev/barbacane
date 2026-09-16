//! Integration tests for the ldap-auth plugin against a real directory.
//!
//! The directory is glauth with its default configuration
//! (`ghcr.io/glauth/glauth`, plain LDAP on 3893, base `dc=glauth,dc=com`,
//! `johndoe`/`dogood` in group `superheros`, `serviceuser`/`mysecret` in group
//! `svcaccts`). CI provides it as a job service; locally, tests that need it
//! skip when nothing listens on `BARBACANE_TEST_LDAP_URL`
//! (default `ldap://127.0.0.1:3893`).
//!
//! Run with: `cargo test -p barbacane-test --test ldap`

use barbacane_test::TestGateway;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

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

const BIND_PASSWORD: &str = "mysecret";

fn directory_url() -> String {
    std::env::var("BARBACANE_TEST_LDAP_URL").unwrap_or_else(|_| "ldap://127.0.0.1:3893".to_string())
}

/// True when something accepts TCP connections at the directory URL.
fn directory_reachable(url: &str) -> bool {
    let authority = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = authority.split('/').next().unwrap_or(authority);
    let target = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:389")
    };
    target
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok())
        .unwrap_or(false)
}

fn basic(user: &str, pass: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    format!("Basic {}", STANDARD.encode(format!("{user}:{pass}")))
}

async fn gateway(url: &str) -> TestGateway {
    TestGateway::from_spec_with_env(
        &fixture("ldap-auth.yaml"),
        &[
            ("BARBACANE_TEST_LDAP_URL", url),
            ("BARBACANE_TEST_LDAP_BIND_PASSWORD", BIND_PASSWORD),
        ],
    )
    .await
    .expect("failed to start gateway")
}

/// Starts a gateway against the live directory, or returns `None` (after
/// logging) when no directory is reachable.
async fn live_gateway() -> Option<TestGateway> {
    let url = directory_url();
    if !directory_reachable(&url) {
        eprintln!("skipping: no LDAP directory at {url} (set BARBACANE_TEST_LDAP_URL)");
        return None;
    }
    Some(gateway(&url).await)
}

// ==================== paths that need no directory ====================

#[tokio::test]
async fn test_ldap_auth_missing_header_is_401_with_basic_challenge() {
    // A dead port is fine: the header check happens before any directory call.
    let gateway = gateway("ldap://127.0.0.1:13389").await;

    let resp = gateway.get("/protected").await.unwrap();
    assert_eq!(resp.status(), 401);
    let challenge = resp
        .headers()
        .get("www-authenticate")
        .expect("missing WWW-Authenticate")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        challenge.starts_with("Basic realm=\"test-api\""),
        "{challenge}"
    );
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/problem+json"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:authentication-failed");
}

#[tokio::test]
async fn test_ldap_auth_directory_unavailable_is_503() {
    let gateway = gateway("ldap://127.0.0.1:13389").await;

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic("johndoe", "dogood"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:ldap-unavailable");
}

#[tokio::test]
async fn test_ldap_auth_public_endpoint_bypasses_auth() {
    let gateway = gateway("ldap://127.0.0.1:13389").await;

    let resp = gateway.get("/public").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Public access");
}

// ==================== live directory ====================

#[tokio::test]
async fn test_ldap_auth_valid_credentials() {
    let Some(gateway) = live_gateway().await else {
        return;
    };

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic("johndoe", "dogood"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        panic!("expected 200 but got {status}. Body: {body}");
    }
}

#[tokio::test]
async fn test_ldap_auth_wrong_password_is_401() {
    let Some(gateway) = live_gateway().await else {
        return;
    };

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic("johndoe", "wrong"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["detail"], "Invalid username or password");
}

#[tokio::test]
async fn test_ldap_auth_unknown_user_matches_wrong_password_response() {
    let Some(gateway) = live_gateway().await else {
        return;
    };

    let unknown = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic("nobody", "dogood"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 401);
    let unknown_body: serde_json::Value = unknown.json().await.unwrap();

    let wrong = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic("johndoe", "wrong"))
        .send()
        .await
        .unwrap();
    let wrong_body: serde_json::Value = wrong.json().await.unwrap();
    assert_eq!(unknown_body, wrong_body);
}

#[tokio::test]
async fn test_ldap_auth_groups_reach_acl() {
    let Some(gateway) = live_gateway().await else {
        return;
    };

    // johndoe is in superheros: allowed by the acl on /admin.
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/admin")
        .header("Authorization", basic("johndoe", "dogood"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        panic!("expected 200 for superheros member but got {status}. Body: {body}");
    }

    // serviceuser is in svcaccts only: authenticated, but denied by the acl.
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/admin")
        .header("Authorization", basic("serviceuser", "mysecret"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_ldap_auth_filter_injection_is_inert() {
    let Some(gateway) = live_gateway().await else {
        return;
    };

    // Unescaped, this username would turn the filter into (cn=*)(cn=*) and
    // match the whole directory, so the bind with johndoe's password would
    // succeed. Escaped, a compliant server matches nothing (401); glauth
    // rejects the escaped filter outright, which the plugin reports as 503.
    // Either way the request must never be authenticated.
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic("*)(cn=*", "dogood"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    assert!(
        status == 401 || status == 503,
        "expected 401 or 503, got {status}"
    );
}
