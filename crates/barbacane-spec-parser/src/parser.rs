use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::ParseError;
use crate::model::{
    ApiSpec, ContentSchema, DispatchConfig, Message, MiddlewareConfig, Operation, Parameter,
    RequestBody, SpecFormat,
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

                // Extract deprecated flag (standard OpenAPI field)
                let deprecated = op_obj
                    .get("deprecated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Extract sunset date from x-sunset extension (RFC 8594)
                let sunset = op_obj
                    .get("x-sunset")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let extensions = extract_extensions(op_obj);

                operations.push(Operation {
                    path: path.clone(),
                    method: method.to_uppercase(),
                    operation_id,
                    parameters: params,
                    request_body,
                    dispatch,
                    middlewares,
                    deprecated,
                    sunset,
                    extensions,
                    messages: Vec::new(), // OpenAPI doesn't use AsyncAPI messages
                    bindings: BTreeMap::new(), // OpenAPI doesn't use protocol bindings
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

/// Parse AsyncAPI 3.x channels and operations.
///
/// AsyncAPI 3.x structure:
/// - `channels`: Map of channel names to channel definitions (address, messages)
/// - `operations`: Map of operation IDs to operation definitions (action, channel ref)
fn parse_asyncapi_channels(
    root: &serde_json::Map<String, Value>,
) -> Result<Vec<Operation>, ParseError> {
    let mut operations = Vec::new();

    // Parse channels first to build a lookup map
    let channels = root.get("channels").and_then(|v| v.as_object());
    let ops = root.get("operations").and_then(|v| v.as_object());

    // If no operations defined, return empty
    let ops = match ops {
        Some(o) => o,
        None => return Ok(operations),
    };

    // Build channel lookup: channel_name -> (address, messages, parameters, bindings)
    let channel_lookup = build_channel_lookup(channels);

    for (op_id, op_value) in ops {
        let op_obj = op_value.as_object().ok_or_else(|| {
            ParseError::SchemaError(format!("operation '{}' must be an object", op_id))
        })?;

        // Extract action (send/receive)
        let action = op_obj
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ParseError::SchemaError(format!("operation '{}' missing 'action' field", op_id))
            })?;

        // Normalize action to uppercase for consistency with HTTP methods
        let method = match action {
            "send" => "SEND",
            "receive" => "RECEIVE",
            other => {
                return Err(ParseError::SchemaError(format!(
                    "operation '{}' has invalid action '{}' (must be 'send' or 'receive')",
                    op_id, other
                )))
            }
        }
        .to_string();

        // Resolve channel reference
        let (address, channel_messages, channel_params, channel_bindings) =
            resolve_channel_ref(op_obj, &channel_lookup)?;

        // Parse operation-level messages (may override or filter channel messages)
        let messages = parse_operation_messages(op_obj, &channel_messages);

        // For SEND operations, create a request body from the first message payload
        let request_body = if method == "SEND" && !messages.is_empty() {
            messages.first().and_then(|msg| {
                msg.payload.as_ref().map(|schema| {
                    let content_type = msg
                        .content_type
                        .clone()
                        .unwrap_or_else(|| "application/json".to_string());
                    let mut content = BTreeMap::new();
                    content.insert(
                        content_type,
                        ContentSchema {
                            schema: Some(schema.clone()),
                        },
                    );
                    RequestBody {
                        required: true,
                        content,
                    }
                })
            })
        } else {
            None
        };

        // Merge channel and operation-level bindings
        let mut bindings = channel_bindings;
        if let Some(op_bindings) = op_obj.get("bindings").and_then(|v| v.as_object()) {
            for (protocol, config) in op_bindings {
                bindings.insert(protocol.clone(), config.clone());
            }
        }

        // Extract dispatch config
        let dispatch = extract_dispatch(op_obj);

        // Extract middlewares
        let middlewares = if op_obj.contains_key("x-barbacane-middlewares") {
            Some(extract_middlewares(op_obj))
        } else {
            None
        };

        // Extract deprecated and sunset
        let deprecated = op_obj
            .get("deprecated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let sunset = op_obj
            .get("x-sunset")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let extensions = extract_extensions(op_obj);

        operations.push(Operation {
            path: address,
            method,
            operation_id: Some(op_id.clone()),
            parameters: channel_params,
            request_body,
            dispatch,
            middlewares,
            deprecated,
            sunset,
            extensions,
            messages,
            bindings,
        });
    }

    Ok(operations)
}

/// Channel info: (address, messages, parameters, bindings).
type ChannelInfo = (
    String,
    Vec<Message>,
    Vec<Parameter>,
    BTreeMap<String, Value>,
);

/// Build a lookup map of channel names to their definitions.
fn build_channel_lookup(
    channels: Option<&serde_json::Map<String, Value>>,
) -> BTreeMap<String, ChannelInfo> {
    let mut lookup = BTreeMap::new();

    let channels = match channels {
        Some(c) => c,
        None => return lookup,
    };

    for (name, channel_value) in channels {
        let channel_obj = match channel_value.as_object() {
            Some(o) => o,
            None => continue,
        };

        // Extract address (defaults to channel name if not specified)
        let address = channel_obj
            .get("address")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| name.clone());

        // Parse messages
        let messages = parse_channel_messages(channel_obj);

        // Parse parameters
        let parameters = parse_channel_parameters(channel_obj);

        // Parse bindings
        let bindings = channel_obj
            .get("bindings")
            .and_then(|v| v.as_object())
            .map(|b| {
                b.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();

        lookup.insert(name.clone(), (address, messages, parameters, bindings));
    }

    lookup
}

/// Parse messages from a channel definition.
fn parse_channel_messages(channel: &serde_json::Map<String, Value>) -> Vec<Message> {
    let messages_obj = match channel.get("messages").and_then(|v| v.as_object()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    messages_obj
        .iter()
        .filter_map(|(name, msg_value)| {
            let msg_obj = msg_value.as_object()?;

            let payload = msg_obj.get("payload").cloned();

            let content_type = msg_obj
                .get("contentType")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let bindings = msg_obj
                .get("bindings")
                .and_then(|v| v.as_object())
                .map(|b| {
                    b.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();

            Some(Message {
                name: name.clone(),
                payload,
                content_type,
                bindings,
            })
        })
        .collect()
}

/// Parse parameters from a channel definition (for templated addresses).
fn parse_channel_parameters(channel: &serde_json::Map<String, Value>) -> Vec<Parameter> {
    let params = match channel.get("parameters").and_then(|v| v.as_object()) {
        Some(p) => p,
        None => return Vec::new(),
    };

    params
        .iter()
        .map(|(name, param_value)| {
            let param_obj = param_value.as_object();

            let schema = param_obj.and_then(|o| o.get("schema").cloned());

            // In AsyncAPI, channel parameters are always required
            Parameter {
                name: name.clone(),
                location: "path".to_string(),
                required: true,
                schema,
            }
        })
        .collect()
}

/// Resolve a channel reference from an operation.
fn resolve_channel_ref(
    op: &serde_json::Map<String, Value>,
    lookup: &BTreeMap<String, ChannelInfo>,
) -> Result<ChannelInfo, ParseError> {
    let channel = op
        .get("channel")
        .ok_or_else(|| ParseError::SchemaError("operation missing 'channel' field".into()))?;

    // Channel can be a $ref or inline
    if let Some(channel_obj) = channel.as_object() {
        if let Some(ref_str) = channel_obj.get("$ref").and_then(|v| v.as_str()) {
            // Parse $ref like "#/channels/userSignedUp"
            let channel_name = ref_str.strip_prefix("#/channels/").ok_or_else(|| {
                ParseError::SchemaError(format!(
                    "invalid channel $ref '{}' (expected #/channels/...)",
                    ref_str
                ))
            })?;

            lookup.get(channel_name).cloned().ok_or_else(|| {
                ParseError::SchemaError(format!(
                    "channel '{}' referenced but not defined",
                    channel_name
                ))
            })
        } else {
            // Inline channel definition
            let address = channel_obj
                .get("address")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();

            let messages = parse_channel_messages(channel_obj);
            let parameters = parse_channel_parameters(channel_obj);
            let bindings = channel_obj
                .get("bindings")
                .and_then(|v| v.as_object())
                .map(|b| {
                    b.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();

            Ok((address, messages, parameters, bindings))
        }
    } else {
        Err(ParseError::SchemaError(
            "operation 'channel' must be an object (either $ref or inline)".into(),
        ))
    }
}

/// Parse messages from an operation (may reference channel messages via $ref).
fn parse_operation_messages(
    op: &serde_json::Map<String, Value>,
    channel_messages: &[Message],
) -> Vec<Message> {
    // If operation has explicit messages array, use those
    if let Some(msgs) = op.get("messages").and_then(|v| v.as_array()) {
        msgs.iter()
            .filter_map(|msg| {
                if let Some(obj) = msg.as_object() {
                    if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str()) {
                        // Reference to channel message
                        // Format: "#/channels/channelName/messages/messageName"
                        let parts: Vec<&str> = ref_str.split('/').collect();
                        if parts.len() >= 5 && parts[3] == "messages" {
                            let msg_name = parts[4];
                            return channel_messages
                                .iter()
                                .find(|m| m.name == msg_name)
                                .cloned();
                        }
                    }
                    // Inline message definition
                    let name = obj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();
                    let payload = obj.get("payload").cloned();
                    let content_type = obj
                        .get("contentType")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let bindings = obj
                        .get("bindings")
                        .and_then(|v| v.as_object())
                        .map(|b| {
                            b.iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect::<BTreeMap<_, _>>()
                        })
                        .unwrap_or_default();

                    return Some(Message {
                        name,
                        payload,
                        content_type,
                        bindings,
                    });
                }
                None
            })
            .collect()
    } else {
        // Use all channel messages
        channel_messages.to_vec()
    }
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

    #[test]
    fn parse_deprecated_operation() {
        let yaml = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /old-endpoint:
    get:
      deprecated: true
      x-sunset: "Sat, 31 Dec 2025 23:59:59 GMT"
      x-barbacane-dispatch:
        name: mock
  /new-endpoint:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 2);

        // Check deprecated operation
        let old_op = spec
            .operations
            .iter()
            .find(|op| op.path == "/old-endpoint")
            .unwrap();
        assert!(old_op.deprecated);
        assert_eq!(
            old_op.sunset,
            Some("Sat, 31 Dec 2025 23:59:59 GMT".to_string())
        );

        // Check non-deprecated operation
        let new_op = spec
            .operations
            .iter()
            .find(|op| op.path == "/new-endpoint")
            .unwrap();
        assert!(!new_op.deprecated);
        assert!(new_op.sunset.is_none());
    }

    // ==================== AsyncAPI 3.x Tests ====================

    #[test]
    fn parse_minimal_asyncapi() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: User Events API
  version: "1.0.0"
channels:
  userSignedUp:
    address: user/signedup
    messages:
      UserSignedUpMessage:
        payload:
          type: object
          properties:
            userId:
              type: string
operations:
  processUserSignup:
    action: receive
    channel:
      $ref: '#/channels/userSignedUp'
    x-barbacane-dispatch:
      name: kafka
      config:
        topic: user-events
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.format, SpecFormat::AsyncApi);
        assert_eq!(spec.version, "3.0.0");
        assert_eq!(spec.title, "User Events API");
        assert_eq!(spec.operations.len(), 1);

        let op = &spec.operations[0];
        assert_eq!(op.path, "user/signedup");
        assert_eq!(op.method, "RECEIVE");
        assert_eq!(op.operation_id, Some("processUserSignup".to_string()));

        // Check dispatch config
        let dispatch = op.dispatch.as_ref().unwrap();
        assert_eq!(dispatch.name, "kafka");

        // Check messages
        assert_eq!(op.messages.len(), 1);
        assert_eq!(op.messages[0].name, "UserSignedUpMessage");
        assert!(op.messages[0].payload.is_some());
    }

    #[test]
    fn parse_asyncapi_send_operation() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Notification Service
  version: "1.0.0"
channels:
  notifications:
    address: notifications/{userId}
    parameters:
      userId:
        schema:
          type: string
    messages:
      NotificationMessage:
        contentType: application/json
        payload:
          type: object
          required:
            - title
            - body
          properties:
            title:
              type: string
            body:
              type: string
operations:
  sendNotification:
    action: send
    channel:
      $ref: '#/channels/notifications'
    x-barbacane-dispatch:
      name: nats
      config:
        subject: notifications
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];

        assert_eq!(op.method, "SEND");
        assert_eq!(op.path, "notifications/{userId}");
        assert_eq!(op.operation_id, Some("sendNotification".to_string()));

        // Check channel parameters
        assert_eq!(op.parameters.len(), 1);
        assert_eq!(op.parameters[0].name, "userId");
        assert_eq!(op.parameters[0].location, "path");
        assert!(op.parameters[0].required);

        // SEND operations should have request_body from message payload
        assert!(op.request_body.is_some());
        let body = op.request_body.as_ref().unwrap();
        assert!(body.required);
        assert!(body.content.contains_key("application/json"));

        // Check messages
        assert_eq!(op.messages.len(), 1);
        assert_eq!(
            op.messages[0].content_type,
            Some("application/json".to_string())
        );
    }

    #[test]
    fn parse_asyncapi_with_bindings() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Order Events
  version: "1.0.0"
channels:
  orderCreated:
    address: orders.created
    bindings:
      kafka:
        topic: order-events
        partitions: 10
        replicas: 3
    messages:
      OrderCreatedMessage:
        bindings:
          kafka:
            key:
              type: string
        payload:
          type: object
operations:
  handleOrderCreated:
    action: receive
    channel:
      $ref: '#/channels/orderCreated'
    bindings:
      kafka:
        groupId: order-processor
    x-barbacane-dispatch:
      name: kafka
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];

        // Check operation-level bindings (merged from channel and operation)
        assert!(op.bindings.contains_key("kafka"));
        let kafka_binding = op.bindings.get("kafka").unwrap();
        // Operation binding should override channel binding
        assert!(kafka_binding.get("groupId").is_some());

        // Check message bindings
        assert!(op.messages[0].bindings.contains_key("kafka"));
    }

    #[test]
    fn parse_asyncapi_inline_channel() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Inline Channel Test
  version: "1.0.0"
operations:
  inlineOp:
    action: receive
    channel:
      address: inline/topic
      messages:
        InlineMessage:
          payload:
            type: string
    x-barbacane-dispatch:
      name: mock
"#;
        let spec = parse_spec(yaml).unwrap();
        let op = &spec.operations[0];

        assert_eq!(op.path, "inline/topic");
        assert_eq!(op.messages.len(), 1);
        assert_eq!(op.messages[0].name, "InlineMessage");
    }

    #[test]
    fn parse_asyncapi_multiple_operations() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Multi-Op API
  version: "1.0.0"
channels:
  events:
    address: events
    messages:
      Event:
        payload:
          type: object
operations:
  publishEvent:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: kafka
  consumeEvent:
    action: receive
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: kafka
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.operations.len(), 2);

        let send_op = spec
            .operations
            .iter()
            .find(|op| op.method == "SEND")
            .unwrap();
        let recv_op = spec
            .operations
            .iter()
            .find(|op| op.method == "RECEIVE")
            .unwrap();

        assert_eq!(send_op.operation_id, Some("publishEvent".to_string()));
        assert_eq!(recv_op.operation_id, Some("consumeEvent".to_string()));
    }

    #[test]
    fn parse_asyncapi_global_middlewares() {
        let yaml = r#"
asyncapi: "3.0.0"
info:
  title: Middleware Test
  version: "1.0.0"
x-barbacane-middlewares:
  - name: auth
    config:
      type: jwt
channels:
  events:
    address: events
    messages:
      Event:
        payload:
          type: object
operations:
  handleEvent:
    action: receive
    channel:
      $ref: '#/channels/events'
    x-barbacane-dispatch:
      name: kafka
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.global_middlewares.len(), 1);
        assert_eq!(spec.global_middlewares[0].name, "auth");
    }

    #[test]
    fn reject_asyncapi_2() {
        let yaml = r#"
asyncapi: "2.6.0"
info:
  title: Old AsyncAPI
  version: "1.0.0"
channels: {}
"#;
        let result = parse_spec(yaml);
        assert!(matches!(result, Err(ParseError::SchemaError(_))));
    }
}
