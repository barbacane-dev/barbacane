//! Differential checks of the parser against whole specs.
//!
//! Unit tests assert what one construct parses to. These read a spec, work out
//! what it should contain by walking the raw document, and compare that against
//! what the parser produced. The failures worth catching here are the ones where
//! covered code produces a plausible but wrong result, which a unit test written
//! from the same understanding as the code will agree with.
//!
//! The corpus is the repository's own fixtures. Point `BARBACANE_CORPUS_DIR` at
//! a directory of further specs to widen it, which the scheduled job uses to run
//! these same checks over published documents.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use barbacane_compiler::spec_parser::{parse_spec_file, ApiSpec};
use serde_json::Value;

/// Methods an OpenAPI path item may hold, including `query` from 3.2.
const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "trace", "query",
];

/// Every spec the corpus covers.
fn corpus() -> Vec<PathBuf> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root");

    let mut dirs = vec![repo_root.join("tests/fixtures")];
    if let Ok(extra) = std::env::var("BARBACANE_CORPUS_DIR") {
        dirs.push(PathBuf::from(extra));
    }

    let mut specs = Vec::new();
    for dir in dirs {
        collect_specs(&dir, &mut specs);
    }
    specs.sort();
    assert!(!specs.is_empty(), "the corpus must not be empty");
    specs
}

fn collect_specs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_specs(&path, out);
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "yaml" | "yml" | "json") {
            continue;
        }
        // A spec declares its format at the root; anything else in these
        // directories (manifests, rulesets, WAF data) is not one.
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(raw) = parse_document(&text) else {
            continue;
        };
        if raw.get("openapi").is_some() || raw.get("asyncapi").is_some() {
            out.push(path);
        }
    }
}

fn parse_document(text: &str) -> Result<Value, String> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok(v);
    }
    serde_yaml::from_str::<Value>(text).map_err(|e| e.to_string())
}

fn raw_of(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).expect("read spec");
    parse_document(&text).expect("spec is valid YAML or JSON")
}

/// Resolve a local `$ref`, following a chain, for the independent walk.
fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    let mut current = node;
    for _ in 0..16 {
        let Some(reference) = current.get("$ref").and_then(|v| v.as_str()) else {
            return current;
        };
        let Some(rest) = reference.strip_prefix("#/") else {
            return current;
        };
        let mut target = root;
        for segment in rest.split('/') {
            let key = segment.replace("~1", "/").replace("~0", "~");
            match target.get(&key) {
                Some(next) => target = next,
                None => return current,
            }
        }
        current = target;
    }
    current
}

/// Operations the document declares, counted without consulting the parser.
fn expected_operations(raw: &Value) -> usize {
    let Some(paths) = raw.get("paths").and_then(|p| p.as_object()) else {
        return 0;
    };
    let mut count = 0;
    for (_, item) in paths {
        let item = resolve(raw, item);
        let Some(item) = item.as_object() else {
            continue;
        };
        count += item
            .keys()
            .filter(|k| HTTP_METHODS.contains(&k.as_str()))
            .count();
        if let Some(extra) = item.get("additionalOperations").and_then(|v| v.as_object()) {
            count += extra.len();
        }
    }
    count
}

/// Header parameter names the document declares for any operation.
fn expected_header_names(raw: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(paths) = raw.get("paths").and_then(|p| p.as_object()) else {
        return names;
    };
    for (_, item) in paths {
        let item = resolve(raw, item);
        let Some(item) = item.as_object() else {
            continue;
        };
        let mut lists = vec![item.get("parameters")];
        for (key, value) in item {
            if HTTP_METHODS.contains(&key.as_str()) {
                lists.push(value.get("parameters"));
            }
        }
        for list in lists.into_iter().flatten() {
            let Some(list) = list.as_array() else {
                continue;
            };
            for entry in list {
                let entry = resolve(raw, entry);
                if entry.get("in").and_then(|v| v.as_str()) == Some("header") {
                    if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                        names.insert(name.to_ascii_lowercase());
                    }
                }
            }
        }
    }
    names
}

/// Every schema the parser attached to an operation.
fn schemas_of(spec: &ApiSpec) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for op in &spec.operations {
        let where_ = format!("{} {}", op.method, op.path);
        for param in &op.parameters {
            if let Some(schema) = &param.schema {
                out.push((
                    format!("{where_} parameter '{}'", param.name),
                    schema.clone(),
                ));
            }
        }
        if let Some(body) = &op.request_body {
            for (media, content) in &body.content {
                if let Some(schema) = &content.schema {
                    out.push((format!("{where_} body {media}"), schema.clone()));
                }
            }
        }
        for (status, response) in &op.responses {
            for (media, content) in &response.content {
                if let Some(schema) = &content.schema {
                    out.push((
                        format!("{where_} response {status} {media}"),
                        schema.clone(),
                    ));
                }
            }
        }
        for message in &op.messages {
            if let Some(payload) = &message.payload {
                out.push((format!("{where_} message payload"), payload.clone()));
            }
        }
    }
    out
}

/// Walk every `$ref` string in a value.
fn each_ref(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::Object(obj) => {
            if let Some(reference) = obj.get("$ref").and_then(|v| v.as_str()) {
                f(reference);
            }
            for v in obj.values() {
                each_ref(v, f);
            }
        }
        Value::Array(items) => {
            for v in items {
                each_ref(v, f);
            }
        }
        _ => {}
    }
}

#[test]
fn every_spec_parses() {
    for path in corpus() {
        if let Err(e) = parse_spec_file(&path) {
            panic!("{} failed to parse: {e}", path.display());
        }
    }
}

#[test]
fn operation_count_matches_the_document() {
    for path in corpus() {
        let raw = raw_of(&path);
        // The independent walk only knows OpenAPI paths.
        if raw.get("openapi").is_none() {
            continue;
        }
        let spec = parse_spec_file(&path).expect("parse");
        assert_eq!(
            spec.operations.len(),
            expected_operations(&raw),
            "{} has a different number of operations than the document declares",
            path.display()
        );
    }
}

#[test]
fn declared_header_parameters_survive() {
    for path in corpus() {
        let raw = raw_of(&path);
        if raw.get("openapi").is_none() {
            continue;
        }
        let spec = parse_spec_file(&path).expect("parse");
        let parsed: BTreeSet<String> = spec
            .operations
            .iter()
            .flat_map(|op| op.parameters.iter())
            .filter(|p| p.location == "header")
            .map(|p| p.name.to_ascii_lowercase())
            .collect();
        for name in expected_header_names(&raw) {
            assert!(
                parsed.contains(&name),
                "{} declares header parameter '{name}', which the parser dropped",
                path.display()
            );
        }
    }
}

#[test]
fn schemas_are_self_contained() {
    for path in corpus() {
        let spec = parse_spec_file(&path).expect("parse");
        for (where_, schema) in schemas_of(&spec) {
            each_ref(&schema, &mut |reference| {
                assert!(
                    reference.starts_with("#/$defs/"),
                    "{} {where_} keeps a reference out of the schema: {reference}. \
                     A schema travels in the artifact alone, with no document to resolve against.",
                    path.display()
                );
            });
        }
    }
}

#[test]
fn every_local_reference_resolves() {
    for path in corpus() {
        let spec = parse_spec_file(&path).expect("parse");
        for (where_, schema) in schemas_of(&spec) {
            let mut dangling = Vec::new();
            each_ref(&schema, &mut |reference| {
                let Some(rest) = reference.strip_prefix("#/") else {
                    return;
                };
                let mut target = &schema;
                for segment in rest.split('/') {
                    let key = segment.replace("~1", "/").replace("~0", "~");
                    match target.get(&key) {
                        Some(next) => target = next,
                        None => {
                            dangling.push(reference.to_string());
                            return;
                        }
                    }
                }
            });
            assert!(
                dangling.is_empty(),
                "{} {where_} points at definitions it does not carry: {dangling:?}",
                path.display()
            );
        }
    }
}

#[test]
fn every_schema_compiles() {
    for path in corpus() {
        let spec = parse_spec_file(&path).expect("parse");
        for (where_, schema) in schemas_of(&spec) {
            if let Err(e) = jsonschema::options().build(&schema) {
                panic!(
                    "{} {where_} produced a schema the validator cannot build: {e}",
                    path.display()
                );
            }
        }
    }
}

/// The allowlist admits what the chain's own configuration tells a plugin to
/// read. Nothing in the spec's vocabulary mentions those headers, so if they
/// are not collected here the plugin would stop seeing them once the list is
/// enforced.
#[test]
fn configured_header_names_reach_the_allowlist() {
    use barbacane_compiler::{compile_with_manifest, CompileOptions, ProjectManifest};

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root");
    let fixtures = repo_root.join("tests/fixtures");
    let manifest_path = fixtures.join("barbacane.yaml");
    let Ok(manifest_text) = std::fs::read_to_string(&manifest_path) else {
        eprintln!("skipping: no fixture manifest");
        return;
    };
    let Ok(manifest) = ProjectManifest::parse(&manifest_text, &manifest_path) else {
        eprintln!("skipping: fixture manifest does not parse");
        return;
    };

    let spec = fixtures.join("rate-limit.yaml");
    let out = std::env::temp_dir().join("barbacane-corpus-allowlist.bca");
    let result = compile_with_manifest(
        &[spec.as_path()],
        &manifest,
        &manifest_path,
        &out,
        &CompileOptions::default(),
    );
    if let Err(e) = result {
        // The plugins must be built for this to compile; skip rather than fail
        // when they are not, as a plain `cargo test` in a clean tree.
        eprintln!("skipping: fixture did not compile ({e})");
        return;
    }

    let routes = barbacane_compiler::load_routes(&out).expect("read routes back");
    let limited = routes
        .operations
        .iter()
        .find(|o| o.path == "/limited")
        .expect("/limited");
    assert!(
        limited
            .allowed_request_headers
            .iter()
            .any(|h| h == "x-client-id"),
        "rate-limit partitions on header:x-client-id, which must be admitted: {:?}",
        limited.allowed_request_headers
    );
}
