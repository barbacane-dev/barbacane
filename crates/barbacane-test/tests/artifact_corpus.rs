//! Every artifact a released 1.x compiler produced loads in this gateway.
//!
//! The artifacts live under `tests/fixtures/artifacts/v<format>/` and are never
//! rebuilt; each is loaded with `barbacane check`, the load path `serve` uses.

use std::path::{Path, PathBuf};
use std::process::Command;

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/artifacts")
        .canonicalize()
        .expect("the artifact corpus exists")
}

/// The `.bca` files of each format directory, by format.
fn artifacts() -> Vec<(u32, PathBuf)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(corpus()).expect("read the corpus") {
        let dir = entry.expect("entry").path();
        let Some(format) = dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix('v'))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        for file in std::fs::read_dir(&dir).expect("read a format directory") {
            let path = file.expect("entry").path();
            if path.extension().is_some_and(|e| e == "bca") {
                found.push((format, path));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn every_released_artifact_loads() {
    let binary = barbacane_test::gateway::find_barbacane_binary()
        .expect("barbacane binary not found; run `cargo build -p barbacane` first");
    let artifacts = artifacts();
    assert!(!artifacts.is_empty(), "the corpus holds no artifact");
    for (format, artifact) in artifacts {
        assert!(
            (barbacane_compiler::OLDEST_ARTIFACT_VERSION..=barbacane_compiler::ARTIFACT_VERSION)
                .contains(&format),
            "{}: format {format} is outside what this major reads",
            artifact.display()
        );
        let output = Command::new(&binary)
            .args(["check", "--allow-unsigned", "--artifact"])
            .arg(&artifact)
            .env_remove("BARBACANE_TRUSTED_PUBKEY")
            .output()
            .expect("run barbacane check");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{} does not load:\n{stdout}{stderr}",
            artifact.display()
        );
        assert!(
            stdout.contains(&format!("artifact format {format},")),
            "{}: {stdout}",
            artifact.display()
        );
    }
}

/// A release that changes the format adds an artifact of it.
#[test]
fn the_current_format_has_an_artifact() {
    assert!(
        artifacts()
            .iter()
            .any(|(format, _)| *format == barbacane_compiler::ARTIFACT_VERSION),
        "no artifact of format {} under {}",
        barbacane_compiler::ARTIFACT_VERSION,
        corpus().display()
    );
}
