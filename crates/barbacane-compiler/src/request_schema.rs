//! The validators that check requests against a spec's schemas.
//!
//! The compiler (E1059) and the data plane build them the same way, so a spec
//! that compiles holds only schemas the gateway can enforce. Nothing outside
//! the schema is fetched: references resolve within it, and the OpenAPI schema
//! dialects are read as JSON Schema 2020-12.

use serde_json::Value;

/// Build the validator for a request schema, with the definitions it reaches
/// from the document's pool attached. The error says why it cannot be built.
pub fn request_validator(
    schema: &Value,
    defs: Option<&Value>,
) -> Result<jsonschema::Validator, String> {
    let schema = crate::spec_parser::attach_reachable_defs(schema, defs);
    jsonschema::options()
        .should_validate_formats(true)
        .with_retriever(LocalOnly)
        .build(&schema)
        .map_err(|e| e.to_string())
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
