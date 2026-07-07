use barbacane_compiler::CompiledOperation;
use serde::Serialize;
use std::collections::BTreeMap;

/// An MCP tool declaration generated from a compiled operation.
#[derive(Debug, Clone, Serialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

/// Metadata linking an MCP tool to its compiled operation.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub tool: McpTool,
    /// Index into the Gateway's operations array.
    pub operation_index: usize,
    /// The HTTP method for this operation.
    pub method: String,
    /// The path template for this operation.
    pub path: String,
    /// Parameters needed for argument decomposition (path, query).
    pub parameters: Vec<barbacane_compiler::Parameter>,
}

/// Generate MCP tools from compiled operations.
///
/// Only includes operations where `mcp_enabled == Some(true)` and `operation_id` is present.
pub fn generate_tools(operations: &[CompiledOperation]) -> Vec<ToolEntry> {
    operations
        .iter()
        .filter(|op| op.mcp_enabled == Some(true) && op.operation_id.is_some())
        .map(|op| {
            let name = op.operation_id.clone().expect("filtered above");
            let description = build_description(op);
            let input_schema = build_input_schema(op);
            let output_schema = build_output_schema(op);

            ToolEntry {
                tool: McpTool {
                    name,
                    description,
                    input_schema,
                    output_schema,
                },
                operation_index: op.index,
                method: op.method.clone(),
                path: op.path.clone(),
                parameters: op.parameters.clone(),
            }
        })
        .collect()
}

/// Build the tool description from MCP override, summary, or description.
fn build_description(op: &CompiledOperation) -> String {
    if let Some(ref desc) = op.mcp_description {
        return desc.clone();
    }
    if let Some(ref summary) = op.summary {
        return summary.clone();
    }
    if let Some(ref description) = op.description {
        return description.clone();
    }
    format!("{} {}", op.method, op.path)
}

/// Build the MCP tool inputSchema by merging path params, query params, and body schema.
fn build_input_schema(op: &CompiledOperation) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    // Add path and query parameters
    for param in &op.parameters {
        if param.location != "path" && param.location != "query" {
            continue;
        }
        let schema = param
            .schema
            .clone()
            .unwrap_or(serde_json::json!({"type": "string"}));
        properties.insert(param.name.clone(), schema);
        if param.required {
            required.push(serde_json::Value::String(param.name.clone()));
        }
    }

    // Merge body schema properties
    if let Some(ref body) = op.request_body {
        if let Some(content) = body.content.get("application/json") {
            if let Some(ref schema) = content.schema {
                merge_body_schema(&mut properties, &mut required, schema);
            }
        }
    }

    let mut schema = serde_json::json!({
        "type": "object",
        "properties": serde_json::Value::Object(properties),
    });

    if !required.is_empty() {
        schema["required"] = serde_json::Value::Array(required);
    }

    schema
}

/// Merge a request body JSON Schema's properties into the tool's inputSchema.
fn merge_body_schema(
    properties: &mut serde_json::Map<String, serde_json::Value>,
    required: &mut Vec<serde_json::Value>,
    body_schema: &serde_json::Value,
) {
    // If the body schema has properties, merge them
    if let Some(body_props) = body_schema.get("properties").and_then(|v| v.as_object()) {
        for (key, val) in body_props {
            properties.insert(key.clone(), val.clone());
        }
    }

    // Merge required fields from body schema
    if let Some(body_required) = body_schema.get("required").and_then(|v| v.as_array()) {
        for r in body_required {
            if !required.contains(r) {
                required.push(r.clone());
            }
        }
    }

    // If the body schema has no properties (e.g., it's a raw type), wrap it as "body"
    if body_schema.get("properties").is_none() && body_schema.get("type").is_some() {
        properties.insert("body".to_string(), body_schema.clone());
    }
}

/// Build the outputSchema from the operation's response definitions.
///
/// Uses the 200 response's application/json schema, falling back to the first 2xx.
fn build_output_schema(op: &CompiledOperation) -> Option<serde_json::Value> {
    // Try 200 first
    if let Some(resp) = op.responses.get("200") {
        if let Some(content) = resp.content.get("application/json") {
            return content.schema.clone();
        }
    }

    // Fall back to first 2xx response with a JSON schema
    for (status, resp) in &op.responses {
        if status.starts_with('2') {
            if let Some(content) = resp.content.get("application/json") {
                if content.schema.is_some() {
                    return content.schema.clone();
                }
            }
        }
    }

    None
}

/// Decomposed HTTP request components: (resolved_path, query_string, body_json).
type DecomposedRequest = (String, Option<String>, Option<Vec<u8>>);

/// Decompose MCP tool call arguments back into HTTP request components.
///
/// Returns `Ok((resolved_path, query_string, body_json))`, or `Err(message)`
/// when a path-parameter value is unsafe.
///
/// MCP callers bypass the HTTP router, which normally guarantees a non-wildcard
/// path parameter is a single URL segment (it splits the request line on `/`).
/// Substituting a raw MCP value straight into the path template would let a
/// caller inject `/` or `..` and reach an upstream resource never exposed as a
/// tool, or forge the `request.path` that prefix-based authz middleware inspects
/// (MCP-1). We therefore validate + percent-encode each substituted segment:
///   * non-wildcard `{name}` — reject `/`, `\`, `?`, `#`, and any `..`; the
///     value must be a single segment, then percent-encode it;
///   * wildcard `{name+}` — may span segments (`/` allowed), but reject `?`,
///     `#`, `\`, and any `..` path segment so it cannot traverse upward.
pub fn decompose_arguments(
    entry: &ToolEntry,
    arguments: &serde_json::Value,
) -> Result<DecomposedRequest, String> {
    let args = arguments.as_object().cloned().unwrap_or_default();

    let mut path = entry.path.clone();
    let mut query_parts: Vec<String> = Vec::new();
    let mut consumed_keys: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Substitute path parameters
    for param in &entry.parameters {
        if param.location == "path" {
            if let Some(val) = args.get(&param.name) {
                let val_str = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let wildcard_placeholder = format!("{{{}+}}", param.name);
                let is_wildcard = path.contains(&wildcard_placeholder);
                let substituted = sanitize_path_param(&param.name, &val_str, is_wildcard)?;
                if is_wildcard {
                    path = path.replace(&wildcard_placeholder, &substituted);
                } else {
                    path = path.replace(&format!("{{{}}}", param.name), &substituted);
                }
                consumed_keys.insert(param.name.clone());
            }
        } else if param.location == "query" {
            if let Some(val) = args.get(&param.name) {
                let val_str = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                // Percent-encode key and value to avoid breaking on & or =
                query_parts.push(format!(
                    "{}={}",
                    percent_encode(&param.name),
                    percent_encode(&val_str)
                ));
                consumed_keys.insert(param.name.clone());
            }
        }
    }

    let query = if query_parts.is_empty() {
        None
    } else {
        Some(query_parts.join("&"))
    };

    // Remaining arguments become the body
    let remaining: BTreeMap<String, serde_json::Value> = args
        .into_iter()
        .filter(|(k, _)| !consumed_keys.contains(k))
        .collect();

    let body = if remaining.is_empty() {
        None
    } else {
        serde_json::to_vec(&remaining).ok()
    };

    Ok((path, query, body))
}

/// Validate and encode a value being substituted into a path template segment.
///
/// Returns the substitution string, or an error message if the value would
/// break out of its intended segment(s). See [`decompose_arguments`] for the
/// rationale (MCP-1).
fn sanitize_path_param(name: &str, value: &str, is_wildcard: bool) -> Result<String, String> {
    // Control characters (incl. CR/LF) are never valid in a path.
    if value.chars().any(|c| c.is_control()) {
        return Err(format!(
            "path parameter '{name}' contains control characters"
        ));
    }
    if value.contains(['?', '#', '\\']) {
        return Err(format!(
            "path parameter '{name}' contains a disallowed character (?, #, or \\)"
        ));
    }
    if is_wildcard {
        // Wildcards may span segments; reject upward traversal in any segment.
        if value.split('/').any(|seg| seg == "..") {
            return Err(format!(
                "path parameter '{name}' contains a '..' path segment"
            ));
        }
        Ok(value.to_string())
    } else {
        // Non-wildcard params must be a single segment: no '/', no '..'.
        if value.contains('/') {
            return Err(format!("path parameter '{name}' must not contain '/'"));
        }
        if value == ".." || value == "." {
            return Err(format!("path parameter '{name}' must not be '.' or '..'"));
        }
        Ok(percent_encode(value))
    }
}

/// Minimal percent-encoding for query string components.
fn percent_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push('%');
                result.push(char::from(HEX[(b >> 4) as usize]));
                result.push(char::from(HEX[(b & 0x0f) as usize]));
            }
        }
    }
    result
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

#[cfg(test)]
mod tests {
    use super::*;
    use barbacane_compiler::{
        ContentSchema, DispatchConfig, Parameter, RequestBody, ResponseContent,
    };

    fn make_operation(
        index: usize,
        method: &str,
        path: &str,
        operation_id: Option<&str>,
        summary: Option<&str>,
        mcp_enabled: Option<bool>,
    ) -> CompiledOperation {
        CompiledOperation {
            index,
            path: path.to_string(),
            method: method.to_string(),
            operation_id: operation_id.map(|s| s.to_string()),
            summary: summary.map(|s| s.to_string()),
            description: None,
            parameters: vec![],
            request_body: None,
            dispatch: DispatchConfig {
                name: "mock".to_string(),
                config: serde_json::json!({}),
            },
            middlewares: vec![],
            deprecated: false,
            sunset: None,
            messages: vec![],
            bindings: BTreeMap::new(),
            responses: BTreeMap::new(),
            mcp_enabled,
            mcp_description: None,
        }
    }

    #[test]
    fn generate_tools_filters_by_mcp_enabled() {
        let ops = vec![
            make_operation(
                0,
                "GET",
                "/health",
                Some("getHealth"),
                Some("Health check"),
                Some(true),
            ),
            make_operation(1, "GET", "/secret", Some("getSecret"), Some("Secret"), None),
            make_operation(
                2,
                "POST",
                "/orders",
                Some("createOrder"),
                Some("Create order"),
                Some(true),
            ),
            make_operation(3, "GET", "/no-id", None, Some("No ID"), Some(true)),
        ];
        let tools = generate_tools(&ops);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].tool.name, "getHealth");
        assert_eq!(tools[1].tool.name, "createOrder");
    }

    #[test]
    fn input_schema_path_and_query_params() {
        let mut op = make_operation(
            0,
            "GET",
            "/users/{id}",
            Some("getUser"),
            Some("Get user"),
            Some(true),
        );
        op.parameters = vec![
            Parameter {
                name: "id".to_string(),
                location: "path".to_string(),
                required: true,
                schema: Some(serde_json::json!({"type": "string"})),
            },
            Parameter {
                name: "fields".to_string(),
                location: "query".to_string(),
                required: false,
                schema: Some(serde_json::json!({"type": "string"})),
            },
        ];
        let schema = build_input_schema(&op);
        let props = schema["properties"].as_object().expect("properties");
        assert!(props.contains_key("id"));
        assert!(props.contains_key("fields"));
        let required = schema["required"].as_array().expect("required");
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "id");
    }

    #[test]
    fn input_schema_with_body() {
        let mut op = make_operation(
            0,
            "POST",
            "/orders",
            Some("createOrder"),
            Some("Create order"),
            Some(true),
        );
        op.request_body = Some(RequestBody {
            required: true,
            content: BTreeMap::from([(
                "application/json".to_string(),
                ContentSchema {
                    schema: Some(serde_json::json!({
                        "type": "object",
                        "required": ["items"],
                        "properties": {
                            "items": {"type": "array"},
                            "note": {"type": "string"}
                        }
                    })),
                },
            )]),
        });
        let schema = build_input_schema(&op);
        let props = schema["properties"].as_object().expect("properties");
        assert!(props.contains_key("items"));
        assert!(props.contains_key("note"));
        let required = schema["required"].as_array().expect("required");
        assert!(required.contains(&serde_json::json!("items")));
    }

    #[test]
    fn output_schema_from_200_response() {
        let mut op = make_operation(
            0,
            "GET",
            "/health",
            Some("getHealth"),
            Some("Health"),
            Some(true),
        );
        op.responses = BTreeMap::from([(
            "200".to_string(),
            ResponseContent {
                content: BTreeMap::from([(
                    "application/json".to_string(),
                    ContentSchema {
                        schema: Some(
                            serde_json::json!({"type": "object", "properties": {"status": {"type": "string"}}}),
                        ),
                    },
                )]),
            },
        )]);
        let schema = build_output_schema(&op).expect("should have output schema");
        assert!(schema["properties"]["status"].is_object());
    }

    #[test]
    fn output_schema_none_when_no_responses() {
        let op = make_operation(
            0,
            "GET",
            "/health",
            Some("getHealth"),
            Some("Health"),
            Some(true),
        );
        assert!(build_output_schema(&op).is_none());
    }

    #[test]
    fn decompose_path_and_query_params() {
        let entry = ToolEntry {
            tool: McpTool {
                name: "getUser".to_string(),
                description: "Get user".to_string(),
                input_schema: serde_json::json!({}),
                output_schema: None,
            },
            operation_index: 0,
            method: "GET".to_string(),
            path: "/users/{id}".to_string(),
            parameters: vec![
                Parameter {
                    name: "id".to_string(),
                    location: "path".to_string(),
                    required: true,
                    schema: None,
                },
                Parameter {
                    name: "fields".to_string(),
                    location: "query".to_string(),
                    required: false,
                    schema: None,
                },
            ],
        };
        let args = serde_json::json!({"id": "123", "fields": "name,email"});
        let (path, query, body) = decompose_arguments(&entry, &args).expect("valid args");
        assert_eq!(path, "/users/123");
        assert_eq!(query, Some("fields=name%2Cemail".to_string()));
        assert!(body.is_none());
    }

    #[test]
    fn decompose_remaining_args_become_body() {
        let entry = ToolEntry {
            tool: McpTool {
                name: "createOrder".to_string(),
                description: "Create".to_string(),
                input_schema: serde_json::json!({}),
                output_schema: None,
            },
            operation_index: 0,
            method: "POST".to_string(),
            path: "/orders".to_string(),
            parameters: vec![],
        };
        let args = serde_json::json!({"items": [{"id": "a"}], "note": "rush"});
        let (path, query, body) = decompose_arguments(&entry, &args).expect("valid args");
        assert_eq!(path, "/orders");
        assert!(query.is_none());
        let body = body.expect("body should be present");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        assert!(parsed["items"].is_array());
    }

    #[test]
    fn percent_encode_special_chars() {
        assert_eq!(percent_encode("hello world"), "hello%20world");
        assert_eq!(percent_encode("a=b&c"), "a%3Db%26c");
        assert_eq!(percent_encode("simple"), "simple");
    }

    #[test]
    fn description_priority() {
        // mcp_description > summary > description > fallback
        let mut op = make_operation(0, "GET", "/a", Some("op"), None, Some(true));
        assert_eq!(build_description(&op), "GET /a");

        op.description = Some("detailed desc".to_string());
        assert_eq!(build_description(&op), "detailed desc");

        op.summary = Some("short summary".to_string());
        assert_eq!(build_description(&op), "short summary");

        op.mcp_description = Some("mcp override".to_string());
        assert_eq!(build_description(&op), "mcp override");
    }

    #[test]
    fn decompose_wildcard_path_param() {
        let entry = ToolEntry {
            tool: McpTool {
                name: "getFile".to_string(),
                description: "Get file".to_string(),
                input_schema: serde_json::json!({}),
                output_schema: None,
            },
            operation_index: 0,
            method: "GET".to_string(),
            path: "/files/{path+}".to_string(),
            parameters: vec![Parameter {
                name: "path".to_string(),
                location: "path".to_string(),
                required: true,
                schema: None,
            }],
        };
        let args = serde_json::json!({"path": "docs/2024/report.pdf"});
        let (path, query, body) = decompose_arguments(&entry, &args).expect("valid args");
        assert_eq!(path, "/files/docs/2024/report.pdf");
        assert!(query.is_none());
        assert!(body.is_none());
    }

    #[test]
    fn decompose_non_string_path_param() {
        let entry = ToolEntry {
            tool: McpTool {
                name: "getUser".to_string(),
                description: "Get user".to_string(),
                input_schema: serde_json::json!({}),
                output_schema: None,
            },
            operation_index: 0,
            method: "GET".to_string(),
            path: "/users/{id}".to_string(),
            parameters: vec![Parameter {
                name: "id".to_string(),
                location: "path".to_string(),
                required: true,
                schema: None,
            }],
        };
        // Numeric value instead of string
        let args = serde_json::json!({"id": 42});
        let (path, _, _) = decompose_arguments(&entry, &args).expect("valid args");
        assert_eq!(path, "/users/42");
    }

    #[test]
    fn decompose_missing_path_param_leaves_placeholder() {
        let entry = ToolEntry {
            tool: McpTool {
                name: "getUser".to_string(),
                description: "Get user".to_string(),
                input_schema: serde_json::json!({}),
                output_schema: None,
            },
            operation_index: 0,
            method: "GET".to_string(),
            path: "/users/{id}".to_string(),
            parameters: vec![Parameter {
                name: "id".to_string(),
                location: "path".to_string(),
                required: true,
                schema: None,
            }],
        };
        // Missing "id" argument
        let args = serde_json::json!({});
        let (path, _, _) = decompose_arguments(&entry, &args).expect("valid args");
        assert_eq!(path, "/users/{id}");
    }

    fn user_id_entry() -> ToolEntry {
        ToolEntry {
            tool: McpTool {
                name: "getUser".to_string(),
                description: "Get user".to_string(),
                input_schema: serde_json::json!({}),
                output_schema: None,
            },
            operation_index: 0,
            method: "GET".to_string(),
            path: "/users/{id}".to_string(),
            parameters: vec![Parameter {
                name: "id".to_string(),
                location: "path".to_string(),
                required: true,
                schema: None,
            }],
        }
    }

    #[test]
    fn decompose_rejects_traversal_in_non_wildcard_param() {
        // MCP-1: a non-wildcard path param must not break out of its segment.
        let entry = user_id_entry();
        for evil in [
            "../admin/secrets",
            "..",
            "a/b",
            "x?inject=1",
            "y#frag",
            "back\\slash",
        ] {
            let args = serde_json::json!({ "id": evil });
            assert!(
                decompose_arguments(&entry, &args).is_err(),
                "value {evil:?} must be rejected"
            );
        }
    }

    #[test]
    fn decompose_percent_encodes_non_wildcard_segment() {
        // Reserved but non-structural characters are encoded, not rejected, so
        // the value stays a single opaque segment.
        let entry = user_id_entry();
        let args = serde_json::json!({"id": "a b:c"});
        let (path, _, _) = decompose_arguments(&entry, &args).expect("encodable");
        assert_eq!(path, "/users/a%20b%3Ac");
    }

    #[test]
    fn decompose_rejects_traversal_segment_in_wildcard_param() {
        // Wildcards may contain '/', but not an upward-traversal segment.
        let entry = ToolEntry {
            tool: McpTool {
                name: "getFile".to_string(),
                description: "Get file".to_string(),
                input_schema: serde_json::json!({}),
                output_schema: None,
            },
            operation_index: 0,
            method: "GET".to_string(),
            path: "/files/{path+}".to_string(),
            parameters: vec![Parameter {
                name: "path".to_string(),
                location: "path".to_string(),
                required: true,
                schema: None,
            }],
        };
        let args = serde_json::json!({"path": "docs/../../etc/passwd"});
        assert!(decompose_arguments(&entry, &args).is_err());
        // A legitimate deep path with no traversal is still allowed.
        let ok = serde_json::json!({"path": "docs/2024/report.pdf"});
        assert!(decompose_arguments(&entry, &ok).is_ok());
    }

    #[test]
    fn percent_encode_utf8() {
        let encoded = percent_encode("café");
        assert!(encoded.contains("%C3%A9")); // é = 0xC3 0xA9 in UTF-8
    }
}
