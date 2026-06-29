//! Fuzz the OpenAPI/AsyncAPI spec parser.
//!
//! Target: `barbacane_compiler::parse_spec(&str) -> Result<ApiSpec, ParseError>`
//! (re-exported from `crates/barbacane-compiler/src/spec_parser/parser.rs`).
//!
//! Invariant: the parser must never panic, overflow the stack, or run away on
//! hostile input — it must always return `Ok`/`Err` for any UTF-8 string.
//! `parse_spec` runs serde_yaml + recursive path/channel walking, so deeply
//! nested YAML, huge anchor expansions, and malformed extension blocks are the
//! interesting cases.
//!
//! Run: `cargo +nightly fuzz run spec_parser`

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The parser takes &str; only feed it valid UTF-8 (invalid UTF-8 is a
    // separate, uninteresting rejection at the boundary).
    if let Ok(input) = std::str::from_utf8(data) {
        // We only care that this does not panic / hang / overflow. The Result is
        // intentionally ignored.
        let _ = barbacane_compiler::parse_spec(input);
    }
});
