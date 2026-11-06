//! BARB-SEC-006 — Artifact integrity (hash, member and signature verification
//! on load).
//!
//! The data plane reads a `.bca` archive once, checks `artifact_hash` (and the
//! Ed25519 signature when `BARBACANE_TRUSTED_PUBKEY` is set), then checks every
//! archive member against the checksums the hash covers before parsing any of
//! it. A duplicate, unlisted or missing member is refused, and a plugin is read
//! only from `plugins/<name>.wasm` under its recorded checksum.
//!
//! ## How these tests work
//!
//! We compile a real `.bca` via `barbacane-compiler`, rewrite the gzip+tar
//! archive, and ask the actual `barbacane` binary to `serve` it, asserting that
//! it refuses to start. Most rewrites keep every member well formed (valid
//! JSON, a valid WASM module, a manifest whose own fields agree), so a refusal
//! can only come from the integrity checks; `untampered_artifact_loads` is the
//! positive control.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use barbacane_compiler::{compile_with_manifest, CompileOptions, ProjectManifest};
use barbacane_test::gateway::{LoggedChild, LISTEN_ANY_PORT, LISTEN_HOST, STARTUP_TIMEOUT};
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

    let mut cmd = Command::new(&binary);
    cmd.arg("serve")
        .arg("--artifact")
        .arg(artifact)
        .arg("--listen")
        .arg(LISTEN_ANY_PORT)
        .arg("--admin-bind")
        .arg(LISTEN_ANY_PORT)
        .arg("--dev")
        .arg("--allow-plaintext-upstream")
        .env("BARBACANE_TRUSTED_PUBKEY", ""); // no trusted key configured

    let mut process = LoggedChild::spawn(&mut cmd).expect("spawn barbacane serve");

    // A gateway that rejects the artifact exits without announcing a port, and
    // `announced_port` returns the moment it does rather than waiting out the
    // timeout. A port means it bound, which is already most of the answer.
    let Some(port) = process.announced_port("listening on", STARTUP_TIMEOUT) else {
        process.kill_and_reap();
        return false;
    };

    let client = reqwest::Client::new();
    let health = format!("http://{LISTEN_HOST}:{port}/__barbacane/health");

    let mut healthy = false;
    for _ in 0..50 {
        if let Ok(resp) = client.get(&health).send().await {
            if resp.status().is_success() {
                healthy = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    process.kill_and_reap();
    healthy
}

/// Tampering a bundled plugin's WASM bytes must make the load FAIL.
#[tokio::test]
async fn tampered_plugin_wasm_is_rejected() {
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

/// Corrupting `routes.json` (one flipped byte) must make the load FAIL.
#[tokio::test]
async fn tampered_routes_is_rejected() {
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
/// No trusted key is configured, so no signature is required.
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

/// Rewrite the members of a freshly compiled artifact with `edit` and write
/// the result to `tampered.bca` in `dir`. Unlike `tamper_artifact`, the edits
/// keep every member well formed, so a rejection can only come from the
/// integrity checks.
fn rewrite_artifact(dir: &Path, edit: impl FnOnce(&mut Vec<(String, Vec<u8>)>)) -> PathBuf {
    let original = compile_minimal_artifact(dir);
    let mut entries = read_archive(&original);
    edit(&mut entries);
    let tampered = dir.join("tampered.bca");
    write_archive(&tampered, &entries);
    tampered
}

fn member<'a>(entries: &'a mut [(String, Vec<u8>)], name: &str) -> &'a mut Vec<u8> {
    &mut entries
        .iter_mut()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("artifact has no {name}"))
        .1
}

/// The module with a custom section appended: still a valid module with the
/// same behaviour, but different bytes and so a different digest.
fn with_custom_section(wasm: &[u8]) -> Vec<u8> {
    let name = b"tampered";
    let mut content = vec![name.len() as u8];
    content.extend_from_slice(name);
    content.push(1);
    let mut out = wasm.to_vec();
    out.push(0x00);
    out.push(content.len() as u8);
    out.extend_from_slice(&content);
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// `routes.json` with every mock response body replaced: valid JSON that
/// deserialises into routes, but not the routes that were compiled.
fn rewritten_routes(routes: &[u8]) -> Vec<u8> {
    fn walk(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    if key == "body" && child.is_string() {
                        *child = serde_json::Value::String(r#"{"tampered":true}"#.into());
                    } else {
                        walk(child);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut parsed: serde_json::Value = serde_json::from_slice(routes).expect("parse routes.json");
    let before = parsed.clone();
    walk(&mut parsed);
    assert_ne!(
        parsed, before,
        "the fixture's routes carry a mock body to rewrite"
    );
    serde_json::to_vec_pretty(&parsed).expect("serialise routes.json")
}

/// Point the manifest's `mock` plugin entry at `wasm_path` with `sha256`.
fn redirect_mock_plugin(entries: &mut [(String, Vec<u8>)], wasm_path: &str, sha256: &str) {
    let manifest = member(entries, "manifest.json");
    let mut parsed: serde_json::Value = serde_json::from_slice(manifest).expect("parse manifest");
    let plugin = parsed["plugins"]
        .as_array_mut()
        .expect("plugins array")
        .iter_mut()
        .find(|p| p["name"] == "mock")
        .expect("mock plugin entry");
    plugin["wasm_path"] = serde_json::Value::String(wasm_path.to_string());
    plugin["sha256"] = serde_json::Value::String(sha256.to_string());
    *manifest = serde_json::to_vec(&parsed).expect("serialise manifest");
}

async fn assert_rejected(artifact: &Path, what: &str) {
    assert!(
        !gateway_loads_artifact(artifact).await,
        "gateway must refuse an artifact whose {what}"
    );
}

/// Valid routes that differ from the compiled ones must be refused: the
/// manifest's checksum for `routes.json` no longer matches.
#[tokio::test]
async fn rewritten_routes_is_rejected() {
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let tampered = rewrite_artifact(dir.path(), |entries| {
        let routes = member(entries, "routes.json");
        *routes = rewritten_routes(routes);
    });
    assert_rejected(
        &tampered,
        "routes.json was replaced with other valid routes",
    )
    .await;
}

/// A different, valid plugin module must be refused even when the manifest's
/// per-plugin `sha256` is updated to match it: that field is not covered by
/// `artifact_hash`, the checksum table is.
#[tokio::test]
async fn plugin_swapped_with_matching_digest_is_rejected() {
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let tampered = rewrite_artifact(dir.path(), |entries| {
        let wasm = member(entries, "plugins/mock.wasm");
        *wasm = with_custom_section(wasm);
        let digest = sha256_hex(wasm);
        redirect_mock_plugin(entries, "plugins/mock.wasm", &digest);
    });
    assert_rejected(&tampered, "mock plugin module was replaced").await;
}

/// A plugin entry pointed at another archive member must be refused: a plugin
/// runs only from `plugins/<name>.wasm`, under its checksum.
#[tokio::test]
async fn plugin_redirected_to_another_member_is_rejected() {
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let tampered = rewrite_artifact(dir.path(), |entries| {
        let other = with_custom_section(member(entries, "plugins/mock.wasm"));
        let digest = sha256_hex(&other);
        entries.push(("plugins/other.wasm".to_string(), other));
        redirect_mock_plugin(entries, "plugins/other.wasm", &digest);
    });
    assert_rejected(&tampered, "mock plugin entry points at another member").await;
}

/// A source spec that differs from the one compiled must be refused: the
/// gateway serves it at `/__barbacane/specs`.
#[tokio::test]
async fn rewritten_spec_is_rejected() {
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let tampered = rewrite_artifact(dir.path(), |entries| {
        let spec = member(entries, "specs/minimal.yaml");
        spec.extend_from_slice(b"\n# tampered\n");
    });
    assert_rejected(&tampered, "source spec was rewritten").await;
}

/// Two members with the same path must be refused, so the bytes that are
/// verified are the bytes that are used.
#[tokio::test]
async fn duplicate_member_is_rejected() {
    if find_barbacane_binary().is_none() {
        eprintln!("skip: barbacane binary not built (run `cargo build -p barbacane`)");
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let tampered = rewrite_artifact(dir.path(), |entries| {
        let routes = rewritten_routes(member(entries, "routes.json"));
        entries.push(("routes.json".to_string(), routes));
    });
    assert_rejected(&tampered, "archive holds routes.json twice").await;
}
