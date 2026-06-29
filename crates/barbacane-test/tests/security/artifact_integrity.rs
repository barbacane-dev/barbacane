//! BARB-SEC-006 — Artifact integrity (hash + signature verification on load).
//!
//! Threat: `Gateway::load` (`crates/barbacane/src/main.rs`) reads the manifest,
//! routes, specs, and plugin WASM out of the `.bca` archive but NEVER verifies
//! them against the per-file `checksums` / `artifact_hash` recorded in
//! `manifest.json`, and there is no signature at all. An attacker who can modify
//! an artifact at rest (or in transit to a data plane) can swap in a malicious
//! plugin, rewrite routes, or alter the manifest, and the gateway will load it.
//!
//! The fix:
//!   * verify each archive entry's SHA-256 against `manifest.checksums` and the
//!     combined `artifact_hash` at load time (reject on mismatch), and
//!   * verify an Ed25519 signature over the artifact against a trusted public
//!     key provided via `BARBACANE_TRUSTED_PUBKEY` (reject if missing/invalid).
//!
//! ## How these tests work
//!
//! We compile a real `.bca` via `barbacane-compiler`, then rebuild the gzip+tar
//! archive flipping exactly one byte inside a chosen member (plugin WASM, routes,
//! or manifest) — leaving the gzip/tar framing intact so the corruption is
//! *content* corruption, not a parse error. We then ask the actual `barbacane`
//! binary to `serve` the tampered artifact and assert it REFUSES TO START.
//!
//! Today there is no verification, so a content-tampered artifact loads and the
//! gateway comes up healthy → RED. Once hash/signature verification lands, the
//! load fails fast → GREEN.

use std::collections::BTreeMap;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use barbacane_compiler::{compile_with_manifest, CompileOptions, ProjectManifest};
use tempfile::TempDir;

use crate::fixtures_dir;

/// Compile `minimal.yaml` (uses only the `mock` plugin) into a `.bca` in `dir`.
fn compile_minimal_artifact(dir: &Path) -> PathBuf {
    let fixtures = fixtures_dir();
    let spec = fixtures.join("minimal.yaml");
    let manifest_path = fixtures.join("barbacane.yaml");

    let project_manifest =
        ProjectManifest::load(&manifest_path).expect("load fixtures barbacane.yaml manifest");

    let artifact = dir.join("artifact.bca");
    let options = CompileOptions {
        allow_plaintext: true,
        ..CompileOptions::default()
    };
    compile_with_manifest(
        &[spec.as_path()],
        &project_manifest,
        &fixtures,
        &artifact,
        &options,
    )
    .expect("compile minimal.yaml");
    artifact
}

/// Read all entries of a gzip+tar `.bca` into (path, bytes), preserving order.
fn read_archive(path: &Path) -> Vec<(String, Vec<u8>)> {
    let file = std::fs::File::open(path).expect("open artifact");
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut entries = Vec::new();
    for entry in archive.entries().expect("read entries") {
        let mut entry = entry.expect("entry");
        let name = entry
            .path()
            .expect("entry path")
            .to_string_lossy()
            .into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry bytes");
        entries.push((name, bytes));
    }
    entries
}

/// Rewrite a gzip+tar `.bca` from the given (path, bytes) entries.
fn write_archive(path: &Path, entries: &[(String, Vec<u8>)]) {
    let file = std::fs::File::create(path).expect("create tampered artifact");
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, bytes.as_slice())
            .expect("append entry");
    }
    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish gzip");
}

/// Compile an artifact, flip one byte inside the first entry whose path matches
/// `predicate`, and write the result to a new `.bca`. Returns its path.
fn tamper_artifact(dir: &Path, predicate: impl Fn(&str) -> bool) -> PathBuf {
    let original = compile_minimal_artifact(dir);
    let mut entries = read_archive(&original);

    let target = entries
        .iter_mut()
        .find(|(name, bytes)| predicate(name) && !bytes.is_empty())
        .expect("found an entry matching the tamper predicate");

    // Flip one byte in the middle of the chosen member.
    let idx = target.1.len() / 2;
    target.1[idx] ^= 0xFF;

    let tampered = dir.join("tampered.bca");
    write_archive(&tampered, &entries);
    tampered
}

/// Free TCP port for the gateway under test.
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let p = l.local_addr().expect("local addr").port();
    drop(l);
    p
}

/// Locate the built `barbacane` data-plane binary.
fn find_barbacane_binary() -> Option<PathBuf> {
    [
        "target/debug/barbacane",
        "target/release/barbacane",
        "../target/debug/barbacane",
        "../target/release/barbacane",
        "../../target/debug/barbacane",
        "../../target/release/barbacane",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

/// Boot `barbacane serve` against `artifact` and report whether the gateway
/// becomes healthy. `true` = loaded & serving (verification did NOT reject);
/// `false` = the process refused to start / never became healthy.
async fn gateway_loads_artifact(artifact: &Path) -> bool {
    let Some(binary) = find_barbacane_binary() else {
        // Caller treats `None`-equivalent as skip; surface via panic-free path.
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        // Returning `false` here would masquerade as "rejected"; instead we make
        // the caller skip by checking the binary separately. See callers.
        return false;
    };

    let port = free_port();
    let admin_port = free_port();

    let mut child = Command::new(&binary)
        .arg("serve")
        .arg("--artifact")
        .arg(artifact)
        .arg("--listen")
        .arg(format!("127.0.0.1:{}", port))
        .arg("--admin-bind")
        .arg(format!("127.0.0.1:{}", admin_port))
        .arg("--dev")
        .arg("--allow-plaintext-upstream")
        .env("BARBACANE_TRUSTED_PUBKEY", "") // no trusted key configured
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn barbacane serve");

    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{}/__barbacane/health", port);

    let mut healthy = false;
    for _ in 0..50 {
        if let Ok(resp) = client.get(&health).send().await {
            if resp.status().is_success() {
                healthy = true;
                break;
            }
        }
        if let Ok(Some(_)) = child.try_wait() {
            // Process exited before becoming healthy → load rejected.
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = child.kill();
    let _ = child.wait();
    healthy
}

/// Tampering a bundled plugin's WASM bytes must make the load FAIL.
#[tokio::test]
async fn tampered_plugin_wasm_is_rejected() {
    // EXPECTED TO FAIL until BARB-SEC-006 is fixed (Gateway::load does not verify
    // archive-entry hashes against manifest.checksums or any signature).
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let tampered = tamper_artifact(dir.path(), |name| name.ends_with(".wasm"));

    let loaded = gateway_loads_artifact(&tampered).await;
    assert!(
        !loaded,
        "gateway must refuse to load an artifact whose bundled plugin WASM was \
         tampered (hash mismatch / signature invalid)"
    );
}

/// Tampering `routes.json` must make the load FAIL.
#[tokio::test]
async fn tampered_routes_is_rejected() {
    // EXPECTED TO FAIL until BARB-SEC-006 is fixed.
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let tampered = tamper_artifact(dir.path(), |name| name == "routes.json");

    let loaded = gateway_loads_artifact(&tampered).await;
    assert!(
        !loaded,
        "gateway must refuse to load an artifact whose routes.json was tampered"
    );
}

/// Tampering the `manifest.json` itself must make the load FAIL — verification
/// must not trust the manifest's own recorded hash blindly; a real signature
/// over the manifest is what makes this detectable.
#[tokio::test]
async fn tampered_manifest_is_rejected() {
    // EXPECTED TO FAIL until BARB-SEC-006 is fixed (no signature over the
    // manifest; an attacker can edit manifest.json and its self-described hash).
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");

    // For the manifest we corrupt a recorded checksum value so that, under the
    // fix, the per-entry hash check (or the signature) fails. We rewrite the
    // manifest JSON rather than flipping a random byte so the JSON still parses.
    let original = compile_minimal_artifact(dir.path());
    let mut entries = read_archive(&original);
    for (name, bytes) in entries.iter_mut() {
        if name == "manifest.json" {
            let mut manifest: serde_json::Value =
                serde_json::from_slice(bytes).expect("parse manifest.json");
            // Corrupt one recorded checksum to a plausible-but-wrong value.
            if let Some(checksums) = manifest
                .get_mut("checksums")
                .and_then(|c| c.as_object_mut())
            {
                let mut replacement = BTreeMap::new();
                for (k, _v) in checksums.iter() {
                    replacement.insert(
                        k.clone(),
                        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                            .to_string(),
                    );
                }
                for (k, v) in replacement {
                    checksums.insert(k, serde_json::Value::String(v));
                }
            }
            *bytes = serde_json::to_vec(&manifest).expect("reserialize manifest");
        }
    }
    let tampered = dir.path().join("tampered.bca");
    write_archive(&tampered, &entries);

    let loaded = gateway_loads_artifact(&tampered).await;
    assert!(
        !loaded,
        "gateway must refuse to load an artifact whose manifest checksums were \
         tampered (or whose signature does not cover/validate the manifest)"
    );
}

/// Sanity / positive control: the UNtampered freshly-compiled artifact loads
/// fine. This guards against the tamper helpers being so destructive that every
/// artifact fails to load (which would make the RED tests above meaningless).
///
/// NOTE: once BARB-SEC-006 lands and load requires a valid `BARBACANE_TRUSTED_PUBKEY`
/// signature, this control will need the signing key wired in; until then a
/// pristine artifact must load.
#[tokio::test]
async fn untampered_artifact_loads() {
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let artifact = compile_minimal_artifact(dir.path());

    let loaded = gateway_loads_artifact(&artifact).await;
    assert!(
        loaded,
        "a pristine, untampered artifact must load and the gateway must become healthy"
    );
}
