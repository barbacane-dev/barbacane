//! MCP server integration tests.
//!
//! Tests the full MCP JSON-RPC 2.0 flow through the gateway:
//! initialization, tool listing, tool calling, session management.
//!
//! Run with: `cargo test -p barbacane-test -- mcp`

use barbacane_test::{assert_status, TestGateway};

fn fixture(name: &str) -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

async fn mcp_gateway() -> TestGateway {
    TestGateway::from_spec(&fixture("mcp.yaml"))
        .await
        .expect("mcp fixture failed to compile")
}

/// POST a JSON-RPC request to the MCP endpoint, optionally with a session ID.
async fn mcp_post(
    gateway: &TestGateway,
    body: &str,
    session_id: Option<&str>,
) -> reqwest::Response {
    let mut builder = gateway
        .request_builder(reqwest::Method::POST, "/__barbacane/mcp")
        .header("content-type", "application/json")
        .body(body.to_string());

    if let Some(sid) = session_id {
        builder = builder.header("mcp-session-id", sid);
    }

    builder.send().await.expect("request failed")
}

/// Extract the session ID from an initialize response.
fn extract_session_id(resp: &reqwest::Response) -> String {
    resp.headers()
        .get("mcp-session-id")
        .expect("missing mcp-session-id header")
        .to_str()
        .expect("invalid header value")
        .to_string()
}

// =========================================================================
// Initialization
// =========================================================================

#[tokio::test]
async fn test_mcp_initialize() {
    let gw = mcp_gateway().await;
    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"test","version":"1.0"}}}"#,
        None,
    )
    .await;

    assert_eq!(resp.status(), 200);
    let session_id = extract_session_id(&resp);
    assert!(!session_id.is_empty());

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(body["result"]["serverInfo"]["name"], "MCP Test Server");
    assert_eq!(body["result"]["serverInfo"]["version"], "1.0.0");
    assert!(body["result"]["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn test_mcp_notification_initialized() {
    let gw = mcp_gateway().await;
    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        None,
    )
    .await;

    // Notifications return 204 No Content
    assert_eq!(resp.status(), 204);
}

// =========================================================================
// Tool listing
// =========================================================================

#[tokio::test]
async fn test_mcp_tools_list() {
    let gw = mcp_gateway().await;

    // Initialize first to get a session
    let init_resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;
    let session_id = extract_session_id(&init_resp);

    // List tools
    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        Some(&session_id),
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().expect("tools array");

    // mcp.yaml has 4 operations, but resetDatabase has mcp enabled: false
    assert_eq!(tools.len(), 3);

    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"getHealth"));
    assert!(tool_names.contains(&"getUser"));
    assert!(tool_names.contains(&"createUser"));
    assert!(!tool_names.contains(&"resetDatabase"));
}

#[tokio::test]
async fn test_mcp_tools_have_input_schema() {
    let gw = mcp_gateway().await;

    let init_resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;
    let session_id = extract_session_id(&init_resp);

    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        Some(&session_id),
    )
    .await;

    let body: serde_json::Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().unwrap();

    // getUser should have path param "id" and query param "fields" in its input schema
    let get_user = tools.iter().find(|t| t["name"] == "getUser").unwrap();
    let schema = &get_user["inputSchema"];
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["id"].is_object());
    // id should be required
    let required = schema["required"].as_array().unwrap();
    assert!(required.iter().any(|r| r == "id"));

    // createUser should have body schema with name and email
    let create_user = tools.iter().find(|t| t["name"] == "createUser").unwrap();
    let schema = &create_user["inputSchema"];
    assert!(schema["properties"]["name"].is_object());
    assert!(schema["properties"]["email"].is_object());
}

// =========================================================================
// Tool calling
// =========================================================================

#[tokio::test]
async fn test_mcp_tools_call_simple() {
    let gw = mcp_gateway().await;

    let init_resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;
    let session_id = extract_session_id(&init_resp);

    // Call getHealth — dispatches through mock, returns {"status":"ok"}
    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"getHealth"}}"#,
        Some(&session_id),
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], 2);
    assert_eq!(body["result"]["isError"], false);

    let content = &body["result"]["content"][0];
    assert_eq!(content["type"], "text");
    let text: serde_json::Value =
        serde_json::from_str(content["text"].as_str().unwrap()).expect("valid json in text");
    assert_eq!(text["status"], "ok");
}

#[tokio::test]
async fn test_mcp_tools_call_with_path_params() {
    let gw = mcp_gateway().await;

    let init_resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;
    let session_id = extract_session_id(&init_resp);

    // Call getUser with path param id=42
    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"getUser","arguments":{"id":"42"}}}"#,
        Some(&session_id),
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false);

    // Verify the tool call dispatched successfully and returned content
    let content = &body["result"]["content"];
    assert!(content.is_array());
    assert_eq!(content[0]["type"], "text");
    // The mock dispatcher returned a response (path param substitution
    // is tested separately in mock unit tests)
    assert!(!content[0]["text"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn test_mcp_tools_call_with_body() {
    let gw = mcp_gateway().await;

    let init_resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;
    let session_id = extract_session_id(&init_resp);

    // Call createUser with body arguments
    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"createUser","arguments":{"name":"Alice","email":"alice@example.com"}}}"#,
        Some(&session_id),
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false);

    // Mock returns 201 with static body
    let text: serde_json::Value =
        serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["id"], "new-user-1");
}

#[tokio::test]
async fn test_mcp_tools_call_unknown_tool() {
    let gw = mcp_gateway().await;

    let init_resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;
    let session_id = extract_session_id(&init_resp);

    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nonexistent"}}"#,
        Some(&session_id),
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"]["code"].is_number());
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));
}

// =========================================================================
// Ping
// =========================================================================

#[tokio::test]
async fn test_mcp_ping() {
    let gw = mcp_gateway().await;

    let init_resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;
    let session_id = extract_session_id(&init_resp);

    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":10,"method":"ping"}"#,
        Some(&session_id),
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"], serde_json::json!({}));
}

// =========================================================================
// Error handling
// =========================================================================

#[tokio::test]
async fn test_mcp_invalid_json() {
    let gw = mcp_gateway().await;
    let resp = mcp_post(&gw, "not json at all", None).await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32700); // PARSE_ERROR
}

#[tokio::test]
async fn test_mcp_unknown_method() {
    let gw = mcp_gateway().await;

    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
        None,
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32601); // METHOD_NOT_FOUND
}

#[tokio::test]
async fn test_mcp_method_not_allowed() {
    let gw = mcp_gateway().await;

    let resp = gw.get("/__barbacane/mcp").await.unwrap();
    assert_eq!(resp.status(), 405);
}

// =========================================================================
// Session management
// =========================================================================

#[tokio::test]
async fn test_mcp_invalid_session_rejected() {
    let gw = mcp_gateway().await;

    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        Some("invalid-session-id"),
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"]["code"].is_number());
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("expired"));
}

#[tokio::test]
async fn test_mcp_delete_session() {
    let gw = mcp_gateway().await;

    // Initialize to get a session
    let init_resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;
    let session_id = extract_session_id(&init_resp);

    // Delete the session
    let resp = gw
        .request_builder(reqwest::Method::DELETE, "/__barbacane/mcp")
        .header("mcp-session-id", &session_id)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Session should now be invalid
    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        Some(&session_id),
    )
    .await;

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"]["code"].is_number());
}

// =========================================================================
// MCP disabled
// =========================================================================

#[tokio::test]
async fn test_mcp_disabled_returns_404() {
    // Use mock.yaml which has no x-barbacane-mcp
    let gw = TestGateway::from_spec(&fixture("mock.yaml"))
        .await
        .expect("mock fixture failed to compile");

    let resp = mcp_post(
        &gw,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    )
    .await;

    assert_status(resp, 404).await;
}
