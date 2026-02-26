//! Response transformer middleware plugin for Barbacane API gateway.
//!
//! Provides declarative response transformations for:
//! - Status code mapping (e.g., 200 → 201, 400 → 403)
//! - Headers (add, set, remove, rename)
//! - JSON body (add, remove, rename using JSON Pointer — RFC 6901)

use barbacane_plugin_sdk::prelude::*;
use jsonptr::{Assign, Delete, Pointer};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Configuration structs
// ---------------------------------------------------------------------------

/// Header transformation configuration.
#[derive(Debug, Clone, Deserialize, Default)]
struct HeaderConfig {
    #[serde(default)]
    add: BTreeMap<String, String>,

    #[serde(default)]
    set: BTreeMap<String, String>,

    #[serde(default)]
    remove: Vec<String>,

    #[serde(default)]
    rename: BTreeMap<String, String>,
}

/// Body transformation configuration (JSON Pointer).
#[derive(Debug, Clone, Deserialize, Default)]
struct BodyConfig {
    #[serde(default)]
    add: BTreeMap<String, String>,

    #[serde(default)]
    remove: Vec<String>,

    #[serde(default)]
    rename: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Plugin struct
// ---------------------------------------------------------------------------

/// Response transformer middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct ResponseTransformer {
    /// Status code mapping (original → replacement).
    #[serde(default)]
    status: Option<BTreeMap<String, u16>>,

    #[serde(default)]
    headers: Option<HeaderConfig>,

    #[serde(default)]
    body: Option<BodyConfig>,
}

impl ResponseTransformer {
    /// Pass through requests unchanged (response-transformer only).
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        Action::Continue(req)
    }

    /// Handle outgoing response — apply transformations.
    ///
    /// Transformations are applied in this order:
    /// 1. Status code mapping
    /// 2. Headers
    /// 3. Body
    pub fn on_response(&mut self, mut resp: Response) -> Response {
        if let Some(status_map) = &self.status {
            resp.status = transform_status(resp.status, status_map);
        }

        if let Some(header_config) = &self.headers {
            transform_headers(&mut resp.headers, header_config);
        }

        if let Some(body_config) = &self.body {
            resp.body = transform_body(&resp.body, body_config);
        }

        resp
    }
}

// ---------------------------------------------------------------------------
// Status code mapping
// ---------------------------------------------------------------------------

/// Map a status code using the configured mapping table.
///
/// Keys are stringified status codes (from JSON/YAML deserialization).
/// Unmapped codes pass through unchanged.
fn transform_status(status: u16, mapping: &BTreeMap<String, u16>) -> u16 {
    mapping.get(&status.to_string()).copied().unwrap_or(status)
}

// ---------------------------------------------------------------------------
// Header transformations
// ---------------------------------------------------------------------------

/// Transform response headers. Order: remove → rename → set → add.
fn transform_headers(headers: &mut BTreeMap<String, String>, config: &HeaderConfig) {
    for header_name in &config.remove {
        headers.remove(&header_name.to_lowercase());
    }

    for (old_name, new_name) in &config.rename {
        if let Some(value) = headers.remove(&old_name.to_lowercase()) {
            headers.insert(new_name.to_lowercase(), value);
        }
    }

    for (header_name, value) in &config.set {
        headers
            .entry(header_name.to_lowercase())
            .or_insert(value.clone());
    }

    for (header_name, value) in &config.add {
        headers.insert(header_name.to_lowercase(), value.clone());
    }
}

// ---------------------------------------------------------------------------
// Body transformations (JSON Pointer — RFC 6901)
// ---------------------------------------------------------------------------

/// Transform JSON body. Order: remove → rename → add.
///
/// Non-JSON bodies are returned unchanged.
fn transform_body(body: &Option<String>, config: &BodyConfig) -> Option<String> {
    let body_str = match body {
        Some(b) if !b.is_empty() => b,
        _ => return body.clone(),
    };

    let mut json: Value = match serde_json::from_str(body_str) {
        Ok(v) => v,
        Err(_) => {
            log_message(1, "Body is not valid JSON, skipping body transformations");
            return body.clone();
        }
    };

    for pointer_str in &config.remove {
        match Pointer::parse(pointer_str) {
            Ok(ptr) => {
                json.delete(ptr);
            }
            Err(e) => {
                log_message(1, &format!("Invalid JSON Pointer '{}': {}", pointer_str, e));
            }
        }
    }

    for (old_pointer_str, new_pointer_str) in &config.rename {
        let (old_ptr, new_ptr) = match (
            Pointer::parse(old_pointer_str),
            Pointer::parse(new_pointer_str),
        ) {
            (Ok(old), Ok(new)) => (old, new),
            (Err(e), _) => {
                log_message(
                    1,
                    &format!("Invalid old pointer '{}': {}", old_pointer_str, e),
                );
                continue;
            }
            (_, Err(e)) => {
                log_message(
                    1,
                    &format!("Invalid new pointer '{}': {}", new_pointer_str, e),
                );
                continue;
            }
        };

        if old_ptr == new_ptr {
            continue;
        }

        if let Ok(value) = old_ptr.resolve(&json) {
            let value_clone = value.clone();

            if let Err(e) = json.assign(new_ptr, value_clone) {
                log_message(
                    1,
                    &format!(
                        "Failed to rename '{}' to '{}': {}",
                        old_pointer_str, new_pointer_str, e
                    ),
                );
                continue;
            }

            json.delete(old_ptr);
        }
    }

    for (pointer_str, value_str) in &config.add {
        let json_value = Value::String(value_str.clone());

        match Pointer::parse(pointer_str) {
            Ok(ptr) => {
                if let Err(e) = json.assign(ptr, json_value) {
                    log_message(1, &format!("Failed to add '{}': {}", pointer_str, e));
                }
            }
            Err(e) => {
                log_message(1, &format!("Invalid JSON Pointer '{}': {}", pointer_str, e));
            }
        }
    }

    match serde_json::to_string(&json) {
        Ok(s) => Some(s),
        Err(e) => {
            log_message(
                0,
                &format!("Failed to serialize JSON after transformation: {}", e),
            );
            body.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Host function bindings
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(_level: i32, _msg: &str) {
    // No-op on native
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_response() -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("server".to_string(), "upstream/1.0".to_string());
        headers.insert("x-powered-by".to_string(), "Express".to_string());

        Response {
            status: 200,
            headers,
            body: Some(r#"{"user":"john","age":30}"#.to_string()),
        }
    }

    // -- Status code mapping tests ------------------------------------------

    #[test]
    fn test_status_mapping_basic() {
        let mut mapping = BTreeMap::new();
        mapping.insert("200".to_string(), 201);

        assert_eq!(transform_status(200, &mapping), 201);
    }

    #[test]
    fn test_status_mapping_unmapped_passthrough() {
        let mut mapping = BTreeMap::new();
        mapping.insert("200".to_string(), 201);

        assert_eq!(transform_status(404, &mapping), 404);
    }

    #[test]
    fn test_status_mapping_multiple() {
        let mut mapping = BTreeMap::new();
        mapping.insert("200".to_string(), 201);
        mapping.insert("400".to_string(), 403);
        mapping.insert("500".to_string(), 503);

        assert_eq!(transform_status(200, &mapping), 201);
        assert_eq!(transform_status(400, &mapping), 403);
        assert_eq!(transform_status(500, &mapping), 503);
        assert_eq!(transform_status(302, &mapping), 302);
    }

    #[test]
    fn test_status_mapping_empty() {
        let mapping = BTreeMap::new();

        assert_eq!(transform_status(200, &mapping), 200);
    }

    // -- Header transformation tests ----------------------------------------

    #[test]
    fn test_headers_add() {
        let resp = create_test_response();
        let mut headers = resp.headers.clone();

        let mut config = HeaderConfig::default();
        config
            .add
            .insert("x-gateway".to_string(), "barbacane".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.get("x-gateway"), Some(&"barbacane".to_string()));
    }

    #[test]
    fn test_headers_add_overwrites_existing() {
        let resp = create_test_response();
        let mut headers = resp.headers.clone();

        let mut config = HeaderConfig::default();
        config
            .add
            .insert("server".to_string(), "barbacane/1.0".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.get("server"), Some(&"barbacane/1.0".to_string()));
    }

    #[test]
    fn test_headers_set_does_not_overwrite() {
        let resp = create_test_response();
        let mut headers = resp.headers.clone();

        let mut config = HeaderConfig::default();
        config
            .set
            .insert("server".to_string(), "should-not-overwrite".to_string());
        config
            .set
            .insert("x-new-header".to_string(), "new-value".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.get("server"), Some(&"upstream/1.0".to_string()));
        assert_eq!(headers.get("x-new-header"), Some(&"new-value".to_string()));
    }

    #[test]
    fn test_headers_remove() {
        let resp = create_test_response();
        let mut headers = resp.headers.clone();

        let mut config = HeaderConfig::default();
        config.remove.push("server".to_string());
        config.remove.push("x-powered-by".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.get("server"), None);
        assert_eq!(headers.get("x-powered-by"), None);
        assert_eq!(
            headers.get("content-type"),
            Some(&"application/json".to_string())
        );
    }

    #[test]
    fn test_headers_remove_case_insensitive() {
        let resp = create_test_response();
        let mut headers = resp.headers.clone();

        let mut config = HeaderConfig::default();
        config.remove.push("Server".to_string());
        config.remove.push("X-Powered-By".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.get("server"), None);
        assert_eq!(headers.get("x-powered-by"), None);
    }

    #[test]
    fn test_headers_rename() {
        let resp = create_test_response();
        let mut headers = resp.headers.clone();

        let mut config = HeaderConfig::default();
        config
            .rename
            .insert("x-powered-by".to_string(), "x-engine".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.get("x-powered-by"), None);
        assert_eq!(headers.get("x-engine"), Some(&"Express".to_string()));
    }

    #[test]
    fn test_headers_rename_nonexistent() {
        let resp = create_test_response();
        let mut headers = resp.headers.clone();
        let original_len = headers.len();

        let mut config = HeaderConfig::default();
        config
            .rename
            .insert("x-nonexistent".to_string(), "x-new".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.len(), original_len);
        assert_eq!(headers.get("x-new"), None);
    }

    #[test]
    fn test_headers_transformation_order() {
        let resp = create_test_response();
        let mut headers = resp.headers.clone();
        headers.insert("to-rename".to_string(), "value".to_string());

        let mut config = HeaderConfig::default();
        config.remove.push("server".to_string());
        config
            .rename
            .insert("to-rename".to_string(), "renamed".to_string());
        config
            .set
            .insert("x-set".to_string(), "set-value".to_string());
        config
            .add
            .insert("x-add".to_string(), "add-value".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.get("server"), None);
        assert_eq!(headers.get("renamed"), Some(&"value".to_string()));
        assert_eq!(headers.get("x-set"), Some(&"set-value".to_string()));
        assert_eq!(headers.get("x-add"), Some(&"add-value".to_string()));
    }

    #[test]
    fn test_headers_combined_add_and_remove() {
        let mut headers = BTreeMap::new();
        headers.insert("server".to_string(), "nginx".to_string());
        headers.insert("x-powered-by".to_string(), "PHP".to_string());

        let mut config = HeaderConfig::default();
        config.remove.push("server".to_string());
        config.remove.push("x-powered-by".to_string());
        config
            .add
            .insert("x-gateway".to_string(), "barbacane".to_string());
        config
            .add
            .insert("x-frame-options".to_string(), "DENY".to_string());

        transform_headers(&mut headers, &config);

        assert_eq!(headers.get("server"), None);
        assert_eq!(headers.get("x-powered-by"), None);
        assert_eq!(headers.get("x-gateway"), Some(&"barbacane".to_string()));
        assert_eq!(headers.get("x-frame-options"), Some(&"DENY".to_string()));
    }

    #[test]
    fn test_headers_remove_and_readd_same_key() {
        let mut headers = BTreeMap::new();
        headers.insert("x-token".to_string(), "old-value".to_string());

        let mut config = HeaderConfig::default();
        config.remove.push("x-token".to_string());
        config
            .add
            .insert("x-token".to_string(), "new-value".to_string());

        transform_headers(&mut headers, &config);

        // remove runs before add — key is re-added with the new value
        assert_eq!(headers.get("x-token"), Some(&"new-value".to_string()));
    }

    // -- Body transformation tests ------------------------------------------

    #[test]
    fn test_body_add_simple_field() {
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .add
            .insert("/gateway".to_string(), "barbacane".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
        assert_eq!(json["gateway"], "barbacane");
    }

    #[test]
    fn test_body_add_nested_field() {
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .add
            .insert("/metadata/gateway".to_string(), "barbacane".to_string());
        config
            .add
            .insert("/metadata/version".to_string(), "1.0".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
        assert_eq!(json["metadata"]["gateway"], "barbacane");
        assert_eq!(json["metadata"]["version"], "1.0");
    }

    #[test]
    fn test_body_add_overwrites_existing() {
        let body = Some(r#"{"status":"old"}"#.to_string());

        let mut config = BodyConfig::default();
        config.add.insert("/status".to_string(), "new".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["status"], "new");
    }

    #[test]
    fn test_body_remove_field() {
        let body = Some(r#"{"user":"john","password":"secret","age":30}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/password".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
        assert_eq!(json["age"], 30);
        assert_eq!(json.get("password"), None);
    }

    #[test]
    fn test_body_remove_nested_field() {
        let body =
            Some(r#"{"user":"john","metadata":{"internal":true,"public":"yes"}}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/metadata/internal".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
        assert_eq!(json["metadata"]["public"], "yes");
        assert_eq!(json["metadata"].get("internal"), None);
    }

    #[test]
    fn test_body_rename_field() {
        let body = Some(r#"{"userName":"john","age":30}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .rename
            .insert("/userName".to_string(), "/user_name".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user_name"], "john");
        assert_eq!(json["age"], 30);
        assert_eq!(json.get("userName"), None);
    }

    #[test]
    fn test_body_rename_nested_field() {
        let body = Some(r#"{"metadata":{"oldName":"value"}}"#.to_string());

        let mut config = BodyConfig::default();
        config.rename.insert(
            "/metadata/oldName".to_string(),
            "/metadata/newName".to_string(),
        );

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["metadata"]["newName"], "value");
        assert_eq!(json["metadata"].get("oldName"), None);
    }

    #[test]
    fn test_body_transformation_order() {
        let body = Some(r#"{"toRemove":"x","toRename":"y","toOverwrite":"old"}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/toRemove".to_string());
        config
            .rename
            .insert("/toRename".to_string(), "/renamed".to_string());
        config
            .add
            .insert("/toOverwrite".to_string(), "new".to_string());
        config.add.insert("/added".to_string(), "value".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json.get("toRemove"), None);
        assert_eq!(json["renamed"], "y");
        assert_eq!(json["toOverwrite"], "new");
        assert_eq!(json["added"], "value");
    }

    #[test]
    fn test_body_non_json() {
        let body = Some("not json".to_string());

        let mut config = BodyConfig::default();
        config.add.insert("/field".to_string(), "value".to_string());

        let result = transform_body(&body, &config);
        assert_eq!(result, Some("not json".to_string()));
    }

    #[test]
    fn test_body_empty() {
        let body: Option<String> = None;

        let mut config = BodyConfig::default();
        config.add.insert("/field".to_string(), "value".to_string());

        let result = transform_body(&body, &config);
        assert_eq!(result, None);
    }

    #[test]
    fn test_body_empty_string() {
        let body = Some(String::new());

        let mut config = BodyConfig::default();
        config.add.insert("/field".to_string(), "value".to_string());

        let result = transform_body(&body, &config);
        assert_eq!(result, Some(String::new()));
    }

    #[test]
    fn test_body_nonexistent_pointer() {
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/nonexistent/deeply/nested".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
    }

    #[test]
    fn test_body_array_pointer() {
        let body = Some(r#"{"items":[{"id":1},{"id":2}]}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .add
            .insert("/items/0/gateway".to_string(), "barbacane".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["items"][0]["gateway"], "barbacane");
        assert_eq!(json["items"][1].get("gateway"), None);
    }

    #[test]
    fn test_body_rename_same_pointer() {
        let body = Some(r#"{"field":"value","other":"keep"}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .rename
            .insert("/field".to_string(), "/field".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        // rename with identical source and destination is a no-op — field must survive
        assert_eq!(json["field"], "value");
        assert_eq!(json["other"], "keep");
    }

    #[test]
    fn test_body_rename_array_element_field() {
        let body = Some(r#"{"items":[{"oldKey":"val1"},{"oldKey":"val2"}]}"#.to_string());

        let mut config = BodyConfig::default();
        config.rename.insert(
            "/items/0/oldKey".to_string(),
            "/items/0/newKey".to_string(),
        );

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["items"][0]["newKey"], "val1");
        assert_eq!(json["items"][0].get("oldKey"), None);
        // items[1] is untouched
        assert_eq!(json["items"][1]["oldKey"], "val2");
    }

    #[test]
    fn test_body_remove_array_element_field() {
        let body =
            Some(r#"{"items":[{"id":1,"secret":"x"},{"id":2,"secret":"y"}]}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/items/0/secret".to_string());

        let result = transform_body(&body, &config);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["items"][0]["id"], 1);
        assert_eq!(json["items"][0].get("secret"), None);
        // items[1] is untouched
        assert_eq!(json["items"][1]["secret"], "y");
    }

    // -- Integration / full plugin tests ------------------------------------

    #[test]
    fn test_on_response_combined() {
        let mut plugin = ResponseTransformer {
            status: Some({
                let mut m = BTreeMap::new();
                m.insert("200".to_string(), 201);
                m
            }),
            headers: Some(HeaderConfig {
                add: {
                    let mut m = BTreeMap::new();
                    m.insert("x-gateway".to_string(), "barbacane".to_string());
                    m
                },
                set: BTreeMap::new(),
                remove: vec!["server".to_string()],
                rename: BTreeMap::new(),
            }),
            body: Some(BodyConfig {
                add: {
                    let mut m = BTreeMap::new();
                    m.insert("/gateway".to_string(), "barbacane".to_string());
                    m
                },
                remove: vec!["/internal".to_string()],
                rename: BTreeMap::new(),
            }),
        };

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("server".to_string(), "nginx".to_string());

        let resp = Response {
            status: 200,
            headers,
            body: Some(r#"{"data":"value","internal":"secret"}"#.to_string()),
        };

        let result = plugin.on_response(resp);

        assert_eq!(result.status, 201);
        assert_eq!(
            result.headers.get("x-gateway"),
            Some(&"barbacane".to_string())
        );
        assert_eq!(result.headers.get("server"), None);

        let json: Value = serde_json::from_str(result.body.as_ref().expect("should have body"))
            .expect("valid json");
        assert_eq!(json["data"], "value");
        assert_eq!(json["gateway"], "barbacane");
        assert_eq!(json.get("internal"), None);
    }

    #[test]
    fn test_on_response_empty_config() {
        let mut plugin = ResponseTransformer {
            status: None,
            headers: None,
            body: None,
        };

        let resp = create_test_response();
        let original_status = resp.status;
        let original_headers = resp.headers.clone();
        let original_body = resp.body.clone();

        let result = plugin.on_response(resp);

        assert_eq!(result.status, original_status);
        assert_eq!(result.headers, original_headers);
        assert_eq!(result.body, original_body);
    }

    #[test]
    fn test_on_request_passthrough() {
        let mut plugin = ResponseTransformer {
            status: Some({
                let mut m = BTreeMap::new();
                m.insert("200".to_string(), 201);
                m
            }),
            headers: Some(HeaderConfig {
                add: {
                    let mut m = BTreeMap::new();
                    m.insert("x-gateway".to_string(), "barbacane".to_string());
                    m
                },
                set: BTreeMap::new(),
                remove: vec![],
                rename: BTreeMap::new(),
            }),
            body: None,
        };

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        };

        let result = plugin.on_request(req.clone());
        let Action::Continue(modified) = result else {
            panic!("Expected Action::Continue");
        };
        assert_eq!(modified.method, req.method);
        assert_eq!(modified.path, req.path);
        assert_eq!(modified.headers, req.headers);
        assert_eq!(modified.body, req.body);
    }

    #[test]
    fn test_config_deserialization_defaults() {
        let config: ResponseTransformer = serde_json::from_str("{}").expect("valid json");
        assert!(config.status.is_none());
        assert!(config.headers.is_none());
        assert!(config.body.is_none());
    }

    #[test]
    fn test_config_deserialization_full() {
        let json = r#"{
            "status": {
                "200": 201,
                "400": 403,
                "500": 503
            },
            "headers": {
                "add": {"x-gateway": "barbacane"},
                "set": {"x-frame-options": "DENY"},
                "remove": ["server", "x-powered-by"],
                "rename": {"x-old": "x-new"}
            },
            "body": {
                "add": {"/gateway": "barbacane"},
                "remove": ["/internal"],
                "rename": {"/oldName": "/newName"}
            }
        }"#;

        let config: ResponseTransformer = serde_json::from_str(json).expect("valid json");
        assert!(config.status.is_some());
        assert!(config.headers.is_some());
        assert!(config.body.is_some());

        let status = config.status.expect("set above");
        assert_eq!(status.get("200"), Some(&201));
        assert_eq!(status.get("400"), Some(&403));
        assert_eq!(status.get("500"), Some(&503));

        let h = config.headers.expect("set above");
        assert_eq!(h.add.get("x-gateway"), Some(&"barbacane".to_string()));
        assert_eq!(h.set.get("x-frame-options"), Some(&"DENY".to_string()));
        assert_eq!(h.remove, vec!["server", "x-powered-by"]);
        assert_eq!(h.rename.get("x-old"), Some(&"x-new".to_string()));
    }

    #[test]
    fn test_status_only_config() {
        let mut plugin = ResponseTransformer {
            status: Some({
                let mut m = BTreeMap::new();
                m.insert("200".to_string(), 201);
                m.insert("400".to_string(), 422);
                m
            }),
            headers: None,
            body: None,
        };

        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        };

        let result = plugin.on_response(resp);
        assert_eq!(result.status, 201);

        let resp2 = Response {
            status: 400,
            headers: BTreeMap::new(),
            body: None,
        };

        let result2 = plugin.on_response(resp2);
        assert_eq!(result2.status, 422);

        let resp3 = Response {
            status: 500,
            headers: BTreeMap::new(),
            body: None,
        };

        let result3 = plugin.on_response(resp3);
        assert_eq!(result3.status, 500);
    }
}
