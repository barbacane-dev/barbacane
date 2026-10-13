//! Checks on every plugin's `config-schema.json`.
//!
//! Plugin config is validated against its schema at compile time (E1023). An
//! object schema that leaves `additionalProperties` unset accepts any key, so a
//! key in the wrong place compiles and is ignored at runtime. Every object
//! schema that declares `properties` must therefore say whether other keys are
//! allowed: `false` to reject them, or `true` (or a schema) where the shape is
//! open by design, such as a JWK.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// Each plugin's name and its parsed `config-schema.json`, or the parse error.
fn plugin_schemas() -> Vec<(String, Result<Value, String>)> {
    let mut schemas: Vec<(String, Result<Value, String>)> = std::fs::read_dir(plugins_dir())
        .expect("plugins directory")
        .filter_map(|entry| {
            let dir = entry.ok()?.path();
            let path = dir.join("config-schema.json");
            let text = std::fs::read_to_string(&path).ok()?;
            let name = dir.file_name()?.to_string_lossy().into_owned();
            let schema = serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"));
            Some((name, schema))
        })
        .collect();
    schemas.sort_by(|a, b| a.0.cmp(&b.0));
    schemas
}

/// Paths of object schemas under `node` that declare `properties` and leave
/// `additionalProperties` unset.
fn open_objects(node: &Value, path: &str, out: &mut Vec<String>) {
    let Some(map) = node.as_object() else {
        return;
    };
    if map.contains_key("properties") && !map.contains_key("additionalProperties") {
        out.push(if path.is_empty() {
            "<root>".into()
        } else {
            path.to_string()
        });
    }
    for (key, value) in map {
        let child = |name: &str| {
            if path.is_empty() {
                name.to_string()
            } else {
                format!("{path}.{name}")
            }
        };
        match key.as_str() {
            "properties" | "patternProperties" | "$defs" | "definitions" => {
                for (name, sub) in value.as_object().into_iter().flatten() {
                    open_objects(sub, &child(name), out);
                }
            }
            "items" | "additionalProperties" | "not" | "if" | "then" | "else" => {
                open_objects(value, &child(&format!("[{key}]")), out);
            }
            "oneOf" | "anyOf" | "allOf" | "prefixItems" => {
                for (i, sub) in value.as_array().into_iter().flatten().enumerate() {
                    open_objects(sub, &child(&format!("[{key}{i}]")), out);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn every_object_schema_states_whether_other_keys_are_allowed() {
    let schemas = plugin_schemas();
    assert!(schemas.len() > 20, "found {} plugin schemas", schemas.len());

    let mut open = Vec::new();
    for (plugin, schema) in &schemas {
        match schema {
            Ok(schema) => {
                let mut paths = Vec::new();
                open_objects(schema, "", &mut paths);
                open.extend(paths.into_iter().map(|p| format!("{plugin}: {p}")));
            }
            Err(e) => open.push(format!("{plugin}: {e}")),
        }
    }
    assert!(
        open.is_empty(),
        "set `additionalProperties` (false, or true where the shape is open by design) on:\n  {}",
        open.join("\n  ")
    );
}

#[test]
fn open_objects_finds_nested_and_combined_schemas() {
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "closed": { "type": "object", "additionalProperties": false, "properties": { "a": {} } },
            "explicitly_open": { "type": "object", "additionalProperties": true, "properties": { "a": {} } },
            "nested": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "inner": { "type": "object", "properties": { "a": {} } } }
            },
            "list": { "type": "array", "items": { "type": "object", "properties": { "a": {} } } },
            "either": { "oneOf": [ { "type": "object", "properties": { "a": {} } } ] },
            "free_form": { "type": "object" }
        }
    });
    let mut paths = Vec::new();
    open_objects(&schema, "", &mut paths);
    paths.sort();
    assert_eq!(
        paths,
        vec!["either.[oneOf0]", "list.[items]", "nested.inner"],
        "objects without `properties` are free-form and not reported"
    );

    let mut root = Vec::new();
    open_objects(&serde_json::json!({ "properties": {} }), "", &mut root);
    assert_eq!(root, vec!["<root>"]);
}

fn schema_of(plugin: &str) -> Value {
    let path = plugins_dir().join(plugin).join("config-schema.json");
    let text = std::fs::read_to_string(&path).expect("schema file");
    serde_json::from_str(&text).expect("valid JSON")
}

fn is_valid(schema: &Value, config: &Value) -> bool {
    jsonschema::options()
        .build(schema)
        .expect("valid schema")
        .is_valid(config)
}

#[test]
fn a_misplaced_request_transformer_key_is_rejected() {
    let schema = schema_of("request-transformer");
    let set = serde_json::json!({ "Authorization": "Bearer $cookie.sso_token" });

    assert!(is_valid(
        &schema,
        &serde_json::json!({ "skip_if_empty": true, "headers": { "set": set } })
    ));
    for misplaced in [
        serde_json::json!({ "headers": { "skip_if_empty": true, "set": set } }),
        serde_json::json!({ "querystring": { "skip_if_empty": true } }),
        serde_json::json!({ "body": { "skip_if_empty": true } }),
        serde_json::json!({ "path": { "strip": "/api" } }),
        serde_json::json!({ "path": { "replace": { "pattern": "a", "replacement": "b", "flags": "g" } } }),
    ] {
        assert!(!is_valid(&schema, &misplaced), "accepted {misplaced}");
    }
}

#[test]
fn a_jwk_may_carry_members_the_schema_does_not_name() {
    let schema = schema_of("jwt-auth");
    let jwk = serde_json::json!({
        "kty": "EC", "crv": "P-256", "x": "x", "y": "y", "use": "sig", "x5t": "thumb"
    });
    assert!(
        is_valid(&schema, &serde_json::json!({ "public_key_jwk": jwk })),
        "RFC 7517 allows members beyond the ones listed"
    );
}
