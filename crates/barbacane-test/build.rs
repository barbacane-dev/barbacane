//! Build script for barbacane-test.
//!
//! Compiles fixture WASM plugins used by integration tests before the test
//! binary is linked. Each plugin is built with:
//!
//!   cargo build --target wasm32-unknown-unknown --release
//!
//! from its own crate directory (plugins are excluded from the workspace).
//!
//! If the `wasm32-unknown-unknown` target is not installed or the build fails,
//! a `cargo:warning` is emitted and the test binary is still produced.
//! Tests that require the missing WASM will panic with a descriptive message.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    // This build script lives at crates/barbacane-test/build.rs.
    // CARGO_MANIFEST_DIR = .../crates/barbacane-test
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

struct FixturePlugin {
    /// Directory of the plugin crate (relative to workspace root).
    dir: &'static str,
    /// Expected output .wasm file name (under target/wasm32-unknown-unknown/release/).
    wasm: &'static str,
}

const FIXTURE_PLUGINS: &[FixturePlugin] = &[FixturePlugin {
    dir: "tests/fixture-plugins/streaming-echo",
    wasm: "streaming_echo.wasm",
}];

fn main() {
    let root = workspace_root();

    for plugin in FIXTURE_PLUGINS {
        let plugin_dir = root.join(plugin.dir);
        let wasm_path = plugin_dir
            .join("target/wasm32-unknown-unknown/release")
            .join(plugin.wasm);

        // Re-run if any source file in the plugin changes.
        println!("cargo:rerun-if-changed={}/src", plugin_dir.display());
        println!("cargo:rerun-if-changed={}/Cargo.toml", plugin_dir.display());

        if wasm_path.exists() {
            // Already built — skip (incremental builds use rerun-if-changed above).
            continue;
        }

        build_fixture_plugin(&plugin_dir, &wasm_path, plugin.wasm);
    }
}

fn build_fixture_plugin(plugin_dir: &Path, wasm_path: &Path, wasm_name: &str) {
    let status = Command::new("cargo")
        .current_dir(plugin_dir)
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .status();

    match status {
        Ok(s) if s.success() => {
            println!(
                "cargo:warning=Built fixture plugin: {}",
                wasm_path.display()
            );
        }
        Ok(s) => {
            println!(
                "cargo:warning=Fixture plugin build failed (exit {}): {}. \
                 Streaming integration tests will be skipped.",
                s.code().unwrap_or(-1),
                wasm_name
            );
        }
        Err(e) => {
            println!(
                "cargo:warning=Could not run `cargo build` for fixture plugin {} \
                 (is `wasm32-unknown-unknown` target installed?): {}. \
                 Run: rustup target add wasm32-unknown-unknown",
                wasm_name, e
            );
        }
    }
}
