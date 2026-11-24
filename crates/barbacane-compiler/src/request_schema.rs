//! The validators that check requests against a spec's schemas.
//!
//! The compiler (E1059) and the data plane build them the same way, so a spec
//! that compiles holds only schemas the gateway can enforce. Nothing outside
//! the schema is fetched: references resolve within it, and the OpenAPI schema
//! dialects are read as JSON Schema 2020-12.

use std::collections::HashSet;

use serde_json::Value;

/// Build the validator for a request schema, with the definitions it reaches
/// from the document's pool attached. The error says why it cannot be built.
pub fn request_validator(
    schema: &Value,
    defs: Option<&Value>,
) -> Result<jsonschema::Validator, String> {
    let mut schema = crate::spec_parser::attach_reachable_defs(schema, defs);
    drop_read_only_requirements(&mut schema);
    jsonschema::options()
        .should_validate_formats(true)
        .with_retriever(LocalOnly)
        .build(&schema)
        .map_err(|e| e.to_string())
}

/// Remove from each `required` list the properties its schema marks
/// `readOnly: true`, on the property or on the definition its `$ref` names.
/// OpenAPI applies such a requirement to responses only: the server assigns
/// the value, and a request need not carry it.
fn drop_read_only_requirements(schema: &mut Value) {
    let defs = schema.get("$defs").and_then(Value::as_object);
    let mut read_only: HashSet<String> = HashSet::new();
    if let Some(defs) = defs {
        // A definition that only refers to a read-only one is read-only too.
        loop {
            let before = read_only.len();
            for (name, body) in defs {
                if is_read_only(body, &read_only) {
                    read_only.insert(name.clone());
                }
            }
            if read_only.len() == before {
                break;
            }
        }
    }
    visit_schemas(schema, &mut |obj| {
        let Some(Value::Object(properties)) = obj.get("properties") else {
            return;
        };
        let dropped: HashSet<String> = properties
            .iter()
            .filter(|(_, property)| is_read_only(property, &read_only))
            .map(|(name, _)| name.clone())
            .collect();
        if dropped.is_empty() {
            return;
        }
        if let Some(Value::Array(required)) = obj.get_mut("required") {
            required.retain(|name| name.as_str().is_none_or(|n| !dropped.contains(n)));
        }
    });
}

/// Whether `schema` is marked `readOnly: true`, or is a `$ref` to one of the
/// `read_only` definitions.
fn is_read_only(schema: &Value, read_only: &HashSet<String>) -> bool {
    schema.get("readOnly") == Some(&Value::Bool(true))
        || schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix("#/$defs/"))
            .is_some_and(|name| read_only.contains(&name.replace("~1", "/").replace("~0", "~")))
}

/// Call `f` on every schema object in `value`: the value itself and each
/// subschema, by the keywords that hold schemas. Instance data (`enum`,
/// `const`, `default`, examples) is not visited.
fn visit_schemas(value: &mut Value, f: &mut impl FnMut(&mut serde_json::Map<String, Value>)) {
    let Value::Object(obj) = value else {
        return;
    };
    f(obj);
    for (keyword, child) in obj.iter_mut() {
        match keyword.as_str() {
            "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas" => {
                if let Value::Object(map) = child {
                    for schema in map.values_mut() {
                        visit_schemas(schema, f);
                    }
                }
            }
            "allOf"
            | "anyOf"
            | "oneOf"
            | "prefixItems"
            | "items"
            | "additionalItems"
            | "additionalProperties"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contains"
            | "propertyNames"
            | "unevaluatedItems"
            | "unevaluatedProperties" => match child {
                Value::Array(list) => {
                    for schema in list {
                        visit_schemas(schema, f);
                    }
                }
                schema => visit_schemas(schema, f),
            },
            _ => {}
        }
    }
}

/// Prefix of the OpenAPI 3.1 and 3.2 schema dialect URIs. Both add
/// annotations to JSON Schema 2020-12 and validate as it does.
const OPENAPI_DIALECT_PREFIX: &str = "https://spec.openapis.org/oas/3.";

/// Answers draft detection for the OpenAPI dialects and refuses every other
/// external resource.
struct LocalOnly;

impl jsonschema::Retrieve for LocalOnly {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<&str>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let uri = uri.as_str();
        if uri.starts_with(OPENAPI_DIALECT_PREFIX) && uri.contains("/dialect/") {
            return Ok(serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema"
            }));
        }
        Err(format!("'{uri}' is not retrieved: a schema resolves within the spec").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A property the server assigns may be required in the schema a response
    /// and a request share; a request without it is valid.
    #[test]
    fn a_read_only_property_is_not_required_of_a_request() {
        let schema = json!({"$ref": "#/$defs/Pet"});
        let pool = json!({
            "Pet": {
                "type": "object",
                "required": ["id", "name", "created", "owner"],
                "properties": {
                    "id": {"type": "integer", "readOnly": true},
                    "name": {"type": "string"},
                    "created": {"$ref": "#/$defs/Timestamp"},
                    "owner": {"$ref": "#/$defs/OwnerRef"},
                    "tags": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["slug"],
                            "properties": {"slug": {"type": "string", "readOnly": true}}
                        }
                    }
                }
            },
            "Timestamp": {"type": "string", "readOnly": true},
            "OwnerRef": {"$ref": "#/$defs/Owner"},
            "Owner": {"type": "string", "readOnly": true}
        });
        let validator = request_validator(&schema, Some(&pool)).expect("builds");
        assert!(validator.is_valid(&json!({"name": "Rex", "tags": [{}]})));
        assert!(
            !validator.is_valid(&json!({"id": 1})),
            "name is still required"
        );
        assert!(
            validator.is_valid(&json!({"id": 1, "name": "Rex"})),
            "a read-only value a client sends is not refused"
        );
        assert!(!validator.is_valid(&json!({"id": "one", "name": "Rex"})));
    }

    /// `required` inside instance data is not a schema keyword.
    #[test]
    fn instance_data_is_left_alone() {
        let mut schema = json!({
            "properties": {"id": {"readOnly": true}},
            "required": ["id"],
            "default": {"properties": {"id": {"readOnly": true}}, "required": ["id"]}
        });
        drop_read_only_requirements(&mut schema);
        assert_eq!(schema["required"], json!([]));
        assert_eq!(schema["default"]["required"], json!(["id"]));
    }
}
