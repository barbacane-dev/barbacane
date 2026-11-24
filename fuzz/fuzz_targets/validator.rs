//! Fuzz request validation, parameter styles and the percent-decoders.
//!
//! Target: `barbacane_lib::validator::OperationValidator::validate_request`
//! (`crates/barbacane/src/validator.rs`), with one declared parameter whose
//! location, `style`, `explode` and schema shape the input chooses, so the
//! style readers in `crates/barbacane/src/params.rs` and the query decoder see
//! arbitrary text.
//!
//! Invariant: validation of arbitrary inputs never panics (notably the
//! decoders must handle truncated `%`, non-hex digits, and overlong sequences
//! without panicking or slicing on a non-char-boundary).
//!
//! Run: `cargo +nightly fuzz run validator`

#![no_main]

use std::collections::HashMap;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use serde_json::json;

use barbacane_compiler::{Parameter, RequestBody};
use barbacane_lib::validator::OperationValidator;

const LOCATIONS: &[&str] = &["query", "path", "header", "cookie"];
const STYLES: &[&str] = &[
    "form",
    "simple",
    "label",
    "matrix",
    "spaceDelimited",
    "pipeDelimited",
    "deepObject",
    "cookie",
];

/// Structured fuzz input so libFuzzer can explore each field independently.
#[derive(Debug, Arbitrary)]
struct Input {
    query_string: String,
    path_value: String,
    body: Vec<u8>,
    content_type: Option<String>,
    // A few (name, value) header pairs.
    headers: Vec<(String, String)>,
    // Whether to declare a parameter named `param_name`.
    declare_param: bool,
    param_name: String,
    location: u8,
    style: Option<u8>,
    explode: Option<bool>,
    // 0: no schema, 1: integer, 2: array of integers, 3: object, 4: JSON content.
    shape: u8,
    require_body: bool,
}

fuzz_target!(|input: Input| {
    let mut params: Vec<Parameter> = Vec::new();
    if input.declare_param && !input.param_name.is_empty() {
        let object = json!({"type": "object", "properties": {"id": {"type": "integer"}}});
        let (schema, media_type) = match input.shape % 5 {
            0 => (None, None),
            1 => (Some(json!({"type": "integer"})), None),
            2 => (
                Some(json!({"type": "array", "items": {"type": "integer"}})),
                None,
            ),
            3 => (Some(object), None),
            _ => (Some(object), Some("application/json".to_string())),
        };
        params.push(Parameter {
            name: input.param_name.clone(),
            location: LOCATIONS[usize::from(input.location) % LOCATIONS.len()].to_string(),
            required: false,
            schema,
            style: input
                .style
                .map(|s| STYLES[usize::from(s) % STYLES.len()].to_string()),
            explode: input.explode,
            media_type,
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

    let validator =
        OperationValidator::new(&params, request_body.as_ref()).expect("schemas compile");

    let headers: HashMap<String, String> = input.headers.into_iter().collect();
    let path_params = [(input.param_name.clone(), input.path_value)];

    let _ = validator.validate_request(
        &path_params,
        Some(input.query_string.as_str()),
        &headers,
        input.content_type.as_deref(),
        &input.body,
    );
});
