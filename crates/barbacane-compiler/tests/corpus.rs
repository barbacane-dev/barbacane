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

use barbacane_compiler::spec_parser::{parse_spec, parse_spec_file, ApiSpec};
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
            // Definitions are held once for the document, so a schema resolves
            // against the pool rather than against itself.
            let schema = spec.self_contained(&schema);
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
                "{} {where_} points at definitions the document does not hold: {dangling:?}",
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
            // The data plane attaches the pool before compiling, so this builds
            // the same value it does.
            let schema = spec.self_contained(&schema);
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

    // The fixture's plugins must be built for it to compile. Skip only when they
    // are absent, as in a clean tree, so a real compilation failure still fails.
    let repo_plugins = repo_root.join("plugins");
    for plugin in ["rate-limit", "basic-auth"] {
        if !repo_plugins
            .join(plugin)
            .join(format!("{plugin}.wasm"))
            .exists()
        {
            eprintln!("skipping: {plugin}.wasm is not built");
            return;
        }
    }

    let spec = fixtures.join("rate-limit.yaml");
    let out = std::env::temp_dir().join("barbacane-corpus-allowlist.bca");
    // Plugin paths in the manifest are relative to the directory holding it.
    compile_with_manifest(
        &[spec.as_path()],
        &manifest,
        &fixtures,
        &out,
        &CompileOptions::default(),
    )
    .expect("the fixture must compile once its plugins are built");

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

/// A definition many operations reach is held once, not once per operation.
///
/// Parsing a published Stripe document used 5.67 GB before this was true: its
/// `error` schema is referenced once per operation, 594 times, and reaches most
/// of a 1454-component graph, so every operation carried its own copy. The cost
/// has to scale with the size of the schema graph, not with the number of
/// operations that reach it.
#[test]
fn a_shared_definition_is_not_copied_per_operation() {
    /// A document where every operation references the same deep schema.
    fn spec_with(operations: usize) -> String {
        let mut paths = String::new();
        for i in 0..operations {
            paths.push_str(&format!(
                r#"
  /thing{i}:
    post:
      operationId: post{i}
      requestBody:
        required: true
        content:
          application/json:
            schema: {{ $ref: '#/components/schemas/Shared' }}
      responses:
        "200": {{ description: ok }}
      x-barbacane-dispatch: {{ name: mock, config: {{ status: 200 }} }}"#
            ));
        }
        format!(
            r#"openapi: "3.1.0"
info: {{ title: Shared, version: "1.0.0" }}
paths:{paths}
components:
  schemas:
    Shared:
      type: object
      properties:
        a: {{ $ref: '#/components/schemas/LevelA' }}
    LevelA:
      type: object
      properties:
        b: {{ $ref: '#/components/schemas/LevelB' }}
    LevelB:
      type: object
      properties:
        c: {{ type: string }}
"#
        )
    }

    let small = parse_spec(&spec_with(2)).expect("parse");
    let large = parse_spec(&spec_with(200)).expect("parse");

    let defs_len = |spec: &ApiSpec| {
        spec.schema_defs
            .as_ref()
            .and_then(|d| d.as_object())
            .map(|o| o.len())
            .unwrap_or(0)
    };

    // The pool holds the three shared definitions once, whatever the operation
    // count. A per-operation copy would grow with it.
    assert_eq!(defs_len(&small), 3, "pool: {:?}", small.schema_defs);
    assert_eq!(
        defs_len(&large),
        defs_len(&small),
        "the pool must not grow with the number of operations that reach it"
    );

    // And no schema carries its own copy of the definitions.
    for op in &large.operations {
        let body = op.request_body.as_ref().expect("request body");
        for content in body.content.values() {
            let schema = content.schema.as_ref().expect("schema");
            assert!(
                schema.get("$defs").is_none(),
                "a schema carried its own definitions: {schema}"
            );
        }
    }
}

/// Two documents in one artifact keep their own definitions.
///
/// Both declare `Wrapper` with a byte-identical body, and both declare `Inner`
/// with different bodies. Sharing one `Wrapper` entry between them would let the
/// rename of the second document's `Inner` reach back and change what the first
/// document's operations resolve, silently swapping one schema for another.
#[test]
fn two_specs_sharing_a_definition_name_keep_their_own() {
    use barbacane_compiler::{compile_with_manifest, CompileOptions, ProjectManifest};
    fn spec_with(inner_type: &str) -> String {
        format!(
            r#"openapi: "3.1.0"
info: {{ title: Shared, version: "1.0.0" }}
paths:
  /{inner_type}:
    post:
      operationId: post{inner_type}
      requestBody:
        required: true
        content:
          application/json:
            schema: {{ $ref: '#/components/schemas/Wrapper' }}
      responses:
        "200": {{ description: ok }}
      x-barbacane-dispatch: {{ name: mock, config: {{ status: 200 }} }}
components:
  schemas:
    Wrapper:
      type: object
      properties:
        inner: {{ $ref: '#/components/schemas/Inner' }}
    Inner:
      type: {inner_type}
"#
        )
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let mut paths = Vec::new();
    for kind in ["string", "integer"] {
        let path = dir.path().join(format!("{kind}.yaml"));
        std::fs::write(&path, spec_with(kind)).expect("write");
        paths.push(path);
    }

    let manifest_path = dir.path().join("barbacane.yaml");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root");
    std::fs::write(
        &manifest_path,
        format!(
            "plugins:\n  mock:\n    path: {}\n",
            repo.join("plugins/mock/mock.wasm").display()
        ),
    )
    .expect("write manifest");

    let out = dir.path().join("out.bca");
    let refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
    let manifest_text = std::fs::read_to_string(&manifest_path).expect("manifest");
    let manifest = ProjectManifest::parse(&manifest_text, &manifest_path).expect("manifest");
    compile_with_manifest(
        &refs,
        &manifest,
        manifest_path.parent().expect("parent"),
        &out,
        &CompileOptions {
            allow_plaintext: true,
            ..Default::default()
        },
    )
    .expect("compile");

    // Each operation must still reach the `Inner` its own document declared.
    let routes = barbacane_compiler::load_routes(&out).expect("read routes back");
    let pool = routes
        .schema_defs
        .expect("the pool holds both documents' definitions");

    for op in &routes.operations {
        let expected = op.path.trim_start_matches('/');
        let schema = op
            .request_body
            .as_ref()
            .expect("body")
            .content
            .values()
            .next()
            .expect("content")
            .schema
            .as_ref()
            .expect("schema");

        // Follow the wrapper, then its inner, through the pool.
        let wrapper_name = schema["$ref"]
            .as_str()
            .expect("ref")
            .rsplit('/')
            .next()
            .unwrap();
        let wrapper = &pool[wrapper_name];
        let inner_ref = wrapper["properties"]["inner"]["$ref"]
            .as_str()
            .expect("inner ref")
            .rsplit('/')
            .next()
            .unwrap();
        let inner_type = pool[inner_ref]["type"].as_str().expect("type");
        assert_eq!(
            inner_type, expected,
            "operation {} resolved to the other document's Inner",
            op.path
        );
    }
}

/// A configuration the plugin's own schema rejects must not compile.
///
/// `ai-proxy` stopped accepting `model` on a target (ADR-0030): the client names
/// the model and the gateway routes on it. A deployment carried the old form
/// past a clean compile and the plugin then refused to initialise, which reached
/// callers as a 500 on every request through that dispatcher. The compiler held
/// the schema that describes this all along.
#[test]
fn a_config_the_plugin_schema_rejects_does_not_compile() {
    use barbacane_compiler::{compile_with_manifest, CompileOptions, ProjectManifest};

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root");
    let wasm = repo.join("plugins/ai-proxy/ai-proxy.wasm");
    if !wasm.exists() {
        eprintln!("skipping: {} is not built", wasm.display());
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let manifest_path = dir.path().join("barbacane.yaml");
    std::fs::write(
        &manifest_path,
        format!("plugins:\n  ai-proxy:\n    path: {}\n", wasm.display()),
    )
    .expect("write manifest");

    let spec_for = |extra: &str| {
        format!(
            r#"openapi: "3.1.0"
info: {{ title: Proxy, version: "1.0.0" }}
paths:
  /chat:
    post:
      operationId: chat
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          targets:
            cloud:
              provider: anthropic{extra}
          default_target: cloud
      responses:
        "200": {{ description: ok }}
"#
        )
    };

    let compile = |body: &str, out: &str| {
        let spec = dir.path().join(format!("{out}.yaml"));
        std::fs::write(&spec, body).expect("write spec");
        let manifest_text = std::fs::read_to_string(&manifest_path).expect("manifest");
        let manifest = ProjectManifest::parse(&manifest_text, &manifest_path).expect("manifest");
        compile_with_manifest(
            &[spec.as_path()],
            &manifest,
            manifest_path.parent().expect("parent"),
            &dir.path().join(format!("{out}.bca")),
            &CompileOptions {
                allow_plaintext: true,
                ..Default::default()
            },
        )
    };

    // The form the plugin no longer accepts.
    let err = compile(
        &spec_for("\n              model: claude-sonnet-5"),
        "legacy",
    )
    .expect_err("a legacy `model` on a target must be refused");
    let message = err.to_string();
    assert!(message.contains("E1023"), "{message}");
    assert!(
        message.contains("model"),
        "the message must name the offending field: {message}"
    );

    // And the same spec without it still compiles, so the check is not simply
    // refusing this plugin.
    compile(&spec_for(""), "current").expect("a valid config must still compile");
}

/// A config value given as a runtime reference compiles even when the plugin's
/// schema constrains what the value looks like. `ws-upstream` requires a URL
/// starting `ws://` or `wss://`, and a spec reads it from the environment.
#[test]
fn a_runtime_reference_satisfies_a_constrained_field() {
    use barbacane_compiler::{compile_with_manifest, CompileOptions, ProjectManifest};

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root");
    let wasm = repo.join("plugins/ws-upstream/ws-upstream.wasm");
    if !wasm.exists() {
        eprintln!("skipping: {} is not built", wasm.display());
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let manifest_path = dir.path().join("barbacane.yaml");
    std::fs::write(
        &manifest_path,
        format!("plugins:\n  ws-upstream:\n    path: {}\n", wasm.display()),
    )
    .expect("write manifest");

    let compile = |url: &str, out: &str| {
        let spec = dir.path().join(format!("{out}.yaml"));
        std::fs::write(
            &spec,
            format!(
                r#"openapi: "3.1.0"
info: {{ title: Socket, version: "1.0.0" }}
paths:
  /ws:
    get:
      operationId: socket
      x-barbacane-dispatch:
        name: ws-upstream
        config:
          url: "{url}"
      responses:
        "101": {{ description: switching }}
"#
            ),
        )
        .expect("write spec");
        let manifest_text = std::fs::read_to_string(&manifest_path).expect("manifest");
        let manifest = ProjectManifest::parse(&manifest_text, &manifest_path).expect("manifest");
        compile_with_manifest(
            &[spec.as_path()],
            &manifest,
            manifest_path.parent().expect("parent"),
            &dir.path().join(format!("{out}.bca")),
            &CompileOptions {
                allow_plaintext: true,
                ..Default::default()
            },
        )
    };

    compile("env://UPSTREAM_WS_URL", "reference").expect("a reference must compile");
    compile("ws://upstream:3000/ws", "literal").expect("a valid literal must compile");
    let err = compile("http://upstream:3000/ws", "wrong")
        .expect_err("a literal the schema rejects must still be refused");
    assert!(err.to_string().contains("E1023"), "{err}");
}

/// An operation that names no `config` is left alone: the plugin applies its own
/// defaults, and a schema does not always restate them.
#[test]
fn an_absent_config_is_not_validated_as_empty() {
    let spec = parse_spec(
        r#"openapi: "3.1.0"
info: { title: Bare, version: "1.0.0" }
paths:
  /ping:
    get:
      operationId: ping
      x-barbacane-dispatch:
        name: mock
"#,
    )
    .expect("parse");
    let dispatch = spec.operations[0].dispatch.as_ref().expect("dispatch");
    assert!(
        dispatch.config.is_null(),
        "an omitted config stays absent rather than becoming an object: {:?}",
        dispatch.config
    );
}
