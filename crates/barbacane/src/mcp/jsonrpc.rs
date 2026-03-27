use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 request (MCP uses JSON-RPC as its wire format).
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// Standard JSON-RPC error codes
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;

impl JsonRpcResponse {
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<serde_json::Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(serde_json::json!(1)));
    }

    #[test]
    fn parse_notification_no_id() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).expect("should parse");
        assert!(req.id.is_none());
        assert!(req.params.is_none());
    }

    #[test]
    fn parse_request_with_string_id() {
        let json = r#"{"jsonrpc":"2.0","id":"abc","method":"tools/list"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.id, Some(serde_json::json!("abc")));
    }

    #[test]
    fn parse_invalid_json() {
        let result = serde_json::from_str::<JsonRpcRequest>("not json");
        assert!(result.is_err());
    }

    #[test]
    fn success_response_serialization() {
        let resp = JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!({}));
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert!(json.get("error").is_none());
    }

    #[test]
    fn error_response_serialization() {
        let resp =
            JsonRpcResponse::error(Some(serde_json::json!(1)), METHOD_NOT_FOUND, "not found");
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(json["error"]["message"], "not found");
        assert!(json.get("result").is_none());
    }
}
