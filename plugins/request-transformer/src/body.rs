//! JSON body transformation functions using JSON Pointer (RFC 6901).

use crate::config::BodyConfig;
use crate::interpolation::interpolate_value;
use barbacane_plugin_sdk::prelude::*;
use jsonptr::{Assign, Delete, Pointer};
use serde_json::Value;

/// Transform JSON body according to configuration.
///
/// Non-JSON bodies are returned unchanged. Errors are logged but don't fail the request.
pub fn transform_body(body: &Option<String>, config: &BodyConfig, req: &Request) -> Option<String> {
    let body_str = match body {
        Some(b) if !b.is_empty() => b,
        _ => return body.clone(),
    };

    // Try to parse as JSON
    let mut json: Value = match serde_json::from_str(body_str) {
        Ok(v) => v,
        Err(_) => {
            // Not valid JSON - log and pass through unchanged
            log_message(1, "Body is not valid JSON, skipping body transformations");
            return body.clone();
        }
    };

    // Apply remove operations first
    for pointer_str in &config.remove {
        match Pointer::parse(pointer_str) {
            Ok(ptr) => {
                json.delete(&ptr);  // Returns Option, None means path didn't exist
            }
            Err(e) => {
                log_message(1, &format!("Invalid JSON Pointer '{}': {}", pointer_str, e));
            }
        }
    }

    // Apply rename operations (get → assign → delete)
    for (old_pointer_str, new_pointer_str) in &config.rename {
        // Parse pointers
        let (old_ptr, new_ptr) = match (Pointer::parse(old_pointer_str), Pointer::parse(new_pointer_str)) {
            (Ok(old), Ok(new)) => (old, new),
            (Err(e), _) => {
                log_message(1, &format!("Invalid old pointer '{}': {}", old_pointer_str, e));
                continue;
            }
            (_, Err(e)) => {
                log_message(1, &format!("Invalid new pointer '{}': {}", new_pointer_str, e));
                continue;
            }
        };

        // Get value at old pointer
        if let Some(value) = json.pointer(old_pointer_str) {
            let value_clone = value.clone();

            // Assign to new pointer
            if let Err(e) = json.assign(&new_ptr, value_clone) {
                log_message(1, &format!("Failed to rename '{}' to '{}': {}", old_pointer_str, new_pointer_str, e));
                continue;
            }

            // Delete old pointer
            json.delete(&old_ptr);
        }
    }

    // Apply add operations (with variable interpolation)
    for (pointer_str, value_template) in &config.add {
        let interpolated_value = interpolate_value(value_template, req);

        match Pointer::parse(pointer_str) {
            Ok(ptr) => {
                // Assign the value (creates intermediate objects if needed)
                if let Err(e) = json.assign(&ptr, Value::String(interpolated_value)) {
                    log_message(1, &format!("Failed to add '{}': {}", pointer_str, e));
                }
            }
            Err(e) => {
                log_message(1, &format!("Invalid JSON Pointer '{}': {}", pointer_str, e));
            }
        }
    }

    // Serialize back to JSON string
    match serde_json::to_string(&json) {
        Ok(s) => Some(s),
        Err(e) => {
            log_message(0, &format!("Failed to serialize JSON after transformation: {}", e));
            body.clone() // Return original on serialization error
        }
    }
}

/// Log a message via host_log.
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn create_test_request() -> Request {
        let mut path_params = BTreeMap::new();
        path_params.insert("id".to_string(), "123".to_string());

        Request {
            method: "POST".to_string(),
            path: "/users/123".to_string(),
            query: Some("page=2".to_string()),
            headers: BTreeMap::new(),
            body: Some(r#"{"user":"john","age":30}"#.to_string()),
            client_ip: "192.168.1.1".to_string(),
            path_params,
        }
    }

    #[test]
    fn test_add_simple_field() {
        let req = create_test_request();
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config.add.insert("/gateway".to_string(), "barbacane".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json["user"], "john");
        assert_eq!(json["gateway"], "barbacane");
    }

    #[test]
    fn test_add_nested_field() {
        let req = create_test_request();
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config.add.insert("/metadata/gateway".to_string(), "barbacane".to_string());
        config.add.insert("/metadata/version".to_string(), "1.0".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json["user"], "john");
        assert_eq!(json["metadata"]["gateway"], "barbacane");
        assert_eq!(json["metadata"]["version"], "1.0");
    }

    #[test]
    fn test_add_with_interpolation() {
        let req = create_test_request();
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config.add.insert("/userId".to_string(), "$path.id".to_string());
        config.add.insert("/clientIp".to_string(), "$client_ip".to_string());
        config.add.insert("/page".to_string(), "$query.page".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json["userId"], "123");
        assert_eq!(json["clientIp"], "192.168.1.1");
        assert_eq!(json["page"], "2");
    }

    #[test]
    fn test_remove_field() {
        let req = create_test_request();
        let body = Some(r#"{"user":"john","password":"secret","age":30}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/password".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json["user"], "john");
        assert_eq!(json["age"], 30);
        assert_eq!(json.get("password"), None);
    }

    #[test]
    fn test_remove_nested_field() {
        let req = create_test_request();
        let body = Some(r#"{"user":"john","metadata":{"internal":true,"public":"yes"}}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/metadata/internal".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json["user"], "john");
        assert_eq!(json["metadata"]["public"], "yes");
        assert_eq!(json["metadata"].get("internal"), None);
    }

    #[test]
    fn test_rename_field() {
        let req = create_test_request();
        let body = Some(r#"{"userName":"john","age":30}"#.to_string());

        let mut config = BodyConfig::default();
        config.rename.insert("/userName".to_string(), "/user_name".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json["user_name"], "john");
        assert_eq!(json["age"], 30);
        assert_eq!(json.get("userName"), None);
    }

    #[test]
    fn test_rename_nested_field() {
        let req = create_test_request();
        let body = Some(r#"{"metadata":{"oldName":"value"}}"#.to_string());

        let mut config = BodyConfig::default();
        config.rename.insert("/metadata/oldName".to_string(), "/metadata/newName".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json["metadata"]["newName"], "value");
        assert_eq!(json["metadata"].get("oldName"), None);
    }

    #[test]
    fn test_transformation_order() {
        let req = create_test_request();
        let body = Some(r#"{"toRemove":"x","toRename":"y","toOverwrite":"old"}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/toRemove".to_string());
        config.rename.insert("/toRename".to_string(), "/renamed".to_string());
        config.add.insert("/toOverwrite".to_string(), "new".to_string());
        config.add.insert("/added".to_string(), "value".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json.get("toRemove"), None);
        assert_eq!(json["renamed"], "y");
        assert_eq!(json["toOverwrite"], "new");
        assert_eq!(json["added"], "value");
    }

    #[test]
    fn test_non_json_body() {
        let req = create_test_request();
        let body = Some("not json".to_string());

        let mut config = BodyConfig::default();
        config.add.insert("/field".to_string(), "value".to_string());

        let result = transform_body(&body, &config, &req);

        // Should return original body unchanged
        assert_eq!(result, Some("not json".to_string()));
    }

    #[test]
    fn test_empty_body() {
        let req = create_test_request();
        let body = None;

        let mut config = BodyConfig::default();
        config.add.insert("/field".to_string(), "value".to_string());

        let result = transform_body(&body, &config, &req);

        // Should return None unchanged
        assert_eq!(result, None);
    }

    #[test]
    fn test_invalid_pointer() {
        let req = create_test_request();
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/nonexistent/deeply/nested".to_string());

        let result = transform_body(&body, &config, &req);

        // Should not fail, just log and continue
        assert!(result.is_some());
    }

    #[test]
    fn test_array_pointer() {
        let req = create_test_request();
        let body = Some(r#"{"items":[{"id":1},{"id":2}]}"#.to_string());

        let mut config = BodyConfig::default();
        config.add.insert("/items/0/gateway".to_string(), "barbacane".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value = serde_json::from_str(&result.unwrap()).unwrap();

        assert_eq!(json["items"][0]["gateway"], "barbacane");
        assert_eq!(json["items"][1].get("gateway"), None);
    }
}
