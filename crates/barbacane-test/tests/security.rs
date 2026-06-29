//! Adversarial security test suite (Layer 1 of the security testing framework).
//!
//! This single test binary aggregates every security category as a submodule
//! (files under `tests/security/`) so they can share the helpers defined here.
//! Each category file asserts the SECURE / hardened behaviour, so the test is
//! RED today and turns GREEN as the corresponding finding is fixed. Every red
//! test carries a comment of the form
//! `// EXPECTED TO FAIL until <FINDING-ID> is fixed`.
//!
//! Findings covered (see docs/contributing/security-testing.md for the full
//! threat model):
//!
//! | Finding         | Category            | File                            |
//! |-----------------|---------------------|---------------------------------|
//! | BARB-SEC-001    | authz / IDOR        | security/authz.rs               |
//! | BARB-SEC-002    | SSRF                | security/ssrf.rs                |
//! | BARB-SEC-003    | DoS / resource caps | security/dos.rs                 |
//! | BARB-SEC-004    | sandbox / capability| security/sandbox.rs             |
//! | BARB-SEC-005    | crypto / auth       | security/crypto_auth.rs         |
//! | BARB-SEC-006    | artifact integrity  | security/artifact_integrity.rs  |
//!
//! Run with: `cargo test -p barbacane-test --test security`
//!
//! Most categories boot the data-plane gateway (and BARB-SEC-001 boots the
//! control-plane binary), which requires the relevant services to be available.
//! See the docs for the Docker / Postgres requirements.

// Shared helpers live here at the test-binary root; category modules reach them
// via `super::`.
#![allow(dead_code)] // Helpers are shared across category modules; not all are used by every module.

use std::path::PathBuf;

mod security {
    pub mod artifact_integrity;
    pub mod authz;
    pub mod crypto_auth;
    pub mod dos;
    pub mod sandbox;
    pub mod ssrf;
}

/// Absolute path to a fixture under `tests/fixtures/`.
///
/// Identical to the `fixture()` helper duplicated across the existing test
/// files, hoisted here so the security modules can share it.
pub fn fixture(name: &str) -> String {
    fixtures_dir().join(name).display().to_string()
}

/// Absolute path to the shared `tests/fixtures` directory.
pub fn fixtures_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../crates/barbacane-test
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .join("tests/fixtures")
}

/// Absolute path to the security-specific fixtures directory
/// (`tests/fixtures/security/`).
pub fn security_fixture(name: &str) -> String {
    fixtures_dir()
        .join("security")
        .join(name)
        .display()
        .to_string()
}

/// Current Unix timestamp in seconds.
pub fn now_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs()
}

/// Build an unsigned JWT (`header.payload.signature`) from the given header and
/// claims JSON. Used to forge `alg:none`, expired, and tampered tokens.
pub fn encode_jwt(
    header: &serde_json::Value,
    claims: &serde_json::Value,
    signature: &[u8],
) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let header_b64 = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());
    let claims_b64 = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature);
    format!("{}.{}.{}", header_b64, claims_b64, sig_b64)
}
