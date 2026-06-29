//! Fuzz request validation and the percent-decoder.
//!
//! Target: `barbacane_lib::validator::OperationValidator::validate_request`
//! (`crates/barbacane/src/validator.rs`). `validate_request` walks path/query/
//! header/body validation; the query path runs the private `urlencoding_decode`
//! percent-decoder, so feeding arbitrary query strings fuzzes the decoder
//! indirectly.
//!
//! Invariant: validation of arbitrary inputs never panics (notably the
//! percent-decoder must handle truncated `%`, non-hex digits, and overlong
//! sequences without panicking or slicing on a non-char-boundary).
//!
//! NOTE for the maintainer: the percent-decoder `urlencoding_decode` is
//! `fn` (private) in `validator.rs`. To fuzz it *directly* (tighter, faster),
//! expose a thin wrapper, e.g.:
//!   `pub fn fuzz_urldecode(s: &str) -> String { urlencoding_decode(s) }`
//! Until then this target reaches it through `validate_request`, which is fine
//! but also exercises the surrounding validation machinery.
//!
//! Run: `cargo +nightly fuzz run validator`

#![no_main]

use std::collections::HashMap;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use barbacane_compiler::{Parameter, RequestBody};
use barbacane_lib::validator::OperationValidator;

/// Structured fuzz input so libFuzzer can explore each field independently.
#[derive(Debug, Arbitrary)]
struct Input {
    query_string: String,
    body: Vec<u8>,
    content_type: Option<String>,
    // A few (name, value) header pairs.
    headers: Vec<(String, String)>,
    // Whether to attach a query parameter named after `param_name` so the
    // query-decode path has a declared param to match.
    declare_query_param: bool,
    param_name: String,
    require_body: bool,
}

fuzz_target!(|input: Input| {
    // Build a validator with optional declared parameters / body so different
    // validation branches (and the percent-decoder) are reachable.
    let mut params: Vec<Parameter> = Vec::new();
    if input.declare_query_param && !input.param_name.is_empty() {
        params.push(Parameter {
            name: input.param_name.clone(),
            location: "query".to_string(),
            required: false,
            schema: None,
        });
    }

    let request_body = if input.require_body {
        Some(RequestBody {
            required: true,
            content: Default::default(),
        })
    } else {
        None
    };

    let validator = OperationValidator::new(&params, request_body.as_ref());

    let headers: HashMap<String, String> = input.headers.into_iter().collect();

    // No path params; the interesting attacker-controlled surfaces are the query
    // string (percent-decoder), headers, and body.
    let _ = validator.validate_request(
        &[],
        Some(input.query_string.as_str()),
        &headers,
        input.content_type.as_deref(),
        &input.body,
    );
});
