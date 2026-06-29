//! Fuzz the `.bca` artifact load path (gzip + tar decompression).
//!
//! Targets the loader functions in
//! `crates/barbacane-compiler/src/artifact.rs`:
//!   * `load_manifest(&Path)`
//!   * `load_routes(&Path)`
//!   * `load_specs(&Path)`
//!   * `load_plugins(&Path)`
//!
//! Each opens the file, wraps it in `flate2::read::GzDecoder` + `tar::Archive`,
//! and walks entries. Hostile input we care about: decompression bombs
//! (tiny gzip → huge expansion), malformed/oversized tar headers, path-traversal
//! entry names, truncated streams. The invariant is: no panic, no unbounded
//! memory blowup that OOM-kills the process for a small input.
//!
//! The loader API takes a `&Path` (there is no bytes/reader variant — see the
//! "maintainer action" note in docs/contributing/security-testing.md), so we
//! materialise the fuzz bytes to a temp file first. The libFuzzer max input size
//! bounds the *compressed* size; a decompression bomb that explodes from a small
//! seed is exactly what we want to surface.
//!
//! Run: `cargo +nightly fuzz run artifact_loader`

#![no_main]

use std::io::Write;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Write the raw fuzz bytes as a candidate .bca and run every loader against
    // it. We do NOT pre-validate gzip framing: malformed framing is part of the
    // attack surface and must be handled gracefully by the loaders.
    let Ok(mut tmp) = tempfile::NamedTempFile::new() else {
        return;
    };
    if tmp.write_all(data).is_err() {
        return;
    }
    if tmp.flush().is_err() {
        return;
    }
    let path = tmp.path();

    // All four loaders share the gzip+tar walk; fuzz each so manifest/routes/
    // specs/plugins-specific JSON handling is also exercised.
    let _ = barbacane_compiler::load_manifest(path);
    let _ = barbacane_compiler::load_routes(path);
    let _ = barbacane_compiler::load_specs(path);
    let _ = barbacane_compiler::load_plugins(path);
});
