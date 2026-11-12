//! Connected mode end to end: a data plane registered with the control plane
//! receives the artifact `deploy` publishes, downloads it and hot-reloads.
//!
//! Needs PostgreSQL in `DATABASE_URL` and both binaries built; skips
//! otherwise.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use barbacane_test::gateway::{
    LoggedChild, TestGateway, LISTEN_ANY_PORT, LISTEN_HOST, STARTUP_TIMEOUT,
};
use serde_json::Value;

const ADMIN_TOKEN: &str = "connected-mode-admin-token";

struct ControlPlane {
    process: LoggedChild,
    url: String,
    port: u16,
}

impl Drop for ControlPlane {
    fn drop(&mut self) {
        self.process.kill_and_reap();
    }
}

fn binary(name: &str) -> Option<PathBuf> {
    ["target/debug", "../target/debug", "../../target/debug"]
        .iter()
        .map(|dir| PathBuf::from(dir).join(name))
        .find(|p| p.exists())
}

impl ControlPlane {
    async fn boot() -> Option<Self> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let mut cmd = Command::new(binary("barbacane-control")?);
        cmd.args(["serve", "--listen", LISTEN_ANY_PORT, "--database-url"])
            .arg(&database_url)
            .env("BARBACANE_CONTROL_ADMIN_TOKEN", ADMIN_TOKEN);
        let mut process = LoggedChild::spawn(&mut cmd).ok()?;
        let port = process.announced_port("Control plane listening on", STARTUP_TIMEOUT)?;
        let url = format!("http://{LISTEN_HOST}:{port}");
        let client = reqwest::Client::new();
        for _ in 0..100 {
            if client
                .get(format!("{url}/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Some(Self { process, url, port });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    /// Run `barbacane control <args>` against this control plane, with
    /// `stdin` as the request body when given, and return its JSON output.
    fn cli(&self, args: &[&str], stdin: Option<&str>) -> Value {
        let mut child = Command::new(binary("barbacane").expect("barbacane is built"))
            .arg("control")
            .args(args)
            .args(["-o", "json"])
            .env("BARBACANE_CONTROL_URL", &self.url)
            .env("BARBACANE_CONTROL_ADMIN_TOKEN", ADMIN_TOKEN)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run barbacane control");
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        drop(child.stdin.take());
        let output = child.wait_with_output().expect("wait");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{args:?}: {stderr}");
        if stdout.trim().is_empty() {
            return Value::Null;
        }
        serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}\n{stderr}"))
    }
}

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
        .to_str()
        .unwrap()
        .to_string()
}

/// `deploy` publishes the project's latest artifact; a data plane connected
/// with that project's API key downloads it and serves it.
#[tokio::test]
async fn a_deployed_artifact_reaches_a_connected_data_plane() {
    let Some(cp) = ControlPlane::boot().await else {
        eprintln!("skip: no database or binaries");
        return;
    };
    let version = format!("1.0.{}", uuid::Uuid::new_v4().as_u128() % 1_000_000);
    let mock = fixture("../../plugins/mock/mock.wasm");
    cp.cli(
        &[
            "plugins",
            "register",
            "--name",
            "mock",
            "--version",
            &version,
            "--type",
            "dispatcher",
            "--file",
            &mock,
        ],
        None,
    );
    let project = cp.cli(
        &["projects", "create", "--file", "-"],
        Some(&format!(
            r#"{{"name": "connected-{}"}}"#,
            uuid::Uuid::new_v4().simple()
        )),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    cp.cli(
        &[
            "project-plugins",
            "add",
            "--project-id",
            &project,
            "--file",
            "-",
        ],
        Some(&format!(
            r#"{{"plugin_name": "mock", "plugin_version": "{version}"}}"#
        )),
    );
    let key = cp.cli(
        &[
            "api-keys",
            "create",
            "--project-id",
            &project,
            "--file",
            "-",
        ],
        Some(r#"{"name": "data-plane"}"#),
    )["key"]
        .as_str()
        .unwrap()
        .to_string();

    // The data plane starts on the first artifact, connected to the control plane.
    let ws_url = format!("ws://{LISTEN_HOST}:{}/ws/data-plane", cp.port);
    let gateway = TestGateway::from_spec_with_args(
        &fixture("minimal.yaml"),
        &[
            "--control-plane",
            &ws_url,
            "--project-id",
            &project,
            "--api-key",
            &key,
        ],
    )
    .await
    .expect("data plane starts");
    assert_eq!(gateway.get("/v2/status").await.unwrap().status(), 404);

    // Push a second artifact and deploy it.
    let compilation = cp.cli(
        &[
            "push",
            "--project",
            &project,
            &fixture("connected-mode-v2.yaml"),
        ],
        None,
    );
    assert_eq!(compilation["status"], "succeeded", "{compilation}");
    let deployed = cp.cli(&["deploy", "--project", &project], None);
    assert_eq!(deployed["data_planes_notified"], 1, "{deployed}");

    let outcome = gateway
        .log()
        .wait_for_line("barbacane: hot-reload ", Duration::from_secs(60))
        .unwrap_or_else(|| panic!("no hot-reload outcome:\n{}", gateway.log().text()));
    assert!(outcome.contains("hot-reload successful"), "{outcome}");
    // The same keep-alive connection as the earlier 404 observes the reload.
    let resp = gateway.get("/v2/status").await.unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["artifact"],
        "v2"
    );
}
