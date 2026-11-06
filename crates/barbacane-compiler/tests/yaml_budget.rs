//! A YAML document may not stand for a value much larger than itself.
//!
//! An alias replays the node its anchor names, so a short document can expand
//! into a value many times its size. Specs come from users and uploads, so the
//! expansion is bounded before the value is built.

use barbacane_compiler::parse_spec;

/// A valid spec carrying `count` aliases to one `size`-byte scalar.
fn spec_repeating(size: usize, count: usize) -> String {
    format!(
        "openapi: \"3.1.0\"\ninfo: {{ title: T, version: \"1\" }}\nx-pad: &pad \"{}\"\nx-repeat: [{}]\npaths: {{}}\n",
        "x".repeat(size),
        vec!["*pad"; count].join(", ")
    )
}

#[test]
fn aliases_repeating_a_large_scalar_are_refused() {
    let spec = spec_repeating(4096, 2000);
    let error = parse_spec(&spec).expect_err("8 MB from a 14 kB document");
    assert!(error.to_string().contains("expands"), "{error}");
}

#[test]
fn aliases_repeating_a_large_mapping_are_refused() {
    let entries: Vec<String> = (0..500).map(|i| format!("k{i}: 1")).collect();
    let spec = format!(
        "openapi: \"3.1.0\"\ninfo: {{ title: T, version: \"1\" }}\nx-pad: &pad {{ {} }}\nx-repeat: [{}]\npaths: {{}}\n",
        entries.join(", "),
        vec!["*pad"; 2000].join(", ")
    );
    let error = parse_spec(&spec).expect_err("a million nodes from a 15 kB document");
    assert!(error.to_string().contains("expands"), "{error}");
}

#[test]
fn ordinary_alias_use_is_accepted() {
    let spec = r#"
openapi: "3.1.0"
info: { title: T, version: "1" }
x-error: &error
  description: Error
  content:
    application/json:
      schema: { type: object, properties: { message: { type: string } } }
paths:
  /a:
    get:
      x-barbacane-dispatch: { name: mock }
      responses: { "200": { description: ok }, "400": *error, "500": *error }
  /b:
    get:
      x-barbacane-dispatch: { name: mock }
      responses: { "200": { description: ok }, "400": *error, "500": *error }
"#;
    let parsed = parse_spec(spec).expect("parses");
    assert_eq!(parsed.operations.len(), 2);
}

#[test]
fn a_large_document_without_aliases_is_accepted() {
    let paths: Vec<String> = (0..5000)
        .map(|i| {
            format!(
                "  /items{i}:\n    get:\n      x-barbacane-dispatch: {{ name: mock }}\n      responses: {{ \"200\": {{ description: ok }} }}\n"
            )
        })
        .collect();
    let spec = format!(
        "openapi: \"3.1.0\"\ninfo: {{ title: T, version: \"1\" }}\npaths:\n{}",
        paths.concat()
    );
    assert_eq!(parse_spec(&spec).expect("parses").operations.len(), 5000);
}
