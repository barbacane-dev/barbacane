use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::ParseError;
use crate::model::{
    ApiSpec, ContentSchema, DispatchConfig, MiddlewareConfig, ObservabilityConfig, Operation,
    Parameter, RequestBody, SpecFormat,
};

/// HTTP methods we recognize in OpenAPI paths.
const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "trace",
];

/// Parse an OpenAPI or AsyncAPI spec from a YAML/JSON string.
pub fn parse_spec(input: &str) -> Result<ApiSpec, ParseError> {
    // Parse YAML (also handles JSON since JSON is valid YAML)
    let root: Value =
        serde_yaml::from_str(input).map_err(|e| ParseError::ParseError(e.to_string()))?;

    let root_obj = root
        .as_object()
        .ok_or_else(|| ParseError::ParseError("spec root must be an object".into()))?;

    // Detect format
    let (format, version) = detect_format(root_obj)?;

    // Extract info
    let info = root_obj
        .get("info")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ParseError::SchemaError("missing 'info' object".into()))?;

    let title = info
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ParseError::SchemaError("missing 'info.title'".into()))?
        .to_string();

    let api_version = info
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();

    // Extract root-level x-barbacane-* extensions
    let extensions = extract_extensions(root_obj);

    // Extract global middlewares
    let global_middlewares = extract_middlewares(root_obj);

    // Extract global observability config
    let observability = extract_observability(root_obj);

    // Parse operations based on format
    let operations = match format {
        SpecFormat::OpenApi => parse_openapi_paths(root_obj)?,
        SpecFormat::AsyncApi => parse_asyncapi_channels(root_obj)?,
    };

    Ok(ApiSpec {
        filename: None,
        format,
        version,
        title,
        api_version,
        operations,
        global_middlewares,
        observability,
        extensions,
    })
}

/// Parse a spec from a file path.
pub fn parse_spec_file(path: &std::path::Path) -> Result<ApiSpec, ParseError> {
    let content = std::fs::read_to_string(path)?;
    let mut spec = parse_spec(&content)?;
    spec.filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());
    Ok(spec)
}

/// Detect whether this is OpenAPI or AsyncAPI and extract the version.
fn detect_format(
    root: &serde_json::Map<String, Value>,
) -> Result<(SpecFormat, String), ParseError> {
    if let Some(version) = root.get("openapi").and_then(|v| v.as_str()) {
        if !version.starts_with("3.") {
            return Err(ParseError::SchemaError(format!(
                "unsupported OpenAPI version: {} (only 3.x supported)",
                version
            )));
        }
        Ok((SpecFormat::OpenApi, version.to_string()))
    } else if let Some(version) = root.get("asyncapi").and_then(|v| v.as_str()) {
        if !version.starts_with("3.") {
            return Err(ParseError::SchemaError(format!(
                "unsupported AsyncAPI version: {} (only 3.x supported)",
                version
            )));
        }
        Ok((SpecFormat::AsyncApi, version.to_string()))
    } else {
        Err(ParseError::UnknownFormat)
    }
}

/// Extract all x-barbacane-* keys from an object.
fn extract_extensions(obj: &serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    obj.iter()
        .filter(|(k, _)| k.starts_with("x-barbacane-"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Extract x-barbacane-middlewares from an object.
fn extract_middlewares(obj: &serde_json::Map<String, Value>) -> Vec<MiddlewareConfig> {
    obj.get("x-barbacane-middlewares")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Extract x-barbacane-dispatch from an operation object.
fn extract_dispatch(obj: &serde_json::Map<String, Value>) -> Option<DispatchConfig> {
    obj.get("x-barbacane-dispatch")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Extract x-barbacane-observability from an object.
fn extract_observability(obj: &serde_json::Map<String, Value>) -> ObservabilityConfig {
    obj.get("x-barbacane-observability")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Extract optional x-barbacane-observability from an object.
fn extract_observability_opt(obj: &serde_json::Map<String, Value>) -> Option<ObservabilityConfig> {
    obj.get("x-barbacane-observability")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Parse OpenAPI 3.x paths into operations.
fn parse_openapi_paths(
    root: &serde_json::Map<String, Value>,
) -> Result<Vec<Operation>, ParseError> {
    let mut operations = Vec::new();

    let paths = match root.get("paths").and_then(|v| v.as_object()) {
        Some(p) => p,
        None => return Ok(operations), // No paths is valid (empty API)
    };

    for (path, path_item) in paths {
        let path_obj = path_item.as_object().ok_or_else(|| {
            ParseError::SchemaError(format!("path item for '{}' must be an object", path))
        })?;

        // Path-level parameters (inherited by all operations)
        let path_params = parse_parameters(path_obj);

        for method in HTTP_METHODS {
            if let Some(op_value) = path_obj.get(*method) {
                let op_obj = op_value.as_object().ok_or_else(|| {
                    ParseError::SchemaError(format!(
                        "operation {} {} must be an object",
                        method.to_uppercase(),
                        path
                    ))
                })?;

                // Merge path-level and operation-level parameters
                let mut params = path_params.clone();
                params.extend(parse_parameters(op_obj));

                let operation_id = op_obj
                    .get("operationId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let request_body = parse_request_body(op_obj);

                let dispatch = extract_dispatch(op_obj);

                let middlewares = if op_obj.contains_key("x-barbacane-middlewares") {
                    Some(extract_middlewares(op_obj))
                } else {
                    None
                };

                let observability = extract_observability_opt(op_obj);

                let extensions = extract_extensions(op_obj);

                operations.push(Operation {
                    path: path.clone(),
                    method: method.to_uppercase(),
                    operation_id,
                    parameters: params,
                    request_body,
                    dispatch,
                    middlewares,
                    observability,
                    extensions,
                });
            }
        }
    }

    Ok(operations)
}

/// Parse parameters from a path item or operation object.
fn parse_parameters(obj: &serde_json::Map<String, Value>) -> Vec<Parameter> {
    obj.get("parameters")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let param_obj = item.as_object()?;
                    Some(Parameter {
                        name: param_obj.get("name")?.as_str()?.to_string(),
                        location: param_obj.get("in")?.as_str()?.to_string(),
                        required: param_obj
                            .get("required")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        schema: param_obj.get("schema").cloned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse request body from an operation object.
fn parse_request_body(obj: &serde_json::Map<String, Value>) -> Option<RequestBody> {
    let body = obj.get("requestBody")?.as_object()?;

    let required = body
        .get("required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let content_obj = body.get("content")?.as_object()?;

    let mut content = BTreeMap::new();
    for (media_type, media_obj) in content_obj {
        let schema = media_obj.as_object().and_then(|o| o.get("schema").cloned());
        content.insert(media_type.clone(), ContentSchema { schema });
    }

    Some(RequestBody { required, content })
}

/// Parse AsyncAPI 3.x channels into operations (stub for M9).
fn parse_asyncapi_channels(
    _root: &serde_json::Map<String, Value>,
) -> Result<Vec<Operation>, ParseError> {
    // AsyncAPI support is M9, return empty for now
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_openapi() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      operationId: getHealth
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.format, SpecFormat::OpenApi);
        assert_eq!(spec.version, "3.1.0");
        assert_eq!(spec.title, "Test API");
        assert_eq!(spec.operations.len(), 1);

        let op = &spec.operations[0];
        assert_eq!(op.path, "/health");
        assert_eq!(op.method, "GET");
        assert_eq!(op.operation_id, Some("getHealth".to_string()));

        let dispatch = op.dispatch.as_ref().unwrap();
        assert_eq!(dispatch.name, "mock");
    }

    #[test]
    fn parse_path_with_parameters() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users/{id}:
    get:
      operationId: getUser
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: integer
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];
        assert_eq!(op.parameters.len(), 1);

        let param = &op.parameters[0];
        assert_eq!(param.name, "id");
        assert_eq!(param.location, "path");
        assert!(param.required);
    }

    #[test]
    fn parse_global_middlewares() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 100
      window: 60
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.global_middlewares.len(), 1);
        assert_eq!(spec.global_middlewares[0].name, "rate-limit");
    }

    #[test]
    fn parse_operation_middlewares_override() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: global-auth
paths:
  /public:
    get:
      x-barbacane-middlewares: []
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];
        // Operation has explicit middlewares (empty array = disable all)
        assert!(op.middlewares.is_some());
        assert_eq!(op.middlewares.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn reject_openapi_2() {
        let yaml = r#"
swagger: "2.0"
info:
  title: Old API
  version: "1.0.0"
paths: {}
"#;
        let result = parse_spec(yaml);
        assert!(matches!(result, Err(ParseError::UnknownFormat)));
    }

    #[test]
    fn parse_multiple_methods() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
    post:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 2);

        let methods: Vec<&str> = spec
            .operations
            .iter()
            .map(|op| op.method.as_str())
            .collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
    }

    #[test]
    fn extract_barbacane_extensions() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-observability:
  trace_sampling: 0.5
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
      x-barbacane-cache:
        ttl: "60s"
"#;
        let spec = parse_spec(yaml).unwrap();
        assert!(spec.extensions.contains_key("x-barbacane-observability"));

        let op = &spec.operations[0];
        assert!(op.extensions.contains_key("x-barbacane-cache"));
    }

    #[test]
    fn parse_observability_config() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-observability:
  trace_sampling: 0.1
  detailed_validation_logs: true
  latency_slo_ms: 50
paths:
  /fast:
    get:
      x-barbacane-dispatch:
        name: mock
      x-barbacane-observability:
        trace_sampling: 1.0
        latency_slo_ms: 10
  /slow:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();

        // Check global observability config
        assert_eq!(spec.observability.trace_sampling, Some(0.1));
        assert_eq!(spec.observability.detailed_validation_logs, Some(true));
        assert_eq!(spec.observability.latency_slo_ms, Some(50));

        // Check operation-level override
        let fast_op = spec.operations.iter().find(|op| op.path == "/fast").unwrap();
        let fast_obs = fast_op.observability.as_ref().unwrap();
        assert_eq!(fast_obs.trace_sampling, Some(1.0));
        assert_eq!(fast_obs.latency_slo_ms, Some(10));

        // Check operation without override
        let slow_op = spec.operations.iter().find(|op| op.path == "/slow").unwrap();
        assert!(slow_op.observability.is_none());
    }

    #[test]
    fn parse_request_body() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users:
    post:
      operationId: createUser
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required:
                - name
              properties:
                name:
                  type: string
                email:
                  type: string
                  format: email
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];

        let body = op.request_body.as_ref().expect("should have request body");
        assert!(body.required);
        assert!(body.content.contains_key("application/json"));

        let json_content = &body.content["application/json"];
        let schema = json_content.schema.as_ref().expect("should have schema");
        assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
    }
}
