//! `barbacane check --artifact`: the load-time checks of `serve`, and each
//! plugin's `init` with its route configurations, without serving.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> PathBuf {
    barbacane_test::gateway::find_barbacane_binary()
        .map(PathBuf::from)
        .expect("barbacane binary not found; run `cargo build -p barbacane` first")
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the repository root is two levels above the crate")
        .to_path_buf()
}

/// A built plugin, or a failed test: a check that skips reads as a pass.
fn plugin(relative: &str) -> PathBuf {
    let path = repo().join(relative);
    assert!(
        path.exists(),
        "{} is not built; run `make plugins` (the test crate's build script builds the fixture plugins)",
        path.display()
    );
    path
}

/// Compile one operation dispatched to `plugin_name` with `config` (YAML).
/// With `signing_key`, the artifact is signed.
fn compile(
    dir: &Path,
    plugin_name: &str,
    wasm: &Path,
    config: &str,
    signing_key: Option<&Path>,
) -> PathBuf {
    let manifest = dir.join("barbacane.yaml");
    std::fs::write(
        &manifest,
        format!("plugins:\n  {plugin_name}:\n    path: {}\n", wasm.display()),
    )
    .expect("write the manifest");
    let spec = dir.join("api.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: "3.0.3"
info: {{ title: check, version: "1.0.0" }}
paths:
  /echo:
    post:
      operationId: echo
      x-barbacane-dispatch: {{ name: {plugin_name}, config: {config} }}
      responses: {{ "200": {{ description: ok }} }}
"#
        ),
    )
    .expect("write the spec");
    let out = dir.join("api.bca");
    let mut cmd = Command::new(binary());
    cmd.args(["compile", "-s"])
        .arg(&spec)
        .arg("-m")
        .arg(&manifest)
        .arg("-o")
        .arg(&out)
        .env_remove("BARBACANE_SIGNING_KEY");
    if let Some(key) = signing_key {
        cmd.env("BARBACANE_SIGNING_KEY", key);
    }
    let output = cmd.output().expect("run the compiler");
    assert!(
        output.status.success(),
        "compiling the fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    out
}

fn mock_artifact(dir: &Path, signing_key: Option<&Path>) -> PathBuf {
    compile(
        dir,
        "mock",
        &plugin("plugins/mock/mock.wasm"),
        r#"{ status: 200, body: "env://BARBACANE_CHECK_TEST_BODY" }"#,
        signing_key,
    )
}

/// A new signing key in `dir` and its public key.
fn signing_key(dir: &Path, name: &str) -> (PathBuf, String) {
    let key = dir.join(name);
    let generate = Command::new(binary())
        .args(["key", "generate", "--out"])
        .arg(&key)
        .output()
        .expect("generate a key");
    assert!(generate.status.success(), "{generate:?}");
    let public = Command::new(binary())
        .args(["key", "public", "--key"])
        .arg(&key)
        .output()
        .expect("print the public key");
    assert!(public.status.success(), "{public:?}");
    (
        key,
        String::from_utf8_lossy(&public.stdout).trim().to_string(),
    )
}

/// `barbacane check --artifact <artifact> <args>` with only `envs` set of the
/// variables `check` reads.
fn check(artifact: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    Command::new(binary())
        .args(["check", "--artifact"])
        .arg(artifact)
        .args(args)
        .env_remove("BARBACANE_TRUSTED_PUBKEY")
        .env_remove("BARBACANE_CHECK_TEST_BODY")
        .envs(envs.iter().copied())
        .output()
        .expect("run barbacane check")
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const SECRET: (&str, &str) = ("BARBACANE_CHECK_TEST_BODY", "pong");

#[test]
fn an_artifact_signed_by_the_pinned_key_passes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (key, public) = signing_key(dir.path(), "signing.der");
    let artifact = mock_artifact(dir.path(), Some(&key));

    let output = check(
        &artifact,
        &[],
        &[("BARBACANE_TRUSTED_PUBKEY", &public), SECRET],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("signature verified"), "{stdout}");
    assert!(
        stdout.contains("1 route(s), 1 plugin(s), 1 plugin configuration(s)"),
        "{stdout}"
    );
    assert!(!stderr.contains("WARN"), "{stderr}");
}

#[test]
fn an_artifact_signed_by_another_key_fails() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (key, _) = signing_key(dir.path(), "signing.der");
    let (_, other) = signing_key(dir.path(), "other.der");
    let artifact = mock_artifact(dir.path(), Some(&key));

    let output = check(
        &artifact,
        &[],
        &[("BARBACANE_TRUSTED_PUBKEY", &other), SECRET],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stderr.contains("signature check failed"), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
}

#[test]
fn an_unsigned_artifact_passes_only_with_allow_unsigned() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = mock_artifact(dir.path(), None);

    let refused = check(&artifact, &[], &[SECRET]);
    let (_, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("no trusted signing key"), "{stderr}");

    let allowed = check(&artifact, &["--allow-unsigned"], &[SECRET]);
    let (stdout, stderr) = text(&allowed);
    assert_eq!(allowed.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("signature not checked"), "{stdout}");
    assert!(
        stderr.contains("WARN") && stderr.contains("artifact signature not checked"),
        "the load warning reaches the operator: {stderr}"
    );
    assert!(
        !stderr.contains('\u{1b}'),
        "no colour codes when stderr is not a terminal: {stderr:?}"
    );
}

#[test]
fn an_unresolved_secret_exits_13_as_serve_does() {
    let dir = tempfile::tempdir().expect("temp dir");
    let artifact = mock_artifact(dir.path(), None);

    let output = check(&artifact, &["--allow-unsigned"], &[]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(13), "{stdout}{stderr}");
    assert!(
        stderr.contains("failed to resolve secrets")
            && stderr.contains("BARBACANE_CHECK_TEST_BODY"),
        "{stderr}"
    );
}

/// A configuration the compiler accepts but the plugin cannot read fails at
/// the plugin's `init`, which `serve` reaches only on the first request.
#[test]
fn a_plugin_that_cannot_read_its_configuration_fails() {
    let dir = tempfile::tempdir().expect("temp dir");
    let echo = plugin(
        "tests/fixture-plugins/body-echo/target/wasm32-unknown-unknown/release/body_echo.wasm",
    );

    // body-echo has no config schema; its config is an empty struct, so a
    // string is not one.
    let artifact = compile(dir.path(), "body-echo", &echo, r#""text""#, None);
    let output = check(&artifact, &["--allow-unsigned"], &[]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains(
            "plugin 'body-echo' does not initialise with its configuration on POST /echo"
        ),
        "names the plugin and the operation: {stderr}"
    );

    let artifact = compile(dir.path(), "body-echo", &echo, "{ any: 1 }", None);
    let output = check(&artifact, &["--allow-unsigned"], &[]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
}

#[test]
fn a_missing_or_unreadable_artifact_fails() {
    let dir = tempfile::tempdir().expect("temp dir");

    let missing = check(&dir.path().join("absent.bca"), &["--allow-unsigned"], &[]);
    let (_, stderr) = text(&missing);
    assert_eq!(missing.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("artifact not found"), "{stderr}");

    let garbage = dir.path().join("garbage.bca");
    std::fs::write(&garbage, b"not an artifact").expect("write");
    let unreadable = check(&garbage, &["--allow-unsigned"], &[]);
    let (_, stderr) = text(&unreadable);
    assert_eq!(unreadable.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("failed to read artifact"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
