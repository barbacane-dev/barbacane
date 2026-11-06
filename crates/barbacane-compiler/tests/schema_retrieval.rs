//! Building a schema validator never fetches anything: not a `$schema`
//! dialect, not a `$ref` target, over HTTP or from a file.
//!
//! Every validator in the workspace is built by the same `jsonschema` crate,
//! with its retrieval features off: the compiler's plugin configs, the data
//! plane's request schemas, the control plane's compliance check.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

/// A local HTTP server that answers 404 and counts the connections it gets.
fn recording_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n");
        }
    });
    (base, hits)
}

/// Builds a validator from a schema, saying whether it could.
type Build = fn(&Value) -> bool;

/// The ways the workspace builds a validator from a schema it is handed.
fn builders() -> Vec<(&'static str, Build)> {
    vec![
        ("options().build", |s| {
            jsonschema::options().build(s).is_ok()
        }),
        ("validator_for", |s| jsonschema::validator_for(s).is_ok()),
        ("Validator::new", |s| jsonschema::Validator::new(s).is_ok()),
        ("request_validator", |s| {
            barbacane_compiler::request_validator(s, None).is_ok()
        }),
    ]
}

#[test]
fn no_schema_build_reaches_the_network() {
    let (base, hits) = recording_server();
    for (builder, build) in builders() {
        for schema in [
            json!({ "$schema": format!("{base}/dialect") }),
            json!({ "$ref": format!("{base}/definition") }),
            json!({ "properties": { "a": { "$ref": format!("{base}/nested") } } }),
        ] {
            assert!(!build(&schema), "{builder} built {schema}");
        }
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "connections made");
}

#[test]
fn no_schema_build_reads_a_file() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let target = dir.path().join("definition.json");
    std::fs::write(&target, r#"{ "type": "integer" }"#).expect("write");
    let schema = json!({ "$ref": format!("file://{}", target.display()) });
    for (builder, build) in builders() {
        assert!(!build(&schema), "{builder} read {}", target.display());
    }
}

#[test]
fn the_openapi_dialects_validate_as_json_schema_2020_12() {
    for dialect in [
        "https://spec.openapis.org/oas/3.1/dialect/base",
        "https://spec.openapis.org/oas/3.2/dialect/2025-09-17",
    ] {
        let validator = barbacane_compiler::request_validator(
            &json!({ "$schema": dialect, "type": "string", "maxLength": 2 }),
            None,
        )
        .unwrap_or_else(|e| panic!("{dialect}: {e}"));
        assert!(validator.is_valid(&json!("ab")));
        assert!(!validator.is_valid(&json!("abc")), "{dialect}");
    }
}

#[test]
fn a_dialect_the_gateway_does_not_know_is_refused() {
    let result = barbacane_compiler::request_validator(
        &json!({ "$schema": "https://example.com/my-dialect", "type": "string" }),
        None,
    );
    assert!(result.is_err());
}
