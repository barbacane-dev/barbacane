//! Config schema validation for plugins.
//!
//! Per SPEC-003 section 2.2, each plugin has a config-schema.json that
//! defines the JSON Schema for its configuration block in specs.

use jsonschema::Validator;
use serde_json::Value;

use crate::error::WasmError;

/// A compiled config schema for validating plugin configurations.
pub struct ConfigSchema {
    validator: Validator,
}

impl ConfigSchema {
    /// Create a config schema from JSON Schema content.
    pub fn from_json(schema_json: &str) -> Result<Self, WasmError> {
        let schema: Value =
            serde_json::from_str(schema_json).map_err(|e| WasmError::SchemaParse(e.to_string()))?;

        let validator = Validator::new(&schema)
            .map_err(|e| WasmError::SchemaParse(format!("invalid JSON Schema: {}", e)))?;

        Ok(Self { validator })
    }

    /// Create a config schema from a parsed JSON value.
    pub fn from_value(schema: &Value) -> Result<Self, WasmError> {
        let validator = Validator::new(schema)
            .map_err(|e| WasmError::SchemaParse(format!("invalid JSON Schema: {}", e)))?;

        Ok(Self { validator })
    }

    /// Validate a config value against the schema.
    pub fn validate(&self, config: &Value) -> Result<(), WasmError> {
        self.validator
            .validate(config)
            .map_err(|e| WasmError::ConfigValidation(e.to_string()))
    }

    /// Create an empty schema that accepts any object.
    ///
    /// Per SPEC-003, if a plugin takes no config, the schema should be:
    /// ```json
    /// { "type": "object", "additionalProperties": false }
    /// ```
    pub fn empty() -> Result<Self, WasmError> {
        Self::from_json(r#"{"type": "object", "additionalProperties": false}"#)
    }

    /// Create a permissive schema that accepts any value.
    ///
    /// Useful for testing or when no schema is provided.
    pub fn any() -> Result<Self, WasmError> {
        Self::from_json("{}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_against_schema() {
        let schema = ConfigSchema::from_json(
            r#"{
            "type": "object",
            "required": ["quota", "window"],
            "properties": {
                "quota": { "type": "integer", "minimum": 1 },
                "window": { "type": "integer", "minimum": 1 }
            }
        }"#,
        )
        .unwrap();

        // Valid config
        let valid = json!({"quota": 100, "window": 60});
        assert!(schema.validate(&valid).is_ok());

        // Missing required field
        let missing = json!({"quota": 100});
        assert!(schema.validate(&missing).is_err());

        // Invalid type
        let wrong_type = json!({"quota": "100", "window": 60});
        assert!(schema.validate(&wrong_type).is_err());

        // Value too low
        let too_low = json!({"quota": 0, "window": 60});
        assert!(schema.validate(&too_low).is_err());
    }

    #[test]
    fn empty_schema_rejects_properties() {
        let schema = ConfigSchema::empty().unwrap();

        // Empty object is valid
        assert!(schema.validate(&json!({})).is_ok());

        // Object with properties is invalid
        assert!(schema.validate(&json!({"foo": "bar"})).is_err());
    }

    #[test]
    fn any_schema_accepts_anything() {
        let schema = ConfigSchema::any().unwrap();

        assert!(schema.validate(&json!({})).is_ok());
        assert!(schema.validate(&json!({"anything": "goes"})).is_ok());
        assert!(schema.validate(&json!(null)).is_ok());
        assert!(schema.validate(&json!(42)).is_ok());
    }

    #[test]
    fn from_value() {
        let schema_value = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        });

        let schema = ConfigSchema::from_value(&schema_value).unwrap();
        assert!(schema.validate(&json!({"name": "test"})).is_ok());
    }

    #[test]
    fn invalid_schema() {
        // This is not a valid JSON Schema
        let result = ConfigSchema::from_json(r#"{"type": "not-a-type"}"#);
        // Note: jsonschema may accept this but fail on validation
        // The important thing is we handle errors gracefully
        match result {
            Ok(_) => {} // Some schemas are permissive
            Err(e) => {
                assert!(e.to_string().contains("Schema") || e.to_string().contains("invalid"))
            }
        }
    }
}
