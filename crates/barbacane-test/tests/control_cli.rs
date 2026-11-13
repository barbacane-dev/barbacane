//! `barbacane control` against a running control plane.
//!
//! Needs PostgreSQL in `DATABASE_URL` and both binaries built; skips
//! otherwise, as the authorization tests do.
//!
//! Run with: `cargo test -p barbacane-test --test control_cli`

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT, LISTEN_HOST, STARTUP_TIMEOUT};
use serde_json::Value;

const ADMIN_TOKEN: &str = "test-admin-token-control-cli-32-bytes-long";

struct ControlPlane {
    process: LoggedChild,
    url: String,
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
                return Some(Self { process, url });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    /// Run `barbacane control <args>` against this control plane.
    fn cli(&self, args: &[&str]) -> Outcome {
        self.cli_with(args, None, ADMIN_TOKEN)
    }

    fn cli_with(&self, args: &[&str], stdin: Option<&str>, token: &str) -> Outcome {
        run_cli(&self.url, token, args, stdin)
    }
}

struct Outcome {
    success: bool,
    stdout: String,
    stderr: String,
}

impl Outcome {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}\n{}", self.stdout, self.stderr))
    }
}

fn run_cli(url: &str, token: &str, args: &[&str], stdin: Option<&str>) -> Outcome {
    let mut child = Command::new(binary("barbacane").expect("barbacane is built"))
        .arg("control")
        .args(args)
        .env("BARBACANE_CONTROL_URL", url)
        .env("BARBACANE_CONTROL_ADMIN_TOKEN", token)
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
    Outcome {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn projects_are_created_listed_shown_and_deleted() {
    let Some(cp) = ControlPlane::boot().await else {
        eprintln!("skip: no database or binaries");
        return;
    };
    let name = unique("cli");
    let created = cp.cli_with(
        &["projects", "create", "--file", "-", "-o", "json"],
        Some(&format!(r#"{{"name": "{name}"}}"#)),
        ADMIN_TOKEN,
    );
    assert!(created.success, "{}", created.stderr);
    let id = created.json()["id"].as_str().unwrap().to_string();

    let listed = cp.cli(&["projects", "list", "-o", "json"]);
    assert!(listed
        .json()
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == name.as_str()));

    let shown = cp.cli(&["projects", "get", "--project-id", &id]);
    assert!(
        shown.success && shown.stdout.contains(&name),
        "{}",
        shown.stdout
    );

    let deleted = cp.cli(&["projects", "delete", "--project-id", &id]);
    assert!(deleted.success, "{}", deleted.stderr);
    assert!(deleted.stdout.is_empty());

    let gone = cp.cli(&["projects", "get", "--project-id", &id]);
    assert!(!gone.success);
    assert!(
        gone.stderr.contains("404 Not Found") && gone.stderr.contains(&id),
        "{}",
        gone.stderr
    );
}

#[tokio::test]
async fn a_rejected_token_and_an_unreachable_control_plane_are_reported() {
    let Some(cp) = ControlPlane::boot().await else {
        eprintln!("skip: no database or binaries");
        return;
    };
    let refused = cp.cli_with(&["projects", "list"], None, "not-the-token");
    assert!(!refused.success);
    assert!(refused.stderr.contains("401"), "{}", refused.stderr);

    let unreachable = run_cli(
        "http://127.0.0.1:1",
        ADMIN_TOKEN,
        &["projects", "list"],
        None,
    );
    assert!(!unreachable.success);
    assert!(
        unreachable
            .stderr
            .contains("cannot reach the control plane"),
        "{}",
        unreachable.stderr
    );
}

#[tokio::test]
async fn push_compiles_a_project_and_deploy_waits_for_its_data_planes() {
    let Some(cp) = ControlPlane::boot().await else {
        eprintln!("skip: no database or binaries");
        return;
    };
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mock = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/mock/mock.wasm");
    let version = format!("1.0.{}", rand_suffix());

    let registered = cp.cli(&[
        "plugins",
        "register",
        "--name",
        "mock",
        "--version",
        &version,
        "--type",
        "dispatcher",
        "--file",
        mock.to_str().unwrap(),
    ]);
    assert!(registered.success, "{}", registered.stderr);

    let created = cp.cli_with(
        &["projects", "create", "--file", "-", "-o", "json"],
        Some(&format!(r#"{{"name": "{}"}}"#, unique("push"))),
        ADMIN_TOKEN,
    );
    let project = created.json()["id"].as_str().unwrap().to_string();
    let enabled = cp.cli_with(
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
        ADMIN_TOKEN,
    );
    assert!(enabled.success, "{}", enabled.stderr);

    let spec = fixtures.join("minimal.yaml");
    let pushed = cp.cli(&[
        "push",
        "--project",
        &project,
        spec.to_str().unwrap(),
        "-o",
        "json",
    ]);
    assert!(pushed.success, "{}\n{}", pushed.stdout, pushed.stderr);
    assert!(pushed.stderr.contains("uploaded"), "{}", pushed.stderr);
    let compilation = pushed.json();
    assert_eq!(compilation["status"], "succeeded");
    let artifact = compilation["artifact_id"].as_str().unwrap().to_string();

    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("api.bca");
    let downloaded = cp.cli(&[
        "artifacts",
        "download",
        "--id",
        &artifact,
        "--out",
        file.to_str().unwrap(),
    ]);
    assert!(downloaded.success, "{}", downloaded.stderr);
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "a gzip archive");

    let deployed = cp.cli(&["deploy", "--project", &project, "--wait", "--timeout", "5"]);
    assert!(deployed.success, "{}", deployed.stderr);
    assert!(
        deployed.stderr.contains("0 data plane(s) run"),
        "{}",
        deployed.stderr
    );
}

#[tokio::test]
async fn a_failed_compilation_fails_the_push() {
    let Some(cp) = ControlPlane::boot().await else {
        eprintln!("skip: no database or binaries");
        return;
    };
    let created = cp.cli_with(
        &["projects", "create", "--file", "-", "-o", "json"],
        Some(&format!(r#"{{"name": "{}"}}"#, unique("fail"))),
        ADMIN_TOKEN,
    );
    let project = created.json()["id"].as_str().unwrap().to_string();
    // `mock` is not enabled in this project.
    let spec = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.yaml");
    let pushed = cp.cli(&["push", "--project", &project, spec.to_str().unwrap()]);
    assert!(!pushed.success);
    assert!(
        pushed.stderr.contains("the compilation failed"),
        "{}",
        pushed.stderr
    );
    assert!(pushed.stdout.contains("failed"), "{}", pushed.stdout);
}

fn rand_suffix() -> u32 {
    uuid::Uuid::new_v4().as_u128() as u32 % 1_000_000
}
