//! Request validation for Barbacane gateway.
//!
//! Validates incoming requests against OpenAPI parameter and body schemas.
//! Used by the data plane to reject non-conforming requests before dispatch.

use std::collections::HashMap;

use serde_json::Value;
use thiserror::Error;

use barbacane_spec_parser::{Parameter, RequestBody};

/// Validation errors returned when a request doesn't conform to the spec.
#[derive(Debug, Error)]
pub enum ValidationError2 {
    #[error("missing required parameter '{name}' in {location}")]
    MissingRequiredParameter { name: String, location: String },

    #[error("invalid parameter '{name}' in {location}: {reason}")]
    InvalidParameter {
        name: String,
        location: String,
        reason: String,
    },

    #[error("missing required request body")]
    MissingRequiredBody,

    #[error("unsupported content-type: {0}")]
    UnsupportedContentType(String),

    #[error("invalid request body: {0}")]
    InvalidBody(String),

    #[error("request body too large: {size} bytes exceeds limit of {limit} bytes")]
    BodyTooLarge { size: usize, limit: usize },

    #[error("too many headers: {count} exceeds limit of {limit}")]
    TooManyHeaders { count: usize, limit: usize },

    #[error("URI too long: {length} characters exceeds limit of {limit}")]
    UriTooLong { length: usize, limit: usize },

    #[error("header '{name}' too large: {size} bytes exceeds limit of {limit} bytes")]
    HeaderTooLarge {
        name: String,
        size: usize,
        limit: usize,
    },
}

/// RFC 9457 problem details for validation errors.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub error_type: String,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Extended fields for dev mode
    #[serde(flatten)]
    pub extensions: HashMap<String, Value>,
}

impl ProblemDetails {
    pub fn validation_error(errors: &[ValidationError2], dev_mode: bool) -> Self {
        let mut extensions = HashMap::new();

        if dev_mode && !errors.is_empty() {
            let error_details: Vec<Value> = errors
                .iter()
                .map(|e| {
                    let mut detail = serde_json::Map::new();
                    match e {
                        ValidationError2::MissingRequiredParameter { name, location } => {
                            detail.insert("field".into(), Value::String(name.clone()));
                            detail.insert("location".into(), Value::String(location.clone()));
                            detail.insert(
                                "reason".into(),
                                Value::String("missing required parameter".into()),
                            );
                        }
                        ValidationError2::InvalidParameter {
                            name,
                            location,
                            reason,
                        } => {
                            detail.insert("field".into(), Value::String(name.clone()));
                            detail.insert("location".into(), Value::String(location.clone()));
                            detail.insert("reason".into(), Value::String(reason.clone()));
                        }
                        ValidationError2::MissingRequiredBody => {
                            detail.insert("field".into(), Value::String("body".into()));
                            detail.insert(
                                "reason".into(),
                                Value::String("missing required request body".into()),
                            );
                        }
                        ValidationError2::UnsupportedContentType(ct) => {
                            detail.insert("field".into(), Value::String("content-type".into()));
                            detail.insert(
                                "reason".into(),
                                Value::String(format!("unsupported: {}", ct)),
                            );
                        }
                        ValidationError2::InvalidBody(reason) => {
                            detail.insert("field".into(), Value::String("body".into()));
                            detail.insert("reason".into(), Value::String(reason.clone()));
                        }
                        ValidationError2::BodyTooLarge { size, limit } => {
                            detail.insert("field".into(), Value::String("body".into()));
                            detail.insert(
                                "reason".into(),
                                Value::String(format!(
                                    "body too large: {} bytes exceeds {} byte limit",
                                    size, limit
                                )),
                            );
                        }
                        ValidationError2::TooManyHeaders { count, limit } => {
                            detail.insert("field".into(), Value::String("headers".into()));
                            detail.insert(
                                "reason".into(),
                                Value::String(format!(
                                    "too many headers: {} exceeds {} limit",
                                    count, limit
                                )),
                            );
                        }
                        ValidationError2::UriTooLong { length, limit } => {
                            detail.insert("field".into(), Value::String("uri".into()));
                            detail.insert(
                                "reason".into(),
                                Value::String(format!(
                                    "URI too long: {} chars exceeds {} char limit",
                                    length, limit
                                )),
                            );
                        }
                        ValidationError2::HeaderTooLarge { name, size, limit } => {
                            detail
                                .insert("field".into(), Value::String(format!("header:{}", name)));
                            detail.insert(
                                "reason".into(),
                                Value::String(format!(
                                    "header too large: {} bytes exceeds {} byte limit",
                                    size, limit
                                )),
                            );
                        }
                    }
                    Value::Object(detail)
                })
                .collect();
            extensions.insert("errors".into(), Value::Array(error_details));
        }

        let detail = if errors.len() == 1 {
            Some(errors[0].to_string())
        } else {
            Some(format!("{} validation errors", errors.len()))
        };

        ProblemDetails {
            error_type: "urn:barbacane:error:validation-failed".into(),
            title: "Request validation failed".into(),
            status: 400,
            detail,
            instance: None,
            extensions,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"type":"urn:barbacane:error:internal","title":"Serialization error","status":500}"#.into()
        })
    }
}

/// Request limits configuration.
#[derive(Debug, Clone)]
pub struct RequestLimits {
    /// Maximum request body size in bytes (default: 1MB).
    pub max_body_size: usize,
    /// Maximum number of headers (default: 100).
    pub max_headers: usize,
    /// Maximum header size in bytes (default: 8KB).
    pub max_header_size: usize,
    /// Maximum URI length in characters (default: 8KB).
    pub max_uri_length: usize,
}

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            max_body_size: 1024 * 1024, // 1 MB
            max_headers: 100,
            max_header_size: 8 * 1024, // 8 KB
            max_uri_length: 8 * 1024,  // 8 KB
        }
    }
}

impl RequestLimits {
    /// Validate URI length.
    pub fn validate_uri(&self, uri: &str) -> Result<(), ValidationError2> {
        if uri.len() > self.max_uri_length {
            return Err(ValidationError2::UriTooLong {
                length: uri.len(),
                limit: self.max_uri_length,
            });
        }
        Ok(())
    }

    /// Validate header count and individual header sizes.
    pub fn validate_headers(
        &self,
        headers: &HashMap<String, String>,
    ) -> Result<(), ValidationError2> {
        if headers.len() > self.max_headers {
            return Err(ValidationError2::TooManyHeaders {
                count: headers.len(),
                limit: self.max_headers,
            });
        }

        for (name, value) in headers {
            let header_size = name.len() + value.len();
            if header_size > self.max_header_size {
                return Err(ValidationError2::HeaderTooLarge {
                    name: name.clone(),
                    size: header_size,
                    limit: self.max_header_size,
                });
            }
        }

        Ok(())
    }

    /// Validate body size.
    pub fn validate_body_size(&self, body_len: usize) -> Result<(), ValidationError2> {
        if body_len > self.max_body_size {
            return Err(ValidationError2::BodyTooLarge {
                size: body_len,
                limit: self.max_body_size,
            });
        }
        Ok(())
    }

    /// Validate all limits at once. Returns errors for all limit violations.
    pub fn validate_all(
        &self,
        uri: &str,
        headers: &HashMap<String, String>,
        body_len: usize,
    ) -> Result<(), Vec<ValidationError2>> {
        let mut errors = Vec::new();

        if let Err(e) = self.validate_uri(uri) {
            errors.push(e);
        }

        if let Err(e) = self.validate_headers(headers) {
            errors.push(e);
        }

        if let Err(e) = self.validate_body_size(body_len) {
            errors.push(e);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Compile a JSON schema with format validation enabled.
///
/// Supports formats: date-time, email, uuid, uri, ipv4, ipv6.
fn compile_schema_with_formats(schema: &Value) -> Option<jsonschema::Validator> {
    jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .ok()
}

/// Compiled validator for an operation.
pub struct OperationValidator {
    /// Path parameters with their compiled schemas.
    path_params: Vec<CompiledParam>,
    /// Query parameters with their compiled schemas.
    query_params: Vec<CompiledParam>,
    /// Header parameters with their compiled schemas.
    header_params: Vec<CompiledParam>,
    /// Request body configuration.
    request_body: Option<CompiledRequestBody>,
}

struct CompiledParam {
    name: String,
    required: bool,
    schema: Option<jsonschema::Validator>,
}

struct CompiledRequestBody {
    required: bool,
    /// Content type -> compiled schema
    content: HashMap<String, Option<jsonschema::Validator>>,
}

impl OperationValidator {
    /// Create a new validator from parsed operation metadata.
    pub fn new(parameters: &[Parameter], request_body: Option<&RequestBody>) -> Self {
        let mut path_params = Vec::new();
        let mut query_params = Vec::new();
        let mut header_params = Vec::new();

        for param in parameters {
            let compiled = CompiledParam {
                name: param.name.clone(),
                required: param.required || param.location == "path", // Path params always required
                schema: param.schema.as_ref().and_then(compile_schema_with_formats),
            };

            match param.location.as_str() {
                "path" => path_params.push(compiled),
                "query" => query_params.push(compiled),
                "header" => header_params.push(compiled),
                _ => {} // Ignore cookie params for now
            }
        }

        let compiled_body = request_body.map(|rb| {
            let mut content = HashMap::new();
            for (media_type, content_schema) in &rb.content {
                let schema = content_schema
                    .schema
                    .as_ref()
                    .and_then(compile_schema_with_formats);
                content.insert(media_type.clone(), schema);
            }
            CompiledRequestBody {
                required: rb.required,
                content,
            }
        });

        Self {
            path_params,
            query_params,
            header_params,
            request_body: compiled_body,
        }
    }

    /// Validate path parameters extracted by the router.
    pub fn validate_path_params(
        &self,
        params: &[(String, String)],
    ) -> Result<(), Vec<ValidationError2>> {
        let mut errors = Vec::new();
        let param_map: HashMap<_, _> = params.iter().cloned().collect();

        for param in &self.path_params {
            match param_map.get(&param.name) {
                Some(value) => {
                    if let Some(schema) = &param.schema {
                        // Path parameters are always strings from the URL.
                        // Wrap in a JSON string value for schema validation.
                        let json_value = Value::String(value.clone());

                        let validation_errors: Vec<_> = schema.iter_errors(&json_value).collect();
                        if !validation_errors.is_empty() {
                            let reasons: Vec<String> =
                                validation_errors.iter().map(|e| e.to_string()).collect();
                            errors.push(ValidationError2::InvalidParameter {
                                name: param.name.clone(),
                                location: "path".into(),
                                reason: reasons.join("; "),
                            });
                        }
                    }
                }
                None if param.required => {
                    errors.push(ValidationError2::MissingRequiredParameter {
                        name: param.name.clone(),
                        location: "path".into(),
                    });
                }
                None => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Validate query parameters.
    pub fn validate_query_params(
        &self,
        query_string: Option<&str>,
    ) -> Result<(), Vec<ValidationError2>> {
        let mut errors = Vec::new();

        // Parse query string into map
        let param_map: HashMap<String, String> = query_string
            .unwrap_or("")
            .split('&')
            .filter(|s| !s.is_empty())
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?;
                let value = parts.next().unwrap_or("");
                Some((urlencoding_decode(key), urlencoding_decode(value)))
            })
            .collect();

        for param in &self.query_params {
            match param_map.get(&param.name) {
                Some(value) => {
                    if let Some(schema) = &param.schema {
                        // Query parameters are always strings from the URL.
                        // Wrap in a JSON string value for schema validation.
                        let json_value = Value::String(value.clone());

                        let validation_errors: Vec<_> = schema.iter_errors(&json_value).collect();
                        if !validation_errors.is_empty() {
                            let reasons: Vec<String> =
                                validation_errors.iter().map(|e| e.to_string()).collect();
                            errors.push(ValidationError2::InvalidParameter {
                                name: param.name.clone(),
                                location: "query".into(),
                                reason: reasons.join("; "),
                            });
                        }
                    }
                }
                None if param.required => {
                    errors.push(ValidationError2::MissingRequiredParameter {
                        name: param.name.clone(),
                        location: "query".into(),
                    });
                }
                None => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Validate request headers.
    pub fn validate_headers(
        &self,
        headers: &HashMap<String, String>,
    ) -> Result<(), Vec<ValidationError2>> {
        let mut errors = Vec::new();

        // Normalize header names to lowercase for comparison
        let headers_lower: HashMap<String, &String> =
            headers.iter().map(|(k, v)| (k.to_lowercase(), v)).collect();

        for param in &self.header_params {
            let header_name = param.name.to_lowercase();
            match headers_lower.get(&header_name) {
                Some(value) => {
                    if let Some(schema) = &param.schema {
                        let json_value = Value::String((*value).clone());

                        let validation_errors: Vec<_> = schema.iter_errors(&json_value).collect();
                        if !validation_errors.is_empty() {
                            let reasons: Vec<String> =
                                validation_errors.iter().map(|e| e.to_string()).collect();
                            errors.push(ValidationError2::InvalidParameter {
                                name: param.name.clone(),
                                location: "header".into(),
                                reason: reasons.join("; "),
                            });
                        }
                    }
                }
                None if param.required => {
                    errors.push(ValidationError2::MissingRequiredParameter {
                        name: param.name.clone(),
                        location: "header".into(),
                    });
                }
                None => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Validate request body.
    pub fn validate_body(
        &self,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<(), Vec<ValidationError2>> {
        let Some(body_spec) = &self.request_body else {
            // No body spec, nothing to validate
            return Ok(());
        };

        // Check if body is required but missing
        if body_spec.required && body.is_empty() {
            return Err(vec![ValidationError2::MissingRequiredBody]);
        }

        // If body is empty and not required, skip validation
        if body.is_empty() {
            return Ok(());
        }

        // Check content type
        let ct = content_type.unwrap_or("application/octet-stream");
        let base_ct = ct.split(';').next().unwrap_or(ct).trim();

        // Find matching content type (with wildcard support)
        let schema = if let Some(schema) = body_spec.content.get(base_ct) {
            schema
        } else if let Some(schema) = body_spec.content.get("*/*") {
            schema
        } else {
            return Err(vec![ValidationError2::UnsupportedContentType(
                base_ct.to_string(),
            )]);
        };

        // Validate JSON body against schema
        if let Some(schema) = schema {
            if base_ct.contains("json") {
                let json_body: Value = match serde_json::from_slice(body) {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(vec![ValidationError2::InvalidBody(format!(
                            "invalid JSON: {}",
                            e
                        ))]);
                    }
                };

                let validation_errors: Vec<_> = schema.iter_errors(&json_body).collect();
                if !validation_errors.is_empty() {
                    let reasons: Vec<String> =
                        validation_errors.iter().map(|e| e.to_string()).collect();
                    return Err(vec![ValidationError2::InvalidBody(reasons.join("; "))]);
                }
            }
        }

        Ok(())
    }

    /// Validate entire request (fail-fast: stops at first error category).
    pub fn validate_request(
        &self,
        path_params: &[(String, String)],
        query_string: Option<&str>,
        headers: &HashMap<String, String>,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<(), Vec<ValidationError2>> {
        // Validate in order: path -> query -> headers -> body
        self.validate_path_params(path_params)?;
        self.validate_query_params(query_string)?;
        self.validate_headers(headers)?;
        self.validate_body(content_type, body)?;
        Ok(())
    }
}

/// Simple URL decoding (handles %XX escapes).
fn urlencoding_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::compile_schema_with_formats;
    use super::*;

    fn make_param(name: &str, location: &str, required: bool, schema: Option<Value>) -> Parameter {
        Parameter {
            name: name.to_string(),
            location: location.to_string(),
            required,
            schema,
        }
    }

    #[test]
    fn validate_required_path_param() {
        let params = vec![make_param("id", "path", true, None)];
        let validator = OperationValidator::new(&params, None);

        // Missing required param
        let result = validator.validate_path_params(&[]);
        assert!(result.is_err());

        // Present param
        let result = validator.validate_path_params(&[("id".into(), "123".into())]);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_path_param_schema() {
        // Path parameters are always strings. Use a pattern to validate format.
        let schema = serde_json::json!({
            "type": "string",
            "pattern": "^[0-9]+$"
        });
        let params = vec![make_param("id", "path", true, Some(schema))];
        let validator = OperationValidator::new(&params, None);

        // Valid: numeric string
        let result = validator.validate_path_params(&[("id".into(), "123".into())]);
        assert!(result.is_ok());

        // Invalid: not matching the numeric pattern
        let result = validator.validate_path_params(&[("id".into(), "abc".into())]);
        assert!(result.is_err());
    }

    #[test]
    fn validate_required_query_param() {
        let params = vec![make_param("page", "query", true, None)];
        let validator = OperationValidator::new(&params, None);

        // Missing required
        let result = validator.validate_query_params(Some(""));
        assert!(result.is_err());

        // Present
        let result = validator.validate_query_params(Some("page=1"));
        assert!(result.is_ok());
    }

    #[test]
    fn validate_optional_query_param() {
        let params = vec![make_param("limit", "query", false, None)];
        let validator = OperationValidator::new(&params, None);

        // Missing optional is OK
        let result = validator.validate_query_params(Some(""));
        assert!(result.is_ok());
    }

    #[test]
    fn validate_required_body() {
        use barbacane_spec_parser::ContentSchema;
        use std::collections::BTreeMap;

        let mut content = BTreeMap::new();
        content.insert(
            "application/json".to_string(),
            ContentSchema { schema: None },
        );

        let request_body = RequestBody {
            required: true,
            content,
        };

        let validator = OperationValidator::new(&[], Some(&request_body));

        // Missing required body
        let result = validator.validate_body(Some("application/json"), &[]);
        assert!(result.is_err());

        // Present body
        let result = validator.validate_body(Some("application/json"), b"{}");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_body_schema() {
        use barbacane_spec_parser::ContentSchema;
        use std::collections::BTreeMap;

        let schema = serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string" }
            }
        });

        let mut content = BTreeMap::new();
        content.insert(
            "application/json".to_string(),
            ContentSchema {
                schema: Some(schema),
            },
        );

        let request_body = RequestBody {
            required: true,
            content,
        };

        let validator = OperationValidator::new(&[], Some(&request_body));

        // Valid body
        let result = validator.validate_body(Some("application/json"), br#"{"name":"test"}"#);
        assert!(result.is_ok());

        // Invalid: missing required field
        let result = validator.validate_body(Some("application/json"), b"{}");
        assert!(result.is_err());
    }

    #[test]
    fn validate_unsupported_content_type() {
        use barbacane_spec_parser::ContentSchema;
        use std::collections::BTreeMap;

        let mut content = BTreeMap::new();
        content.insert(
            "application/json".to_string(),
            ContentSchema { schema: None },
        );

        let request_body = RequestBody {
            required: true,
            content,
        };

        let validator = OperationValidator::new(&[], Some(&request_body));

        let result = validator.validate_body(Some("text/plain"), b"hello");
        assert!(result.is_err());

        if let Err(errors) = result {
            assert!(matches!(
                errors[0],
                ValidationError2::UnsupportedContentType(_)
            ));
        }
    }

    #[test]
    fn problem_details_format() {
        let errors = vec![ValidationError2::MissingRequiredParameter {
            name: "id".into(),
            location: "path".into(),
        }];

        let problem = ProblemDetails::validation_error(&errors, false);
        assert_eq!(problem.status, 400);
        assert_eq!(problem.error_type, "urn:barbacane:error:validation-failed");

        let json = problem.to_json();
        assert!(json.contains("validation-failed"));
    }

    #[test]
    fn problem_details_dev_mode() {
        let errors = vec![ValidationError2::MissingRequiredParameter {
            name: "id".into(),
            location: "path".into(),
        }];

        let problem = ProblemDetails::validation_error(&errors, true);
        let json = problem.to_json();

        // Dev mode should include error details
        assert!(json.contains("errors"));
        assert!(json.contains("field"));
    }

    // ========================
    // Request Limits Tests
    // ========================

    #[test]
    fn validate_uri_length_ok() {
        let limits = RequestLimits::default();
        let uri = "/api/users/123";
        assert!(limits.validate_uri(uri).is_ok());
    }

    #[test]
    fn validate_uri_length_too_long() {
        let limits = RequestLimits {
            max_uri_length: 10,
            ..Default::default()
        };
        let uri = "/api/users/123456789";
        let result = limits.validate_uri(uri);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError2::UriTooLong { .. }
        ));
    }

    #[test]
    fn validate_header_count_ok() {
        let limits = RequestLimits::default();
        let headers: HashMap<String, String> = (0..10)
            .map(|i| (format!("Header-{}", i), "value".to_string()))
            .collect();
        assert!(limits.validate_headers(&headers).is_ok());
    }

    #[test]
    fn validate_header_count_too_many() {
        let limits = RequestLimits {
            max_headers: 5,
            ..Default::default()
        };
        let headers: HashMap<String, String> = (0..10)
            .map(|i| (format!("Header-{}", i), "value".to_string()))
            .collect();
        let result = limits.validate_headers(&headers);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError2::TooManyHeaders { .. }
        ));
    }

    #[test]
    fn validate_header_size_ok() {
        let limits = RequestLimits::default();
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        assert!(limits.validate_headers(&headers).is_ok());
    }

    #[test]
    fn validate_header_size_too_large() {
        let limits = RequestLimits {
            max_header_size: 20,
            ..Default::default()
        };
        let mut headers = HashMap::new();
        headers.insert("X-Very-Long-Header".to_string(), "a".repeat(100));
        let result = limits.validate_headers(&headers);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError2::HeaderTooLarge { .. }
        ));
    }

    #[test]
    fn validate_body_size_ok() {
        let limits = RequestLimits::default();
        assert!(limits.validate_body_size(1000).is_ok());
    }

    #[test]
    fn validate_body_size_too_large() {
        let limits = RequestLimits {
            max_body_size: 100,
            ..Default::default()
        };
        let result = limits.validate_body_size(1000);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError2::BodyTooLarge { .. }
        ));
    }

    // ========================
    // Format Validation Tests
    // ========================

    #[test]
    fn format_validation_email() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "email": { "type": "string", "format": "email" }
            }
        });
        let validator = compile_schema_with_formats(&schema).unwrap();

        // Valid email
        let valid = serde_json::json!({"email": "user@example.com"});
        assert!(validator.is_valid(&valid));

        // Invalid email
        let invalid = serde_json::json!({"email": "not-an-email"});
        assert!(!validator.is_valid(&invalid));
    }

    #[test]
    fn format_validation_uuid() {
        let schema = serde_json::json!({
            "type": "string",
            "format": "uuid"
        });
        let validator = compile_schema_with_formats(&schema).unwrap();

        // Valid UUID
        let valid = serde_json::json!("550e8400-e29b-41d4-a716-446655440000");
        assert!(validator.is_valid(&valid));

        // Invalid UUID
        let invalid = serde_json::json!("not-a-uuid");
        assert!(!validator.is_valid(&invalid));
    }

    #[test]
    fn format_validation_date_time() {
        let schema = serde_json::json!({
            "type": "string",
            "format": "date-time"
        });
        let validator = compile_schema_with_formats(&schema).unwrap();

        // Valid date-time (RFC 3339)
        let valid = serde_json::json!("2024-01-29T12:30:00Z");
        assert!(validator.is_valid(&valid));

        // Invalid date-time
        let invalid = serde_json::json!("not-a-date");
        assert!(!validator.is_valid(&invalid));
    }

    #[test]
    fn format_validation_uri() {
        let schema = serde_json::json!({
            "type": "string",
            "format": "uri"
        });
        let validator = compile_schema_with_formats(&schema).unwrap();

        // Valid URI
        let valid = serde_json::json!("https://example.com/path?query=1");
        assert!(validator.is_valid(&valid));

        // Invalid URI (relative path)
        let invalid = serde_json::json!("not a uri");
        assert!(!validator.is_valid(&invalid));
    }

    #[test]
    fn format_validation_ipv4() {
        let schema = serde_json::json!({
            "type": "string",
            "format": "ipv4"
        });
        let validator = compile_schema_with_formats(&schema).unwrap();

        // Valid IPv4
        let valid = serde_json::json!("192.168.1.1");
        assert!(validator.is_valid(&valid));

        // Invalid IPv4
        let invalid = serde_json::json!("999.999.999.999");
        assert!(!validator.is_valid(&invalid));
    }

    #[test]
    fn format_validation_ipv6() {
        let schema = serde_json::json!({
            "type": "string",
            "format": "ipv6"
        });
        let validator = compile_schema_with_formats(&schema).unwrap();

        // Valid IPv6
        let valid = serde_json::json!("2001:0db8:85a3:0000:0000:8a2e:0370:7334");
        assert!(validator.is_valid(&valid));

        // Invalid IPv6
        let invalid = serde_json::json!("not-ipv6");
        assert!(!validator.is_valid(&invalid));
    }

    // ========================
    // Request Limits Tests
    // ========================

    #[test]
    fn validate_all_limits() {
        let limits = RequestLimits {
            max_uri_length: 10,
            max_headers: 2,
            max_body_size: 50,
            ..Default::default()
        };

        let mut headers = HashMap::new();
        headers.insert("A".to_string(), "1".to_string());

        // All within limits
        assert!(limits.validate_all("/short", &headers, 10).is_ok());

        // URI too long
        let result = limits.validate_all("/this/is/a/very/long/uri", &headers, 10);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().len(), 1);

        // Multiple violations
        let many_headers: HashMap<String, String> = (0..5)
            .map(|i| (format!("H{}", i), "v".to_string()))
            .collect();
        let result = limits.validate_all("/this/is/a/very/long/uri", &many_headers, 100);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().len(), 3); // URI + headers + body
    }
}
