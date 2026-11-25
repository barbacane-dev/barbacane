//! Connected mode end to end: a data plane registered with the control plane
//! receives the artifact `deploy` publishes, downloads it and hot-reloads.
//!
//! Needs PostgreSQL in `DATABASE_URL` and both binaries built; a local run
//! skips without them and a CI run fails (`barbacane_test::require`).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use barbacane_test::gateway::{
    LoggedChild, TestGateway, LISTEN_ANY_PORT, LISTEN_HOST, STARTUP_TIMEOUT,
};
use serde_json::Value;

const ADMIN_TOKEN: &str = "connected-mode-admin-token-of-32-bytes-plus";

/// A control plane marks every data plane of the database offline when it
/// starts, so the tests here run their control planes one at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ControlPlane {
    process: LoggedChild,
    url: String,
    port: u16,
    /// Held until the control plane is gone.
    _turn: tokio::sync::MutexGuard<'static, ()>,
}

impl Drop for ControlPlane {
    fn drop(&mut self) {
        self.process.kill_and_reap();
    }
}

fn binary(name: &str) -> Option<PathBuf> {
    barbacane_test::find_workspace_binary(name)
}

impl ControlPlane {
    async fn boot() -> Option<Self> {
        Self::boot_with_env(&[]).await
    }

    /// A control plane with `env` set, such as `BARBACANE_SIGNING_KEY`.
    async fn boot_with_env(env: &[(&str, &std::ffi::OsStr)]) -> Option<Self> {
        let turn = ONE_AT_A_TIME.lock().await;
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let mut cmd = Command::new(binary("barbacane-control")?);
        cmd.args(["serve", "--listen", LISTEN_ANY_PORT, "--database-url"])
            .arg(&database_url)
            .env("BARBACANE_CONTROL_ADMIN_TOKEN", ADMIN_TOKEN)
            .envs(env.iter().copied());
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
                return Some(Self {
                    process,
                    url,
                    port,
                    _turn: turn,
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    /// Run `barbacane control <args>`, without input, and return its output
    /// whether it succeeds or not.
    fn cli_output(&self, args: &[&str]) -> std::process::Output {
        Command::new(barbacane_test::gateway::find_barbacane_binary().expect("barbacane is built"))
            .arg("control")
            .args(args)
            .env("BARBACANE_CONTROL_URL", &self.url)
            .env("BARBACANE_CONTROL_ADMIN_TOKEN", ADMIN_TOKEN)
            .output()
            .expect("run barbacane control")
    }

    /// Run `barbacane control <args>` against this control plane, with
    /// `stdin` as the request body when given, and return its JSON output.
    fn cli(&self, args: &[&str], stdin: Option<&str>) -> Value {
        let mut child = Command::new(
            barbacane_test::gateway::find_barbacane_binary().expect("barbacane is built"),
        )
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
    let Some(cp) = barbacane_test::required(
        ControlPlane::boot().await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with both binaries built",
    ) else {
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

/// A project with the mock plugin and a data-plane key: its id and the key.
fn project_with_key(cp: &ControlPlane) -> (String, String) {
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
    (project, key)
}

/// Compile `connected-mode-v2.yaml` in the project and deploy it; returns
/// the deployed artifact's id.
fn deploy_v2(cp: &ControlPlane, project: &str) -> String {
    let compilation = cp.cli(
        &[
            "push",
            "--project",
            project,
            &fixture("connected-mode-v2.yaml"),
        ],
        None,
    );
    assert_eq!(compilation["status"], "succeeded", "{compilation}");
    cp.cli(&["deploy", "--project", project], None)["artifact_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// `barbacane serve` in connected mode with no `--artifact`, keeping
/// deployments in `artifact_dir`; returns the process and its base URL.
fn serve_without_artifact(
    cp: &ControlPlane,
    project: &str,
    key: &str,
    artifact_dir: &std::path::Path,
) -> (LoggedChild, String) {
    serve_connected(cp, project, key, artifact_dir, None, None)
}

/// `barbacane serve` in connected mode, from `artifact` when given, keeping
/// deployments in `artifact_dir`, and pinned to `pubkey` when given; returns
/// the process and its base URL.
fn serve_connected(
    cp: &ControlPlane,
    project: &str,
    key: &str,
    artifact_dir: &std::path::Path,
    artifact: Option<&std::path::Path>,
    pubkey: Option<&str>,
) -> (LoggedChild, String) {
    let ws_url = format!("ws://{LISTEN_HOST}:{}/ws/data-plane", cp.port);
    let mut cmd =
        Command::new(barbacane_test::gateway::find_barbacane_binary().expect("barbacane is built"));
    if let Some(artifact) = artifact {
        cmd.arg("serve").arg("--artifact").arg(artifact);
    } else {
        cmd.arg("serve");
    }
    if let Some(pubkey) = pubkey {
        cmd.env("BARBACANE_TRUSTED_PUBKEY", pubkey);
    }
    cmd.args(["--listen", LISTEN_ANY_PORT, "--admin-bind", "off", "--dev"])
        .args([
            "--control-plane",
            &ws_url,
            "--project-id",
            project,
            "--api-key",
            key,
        ])
        .arg("--artifact-dir")
        .arg(artifact_dir);
    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn the data plane");
    let port = process
        .announced_port("listening on", STARTUP_TIMEOUT)
        .unwrap_or_else(|| panic!("the data plane did not start:\n{}", process.log().text()));
    (process, format!("http://{LISTEN_HOST}:{port}"))
}

/// The artifact the project's first data plane registered with. A data plane
/// serves before its WebSocket registration lands, so this waits for it.
async fn registered_artifact(cp: &ControlPlane, project: &str) -> Value {
    for _ in 0..300 {
        let planes = cp.cli(&["data-planes", "list", "--project-id", project], None);
        if let Some(plane) = planes.as_array().and_then(|planes| planes.first()) {
            return plane["artifact_id"].clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no data plane of {project} registered");
}

/// A data plane started with no artifact fetches the project's deployment
/// from the control plane, serves it, and registers as running it; started
/// again, it serves the copy it kept, and is sent nothing.
#[tokio::test]
async fn a_data_plane_without_an_artifact_starts_from_the_deployment() {
    let Some(cp) = barbacane_test::required(
        ControlPlane::boot().await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with both binaries built",
    ) else {
        return;
    };
    let (project, key) = project_with_key(&cp);
    let deployed = deploy_v2(&cp, &project);
    let artifact_dir = tempfile::TempDir::new().unwrap();

    let (mut process, url) = serve_without_artifact(&cp, &project, &key, artifact_dir.path());
    let resp = reqwest::get(format!("{url}/v2/status")).await.unwrap();
    assert_eq!(resp.status(), 200, "{}", process.log().text());
    assert_eq!(registered_artifact(&cp, &project).await, deployed.as_str());
    process.kill_and_reap();

    let (mut again, url) = serve_without_artifact(&cp, &project, &key, artifact_dir.path());
    let resp = reqwest::get(format!("{url}/v2/status")).await.unwrap();
    assert_eq!(resp.status(), 200);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !again.log().contains("new artifact available"),
        "{}",
        again.log().text()
    );
    again.kill_and_reap();
}

/// A data plane started on another artifact after a deploy, a new replica or
/// one restarted from its image, is sent the deployment and loads it.
#[tokio::test]
async fn a_data_plane_registering_after_a_deploy_is_sent_it() {
    let Some(cp) = barbacane_test::required(
        ControlPlane::boot().await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with both binaries built",
    ) else {
        return;
    };
    let (project, key) = project_with_key(&cp);
    deploy_v2(&cp, &project);

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
    let outcome = gateway
        .log()
        .wait_for_line("barbacane: hot-reload ", Duration::from_secs(60))
        .unwrap_or_else(|| panic!("no hot-reload outcome:\n{}", gateway.log().text()));
    assert!(outcome.contains("hot-reload successful"), "{outcome}");
    assert_eq!(gateway.get("/v2/status").await.unwrap().status(), 200);
}

/// Wait until a data plane of `project` is registered and online.
async fn registered(cp: &ControlPlane, project: &str) {
    for _ in 0..300 {
        let planes = cp.cli(&["data-planes", "list", "--project-id", project], None);
        if planes
            .as_array()
            .is_some_and(|planes| planes.iter().any(|p| p["status"] == "online"))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no data plane of {project} registered");
}

/// A new signing key in `dir`: its file and its public key.
fn signing_key(dir: &std::path::Path, name: &str) -> (PathBuf, String) {
    let path = dir.join(format!("{name}.pk8"));
    let output =
        Command::new(barbacane_test::gateway::find_barbacane_binary().expect("barbacane is built"))
            .args(["key", "generate", "--out"])
            .arg(&path)
            .output()
            .expect("barbacane key generate");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let public = String::from_utf8(output.stdout).unwrap().trim().to_string();
    (path, public)
}

/// `minimal.yaml` compiled into `dir`, signed with the key at `key`.
fn signed_minimal(dir: &std::path::Path, key: &std::path::Path) -> PathBuf {
    let artifact = dir.join("minimal.bca");
    let output =
        Command::new(barbacane_test::gateway::find_barbacane_binary().expect("barbacane is built"))
            .args(["compile", "--spec"])
            .arg(fixture("minimal.yaml"))
            .arg("--manifest")
            .arg(fixture("barbacane.yaml"))
            .arg("--output")
            .arg(&artifact)
            .env("BARBACANE_SIGNING_KEY", key)
            .output()
            .expect("barbacane compile");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    artifact
}

/// A control plane with `BARBACANE_SIGNING_KEY` signs what it compiles: a
/// data plane pinned to that key starts from the deployment and serves it.
#[tokio::test]
async fn a_signing_control_plane_deploys_to_a_pinned_data_plane() {
    let keys = tempfile::TempDir::new().unwrap();
    let (key_file, public) = signing_key(keys.path(), "control-plane");
    let Some(cp) = barbacane_test::required(
        ControlPlane::boot_with_env(&[("BARBACANE_SIGNING_KEY", key_file.as_os_str())]).await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with both binaries built",
    ) else {
        return;
    };
    let (project, key) = project_with_key(&cp);
    deploy_v2(&cp, &project);

    let artifact_dir = tempfile::TempDir::new().unwrap();
    let (mut process, url) = serve_connected(
        &cp,
        &project,
        &key,
        artifact_dir.path(),
        None,
        Some(&public),
    );
    let resp = reqwest::get(format!("{url}/v2/status")).await.unwrap();
    assert_eq!(resp.status(), 200, "{}", process.log().text());
    process.kill_and_reap();
}

/// A data plane pinned to a key refuses a deployment the key did not sign,
/// unsigned or signed by another, and keeps serving its artifact; the
/// control plane records why, and `deploy --wait` fails.
async fn a_pinned_data_plane_refuses(signing: Option<&std::path::Path>, what: &str) {
    let keys = tempfile::TempDir::new().unwrap();
    let (pinned_key, pinned) = signing_key(keys.path(), "pinned");
    let env: Vec<(&str, &std::ffi::OsStr)> = signing
        .map(|key| ("BARBACANE_SIGNING_KEY", key.as_os_str()))
        .into_iter()
        .collect();
    let Some(cp) = barbacane_test::required(
        ControlPlane::boot_with_env(&env).await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with both binaries built",
    ) else {
        return;
    };
    let (project, key) = project_with_key(&cp);
    let artifact_dir = tempfile::TempDir::new().unwrap();
    let running = signed_minimal(artifact_dir.path(), &pinned_key);
    let (mut process, url) = serve_connected(
        &cp,
        &project,
        &key,
        artifact_dir.path(),
        Some(&running),
        Some(&pinned),
    );
    assert_eq!(
        reqwest::get(format!("{url}/health"))
            .await
            .unwrap()
            .status(),
        200,
        "{}",
        process.log().text()
    );
    registered(&cp, &project).await;

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
    let waited = cp.cli_output(&["deploy", "--project", &project, "--wait", "--timeout", "15"]);
    let stderr = String::from_utf8_lossy(&waited.stderr);
    assert!(!waited.status.success(), "{what}: {stderr}");
    assert!(stderr.contains("do not run"), "{what}: {stderr}");

    let outcome = process
        .log()
        .wait_for_line("barbacane: hot-reload ", Duration::from_secs(30))
        .unwrap_or_else(|| panic!("{what}: no hot-reload outcome:\n{}", process.log().text()));
    assert!(outcome.contains("hot-reload failed"), "{what}: {outcome}");
    assert!(outcome.contains("signature"), "{what}: {outcome}");
    assert_eq!(
        reqwest::get(format!("{url}/health"))
            .await
            .unwrap()
            .status(),
        200,
        "{what}: the running artifact keeps serving"
    );
    assert_eq!(
        reqwest::get(format!("{url}/v2/status"))
            .await
            .unwrap()
            .status(),
        404,
        "{what}: the refused one is not served"
    );
    let data_planes = cp.cli(&["data-planes", "list", "--project-id", &project], None);
    assert!(
        data_planes[0]["deploy_error"]
            .as_str()
            .is_some_and(|e| e.contains("signature")),
        "{what}: {data_planes}"
    );
    process.kill_and_reap();
}

#[tokio::test]
async fn a_pinned_data_plane_refuses_an_unsigned_deployment() {
    a_pinned_data_plane_refuses(None, "unsigned").await;
}

#[tokio::test]
async fn a_pinned_data_plane_refuses_a_deployment_signed_by_another_key() {
    let keys = tempfile::TempDir::new().unwrap();
    let (other, _) = signing_key(keys.path(), "other");
    a_pinned_data_plane_refuses(Some(&other), "signed by another key").await;
}

/// `deploy --wait` returns once the connected data plane runs the artifact.
#[tokio::test]
async fn deploy_wait_returns_once_the_data_plane_runs_the_artifact() {
    let Some(cp) = barbacane_test::required(
        ControlPlane::boot().await,
        "a control plane on PostgreSQL (`DATABASE_URL`) with both binaries built",
    ) else {
        return;
    };
    let (project, key) = project_with_key(&cp);
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
    registered(&cp, &project).await;
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

    let waited = cp.cli_output(&["deploy", "--project", &project, "--wait", "--timeout", "60"]);
    let stderr = String::from_utf8_lossy(&waited.stderr);
    assert!(waited.status.success(), "{stderr}");
    assert!(stderr.contains("1 data plane(s) run"), "{stderr}");
    // Nothing more to wait for: the data plane serves the new artifact.
    assert_eq!(gateway.get("/v2/status").await.unwrap().status(), 200);
}
