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

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A booted control-plane process for authz testing.
struct TestControlPlane {
    child: Child,
    base_url: String,
    client: reqwest::Client,
}

impl Drop for TestControlPlane {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        let port = free_port()?;
        let base_url = format!("http://127.0.0.1:{}", port);

        let mut cmd = Command::new(&binary);
        cmd.arg("serve")
            .arg("--listen")
            .arg(format!("127.0.0.1:{}", port))
            .arg("--database-url")
            .arg(&database_url)
            .env("DATABASE_URL", &database_url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        match admin_token {
            Some(tok) => {
                cmd.env("BARBACANE_CONTROL_ADMIN_TOKEN", tok);
            }
            None => {
                cmd.env_remove("BARBACANE_CONTROL_ADMIN_TOKEN");
            }
        }

        let child = cmd.spawn().ok()?;
        let client = reqwest::Client::new();

        let mut cp = TestControlPlane {
            child,
            base_url,
            client,
        };

        // Poll /health until ready (or give up → skip).
        let health = format!("{}/health", cp.base_url);
        for _ in 0..100 {
            if let Ok(resp) = cp.client.get(&health).send().await {
                if resp.status().is_success() {
                    return Some(cp);
                }
            }
            if let Ok(Some(_)) = cp.child.try_wait() {
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
    let candidates = [
        "target/debug/barbacane-control",
        "target/release/barbacane-control",
        "../target/debug/barbacane-control",
        "../target/release/barbacane-control",
        "../../target/debug/barbacane-control",
        "../../target/release/barbacane-control",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| Path::new(p).exists())
}

/// Grab an OS-assigned free TCP port.
fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

const ADMIN_TOKEN: &str = "test-admin-token-do-not-use-in-prod";

/// Every mutating control-plane route must reject an unauthenticated request
/// with 401. We exercise a representative set spanning projects, specs,
/// plugins, artifacts, compilations, and api-keys.
#[tokio::test]
async fn mutating_routes_require_admin_token() {
    // EXPECTED TO FAIL until BARB-SEC-001 is fixed (require_admin not yet wired
    // into create_router; mutating routes are currently reachable unauthenticated).
    let Some(cp) = TestControlPlane::boot(Some(ADMIN_TOKEN)).await else {
        eprintln!("skip: control plane unavailable (need DATABASE_URL + built barbacane-control)");
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
    // EXPECTED TO FAIL until BARB-SEC-001 is fixed.
    let Some(cp) = TestControlPlane::boot(Some(ADMIN_TOKEN)).await else {
        eprintln!("skip: control plane unavailable (need DATABASE_URL + built barbacane-control)");
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
    let Some(cp) = TestControlPlane::boot(Some(ADMIN_TOKEN)).await else {
        eprintln!("skip: control plane unavailable (need DATABASE_URL + built barbacane-control)");
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
    // EXPECTED TO FAIL until BARB-SEC-001 is fixed (serve currently starts with
    // no token configured).
    //
    // We assert via `boot(None)` returning None *because the process exited*.
    // To distinguish "exited (good)" from "no DB / no binary (skip)", we first
    // require the prerequisites to exist, then assert boot fails.
    if std::env::var("DATABASE_URL").is_err() || find_control_binary().is_none() {
        eprintln!("skip: control plane unavailable (need DATABASE_URL + built barbacane-control)");
        return;
    }

    let booted = TestControlPlane::boot(None).await;
    assert!(
        booted.is_none(),
        "control plane must fail-closed and refuse to serve without BARBACANE_CONTROL_ADMIN_TOKEN"
    );
}

/// Project-scoped IDOR: a credential scoped to project A must not be able to
/// read project B's spec / artifact / api-keys.
///
/// NOTE: per-project credentials do not exist yet in the model (only a single
/// shared admin token is planned for the first hardening pass), and the global
/// `/specs/{id}` / `/artifacts/{id}` routes carry no project scoping at all.
/// This test documents the intended end-state and is parked behind `#[ignore]`
/// until the ownership model lands.
#[tokio::test]
#[ignore = "BLOCKED: per-project credentials + ownership checks on global /specs/{id} and /artifacts/{id} not implemented (BARB-SEC-001 phase 2)"]
async fn project_scoped_idor_is_blocked() {
    // EXPECTED TO FAIL until BARB-SEC-001 (phase 2) is fixed.
    let Some(cp) = TestControlPlane::boot(Some(ADMIN_TOKEN)).await else {
        eprintln!("skip: control plane unavailable");
        return;
    };

    // Intent: create project A and project B, mint a project-A-scoped token,
    // upload a spec under B, then assert A's token gets 403/404 reading B's
    // resources. Wiring this requires the per-project token API, which is not
    // yet present, hence #[ignore]. The shared-token boot above keeps the test
    // compiling against the current binary surface.
    let _ = cp.url("/projects");
}
