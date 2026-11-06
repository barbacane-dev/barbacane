//! A production data plane loads only artifacts signed by the key it pins.
//!
//! `barbacane serve` without `--dev` refuses to start unless
//! `BARBACANE_TRUSTED_PUBKEY` pins a key, or `--allow-unsigned` is passed. A
//! pinned key always requires its signature. Keys come from
//! `barbacane key generate`.
//!
//! Each test runs the real binary against an artifact compiled from
//! `minimal.yaml`, signed or not.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use barbacane_compiler::{compile_with_manifest, CompileOptions, ProjectManifest};
use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT, LISTEN_HOST, STARTUP_TIMEOUT};
use tempfile::TempDir;

use crate::fixtures_dir;

fn barbacane_binary() -> PathBuf {
    [
        "target/debug/barbacane",
        "../target/debug/barbacane",
        "../../target/debug/barbacane",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
    .expect("the barbacane binary is built (cargo build -p barbacane)")
}

/// Generate a key with `barbacane key generate`; returns its path and the
/// public key the command printed.
fn generate_key(dir: &Path, name: &str) -> (PathBuf, String) {
    let path = dir.join(name);
    let output = Command::new(barbacane_binary())
        .args(["key", "generate", "--out"])
        .arg(&path)
        .output()
        .expect("run barbacane key generate");
    assert!(output.status.success(), "{output:?}");
    let public = String::from_utf8(output.stdout).unwrap().trim().to_string();
    (path, public)
}

/// Compile `minimal.yaml`, signed with `signing_key` when given.
fn compile_minimal(dir: &Path, signing_key: Option<&Path>) -> PathBuf {
    let fixtures = fixtures_dir();
    let manifest =
        ProjectManifest::load(&fixtures.join("barbacane.yaml")).expect("fixtures manifest");
    let artifact = dir.join(format!(
        "{}.bca",
        if signing_key.is_some() {
            "signed"
        } else {
            "unsigned"
        }
    ));
    let options = CompileOptions {
        allow_plaintext: true,
        signing_key: signing_key.map(Path::to_path_buf),
        ..CompileOptions::default()
    };
    compile_with_manifest(
        &[fixtures.join("minimal.yaml").as_path()],
        &manifest,
        &fixtures,
        &artifact,
        &options,
    )
    .expect("compile minimal.yaml");
    artifact
}

/// How a `serve` attempt ended.
struct Outcome {
    healthy: bool,
    log: String,
}

/// Run `barbacane serve` (without `--dev` unless `args` adds it) and report
/// whether the gateway became healthy, with what it logged.
async fn serve(artifact: &Path, pinned: Option<&str>, args: &[&str]) -> Outcome {
    let mut cmd = Command::new(barbacane_binary());
    cmd.arg("serve")
        .arg("--artifact")
        .arg(artifact)
        .arg("--listen")
        .arg(LISTEN_ANY_PORT)
        .arg("--admin-bind")
        .arg(LISTEN_ANY_PORT)
        .arg("--allow-plaintext-upstream")
        .args(args)
        .env("BARBACANE_TRUSTED_PUBKEY", pinned.unwrap_or(""));
    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn barbacane serve");

    let mut healthy = false;
    if let Some(port) = process.announced_port("listening on", STARTUP_TIMEOUT) {
        let health = format!("http://{LISTEN_HOST}:{port}/__barbacane/health");
        let client = reqwest::Client::new();
        for _ in 0..50 {
            if client
                .get(&health)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                healthy = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    // Give a refusing process time to write its reason before reading it.
    for _ in 0..50 {
        if healthy || process.has_exited() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let log = process.log().text();
    process.kill_and_reap();
    Outcome { healthy, log }
}

#[tokio::test]
async fn an_unsigned_artifact_does_not_start_without_a_pinned_key() {
    let dir = TempDir::new().unwrap();
    let artifact = compile_minimal(dir.path(), None);
    let outcome = serve(&artifact, None, &[]).await;
    assert!(!outcome.healthy, "served an unsigned artifact");
    assert!(
        outcome.log.contains("BARBACANE_TRUSTED_PUBKEY")
            && outcome.log.contains("--allow-unsigned"),
        "the refusal names both ways forward: {}",
        outcome.log
    );
}

#[tokio::test]
async fn allow_unsigned_loads_an_unsigned_artifact_and_says_so() {
    let dir = TempDir::new().unwrap();
    let artifact = compile_minimal(dir.path(), None);
    let outcome = serve(&artifact, None, &["--allow-unsigned"]).await;
    assert!(outcome.healthy, "{}", outcome.log);
    assert!(
        outcome.log.contains("artifact signature not checked"),
        "{}",
        outcome.log
    );
}

#[tokio::test]
async fn dev_mode_loads_an_unsigned_artifact() {
    let dir = TempDir::new().unwrap();
    let artifact = compile_minimal(dir.path(), None);
    let outcome = serve(&artifact, None, &["--dev"]).await;
    assert!(outcome.healthy, "{}", outcome.log);
}

#[tokio::test]
async fn a_signed_artifact_starts_with_its_key_pinned() {
    let dir = TempDir::new().unwrap();
    let (key, public) = generate_key(dir.path(), "signing.pk8");
    let artifact = compile_minimal(dir.path(), Some(&key));
    let outcome = serve(&artifact, Some(&public), &[]).await;
    assert!(outcome.healthy, "{}", outcome.log);
    assert!(
        outcome.log.contains("signature verified"),
        "{}",
        outcome.log
    );
}

#[tokio::test]
async fn a_pinned_key_requires_its_signature_whatever_the_flags() {
    let dir = TempDir::new().unwrap();
    let (key, _) = generate_key(dir.path(), "signing.pk8");
    let (_, other_public) = generate_key(dir.path(), "other.pk8");
    let signed = compile_minimal(dir.path(), Some(&key));
    let unsigned = compile_minimal(dir.path(), None);

    for flags in [&[][..], &["--allow-unsigned"], &["--dev"]] {
        let outcome = serve(&signed, Some(&other_public), flags).await;
        assert!(
            !outcome.healthy,
            "another key's signature accepted with {flags:?}"
        );
        assert!(
            outcome.log.contains("not signed by the key"),
            "{}",
            outcome.log
        );

        let outcome = serve(&unsigned, Some(&other_public), flags).await;
        assert!(
            !outcome.healthy,
            "an unsigned artifact accepted with {flags:?}"
        );
        assert!(outcome.log.contains("unsigned"), "{}", outcome.log);
    }
}

#[tokio::test]
async fn a_malformed_pinned_key_does_not_start() {
    let dir = TempDir::new().unwrap();
    let artifact = compile_minimal(dir.path(), None);
    let outcome = serve(&artifact, Some("not-a-key"), &["--allow-unsigned"]).await;
    assert!(!outcome.healthy);
    assert!(
        outcome.log.contains("not an Ed25519 public key"),
        "{}",
        outcome.log
    );
}

#[test]
fn key_generate_writes_a_private_key_once_and_public_prints_its_key() {
    let dir = TempDir::new().unwrap();
    let (key, printed) = generate_key(dir.path(), "signing.pk8");
    assert_eq!(printed.len(), 64, "{printed}");

    let output = Command::new(barbacane_binary())
        .args(["key", "public", "--key"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), printed);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "only the owner reads the key");
    }

    let again = Command::new(barbacane_binary())
        .args(["key", "generate", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(!again.status.success(), "an existing key was overwritten");
    let output = Command::new(barbacane_binary())
        .args(["key", "public", "--key"])
        .arg(&key)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), printed);
}

/// `barbacane compile`, like the control plane, signs with the key
/// `BARBACANE_SIGNING_KEY` names.
#[tokio::test]
async fn compile_signs_with_the_key_the_environment_names() {
    let dir = TempDir::new().unwrap();
    let (key, public) = generate_key(dir.path(), "signing.pk8");
    let fixtures = fixtures_dir();
    let artifact = dir.path().join("cli.bca");
    let output = Command::new(barbacane_binary())
        .arg("compile")
        .arg("--spec")
        .arg(fixtures.join("minimal.yaml"))
        .arg("--manifest")
        .arg(fixtures.join("barbacane.yaml"))
        .arg("--output")
        .arg(&artifact)
        .arg("--allow-plaintext")
        .env("BARBACANE_SIGNING_KEY", &key)
        .output()
        .expect("run barbacane compile");
    assert!(output.status.success(), "{output:?}");

    let outcome = serve(&artifact, Some(&public), &[]).await;
    assert!(outcome.healthy, "{}", outcome.log);
}

/// Compose files often pass the variable empty; that means no key.
#[test]
fn an_empty_signing_key_variable_compiles_unsigned() {
    let dir = TempDir::new().unwrap();
    let fixtures = fixtures_dir();
    let artifact = dir.path().join("cli.bca");
    let output = Command::new(barbacane_binary())
        .arg("compile")
        .arg("--spec")
        .arg(fixtures.join("minimal.yaml"))
        .arg("--manifest")
        .arg(fixtures.join("barbacane.yaml"))
        .arg("--output")
        .arg(&artifact)
        .arg("--allow-plaintext")
        .env("BARBACANE_SIGNING_KEY", "")
        .output()
        .expect("run barbacane compile");
    assert!(output.status.success(), "{output:?}");
    let manifest = barbacane_compiler::load_manifest(&artifact).unwrap();
    assert!(manifest.signature.is_none());
}
