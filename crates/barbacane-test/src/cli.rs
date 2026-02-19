//! CLI regression tests for the `barbacane` binary.
//!
//! These tests invoke the binary as a subprocess to catch regressions in flag
//! names, exit codes, and output formats — things the Rust API tests can't catch.
//!
//! Run with: `cargo test -p barbacane-test`
//! Requires the `barbacane` binary to be built first (`cargo build -p barbacane`).

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns an assert_cmd Command wrapping the `barbacane` binary.
fn barbacane() -> Command {
    // cargo_bin is deprecated for custom build-dir setups; fine for standard workspace use.
    #[allow(deprecated)]
    Command::cargo_bin("barbacane")
        .expect("barbacane binary not found — run `cargo build -p barbacane` first")
}

/// Absolute path to the shared test fixtures directory.
fn fixtures() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../crates/barbacane-test
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .join("tests/fixtures")
}

// ---------------------------------------------------------------------------
// barbacane validate
// ---------------------------------------------------------------------------

#[test]
fn validate_valid_spec_exits_zero() {
    barbacane()
        .args(["validate", "--spec"])
        .arg(fixtures().join("minimal.yaml"))
        .assert()
        .success();
}

#[test]
fn validate_invalid_spec_exits_one() {
    barbacane()
        .args(["validate", "--spec"])
        .arg(fixtures().join("invalid-parse-error.yaml"))
        .assert()
        .failure()
        .code(1);
}

#[test]
fn validate_missing_file_exits_one() {
    barbacane()
        .args(["validate", "--spec", "this-file-does-not-exist.yaml"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn validate_routing_conflict_exits_one() {
    barbacane()
        .args(["validate", "--spec"])
        .arg(fixtures().join("invalid-routing-conflict/spec-a.yaml"))
        .arg("--spec")
        .arg(fixtures().join("invalid-routing-conflict/spec-b.yaml"))
        .assert()
        .failure()
        .code(1)
        .stderr(contains("E1010"));
}

#[test]
fn validate_json_format_outputs_valid_json() {
    let output = barbacane()
        .args(["validate", "--spec"])
        .arg(fixtures().join("minimal.yaml"))
        .args(["--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout should be valid UTF-8");
    let v: serde_json::Value =
        serde_json::from_str(&s).expect("--format json output should be valid JSON");
    assert!(
        v.get("results").is_some(),
        "JSON output missing 'results' key"
    );
    assert!(
        v.get("summary").is_some(),
        "JSON output missing 'summary' key"
    );
}

#[test]
fn validate_json_format_invalid_spec_exits_one_with_json() {
    let output = barbacane()
        .args(["validate", "--spec"])
        .arg(fixtures().join("invalid-parse-error.yaml"))
        .args(["--format", "json"])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout should be valid UTF-8");
    let v: serde_json::Value =
        serde_json::from_str(&s).expect("--format json output should be valid JSON even on error");
    let results = v["results"].as_array().expect("results should be an array");
    assert!(!results.is_empty());
    assert_eq!(results[0]["valid"], false);
}

// ---------------------------------------------------------------------------
// barbacane compile
// ---------------------------------------------------------------------------

#[test]
fn compile_missing_manifest_flag_exits_two() {
    // --manifest is required; clap returns exit code 2 for missing required args
    let tmp = TempDir::new().expect("temp dir");
    barbacane()
        .args(["compile", "--spec"])
        .arg(fixtures().join("minimal.yaml"))
        .arg("--output")
        .arg(tmp.path().join("out.bca"))
        .assert()
        .failure()
        .code(2);
}

#[test]
fn compile_missing_output_flag_exits_two() {
    let tmp = TempDir::new().expect("temp dir");
    let manifest = tmp.path().join("barbacane.yaml");
    std::fs::write(&manifest, "plugins: {}\n").expect("write manifest");

    barbacane()
        .args(["compile", "--spec"])
        .arg(fixtures().join("minimal.yaml"))
        .arg("--manifest")
        .arg(&manifest)
        .assert()
        .failure()
        .code(2);
}

#[test]
fn compile_nonexistent_spec_exits_one() {
    let tmp = TempDir::new().expect("temp dir");
    let manifest = tmp.path().join("barbacane.yaml");
    std::fs::write(&manifest, "plugins: {}\n").expect("write manifest");

    barbacane()
        .args(["compile", "--spec", "nonexistent.yaml", "--manifest"])
        .arg(&manifest)
        .arg("--output")
        .arg(tmp.path().join("out.bca"))
        .assert()
        .failure()
        .code(1);
}

#[test]
fn compile_invalid_spec_exits_one() {
    let tmp = TempDir::new().expect("temp dir");
    let manifest = tmp.path().join("barbacane.yaml");
    std::fs::write(&manifest, "plugins: {}\n").expect("write manifest");

    barbacane()
        .args(["compile", "--spec"])
        .arg(fixtures().join("invalid-parse-error.yaml"))
        .arg("--manifest")
        .arg(&manifest)
        .arg("--output")
        .arg(tmp.path().join("out.bca"))
        .assert()
        .failure()
        .code(1);
}

#[test]
fn compile_undeclared_plugin_exits_one() {
    // minimal.yaml uses the `mock` plugin but the manifest declares no plugins,
    // so compilation fails with an undeclared-plugin error (exit 1).
    let tmp = TempDir::new().expect("temp dir");
    let manifest = tmp.path().join("barbacane.yaml");
    std::fs::write(&manifest, "plugins: {}\n").expect("write manifest");

    barbacane()
        .args(["compile", "--spec"])
        .arg(fixtures().join("minimal.yaml"))
        .arg("--manifest")
        .arg(&manifest)
        .arg("--output")
        .arg(tmp.path().join("out.bca"))
        .assert()
        .failure()
        .code(1);
}

// ---------------------------------------------------------------------------
// barbacane init
// ---------------------------------------------------------------------------

#[test]
fn init_creates_project_directory() {
    let tmp = TempDir::new().expect("temp dir");
    let project = tmp.path().join("my-api");

    barbacane().args(["init"]).arg(&project).assert().success();

    assert!(
        project.join("barbacane.yaml").exists(),
        "barbacane.yaml missing"
    );
    assert!(project.join("api.yaml").exists(), "api.yaml missing");
    assert!(project.join("plugins").is_dir(), "plugins/ dir missing");
    assert!(project.join(".gitignore").exists(), ".gitignore missing");
}

#[test]
fn init_in_current_directory() {
    let tmp = TempDir::new().expect("temp dir");

    barbacane()
        .current_dir(tmp.path())
        .args(["init", "."])
        .assert()
        .success();

    assert!(tmp.path().join("barbacane.yaml").exists());
    assert!(tmp.path().join("api.yaml").exists());
}

#[test]
fn init_minimal_template() {
    let tmp = TempDir::new().expect("temp dir");
    let project = tmp.path().join("my-api");

    barbacane()
        .args(["init"])
        .arg(&project)
        .args(["--template", "minimal"])
        .assert()
        .success();

    assert!(project.join("barbacane.yaml").exists());
    assert!(project.join("api.yaml").exists());
}

#[test]
fn init_short_template_flag() {
    let tmp = TempDir::new().expect("temp dir");
    let project = tmp.path().join("my-api");

    barbacane()
        .args(["init"])
        .arg(&project)
        .args(["-t", "minimal"])
        .assert()
        .success();

    assert!(project.join("barbacane.yaml").exists());
}

#[test]
fn init_fails_on_existing_nonempty_directory() {
    let tmp = TempDir::new().expect("temp dir");
    let project = tmp.path().join("existing");
    std::fs::create_dir(&project).expect("create dir");
    std::fs::write(project.join("some-file.txt"), "content").expect("write file");

    barbacane()
        .args(["init"])
        .arg(&project)
        .assert()
        .failure()
        .code(1);
}
