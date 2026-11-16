//! BARB-SEC-001 — Control-plane authorization & project-scoped IDOR.
//!
//! Threat: the control plane is the administrative surface — it can create
//! projects, upload plugin WASM, deploy artifacts, and mint data-plane API
//! keys. Today every route is reachable with no credential, and global
//! `/specs/{id}` / `/artifacts/{id}` reads perform no project-ownership check.
//!
//! The fix (per the hardening plan):
//!   * Require a Bearer token on every route except `/health` and
//!     `/ws/data-plane`. The token is read from `BARBACANE_CONTROL_ADMIN_TOKEN`
//!     and the server fails to start without it.
//!   * Enforce project ownership so project A's credential cannot read or
//!     mutate project B's specs / artifacts / api-keys (IDOR).
//!
//! The `require_admin` middleware already exists in
//! `crates/barbacane-control/src/api/auth.rs` but is NOT yet wired into
//! `create_router`, so these tests are RED.
//!
//! ## Why these tests boot a subprocess
//!
//! `barbacane-control` is a **binary-only** crate (no `src/lib.rs`) and
//! `barbacane-test` does not depend on it, so we cannot drive its Axum router
//! in-process. We therefore boot the `barbacane-control` binary the same way
//! `TestGateway` boots the data plane. This requires:
//!   * a reachable PostgreSQL (`DATABASE_URL`, see docs), and
//!   * the `barbacane-control` binary to be built (`cargo build -p barbacane-control`).
//!
//! When either is missing the harness returns `None` and the test SKIPS (prints
//! a `skip:` line and returns) rather than failing spuriously — matching the
//! pattern in `crates/barbacane-control/src/api/tests.rs`. The security
//! assertions only run when the control plane is actually up, so they are RED
//! against a running-but-unhardened control plane and GREEN once the fix lands.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT, LISTEN_HOST, STARTUP_TIMEOUT};

/// A booted control-plane process for authz testing.
struct TestControlPlane {
    process: LoggedChild,
    base_url: String,
    client: reqwest::Client,
}

impl Drop for TestControlPlane {
    fn drop(&mut self) {
        self.process.kill_and_reap();
    }
}

impl TestControlPlane {
    /// Boot the control plane with the given admin token in the environment.
    ///
    /// Returns `None` (test should SKIP) when prerequisites are unavailable:
    /// no `DATABASE_URL`, no built binary, or the server never becomes healthy.
    async fn boot(admin_token: Option<&str>) -> Option<Self> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let binary = find_control_binary()?;
        let mut cmd = Command::new(&binary);
        cmd.arg("serve")
            .arg("--listen")
            .arg(LISTEN_ANY_PORT)
            .arg("--database-url")
            .arg(&database_url)
            .env("DATABASE_URL", &database_url);

        match admin_token {
            Some(tok) => {
                cmd.env("BARBACANE_CONTROL_ADMIN_TOKEN", tok);
            }
            None => {
                cmd.env_remove("BARBACANE_CONTROL_ADMIN_TOKEN");
            }
        }

        let mut process = LoggedChild::spawn(&mut cmd).ok()?;

        // The control plane announces the address it bound. Without a port
        // there is nothing to poll, and a process that exits first says so at
        // once rather than after the whole timeout.
        let port = process.announced_port("Control plane listening on", STARTUP_TIMEOUT)?;
        let base_url = format!("http://{LISTEN_HOST}:{port}");

        let mut cp = TestControlPlane {
            process,
            base_url,
            client: reqwest::Client::new(),
        };

        // Poll /health until ready (or give up → skip).
        let health = format!("{}/health", cp.base_url);
        for _ in 0..100 {
            if let Ok(resp) = cp.client.get(&health).send().await {
                if resp.status().is_success() {
                    return Some(cp);
                }
            }
            if cp.process.has_exited() {
                // Process exited before becoming healthy (e.g. fail-closed on a
                // missing admin token, or no DB) — treat as skip.
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

/// Locate the built `barbacane-control` binary.
fn find_control_binary() -> Option<PathBuf> {
    barbacane_test::find_workspace_binary("barbacane-control")
}

const ADMIN_TOKEN: &str = "test-admin-token-do-not-use-in-prod";

/// Every mutating control-plane route must reject an unauthenticated request
/// with 401. We exercise a representative set spanning projects, specs,
/// plugins, artifacts, compilations, and api-keys.
#[tokio::test]
async fn mutating_routes_require_admin_token() {
    let Some(cp) = barbacane_test::required(
        TestControlPlane::boot(Some(ADMIN_TOKEN)).await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with `barbacane-control` built",
    ) else {
        return;
    };

    // (method, path, json-body) — one per mutating handler family.
    let mutating: &[(reqwest::Method, &str, &str)] = &[
        (reqwest::Method::POST, "/projects", r#"{"name":"x"}"#),
        (
            reqwest::Method::PUT,
            "/projects/00000000-0000-0000-0000-000000000001",
            r#"{"name":"y"}"#,
        ),
        (
            reqwest::Method::DELETE,
            "/projects/00000000-0000-0000-0000-000000000001",
            "",
        ),
        (
            reqwest::Method::POST,
            "/specs",
            r#"{"name":"x","content":"openapi: 3.1.0"}"#,
        ),
        (
            reqwest::Method::DELETE,
            "/specs/00000000-0000-0000-0000-000000000002",
            "",
        ),
        (
            reqwest::Method::POST,
            "/specs/00000000-0000-0000-0000-000000000002/compile",
            "{}",
        ),
        (
            reqwest::Method::POST,
            "/plugins",
            r#"{"name":"x","version":"0.1.0"}"#,
        ),
        (reqwest::Method::DELETE, "/plugins/x/0.1.0", ""),
        (
            reqwest::Method::DELETE,
            "/artifacts/00000000-0000-0000-0000-000000000003",
            "",
        ),
        (
            reqwest::Method::DELETE,
            "/compilations/00000000-0000-0000-0000-000000000004",
            "",
        ),
        (
            reqwest::Method::POST,
            "/projects/00000000-0000-0000-0000-000000000001/api-keys",
            "{}",
        ),
        (
            reqwest::Method::POST,
            "/projects/00000000-0000-0000-0000-000000000001/deploy",
            "{}",
        ),
    ];

    for (method, path, body) in mutating {
        let resp = cp
            .client
            .request(method.clone(), cp.url(path))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .expect("request to control plane failed");
        assert_eq!(
            resp.status(),
            401,
            "{} {} must require an admin token (got {})",
            method,
            path,
            resp.status()
        );
    }
}

/// With a valid admin token, a representative mutating route is accepted
/// (i.e. the auth layer is gating, not blanket-denying). We assert the status
/// is anything other than 401 — the request reaches the handler.
#[tokio::test]
async fn valid_admin_token_is_accepted() {
    let Some(cp) = barbacane_test::required(
        TestControlPlane::boot(Some(ADMIN_TOKEN)).await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with `barbacane-control` built",
    ) else {
        return;
    };

    let resp = cp
        .client
        .post(cp.url("/projects"))
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
        .body(r#"{"name":"authz-smoke-test"}"#.to_string())
        .send()
        .await
        .expect("request failed");

    assert_ne!(
        resp.status(),
        401,
        "a valid admin token must not be rejected as unauthorized"
    );
}

/// `/health` must remain reachable WITHOUT a token (it is the liveness probe
/// and is explicitly excluded from the admin-auth requirement).
#[tokio::test]
async fn health_is_exempt_from_auth() {
    // This is a positive control: it should pass both before and after the fix.
    let Some(cp) = barbacane_test::required(
        TestControlPlane::boot(Some(ADMIN_TOKEN)).await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with `barbacane-control` built",
    ) else {
        return;
    };

    let resp = cp
        .client
        .get(cp.url("/health"))
        .send()
        .await
        .expect("request failed");
    assert!(
        resp.status().is_success(),
        "/health must be reachable without an admin token"
    );
}

/// The server must fail-closed: starting `serve` with no
/// `BARBACANE_CONTROL_ADMIN_TOKEN` set must refuse to start (so an operator
/// cannot accidentally expose an unauthenticated control plane).
#[tokio::test]
async fn server_refuses_to_start_without_admin_token() {
    // `boot(None)` returning `None` means the process exited, once the database
    // and the binary are known to be there.
    if !barbacane_test::require(
        std::env::var("DATABASE_URL").is_ok() && find_control_binary().is_some(),
        "a control plane on PostgreSQL (`DATABASE_URL`) with `barbacane-control` built",
    ) {
        return;
    }

    let booted = TestControlPlane::boot(None).await;
    assert!(
        booted.is_none(),
        "control plane must fail-closed and refuse to serve without BARBACANE_CONTROL_ADMIN_TOKEN"
    );
}

/// Revoking a data-plane API key ends the WebSocket sessions registered with
/// it, so a revoked credential stops receiving artifacts at once rather than
/// at the data plane's next reconnect.
#[tokio::test]
async fn revoking_an_api_key_closes_its_data_plane_sessions() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let Some(cp) = barbacane_test::required(
        TestControlPlane::boot(Some(ADMIN_TOKEN)).await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with `barbacane-control` built",
    ) else {
        return;
    };
    let bearer = format!("Bearer {ADMIN_TOKEN}");
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();

    let project: serde_json::Value = cp
        .client
        .post(cp.url("/projects"))
        .header("Authorization", &bearer)
        .json(&serde_json::json!({ "name": format!("revoke-{unique}") }))
        .send()
        .await
        .expect("create project")
        .json()
        .await
        .expect("project json");
    let project_id = project["id"].as_str().expect("project id").to_string();

    let key: serde_json::Value = cp
        .client
        .post(cp.url(&format!("/projects/{project_id}/api-keys")))
        .header("Authorization", &bearer)
        .json(&serde_json::json!({ "name": "data-plane" }))
        .send()
        .await
        .expect("create key")
        .json()
        .await
        .expect("key json");
    let key_id = key["id"].as_str().expect("key id").to_string();

    let ws_url = cp.url("/ws/data-plane").replacen("http", "ws", 1);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("connect");
    let register = serde_json::json!({
        "type": "register",
        "project_id": project_id,
        "api_key": key["key"],
    });
    ws.send(Message::Text(register.to_string().into()))
        .await
        .expect("send register");
    let registered = loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => break text.to_string(),
            Ok(Some(Ok(_))) => continue,
            other => panic!("no registration reply: {other:?}"),
        }
    };
    assert!(registered.contains("\"registered\""), "{registered}");

    let revoked = cp
        .client
        .delete(cp.url(&format!("/projects/{project_id}/api-keys/{key_id}")))
        .header("Authorization", &bearer)
        .send()
        .await
        .expect("revoke");
    assert_eq!(revoked.status(), 204);

    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "the session must end once its API key is revoked"
    );
}
