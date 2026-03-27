//! MCP (Model Context Protocol) server support for Barbacane.
//!
//! Automatically generates MCP tools from compiled OpenAPI operations,
//! enabling AI agents to discover and call API endpoints via JSON-RPC 2.0.

pub mod jsonrpc;
pub mod session;
pub mod tools;

use std::collections::HashMap;
use std::time::Duration;

use barbacane_compiler::{CompiledOperation, McpConfig};

use self::jsonrpc::{
    JsonRpcRequest, JsonRpcResponse, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};
use self::session::SessionStore;
use self::tools::{McpTool, ToolEntry};

/// MCP protocol version supported by this implementation.
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Pre-computed MCP server state, constructed at Gateway load time.
pub struct McpServer {
    /// Pre-computed tool definitions.
    tool_entries: Vec<ToolEntry>,
    /// Maps tool name (operationId) → index in tool_entries.
    tool_index: HashMap<String, usize>,
    /// Session store for MCP clients.
    session_store: SessionStore,
    /// Server name from manifest.
    server_name: String,
    /// Server version from manifest.
    server_version: String,
}

/// Result of handling an MCP JSON-RPC request.
pub enum McpResult {
    /// A tools/call that needs dispatch through the middleware pipeline.
    NeedsDispatch {
        operation_index: usize,
        path: String,
        query: Option<String>,
        body: Option<Vec<u8>>,
        json_rpc_id: Option<serde_json::Value>,
    },
    /// Direct JSON-RPC response (all non-tools/call methods, or errors).
    Response {
        body: Vec<u8>,
        /// Session ID to set via Mcp-Session-Id header (only for initialize).
        session_id: Option<String>,
    },
    /// Notification — no response needed.
    NoResponse,
}

impl McpServer {
    /// Construct an McpServer from compiled operations and MCP config.
    pub fn new(operations: &[CompiledOperation], config: &McpConfig) -> Self {
        let tool_entries = tools::generate_tools(operations);
        let tool_index: HashMap<String, usize> = tool_entries
            .iter()
            .enumerate()
            .map(|(i, entry)| (entry.tool.name.clone(), i))
            .collect();

        Self {
            tool_entries,
            tool_index,
            session_store: SessionStore::new(Duration::from_secs(30 * 60)),
            server_name: config
                .server_name
                .clone()
                .unwrap_or_else(|| "Barbacane MCP Server".to_string()),
            server_version: config
                .server_version
                .clone()
                .unwrap_or_else(|| "1.0.0".to_string()),
        }
    }

    /// Handle an MCP JSON-RPC request.
    ///
    /// Single entry point — parses the request once and returns the appropriate result.
    /// For `tools/call`, returns `NeedsDispatch` so the Gateway can route through
    /// the middleware pipeline. Everything else is handled directly.
    pub fn handle_request(&self, body: &[u8], session_id: Option<&str>) -> McpResult {
        let req: JsonRpcRequest = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(_) => {
                let resp = JsonRpcResponse::error(None, PARSE_ERROR, "invalid JSON");
                return McpResult::Response {
                    body: serde_json::to_vec(&resp).unwrap_or_default(),
                    session_id: None,
                };
            }
        };

        if req.jsonrpc != "2.0" {
            let resp =
                JsonRpcResponse::error(req.id.clone(), INVALID_REQUEST, "jsonrpc must be \"2.0\"");
            return McpResult::Response {
                body: serde_json::to_vec(&resp).unwrap_or_default(),
                session_id: None,
            };
        }

        let is_notification = req.id.is_none();

        // Session validation for non-initialize, non-notification requests
        if req.method != "initialize" && !is_notification {
            if let Some(sid) = session_id {
                if !self.session_store.touch(sid) {
                    let resp = JsonRpcResponse::error(
                        req.id,
                        INVALID_REQUEST,
                        "invalid or expired session",
                    );
                    return McpResult::Response {
                        body: serde_json::to_vec(&resp).unwrap_or_default(),
                        session_id: None,
                    };
                }
            }
        }

        match req.method.as_str() {
            "initialize" => {
                let client_info = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("clientInfo").cloned());
                let new_session_id = self.session_store.create(client_info);
                let resp = JsonRpcResponse::success(
                    req.id,
                    serde_json::json!({
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {
                            "tools": {
                                "listChanged": false
                            }
                        },
                        "serverInfo": {
                            "name": self.server_name,
                            "version": self.server_version
                        }
                    }),
                );
                McpResult::Response {
                    body: serde_json::to_vec(&resp).unwrap_or_default(),
                    session_id: Some(new_session_id),
                }
            }

            "notifications/initialized" => McpResult::NoResponse,

            "ping" => {
                let resp = JsonRpcResponse::success(req.id, serde_json::json!({}));
                McpResult::Response {
                    body: serde_json::to_vec(&resp).unwrap_or_default(),
                    session_id: None,
                }
            }

            "tools/list" => {
                let tools: Vec<&McpTool> = self.tool_entries.iter().map(|e| &e.tool).collect();
                let resp = JsonRpcResponse::success(req.id, serde_json::json!({ "tools": tools }));
                McpResult::Response {
                    body: serde_json::to_vec(&resp).unwrap_or_default(),
                    session_id: None,
                }
            }

            "tools/call" => self.handle_tools_call(req),

            _ => {
                if is_notification {
                    McpResult::NoResponse
                } else {
                    let resp = JsonRpcResponse::error(
                        req.id,
                        METHOD_NOT_FOUND,
                        format!("unknown method: {}", req.method),
                    );
                    McpResult::Response {
                        body: serde_json::to_vec(&resp).unwrap_or_default(),
                        session_id: None,
                    }
                }
            }
        }
    }

    /// Handle a tools/call request: validate the tool name, decompose arguments,
    /// and return NeedsDispatch for the Gateway to execute.
    fn handle_tools_call(&self, req: JsonRpcRequest) -> McpResult {
        let params = req.params.unwrap_or(serde_json::json!({}));
        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                let resp = JsonRpcResponse::error(req.id, INVALID_PARAMS, "missing tool name");
                return McpResult::Response {
                    body: serde_json::to_vec(&resp).unwrap_or_default(),
                    session_id: None,
                };
            }
        };

        let idx = match self.tool_index.get(tool_name) {
            Some(&i) => i,
            None => {
                let resp = JsonRpcResponse::error(
                    req.id,
                    INVALID_PARAMS,
                    format!("unknown tool: {}", tool_name),
                );
                return McpResult::Response {
                    body: serde_json::to_vec(&resp).unwrap_or_default(),
                    session_id: None,
                };
            }
        };

        let entry = &self.tool_entries[idx];
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        let (path, query, body) = tools::decompose_arguments(entry, &arguments);

        McpResult::NeedsDispatch {
            operation_index: entry.operation_index,
            path,
            query,
            body,
            json_rpc_id: req.id,
        }
    }

    /// Remove a session (for DELETE requests).
    pub fn remove_session(&self, session_id: &str) {
        self.session_store.remove(session_id);
    }

    /// Evict expired sessions.
    pub fn evict_expired_sessions(&self) {
        self.session_store.evict_expired();
    }
}

/// Format a dispatch result as an MCP tools/call response.
pub fn format_tool_result(
    id: Option<serde_json::Value>,
    status: u16,
    body: Option<&[u8]>,
) -> Vec<u8> {
    let text = body.and_then(|b| std::str::from_utf8(b).ok()).unwrap_or("");
    let is_error = status >= 400;

    let resp = JsonRpcResponse::success(
        id,
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": text
            }],
            "isError": is_error
        }),
    );
    serde_json::to_vec(&resp).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use barbacane_compiler::{CompiledOperation, DispatchConfig};
    use std::collections::BTreeMap;

    fn make_test_ops() -> Vec<CompiledOperation> {
        vec![
            CompiledOperation {
                index: 0,
                path: "/health".to_string(),
                method: "GET".to_string(),
                operation_id: Some("getHealth".to_string()),
                summary: Some("Health check".to_string()),
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
                mcp_enabled: Some(true),
                mcp_description: None,
            },
            CompiledOperation {
                index: 1,
                path: "/secret".to_string(),
                method: "GET".to_string(),
                operation_id: Some("getSecret".to_string()),
                summary: Some("Not exposed".to_string()),
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
                mcp_enabled: None,
                mcp_description: None,
            },
        ]
    }

    fn make_server() -> McpServer {
        let ops = make_test_ops();
        let config = McpConfig {
            enabled: true,
            server_name: Some("Test API".to_string()),
            server_version: Some("1.0.0".to_string()),
        };
        McpServer::new(&ops, &config)
    }

    #[test]
    fn initialize_returns_capabilities_and_session() {
        let server = make_server();
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let result = server.handle_request(body, None);
        match result {
            McpResult::Response {
                body: resp_body,
                session_id,
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                assert_eq!(json["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
                assert!(json["result"]["capabilities"]["tools"].is_object());
                assert_eq!(json["result"]["serverInfo"]["name"], "Test API");
                assert!(session_id.is_some());
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn tools_list_returns_mcp_enabled_tools() {
        let server = make_server();
        let body = br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        match server.handle_request(body, None) {
            McpResult::Response {
                body: resp_body, ..
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                let tools = json["result"]["tools"].as_array().expect("tools array");
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0]["name"], "getHealth");
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn tools_call_returns_needs_dispatch() {
        let server = make_server();
        let body =
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"getHealth"}}"#;
        match server.handle_request(body, None) {
            McpResult::NeedsDispatch {
                operation_index,
                path,
                ..
            } => {
                assert_eq!(operation_index, 0);
                assert_eq!(path, "/health");
            }
            _ => panic!("expected NeedsDispatch"),
        }
    }

    #[test]
    fn tools_call_unknown_tool() {
        let server = make_server();
        let body =
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nonexistent"}}"#;
        match server.handle_request(body, None) {
            McpResult::Response {
                body: resp_body, ..
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                assert_eq!(json["error"]["code"], INVALID_PARAMS);
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn unknown_method_returns_error() {
        let server = make_server();
        let body = br#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#;
        match server.handle_request(body, None) {
            McpResult::Response {
                body: resp_body, ..
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                assert_eq!(json["error"]["code"], METHOD_NOT_FOUND);
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn notification_returns_no_response() {
        let server = make_server();
        let body = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(matches!(
            server.handle_request(body, None),
            McpResult::NoResponse
        ));
    }

    #[test]
    fn ping_returns_empty_result() {
        let server = make_server();
        let body = br#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#;
        match server.handle_request(body, None) {
            McpResult::Response {
                body: resp_body, ..
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                assert_eq!(json["result"], serde_json::json!({}));
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let server = make_server();
        match server.handle_request(b"not json", None) {
            McpResult::Response {
                body: resp_body, ..
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                assert_eq!(json["error"]["code"], PARSE_ERROR);
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn format_tool_result_success() {
        let result =
            format_tool_result(Some(serde_json::json!(1)), 200, Some(br#"{"status":"ok"}"#));
        let json: serde_json::Value = serde_json::from_slice(&result).expect("valid json");
        assert_eq!(json["result"]["isError"], false);
        assert_eq!(json["result"]["content"][0]["text"], r#"{"status":"ok"}"#);
    }

    #[test]
    fn format_tool_result_error() {
        let result = format_tool_result(Some(serde_json::json!(1)), 404, Some(b"not found"));
        let json: serde_json::Value = serde_json::from_slice(&result).expect("valid json");
        assert_eq!(json["result"]["isError"], true);
    }

    #[test]
    fn session_validation_invalid_session() {
        let server = make_server();
        let body = br#"{"jsonrpc":"2.0","id":10,"method":"tools/list"}"#;
        // Pass an invalid session ID
        match server.handle_request(body, Some("invalid-session-id")) {
            McpResult::Response {
                body: resp_body, ..
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                assert_eq!(json["error"]["code"], INVALID_REQUEST);
                assert!(json["error"]["message"]
                    .as_str()
                    .expect("message")
                    .contains("expired"));
            }
            _ => panic!("expected Response with error"),
        }
    }

    #[test]
    fn session_validation_valid_session() {
        let server = make_server();
        // Initialize to get a session
        let init_body = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let session_id = match server.handle_request(init_body, None) {
            McpResult::Response { session_id, .. } => session_id.expect("session id"),
            _ => panic!("expected Response"),
        };

        // Use the session for tools/list
        let body = br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        match server.handle_request(body, Some(&session_id)) {
            McpResult::Response {
                body: resp_body, ..
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                assert!(json["result"]["tools"].is_array());
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn tools_call_missing_name_field() {
        let server = make_server();
        let body = br#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{}}"#;
        match server.handle_request(body, None) {
            McpResult::Response {
                body: resp_body, ..
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(&resp_body).expect("valid json");
                assert_eq!(json["error"]["code"], INVALID_PARAMS);
                assert!(json["error"]["message"]
                    .as_str()
                    .expect("message")
                    .contains("missing tool name"));
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn tools_call_with_arguments() {
        // Build a server with an operation that has path params
        let ops = vec![CompiledOperation {
            index: 0,
            path: "/users/{id}".to_string(),
            method: "GET".to_string(),
            operation_id: Some("getUser".to_string()),
            summary: Some("Get user".to_string()),
            description: None,
            parameters: vec![barbacane_compiler::Parameter {
                name: "id".to_string(),
                location: "path".to_string(),
                required: true,
                schema: None,
            }],
            request_body: None,
            dispatch: DispatchConfig {
                name: "http-upstream".to_string(),
                config: serde_json::json!({}),
            },
            middlewares: vec![],
            deprecated: false,
            sunset: None,
            messages: vec![],
            bindings: BTreeMap::new(),
            responses: BTreeMap::new(),
            mcp_enabled: Some(true),
            mcp_description: None,
        }];
        let config = McpConfig {
            enabled: true,
            server_name: None,
            server_version: None,
        };
        let server = McpServer::new(&ops, &config);

        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"getUser","arguments":{"id":"123"}}}"#;
        match server.handle_request(body, None) {
            McpResult::NeedsDispatch {
                operation_index,
                path,
                ..
            } => {
                assert_eq!(operation_index, 0);
                assert_eq!(path, "/users/123");
            }
            _ => panic!("expected NeedsDispatch"),
        }
    }
}
